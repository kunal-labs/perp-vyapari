//! Telegram message formatting (HTML parse mode). Pure functions.

use venue_proto::v1::{
    event, Account, Event, Order, OrderKind, Position, PriceAlert, PriceDirection, Severity,
};

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn num(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

/// Thousands-separated with fixed decimals.
pub fn money(s: &str, dp: usize) -> String {
    let x = num(s);
    let neg = x < 0.0;
    let raw = format!("{:.*}", dp, x.abs());
    let (int, frac) = raw
        .split_once('.')
        .map_or((raw.as_str(), ""), |(a, b)| (a, b));
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    if neg && x != 0.0 {
        format!("-{out}")
    } else {
        out
    }
}

fn signed_money(s: &str) -> String {
    let m = money(s, 2);
    if num(s) > 0.0 {
        format!("+{m}")
    } else {
        m
    }
}

pub fn account(venue: &str, a: &Account) -> String {
    let mut out = format!(
        "<b>{}</b> account\nEquity (perps): <b>{}</b>\nUnrealised PnL: {}\nMargin used: {}\nMaintenance: {}\nWithdrawable: {}",
        esc(venue),
        money(&a.account_value, 2),
        signed_money(&a.unrealized_pnl),
        money(&a.margin_used, 2),
        money(&a.maintenance_margin, 2),
        money(&a.withdrawable, 2),
    );
    if !a.total_collateral.is_empty() {
        out.push_str(&format!(
            "\nCollateral (unified USDC): {}",
            money(&a.total_collateral, 2)
        ));
    }
    out
}

pub fn positions(venue: &str, ps: &[Position]) -> String {
    if ps.is_empty() {
        return format!("<b>{}</b>: no open positions", esc(venue));
    }
    let mut out = format!("<b>{}</b> positions", esc(venue));
    for p in ps {
        let size = num(&p.size);
        let side = if size > 0.0 { "LONG" } else { "SHORT" };
        let stop = if p.has_exchange_stop {
            format!("🛡 stop {}", money(&p.stop_price, 2))
        } else {
            "⚠️ <b>NO STOP</b>".to_string()
        };
        let liq = if p.liquidation_price.is_empty() {
            "liq –".to_string()
        } else {
            let (m, l) = (num(&p.mark_price), num(&p.liquidation_price));
            let dist = if m > 0.0 {
                (m - l).abs() / m * 100.0
            } else {
                0.0
            };
            format!("liq {} ({dist:.1}% away)", money(&p.liquidation_price, 2))
        };
        out.push_str(&format!(
            "\n\n<b>{} {side}</b> {} @ {} · {}\nmark {} · uPnL {}\n{stop} · {liq}",
            esc(&p.symbol),
            esc(p.size.trim_start_matches('-')),
            money(&p.entry_price, 2),
            esc(&p.leverage),
            money(&p.mark_price, 2),
            signed_money(&p.unrealized_pnl),
        ));
    }
    out
}

fn kind_label(o: &Order) -> &'static str {
    match o.kind() {
        OrderKind::StopMarket => "stop",
        OrderKind::StopLimit => "stop-limit",
        OrderKind::TakeProfitMarket => "take-profit",
        OrderKind::TakeProfitLimit => "tp-limit",
        OrderKind::Limit | OrderKind::Unspecified => "limit",
    }
}

pub fn orders(venue: &str, os: &[Order]) -> String {
    if os.is_empty() {
        return format!("<b>{}</b>: no open orders", esc(venue));
    }
    let mut out = format!("<b>{}</b> open orders", esc(venue));
    for o in os {
        let side = if num(&o.size) < 0.0 { "sell" } else { "buy" };
        let px = if o.trigger_price.is_empty() {
            format!("@ {}", money(&o.limit_price, 2))
        } else {
            format!("trigger {}", money(&o.trigger_price, 2))
        };
        out.push_str(&format!(
            "\n• {} {side} {} {} {px}{}",
            esc(&o.symbol),
            esc(o.size.trim_start_matches('-')),
            kind_label(o),
            if o.reduce_only { " (reduce-only)" } else { "" },
        ));
    }
    out
}

pub fn pnl(venue: &str, a: &Account, ps: &[Position]) -> String {
    let mut out = format!(
        "<b>{}</b> unrealised PnL: <b>{}</b>",
        esc(venue),
        signed_money(&a.unrealized_pnl)
    );
    for p in ps {
        out.push_str(&format!(
            "\n• {} {}",
            esc(&p.symbol),
            signed_money(&p.unrealized_pnl)
        ));
    }
    out
}

