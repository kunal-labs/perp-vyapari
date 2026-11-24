//! `replay signals <replay.jsonl>`: use momentum-labs breakout alerts as entries and
//! compare plain vs smart management, against a random-entry control on the same markets.
//!
//! Each alert line has `coin` (e.g. "xyz:MU"), `t` (unix ms), `kind` ("alert" or
//! "breakout"), `direction` (+1/-1), `session`. Prices come from `bars/xyz_<COIN>.parquet`.
//! `t` is the bar START; the signal is only known at the bar close, so entries use t + 60 s.

use std::{collections::BTreeMap, fmt::Write as _, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;
use smart_sltp::{
    sim::{simulate, Costs, SimConfig, Tick},
    Plan, Side,
};

use crate::{data, variants};

#[derive(Debug, Deserialize)]
struct Signal {
    coin: String,
    t: i64,
    kind: String,
    direction: i64,
    #[serde(default)]
    session: String,
}

struct Res {
    variant: usize,
    control: bool,
    session: String,
    r: f64,
    key: (String, i64, bool),
}

fn rng(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

pub fn run(file: &str, data_dir: &Path, kind: &str) -> Result<String> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read {file}"))?;
    let mut by_coin: BTreeMap<String, Vec<Signal>> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let s: Signal = serde_json::from_str(line)?;
        if s.kind == kind && s.direction != 0 {
            by_coin.entry(s.coin.clone()).or_default().push(s);
        }
    }
    let vars = variants();
    let max_hold = 7 * 24 * 3600;
    let mut results: Vec<Res> = Vec::new();
    let mut seed = 11u64;
    let mut skipped = 0usize;

    for (coin, sigs) in &by_coin {
        let short = coin.trim_start_matches("xyz:");
        let path = data_dir.join(format!("bars/xyz_{short}.parquet"));
        let Ok((ticks, cost)) = data::load_1m_ohlc(&path) else {
            skipped += sigs.len();
            continue;
        };
        let costs = Costs {
            maker_fee: 0.00015,
            taker_fee: 0.00045,
            slippage: cost.unwrap_or(2.0e-4).max(0.5e-4),
        };
        let cfg = SimConfig {
            update_secs: 5,
            max_hold_secs: max_hold,
            costs,
        };
        let (h0, atr) = data::hourly_atr(&ticks, 14);
        let (t_first, t_last) = (ticks[0].t, ticks[ticks.len() - 1].t);

        for s in sigs {
            // Signal entry, plus a control trade on the same coin at a random time and side.
            let ctl_t = t_first
                + 15 * 86400
                + (rng(&mut seed) * ((t_last - max_hold - t_first - 15 * 86400).max(1)) as f64)
                    as i64;
            let ctl_side = if rng(&mut seed) < 0.5 {
                Side::Long
            } else {
                Side::Short
            };
            let sig_side = if s.direction > 0 {
                Side::Long
            } else {
                Side::Short
            };
            // `t` is the START of the 1-minute bar whose close fired the signal, so the
            // earliest tradable moment is the bar close: t + 60 s (no look-ahead).
            let known_at = s.t / 1000 + 60;
            for (control, when, side) in [(false, known_at, sig_side), (true, ctl_t, ctl_side)] {
                if when + max_hold > t_last {
                    if !control {
                        skipped += 1;
                    }
                    continue;
                }
                let idx = ticks.partition_point(|k| k.t < when);
                let Some(entry) = ticks.get(idx).copied() else {
                    continue;
                };
                if entry.t - when > 120 {
                    if !control {
                        skipped += 1;
                    }
                    continue;
                }
                let Some(Some(a)) = atr.get((entry.t / 3600 - h0) as usize).copied() else {
                    continue;
                };
                if a / entry.px < 1e-4 {
                    continue;
                }
                let sgn = side.sign();
                let Ok(plan) =
                    Plan::new(side, entry.px, entry.px - sgn * a, entry.px + sgn * 2.0 * a)
                else {
                    continue;
                };
                let path: &[Tick] = &ticks[idx..];
                for (vi, (_, p)) in vars.iter().enumerate() {
                    let o = simulate(&plan, p, path, &cfg);
                    results.push(Res {
                        variant: vi,
                        control,
                        session: s.session.clone(),
                        r: o.r_net,
                        key: (coin.clone(), s.t, control),
                    });
                }
            }
        }
    }

    let mut md = String::new();
    let n_sig = results
        .iter()
        .filter(|r| r.variant == 0 && !r.control)
        .count();
    let _ = writeln!(
        md,
        "# Smart SL/TP on momentum-labs `{kind}` entries\n\n- {} entries simulated across {} markets ({skipped} skipped: no price data or too close to the end of the data)\n- Same setup as the random replay: stop 1 x hourly ATR(14), target 2R, max hold 7 days, same costs\n- **Control:** for each signal, one trade on the same market at a random time and in a random direction\n",
        n_sig,
        by_coin.len()
    );
    let plain: std::collections::HashMap<(String, i64, bool), f64> = results
        .iter()
        .filter(|r| r.variant == 0)
        .map(|r| (r.key.clone(), r.r))
        .collect();
    for (title, control) in [
        ("Signal entries", false),
        ("Control: random entries, same markets", true),
    ] {
        let _ = writeln!(md, "## {title}\n\n| Strategy | Trades | Win rate | Avg R | Δ avg R vs plain (±2 SE) | Better / worse |\n|---|---|---|---|---|---|");
        for (vi, (name, _)) in vars.iter().enumerate() {
            let rs: Vec<&Res> = results
                .iter()
                .filter(|r| r.variant == vi && r.control == control)
                .collect();
            if rs.is_empty() {
                continue;
            }
            let n = rs.len() as f64;
            let avg = rs.iter().map(|r| r.r).sum::<f64>() / n;
            let win = rs.iter().filter(|r| r.r > 0.0).count() as f64 / n;
            let d: Vec<f64> = rs.iter().map(|r| r.r - plain[&r.key]).collect();
            let dm = d.iter().sum::<f64>() / n;
            let se =
                (d.iter().map(|x| (x - dm).powi(2)).sum::<f64>() / (n - 1.0).max(1.0) / n).sqrt();
            let b = d.iter().filter(|&&x| x > 1e-9).count() as f64 / n;
            let w = d.iter().filter(|&&x| x < -1e-9).count() as f64 / n;
            let delta = if vi == 0 {
                "—".to_string()
            } else {
                format!("{dm:+.3} ± {:.3}", 2.0 * se)
            };
            let bw = if vi == 0 {
                "—".to_string()
            } else {
                format!("{:.1}% / {:.1}%", 100.0 * b, 100.0 * w)
            };
            let _ = writeln!(
                md,
                "| {name} | {} | {:.1}% | {avg:+.3} | {delta} | {bw} |",
                rs.len(),
                100.0 * win
            );
        }
        let _ = writeln!(md);
    }
    let _ = writeln!(md, "## Signal entries by US-equity session (plain → smart avg R)\n\n| Session | Trades | Plain | Smart | Δ |\n|---|---|---|---|---|");
    let mut sessions: Vec<String> = results
        .iter()
        .filter(|r| !r.control)
        .map(|r| r.session.clone())
        .collect();
    sessions.sort();
    sessions.dedup();
    for sess in sessions {
        let avg = |vi: usize| {
            let v: Vec<f64> = results
                .iter()
                .filter(|r| !r.control && r.variant == vi && r.session == sess)
                .map(|r| r.r)
                .collect();
            (v.len(), v.iter().sum::<f64>() / v.len().max(1) as f64)
        };
        let ((n, p), (_, s)) = (avg(0), avg(1));
        let _ = writeln!(md, "| {sess} | {n} | {p:+.3} | {s:+.3} | {:+.3} |", s - p);
    }
    Ok(md)
}
