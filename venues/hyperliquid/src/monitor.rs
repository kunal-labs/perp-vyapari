//! Background monitor: polls the account, evaluates alerts, emits fills.
//! Events fan out to every `StreamEvents` subscriber via a broadcast channel.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};
use venue_proto::v1::{event, Alert, Event, Fill, PriceAlert, Severity};

use crate::alerts::Raised;

use crate::{
    alerts::{AlertEngine, Thresholds},
    config::Config,
    hl::{InfoClient, UserFill},
    now_ms,
    snapshot::Snapshot,
};

/// State shared between the gRPC service and the monitor.
pub struct Shared {
    pub price_alerts: Mutex<Vec<PriceAlert>>,
    /// Conditions true as of the last successful poll; replayed to new subscribers.
    pub active_alerts: Mutex<Vec<Raised>>,
    pub connected: AtomicBool,
    pub events: broadcast::Sender<Event>,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            price_alerts: Mutex::new(Vec::new()),
            active_alerts: Mutex::new(Vec::new()),
            connected: AtomicBool::new(false),
            events,
        })
    }

    pub fn emit(&self, severity: Severity, kind: event::Kind) {
        // No subscribers is fine: alerts are best-effort pushes; state is queryable.
        let _ = self.events.send(Event {
            time_ms: now_ms(),
            severity: severity.into(),
            kind: Some(kind),
        });
    }
}

const FAILS_BEFORE_ALERT: u32 = 3;

pub async fn run(cfg: Config, client: InfoClient, shared: Arc<Shared>) {
    let th: Thresholds = cfg.thresholds.clone();
    let mut engine = AlertEngine::default();
    let mut tick = tokio::time::interval(cfg.poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut fails: u32 = 0;
    // Start from "now" so a restart does not replay history as new fills.
    let mut fills_cursor = now_ms();
    let mut seen_tids: HashSet<(u64, i64)> = HashSet::new();

    loop {
        tick.tick().await;
        match Snapshot::fetch(&client, &cfg.address).await {
            Ok(snap) => {
                if fails >= FAILS_BEFORE_ALERT {
                    info!("hyperliquid API recovered");
                    shared.emit(
                        Severity::Info,
                        event::Kind::Alert(api_alert("Hyperliquid API reachable again")),
                    );
                }
                fails = 0;
                shared.connected.store(true, Ordering::Relaxed);

                let pending = shared.price_alerts.lock().await.clone();
                let ev = engine.evaluate(&snap, &th, &pending, now_ms());
                if !ev.fired_price_alerts.is_empty() {
                    let fired = ev.fired_price_alerts;
                    shared
                        .price_alerts
                        .lock()
                        .await
                        .retain(|a| !fired.contains(&a.alert_id));
                }
                *shared.active_alerts.lock().await = ev.active;
                for r in ev.raised {
                    shared.emit(r.severity, event::Kind::Alert(r.alert));
                }
            }
            Err(e) => {
                fails += 1;
                shared.connected.store(false, Ordering::Relaxed);
                warn!(error = %format!("{e:#}"), fails, "snapshot failed");
                if fails == FAILS_BEFORE_ALERT {
                    shared.emit(
                        Severity::Warn,
                        event::Kind::Alert(api_alert(&format!("Hyperliquid API failing: {e:#}"))),
                    );
                }
                continue;
            }
        }

        match client.fills_since(&cfg.address, fills_cursor).await {
            Ok(fills) => {
                for f in new_fills(fills, &mut fills_cursor, &mut seen_tids) {
                    shared.emit(Severity::Info, event::Kind::Fill(map_fill(&f)));
                }
            }
            Err(e) => warn!(error = %format!("{e:#}"), "fills poll failed"),
        }
    }
}

fn api_alert(msg: &str) -> Alert {
    Alert {
        symbol: String::new(),
        code: "VENUE_API".into(),
        message: msg.into(),
    }
}

/// Dedupe fills across polls. The cursor stays at the newest fill time (inclusive
/// query), so same-millisecond fills are not lost; `seen` filters repeats.
pub fn new_fills(
    mut fills: Vec<UserFill>,
    cursor: &mut i64,
    seen: &mut HashSet<(u64, i64)>,
) -> Vec<UserFill> {
    fills.sort_by_key(|f| f.time);
    let fresh: Vec<UserFill> = fills
        .into_iter()
        .filter(|f| seen.insert((f.tid, f.time)))
        .collect();
    if let Some(last) = fresh.last() {
        *cursor = (*cursor).max(last.time);
    }
    // Entries older than the cursor can never be returned again by an inclusive query.
    let c = *cursor;
    seen.retain(|(_, t)| *t >= c);
    fresh
}

pub fn map_fill(f: &UserFill) -> Fill {
    Fill {
        symbol: f.coin.clone(),
        size: if f.side == "A" {
            format!("-{}", f.sz)
        } else {
            f.sz.clone()
        },
        price: f.px.clone(),
        fee: f.fee.clone(),
        closed_pnl: f.closed_pnl.clone(),
        order_id: f.oid.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(tid: u64, time: i64, side: &str) -> UserFill {
        UserFill {
            coin: "BTC".into(),
            px: "59000".into(),
            sz: "0.01".into(),
            side: side.into(),
            time,
            closed_pnl: "0".into(),
            fee: "0.1".into(),
            oid: 7,
            tid,
        }
    }

    #[test]
    fn dedupes_across_polls_including_same_ms() {
        let (mut cur, mut seen) = (0, HashSet::new());
        let a = new_fills(
            vec![fill(1, 10, "B"), fill(2, 20, "A")],
            &mut cur,
            &mut seen,
        );
        assert_eq!(a.len(), 2);
        assert_eq!(cur, 20);
        // Next poll (inclusive from 20) returns tid 2 again plus a same-ms tid 3.
        let b = new_fills(
            vec![fill(2, 20, "A"), fill(3, 20, "B")],
            &mut cur,
            &mut seen,
        );
        assert_eq!(b.iter().map(|f| f.tid).collect::<Vec<_>>(), vec![3]);
        let c = new_fills(
            vec![fill(2, 20, "A"), fill(3, 20, "B")],
            &mut cur,
            &mut seen,
        );
        assert!(c.is_empty());
    }

    #[test]
    fn sell_fill_is_negative() {
        assert_eq!(map_fill(&fill(1, 1, "A")).size, "-0.01");
        assert_eq!(map_fill(&fill(1, 1, "B")).size, "0.01");
    }
}