pub fn price_alerts(venue: &str, alerts: &[PriceAlert]) -> String {
    if alerts.is_empty() {
        return format!("<b>{}</b>: no price alerts", esc(venue));
    }
    let mut out = format!("<b>{}</b> price alerts", esc(venue));
    for a in alerts {
        let dir = if a.direction() == PriceDirection::Above {
            "≥"
        } else {
            "≤"
        };
        out.push_str(&format!(
            "\n• <code>{}</code> {} {dir} {}",
            esc(&a.alert_id),
            esc(&a.symbol),
            money(&a.price, 2)
        ));
    }
    out
}

/// Render a pushed venue event, or `None` for events that are not user-facing (heartbeats).
pub fn event(venue: &str, ev: &Event) -> Option<String> {
    let icon = match ev.severity() {
        Severity::Critical => "🚨",
        Severity::Warn => "⚠️",
        _ => "ℹ️",
    };
    match ev.kind.as_ref()? {
        event::Kind::Heartbeat(_) => None,
        event::Kind::Alert(a) => Some(format!(
            "{icon} <b>{}</b> {}: {}",
            esc(venue),
            esc(&a.code),
            esc(&a.message)
        )),
        event::Kind::Fill(f) => {
            let side = if num(&f.size) < 0.0 { "SOLD" } else { "BOUGHT" };
            let pnl = if num(&f.closed_pnl) != 0.0 {
                format!(" · closed PnL {}", signed_money(&f.closed_pnl))
            } else {
                String::new()
            };
            Some(format!(
                "✅ <b>{}</b> fill: {side} {} {} @ {}{pnl}",
                esc(venue),
                esc(f.size.trim_start_matches('-')),
                esc(&f.symbol),
                money(&f.price, 2)
            ))
        }
        event::Kind::RuleAction(r) => Some(format!(
            "🔧 <b>{}</b> {}: {}",
            esc(venue),
            esc(&r.symbol),
            esc(&r.description)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use venue_proto::v1::{Alert, Fill, Heartbeat};

    fn pos(sym: &str, size: &str, stop: Option<&str>, liq: &str) -> Position {
        Position {
            symbol: sym.into(),
            size: size.into(),
            entry_price: "59000".into(),
            mark_price: "59205".into(),
            unrealized_pnl: "20.5".into(),
            liquidation_price: liq.into(),
            leverage: "20x cross".into(),
            margin_used: "300".into(),
            has_exchange_stop: stop.is_some(),
            stop_price: stop.unwrap_or_default().into(),
            funding_rate_1h: "0.0000125".into(),
        }
    }

    #[test]
    fn money_formats() {
        assert_eq!(money("1234567.891", 2), "1,234,567.89");
        assert_eq!(money("-5764.7", 2), "-5,764.70");
        assert_eq!(money("0.0", 2), "0.00");
        assert_eq!(money("999", 0), "999");
        assert_eq!(money("garbage", 2), "0.00");
    }

    #[test]
    fn positions_show_stop_and_flag_missing() {
        let s = positions(
            "hl",
            &[
                pos("BTC", "0.1", Some("58500"), "56000"),
                pos("ETH", "-2", None, ""),
            ],
        );
        assert!(s.contains("BTC LONG"));
        assert!(s.contains("🛡 stop 58,500.00"));
        assert!(s.contains("5.4% away"));
        assert!(s.contains("ETH SHORT</b> 2 @"));
        assert!(s.contains("NO STOP"));
        assert!(s.contains("liq –"));
    }

    #[test]
    fn escapes_html() {
        assert_eq!(esc("<a&b>"), "&lt;a&amp;b&gt;");
    }

    #[test]
    fn events_render_and_heartbeats_are_silent() {
        let ev = |sev: Severity, kind| Event {
            time_ms: 0,
            severity: sev.into(),
            kind: Some(kind),
        };
        assert!(event(
            "hl",
            &ev(Severity::Info, event::Kind::Heartbeat(Heartbeat::default()))
        )
        .is_none());
        let a = event(
            "hl",
            &ev(
                Severity::Critical,
                event::Kind::Alert(Alert {
                    symbol: "BTC".into(),
                    code: "LIQ_DISTANCE".into(),
                    message: "close".into(),
                }),
            ),
        )
        .unwrap();
        assert!(a.starts_with("🚨"));
        let f = event(
            "hl",
            &ev(
                Severity::Info,
                event::Kind::Fill(Fill {
                    symbol: "BTC".into(),
                    size: "-0.1".into(),
                    price: "60000".into(),
                    fee: "1".into(),
                    closed_pnl: "100".into(),
                    order_id: "1".into(),
                }),
            ),
        )
        .unwrap();
        assert!(f.contains("SOLD 0.1 BTC @ 60,000.00"));
        assert!(f.contains("closed PnL +100.00"));
    }
}
