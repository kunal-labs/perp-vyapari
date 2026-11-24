//! `replay trades <file.json>`: replay real trades (entry, original SL/TP, candles) through
//! the engine and compare plain vs smart vs what actually happened.
//!
//! Input: JSON array of trades as written by a fetch script, each with `coin`, `side`
//! ("long"/"short"), `entry_t` (unix s), `entry`, `size`, `pnl`, `fees`, `sl`, `tp`
//! (null if none set), `interval` and `candles` ([[t, o, h, l, c], ...]).

use std::fmt::Write as _;

use anyhow::{Context, Result};
use serde::Deserialize;
use smart_sltp::{
    sim::{simulate, Costs, SimConfig, Tick},
    step, stop_target, Params, Plan, Side,
};

#[derive(Debug, Deserialize)]
pub struct OwnTrade {
    pub coin: String,
    pub side: String,
    pub entry_t: i64,
    pub entry: f64,
    pub size: f64,
    pub pnl: f64,
    pub fees: f64,
    pub sl: Option<f64>,
    pub tp: Option<f64>,
    pub interval: String,
    pub candles: Vec<[f64; 5]>,
}

/// OHLC candles -> ticks (open, first extreme, second extreme, close), from the entry onward.
fn ticks(t: &OwnTrade) -> Vec<Tick> {
    let mut out = vec![Tick {
        t: t.entry_t,
        px: t.entry,
    }];
    for c in &t.candles {
        let (ts, o, h, l, cl) = (c[0] as i64, c[1], c[2], c[3], c[4]);
        let (a, b) = if cl >= o { (l, h) } else { (h, l) };
        for (k, px) in [o, a, b, cl].into_iter().enumerate() {
            let tt = ts + 15 * k as i64;
            if tt > t.entry_t {
                out.push(Tick { t: tt, px });
            }
        }
    }
    out
}

pub fn run(path: &str) -> Result<String> {
    let trades: Vec<OwnTrade> = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("read {path}"))?,
    )?;
    let cfg = SimConfig {
        update_secs: 5,
        max_hold_secs: 7 * 24 * 3600,
        costs: Costs {
            maker_fee: 0.00015,
            taker_fee: 0.00045,
            slippage: 1.0e-4,
        },
    };
    let mut md = String::from(
        "| Trade | Entry → your SL / TP | What you actually got | Plain (SL/TP untouched) | Smart | What smart did |\n|---|---|---|---|---|---|\n",
    );
    let (mut sum_act, mut sum_plain, mut sum_smart, mut n) = (0.0, 0.0, 0.0, 0);
    for t in &trades {
        let side = if t.side == "long" {
            Side::Long
        } else {
            Side::Short
        };
        let name = format!("{} {} ({})", t.coin, t.side, t.interval);
        let (Some(sl), Some(tp)) = (t.sl, t.tp) else {
            let _ = writeln!(md, "| {name} | {:.4} → no SL/TP set | {:+.2} USDC | — | — | nothing to manage without an SL |", t.entry, t.pnl - t.fees);
            continue;
        };
        let Ok(plan) = Plan::new(side, t.entry, sl, tp) else {
            let _ = writeln!(md, "| {name} | {:.4} → {sl} / {tp} | {:+.2} USDC | — | — | SL/TP on the wrong side of entry |", t.entry, t.pnl - t.fees);
            continue;
        };
        let path = ticks(t);
        let plain = simulate(&plan, &Params::PLAIN, &path, &cfg);
        let smart = simulate(&plan, &Params::STANDARD, &path, &cfg);
        let actual_r = (t.pnl - t.fees) / (t.size * plan.risk());
        // Furthest step smart reached, for a plain-words note.
        let mut best = plan.entry;
        for k in &path {
            best = side.tighter(best, k.px);
            if side.sign() * (k.px - plan.sl) <= 0.0 {
                break;
            }
        }
        let reached = step(&plan, &Params::STANDARD, best, smart.tp_hit);
        let what = match reached {
            0 => "never got halfway to TP: left your SL alone".to_string(),
            1 => format!(
                "moved SL to no-loss ({:.4})",
                stop_target(&plan, &Params::STANDARD, best, false)
            ),
            2 => "moved SL to lock half the gain".to_string(),
            _ => "took half at TP, trailed the rest".to_string(),
        };
        let _ = writeln!(
            md,
            "| {name} | {:.4} → {sl} / {tp} | {actual_r:+.2}R ({:+.2} USDC) | {:+.2}R | {:+.2}R | {what} |",
            t.entry,
            t.pnl - t.fees,
            plain.r_net,
            smart.r_net
        );
        sum_act += actual_r;
        sum_plain += plain.r_net;
        sum_smart += smart.r_net;
        n += 1;
    }
    let _ = writeln!(md, "| **Total ({n} trades with SL/TP)** | | **{sum_act:+.2}R** | **{sum_plain:+.2}R** | **{sum_smart:+.2}R** | |");

    // Every variant, totals only.
    let _ = writeln!(
        md,
        "\n| Strategy | Total R on your trades | Avg R per trade |\n|---|---|---|"
    );
    let _ = writeln!(
        md,
        "| what you actually did | {sum_act:+.2} | {:+.2} |",
        sum_act / n.max(1) as f64
    );
    for (name, p) in crate::variants() {
        let mut tot = 0.0;
        for t in &trades {
            let side = if t.side == "long" {
                Side::Long
            } else {
                Side::Short
            };
            let (Some(sl), Some(tp)) = (t.sl, t.tp) else {
                continue;
            };
            let Ok(plan) = Plan::new(side, t.entry, sl, tp) else {
                continue;
            };
            tot += simulate(&plan, &p, &ticks(t), &cfg).r_net;
        }
        let _ = writeln!(md, "| {name} | {tot:+.2} | {:+.2} |", tot / n.max(1) as f64);
    }
    Ok(md)
}
