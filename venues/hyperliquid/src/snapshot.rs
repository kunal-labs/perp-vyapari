//! A point-in-time view of the account, and pure mappings to the venue contract.
//!
//! Everything here is deterministic and unit-tested; network I/O lives in `hl.rs`.
//!
//! Hyperliquid has several perp dexes: the main one plus HIP-3 dexes such as `xyz`
//! (stocks, commodities). Each has its own clearinghouse state, open orders and
//! markets. A snapshot merges all of them into one account view.

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use futures::future::try_join_all;
use venue_proto::v1::{Account, Order, OrderKind, Position};

use crate::hl::{
    AssetCtx, ClearinghouseState, FrontendOrder, InfoClient, MarginSummary, Meta, SpotState,
};

/// What a fetch must cover beyond dexes where the account holds positions.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Fetch open orders on every dex (for `/orders`), not just dexes with positions.
    pub all_orders: bool,
    /// Extra dexes whose markets are needed (e.g. for price alerts on `xyz:NVDA`).
    pub extra_dexes: Vec<String>,
}

/// Sum per-dex clearinghouse states into one (positions concatenated).
pub fn merge_states(states: Vec<ClearinghouseState>) -> ClearinghouseState {
    let f = |s: &str| s.parse::<f64>().unwrap_or(0.0);
    let mut acc = (0.0, 0.0, 0.0, 0.0, 0.0);
    let mut positions = Vec::new();
    let mut time = 0;
    for s in states {
        acc.0 += f(&s.margin_summary.account_value);
        acc.1 += f(&s.margin_summary.total_margin_used);
        acc.2 += f(&s.margin_summary.total_ntl_pos);
        acc.3 += f(&s.cross_maintenance_margin_used);
        acc.4 += f(&s.withdrawable);
        positions.extend(s.asset_positions);
        time = time.max(s.time);
    }
    ClearinghouseState {
        margin_summary: MarginSummary {
            account_value: fmt_num(acc.0),
            total_margin_used: fmt_num(acc.1),
            total_ntl_pos: fmt_num(acc.2),
        },
        cross_maintenance_margin_used: fmt_num(acc.3),
        withdrawable: fmt_num(acc.4),
        asset_positions: positions,
        time,
    }
}

#[derive(Debug, Clone, Default)]
pub struct Market {
    pub mark_px: String,
    pub funding_1h: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: ClearinghouseState,
    pub spot: SpotState,
    pub orders: Vec<FrontendOrder>,
    pub markets: HashMap<String, Market>,
    pub abstraction: String,
}

impl Snapshot {
    pub async fn fetch(client: &InfoClient, user: &str, scope: &Scope) -> Result<Self> {
        let dexes = client.perp_dexes().await?;
        let states =
            try_join_all(dexes.iter().map(|d| client.clearinghouse_state(user, d))).await?;

        // Dexes that need markets (marks, funding) and orders (stop detection).
        let mut active: BTreeSet<String> = BTreeSet::from([String::new()]);
        for (d, s) in dexes.iter().zip(&states) {
            if !s.asset_positions.is_empty() {
                active.insert(d.clone());
            }
        }
        active.extend(
            scope
                .extra_dexes
                .iter()
                .filter(|d| dexes.contains(d))
                .cloned(),
        );
        let order_dexes: Vec<&String> = if scope.all_orders {
            dexes.iter().collect()
        } else {
            active.iter().collect()
        };

        let (spot, abstraction, orders, metas) = tokio::try_join!(
            client.spot_state(user),
            client.user_abstraction(user),
            try_join_all(order_dexes.iter().map(|d| client.open_orders(user, d))),
            try_join_all(active.iter().map(|d| client.meta_and_ctxs(d))),
        )?;
        Ok(Self::from_dexes(
            states,
            spot,
            orders.into_iter().flatten().collect(),
            metas,
            abstraction,
        ))
    }

    /// Build from several dexes' responses.
    pub fn from_dexes(
        states: Vec<ClearinghouseState>,
        spot: SpotState,
        orders: Vec<FrontendOrder>,
        metas: Vec<(Meta, Vec<AssetCtx>)>,
        abstraction: String,
    ) -> Self {
        let mut snap = Self::from_parts(
            merge_states(states),
            spot,
            orders,
            (Meta { universe: vec![] }, vec![]),
            abstraction,
        );
        for (meta, ctxs) in metas {
            for (a, c) in meta.universe.into_iter().zip(ctxs) {
                snap.markets.insert(
                    a.name,
                    Market {
                        mark_px: c.mark_px,
                        funding_1h: c.funding,
                    },
                );
            }
        }
        snap
    }

