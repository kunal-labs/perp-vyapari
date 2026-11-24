//! Alert evaluation: pure function of (snapshot, thresholds, user price alerts, prior state).
//!
//! Firing policy: an alert fires when its condition becomes true, then repeats every
//! `repeat_ms` while it stays true. When the condition clears, the next occurrence
//! fires immediately again. User price alerts are one-shot.

use std::collections::HashMap;

use venue_proto::v1::{Alert, PriceAlert, PriceDirection, Severity};

use crate::snapshot::{fmt_num, Snapshot};

#[derive(Debug, Clone)]
pub struct Thresholds {
    /// Alert when mark is within this % of the liquidation price.
    pub liq_distance_pct: f64,
    /// Alert when annualised funding *against* the position exceeds this %.
    pub funding_apr_pct: f64,
    pub repeat_ms: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            liq_distance_pct: 5.0,
            funding_apr_pct: 100.0,
            repeat_ms: 30 * 60 * 1000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Raised {
    pub severity: Severity,
    pub alert: Alert,
}

/// Result of one evaluation pass.
#[derive(Debug, Default)]
pub struct Evaluation {
    /// Alerts due to be pushed now (new or repeat).
    pub raised: Vec<Raised>,
    /// Every condition currently true (replayed to new subscribers).
    pub active: Vec<Raised>,
    /// One-shot price alerts that fired and must be removed.
    pub fired_price_alerts: Vec<String>,
}

#[derive(Debug, Default)]
pub struct AlertEngine {
    last_fired: HashMap<(String, String), i64>,
}

const HOURS_PER_YEAR: f64 = 24.0 * 365.0;

impl AlertEngine {
    pub fn evaluate(
        &mut self,
        snap: &Snapshot,
        th: &Thresholds,
        price_alerts: &[PriceAlert],
        now_ms: i64,
    ) -> Evaluation {
        let mut active: Vec<(String, String, Severity, String)> = Vec::new();

        for p in snap.positions() {
            let szi: f64 = p.size.parse().unwrap_or(0.0);
            if szi == 0.0 {
                continue;
            }
            let side = if szi > 0.0 { "long" } else { "short" };
            if !p.has_exchange_stop {
                active.push((
                    "NO_STOP".into(),
                    p.symbol.clone(),
                    Severity::Warn,
                    format!("{} {side} {} has no exchange-side stop", p.symbol, p.size),
                ));
            }
            if let (Ok(mark), Ok(liq)) = (
                p.mark_price.parse::<f64>(),
                p.liquidation_price.parse::<f64>(),
            ) {
                if mark > 0.0 {
                    let dist = (mark - liq).abs() / mark * 100.0;
                    if dist < th.liq_distance_pct {
                        active.push((
                            "LIQ_DISTANCE".into(),
                            p.symbol.clone(),
                            Severity::Critical,
                            format!(
                                "{} {side}: mark {} is {:.2}% from liquidation {}",
                                p.symbol, p.mark_price, dist, p.liquidation_price
                            ),
                        ));
                    }
                }
            }
            if let Ok(f) = p.funding_rate_1h.parse::<f64>() {
                // Positive funding: longs pay. Negative: shorts pay.
                let against = if szi > 0.0 { f } else { -f };
                let apr = against * HOURS_PER_YEAR * 100.0;
                if apr > th.funding_apr_pct {
                    active.push((
                        "FUNDING".into(),
                        p.symbol.clone(),
                        Severity::Warn,
                        format!(
                            "{} {side} paying funding at {:.1}% APR ({}/h)",
                            p.symbol, apr, p.funding_rate_1h
                        ),
                    ));
                }
            }
        }

        let mut raised = Vec::new();
        let current: Vec<Raised> = active
            .iter()
            .map(|(code, symbol, severity, message)| Raised {
                severity: *severity,
                alert: Alert {
                    symbol: symbol.clone(),
                    code: code.clone(),
                    message: message.clone(),
                },
            })
            .collect();
        let active_keys: Vec<(String, String)> =
            active.iter().map(|a| (a.0.clone(), a.1.clone())).collect();
        self.last_fired.retain(|k, _| active_keys.contains(k));
        for (code, symbol, severity, message) in active {
            let key = (code.clone(), symbol.clone());
            let due = self
                .last_fired
                .get(&key)
                .is_none_or(|t| now_ms - t >= th.repeat_ms);
            if due {
                self.last_fired.insert(key, now_ms);
                raised.push(Raised {
                    severity,
                    alert: Alert {
                        symbol,
                        code,
                        message,
                    },
                });
            }
        }

        let mut fired_ids = Vec::new();
        for pa in price_alerts {
            let Some(mark) = snap
                .markets
                .get(&pa.symbol)
                .and_then(|m| m.mark_px.parse::<f64>().ok())
            else {
                continue;
            };
            let Ok(target) = pa.price.parse::<f64>() else {
                continue;
            };
            let hit = match pa.direction() {
                PriceDirection::Above => mark >= target,
                PriceDirection::Below => mark <= target,
                PriceDirection::Unspecified => false,
            };
            if hit {
                let dir = if pa.direction() == PriceDirection::Above {
                    "above"
                } else {
                    "below"
                };
                fired_ids.push(pa.alert_id.clone());
                raised.push(Raised {
                    severity: Severity::Info,
                    alert: Alert {
                        symbol: pa.symbol.clone(),
                        code: "PRICE".into(),
                        message: format!(
                            "{} mark {} is {dir} {}",
                            pa.symbol,
                            fmt_num(mark),
                            pa.price
                        ),
                    },
                });
            }
        }
        Evaluation {
            raised,
            active: current,
            fired_price_alerts: fired_ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::tests::snapshot;

    fn codes(r: &[Raised]) -> Vec<String> {
        r.iter()
            .map(|x| format!("{}:{}", x.alert.code, x.alert.symbol))
            .collect()
    }

    #[test]
    fn flags_unprotected_short_only() {
        let mut e = AlertEngine::default();
        let ev = e.evaluate(&snapshot("default"), &Thresholds::default(), &[], 0);
        assert_eq!(codes(&ev.raised), vec!["NO_STOP:ETH"]);
        // Still active on the next pass, but not re-raised within the cooldown.
        let ev = e.evaluate(&snapshot("default"), &Thresholds::default(), &[], 1);
        assert!(ev.raised.is_empty());
        assert_eq!(codes(&ev.active), vec!["NO_STOP:ETH"]);
    }

    #[test]
    fn repeats_after_cooldown_and_refires_after_clear() {
        let th = Thresholds {
            repeat_ms: 1000,
            ..Default::default()
        };
        let mut e = AlertEngine::default();
        let s = snapshot("default");
        assert_eq!(e.evaluate(&s, &th, &[], 0).raised.len(), 1);
        assert_eq!(
            e.evaluate(&s, &th, &[], 500).raised.len(),
            0,
            "within cooldown"
        );
        assert_eq!(
            e.evaluate(&s, &th, &[], 1000).raised.len(),
            1,
            "cooldown elapsed"
        );

        // Condition clears (stop added) then returns: fires immediately.
        let mut protected = s.clone();
        protected.orders.push(
            serde_json::from_str(
                r#"{"coin":"ETH","side":"B","limitPx":"3500","sz":"2","oid":99,"timestamp":0,"isTrigger":true,"triggerPx":"3300","reduceOnly":true,"orderType":"Stop Market"}"#,
            )
            .unwrap(),
        );
        assert_eq!(e.evaluate(&protected, &th, &[], 1100).raised.len(), 0);
        assert_eq!(e.evaluate(&s, &th, &[], 1200).raised.len(), 1);
    }

    #[test]
    fn liquidation_distance() {
        // BTC mark 59205, liq 56000 => 5.41% away.
        let mut e = AlertEngine::default();
        let th = Thresholds {
            liq_distance_pct: 6.0,
            ..Default::default()
        };
        let r = e.evaluate(&snapshot("default"), &th, &[], 0).raised;
        assert!(codes(&r).contains(&"LIQ_DISTANCE:BTC".to_string()));
        let r = AlertEngine::default()
            .evaluate(&snapshot("default"), &Thresholds::default(), &[], 0)
            .raised;
        assert!(!codes(&r).contains(&"LIQ_DISTANCE:BTC".to_string()));
    }

    #[test]
    fn funding_only_when_against_position() {
        // BTC long, funding +0.0000125/h = 10.95% APR (long pays).
        // ETH short, funding -0.00003/h => shorts pay 26.28% APR.
        let th = Thresholds {
            funding_apr_pct: 10.0,
            ..Default::default()
        };
        let r = AlertEngine::default()
            .evaluate(&snapshot("default"), &th, &[], 0)
            .raised;
        let c = codes(&r);
        assert!(c.contains(&"FUNDING:BTC".to_string()));
        assert!(c.contains(&"FUNDING:ETH".to_string()));
        let th = Thresholds {
            funding_apr_pct: 20.0,
            ..Default::default()
        };
        let c = codes(
            &AlertEngine::default()
                .evaluate(&snapshot("default"), &th, &[], 0)
                .raised,
        );
        assert!(!c.contains(&"FUNDING:BTC".to_string()));
        assert!(c.contains(&"FUNDING:ETH".to_string()));
    }

    #[test]
    fn price_alerts_fire_once_by_direction() {
        let pa = |id: &str, dir: PriceDirection, px: &str| PriceAlert {
            alert_id: id.into(),
            symbol: "BTC".into(),
            direction: dir.into(),
            price: px.into(),
            created_ms: 0,
        };
        let alerts = vec![
            pa("a", PriceDirection::Above, "59000"),
            pa("b", PriceDirection::Above, "60000"),
            pa("c", PriceDirection::Below, "59300"),
        ];
        let ev = AlertEngine::default().evaluate(
            &snapshot("default"),
            &Thresholds::default(),
            &alerts,
            0,
        );
        assert_eq!(ev.fired_price_alerts, vec!["a", "c"]);
        assert_eq!(
            ev.raised.iter().filter(|x| x.alert.code == "PRICE").count(),
            2
        );
    }
}