    pub fn from_parts(
        state: ClearinghouseState,
        spot: SpotState,
        orders: Vec<FrontendOrder>,
        (meta, ctxs): (Meta, Vec<AssetCtx>),
        abstraction: String,
    ) -> Self {
        let markets = meta
            .universe
            .into_iter()
            .zip(ctxs)
            .map(|(a, c)| {
                (
                    a.name,
                    Market {
                        mark_px: c.mark_px,
                        funding_1h: c.funding,
                    },
                )
            })
            .collect();
        Self {
            state,
            spot,
            orders,
            markets,
            abstraction,
        }
    }

    pub fn is_unified(&self) -> bool {
        self.abstraction == "unifiedAccount"
    }

    pub fn account(&self) -> Account {
        let ms = &self.state.margin_summary;
        let total_collateral = if self.is_unified() {
            self.spot
                .balances
                .iter()
                .find(|b| b.coin == "USDC")
                .map(|b| b.total.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let upnl: f64 = self
            .state
            .asset_positions
            .iter()
            .filter_map(|p| p.position.unrealized_pnl.parse::<f64>().ok())
            .sum();
        Account {
            account_value: ms.account_value.clone(),
            margin_used: ms.total_margin_used.clone(),
            withdrawable: self.state.withdrawable.clone(),
            unrealized_pnl: fmt_num(upnl),
            as_of_ms: self.state.time,
            total_collateral,
            maintenance_margin: self.state.cross_maintenance_margin_used.clone(),
        }
    }

    /// Tightest reduce-only stop protecting a position of signed size `szi`.
    pub fn exchange_stop(&self, coin: &str, szi: f64) -> Option<f64> {
        // A long is protected by a sell ("A") stop; a short by a buy ("B") stop.
        let closing_side = if szi > 0.0 { "A" } else { "B" };
        let stops = self.orders.iter().filter(|o| {
            o.coin == coin
                && o.is_trigger
                && o.reduce_only
                && o.side == closing_side
                && o.order_type.starts_with("Stop")
        });
        let prices = stops.filter_map(|o| o.trigger_px.parse::<f64>().ok());
        if szi > 0.0 {
            prices.fold(None, |acc: Option<f64>, p| {
                Some(acc.map_or(p, |a| a.max(p)))
            })
        } else {
            prices.fold(None, |acc: Option<f64>, p| {
                Some(acc.map_or(p, |a| a.min(p)))
            })
        }
    }

    pub fn positions(&self) -> Vec<Position> {
        self.state
            .asset_positions
            .iter()
            .map(|ap| {
                let p = &ap.position;
                let szi: f64 = p.szi.parse().unwrap_or(0.0);
                let market = self.markets.get(&p.coin).cloned().unwrap_or_default();
                let stop = self.exchange_stop(&p.coin, szi);
                Position {
                    symbol: p.coin.clone(),
                    size: p.szi.clone(),
                    entry_price: p.entry_px.clone().unwrap_or_default(),
                    mark_price: market.mark_px,
                    unrealized_pnl: p.unrealized_pnl.clone(),
                    liquidation_price: p.liquidation_px.clone().unwrap_or_default(),
                    leverage: format!("{}x {}", p.leverage.value, p.leverage.kind),
                    margin_used: p.margin_used.clone(),
                    has_exchange_stop: stop.is_some(),
                    stop_price: stop.map(fmt_num).unwrap_or_default(),
                    funding_rate_1h: market.funding_1h,
                }
            })
            .collect()
    }

    pub fn orders(&self, symbol_filter: &str) -> Vec<Order> {
        self.orders
            .iter()
            .filter(|o| symbol_filter.is_empty() || o.coin.eq_ignore_ascii_case(symbol_filter))
            .map(map_order)
            .collect()
    }
}

pub fn map_order(o: &FrontendOrder) -> Order {
    let kind = match o.order_type.as_str() {
        "Stop Market" => OrderKind::StopMarket,
        "Stop Limit" => OrderKind::StopLimit,
        "Take Profit Market" => OrderKind::TakeProfitMarket,
        "Take Profit Limit" => OrderKind::TakeProfitLimit,
        _ => OrderKind::Limit,
    };
    let size = if o.side == "A" {
        format!("-{}", o.sz)
    } else {
        o.sz.clone()
    };
    Order {
        order_id: o.oid.to_string(),
        symbol: o.coin.clone(),
        size,
        limit_price: o.limit_px.clone(),
        trigger_price: if o.is_trigger {
            o.trigger_px.clone()
        } else {
            String::new()
        },
        kind: kind.into(),
        reduce_only: o.reduce_only,
        created_ms: o.timestamp,
    }
}

/// Compact decimal formatting without float noise (max 8 dp, trailing zeros trimmed).
pub fn fmt_num(x: f64) -> String {
    let s = format!("{x:.8}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const STATE: &str = r#"{"marginSummary":{"accountValue":"320.5","totalNtlPos":"6000.0","totalRawUsd":"-5679.5","totalMarginUsed":"300.0"},
      "crossMaintenanceMarginUsed":"75.0","withdrawable":"0.0",
      "assetPositions":[{"type":"oneWay","position":{"coin":"BTC","szi":"0.1","leverage":{"type":"cross","value":20},
        "entryPx":"59000.0","positionValue":"6000.0","unrealizedPnl":"20.5","returnOnEquity":"0.07","liquidationPx":"56000.0",
        "marginUsed":"300.0","maxLeverage":40}},
        {"type":"oneWay","position":{"coin":"ETH","szi":"-2.0","leverage":{"type":"isolated","value":5},
        "entryPx":"3000.0","positionValue":"6100.0","unrealizedPnl":"-100.0","liquidationPx":null,"marginUsed":"1200.0","maxLeverage":25}}],
      "time":1795222383028}"#;
    pub const SPOT: &str = r#"{"balances":[{"coin":"USDC","token":0,"total":"400.25","hold":"400.25","entryNtl":"0.0"}]}"#;
    pub const ORDERS: &str = r#"[
      {"coin":"BTC","side":"A","limitPx":"53000.0","sz":"0.1","oid":11,"timestamp":1795213643987,"triggerCondition":"Price below 58000","isTrigger":true,"triggerPx":"58000.0","children":[],"isPositionTpsl":false,"reduceOnly":true,"orderType":"Stop Market","origSz":"0.1","tif":null,"cloid":null},
      {"coin":"BTC","side":"A","limitPx":"53000.0","sz":"0.05","oid":12,"timestamp":1795213643987,"isTrigger":true,"triggerPx":"58500.0","reduceOnly":true,"orderType":"Stop Market"},
      {"coin":"BTC","side":"A","limitPx":"55000.0","sz":"0.1","oid":13,"timestamp":1795213643987,"isTrigger":true,"triggerPx":"60000.0","reduceOnly":true,"orderType":"Take Profit Market"},
      {"coin":"ETH","side":"B","limitPx":"2900.0","sz":"1.0","oid":14,"timestamp":1795213643000,"isTrigger":false,"triggerPx":"0.0","reduceOnly":false,"orderType":"Limit"}
    ]"#;
    pub const META: &str = r#"[{"universe":[{"szDecimals":5,"name":"BTC","maxLeverage":40},{"szDecimals":4,"name":"ETH","maxLeverage":25}]},
      [{"funding":"0.0000125","markPx":"59205.0","midPx":"59205.5"},{"funding":"-0.00003","markPx":"3050.0","midPx":"3050.1"}]]"#;

    pub fn snapshot(abstraction: &str) -> Snapshot {
        Snapshot::from_parts(
            serde_json::from_str(STATE).unwrap(),
            serde_json::from_str(SPOT).unwrap(),
            serde_json::from_str(ORDERS).unwrap(),
            serde_json::from_str(META).unwrap(),
            abstraction.into(),
        )
    }

    #[test]
    fn account_unified_reports_spot_collateral() {
        let a = snapshot("unifiedAccount").account();
        assert_eq!(a.account_value, "320.5");
        assert_eq!(a.total_collateral, "400.25");
        assert_eq!(a.unrealized_pnl, "-79.5");
        assert_eq!(a.maintenance_margin, "75.0");
        assert_eq!(snapshot("default").account().total_collateral, "");
    }

    #[test]
    fn long_stop_is_highest_sell_stop_and_ignores_take_profit() {
        let s = snapshot("unifiedAccount");
        assert_eq!(s.exchange_stop("BTC", 0.1), Some(58500.0));
    }

    #[test]
    fn short_without_buy_stop_is_unprotected() {
        // The ETH buy order is a plain, non-reduce-only limit: not a stop.
        assert_eq!(snapshot("default").exchange_stop("ETH", -2.0), None);
    }

    #[test]
    fn positions_carry_mark_funding_and_stop() {
        let ps = snapshot("default").positions();
        let btc = &ps[0];
        assert_eq!(btc.symbol, "BTC");
        assert_eq!(btc.mark_price, "59205.0");
        assert_eq!(btc.funding_rate_1h, "0.0000125");
        assert!(btc.has_exchange_stop);
        assert_eq!(btc.stop_price, "58500");
        assert_eq!(btc.leverage, "20x cross");
        let eth = &ps[1];
        assert!(!eth.has_exchange_stop);
        assert_eq!(eth.liquidation_price, "");
    }

    #[test]
    fn orders_map_kind_side_and_filter() {
        let s = snapshot("default");
        let all = s.orders("");
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].kind(), OrderKind::StopMarket);
        assert_eq!(all[0].size, "-0.1");
        assert_eq!(all[2].kind(), OrderKind::TakeProfitMarket);
        assert_eq!(all[3].trigger_price, "");
        assert_eq!(s.orders("eth").len(), 1);
    }

    #[test]
    fn merges_hip3_dex_into_one_view() {
        let main: ClearinghouseState = serde_json::from_str(STATE).unwrap();
        let xyz: ClearinghouseState = serde_json::from_str(
            r#"{"marginSummary":{"accountValue":"100.0","totalNtlPos":"500.0","totalRawUsd":"0","totalMarginUsed":"50.0"},
              "crossMaintenanceMarginUsed":"10.0","withdrawable":"5.0",
              "assetPositions":[{"type":"oneWay","position":{"coin":"xyz:NVDA","szi":"-3.0","leverage":{"type":"cross","value":10},
                "entryPx":"180.0","positionValue":"540.0","unrealizedPnl":"-6.0","liquidationPx":"230.0","marginUsed":"54.0"}}],
              "time":1795222383999}"#,
        )
        .unwrap();
        let xyz_orders: Vec<FrontendOrder> = serde_json::from_str(
            r#"[{"coin":"xyz:NVDA","side":"B","limitPx":"200","sz":"3","oid":77,"timestamp":0,"isTrigger":true,"triggerPx":"187.0","reduceOnly":true,"orderType":"Stop Market"}]"#,
        )
        .unwrap();
        let xyz_meta: (Meta, Vec<AssetCtx>) = serde_json::from_str(
            r#"[{"universe":[{"szDecimals":3,"name":"xyz:NVDA","maxLeverage":20}]},[{"funding":"0.00001","markPx":"182.0"}]]"#,
        )
        .unwrap();
        let mut orders: Vec<FrontendOrder> = serde_json::from_str(ORDERS).unwrap();
        orders.extend(xyz_orders);
        let snap = Snapshot::from_dexes(
            vec![main, xyz],
            serde_json::from_str(SPOT).unwrap(),
            orders,
            vec![serde_json::from_str(META).unwrap(), xyz_meta],
            "unifiedAccount".into(),
        );
        let a = snap.account();
        assert_eq!(a.account_value, "420.5");
        assert_eq!(a.margin_used, "350");
        assert_eq!(a.unrealized_pnl, "-85.5");
        let ps = snap.positions();
        assert_eq!(ps.len(), 3);
        let nvda = ps.iter().find(|p| p.symbol == "xyz:NVDA").unwrap();
        assert!(nvda.has_exchange_stop);
        assert_eq!(nvda.stop_price, "187");
        assert_eq!(nvda.mark_price, "182.0");
        assert_eq!(snap.state.time, 1795222383999);
    }

    #[test]
    fn fmt_num_trims() {
        assert_eq!(fmt_num(58500.0), "58500");
        assert_eq!(fmt_num(0.000012500), "0.0000125");
        assert_eq!(fmt_num(-0.0), "0");
    }
}
