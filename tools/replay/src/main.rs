//! Replay: plain SL/TP vs smart-managed SL/TP over a year of Hyperliquid prices.
//!
//! Synthetic trades are opened at regular (jittered) times on each market, in a random
//! direction, with the stop placed `sl_mult` x hourly ATR(14) away (known at entry, no
//! look-ahead) and the target at `tp_r` x that risk. Every trade is run once per
//! strategy variant on the identical price path, so differences come only from management.
//!
//! Usage (all optional):
//!   REPLAY_DATA_DIR=~/scratchpad/momentum-labs/data/history \
//!   REPLAY_OUT=/tmp/replay  REPLAY_SPACING_H=4  REPLAY_SEED=7  REPLAY_THREADS=6 \
//!   cargo run --release -p replay
//!
//! Real trades: `cargo run --release -p replay -- trades <file.json>` (see `own.rs`).
//! Signal entries: `cargo run --release -p replay -- signals <replay.jsonl> [alert|breakout]` (see `signals.rs`).

mod data;
mod own;
mod report;
mod signals;

use std::{path::PathBuf, sync::Mutex, time::Instant};

use anyhow::{Context, Result};
use smart_sltp::{
    sim::{simulate, Costs, Exit, SimConfig, Tick},
    Params, Plan, Side,
};

pub const CRYPTO: &[&str] = &[
    "BTC", "ETH", "SOL", "HYPE", "XRP", "DOGE", "BNB", "AVAX", "ADA", "LINK", "LTC", "SUI", "ENA",
    "AAVE",
];
pub const STOCKS: &[&str] = &[
    "NVDA", "MSFT", "TSLA", "AAPL", "AMZN", "META", "GOOGL", "AMD", "COIN", "MSTR", "HOOD", "RKLB",
];

/// Strategy variants compared against PLAIN on the base grid point.
pub fn variants() -> Vec<(&'static str, Params)> {
    let s = Params::STANDARD;
    vec![
        ("plain (your SL/TP untouched)", Params::PLAIN),
        ("smart (standard)", s),
        (
            "no-loss only",
            Params {
                lock_at: None,
                tp_fraction: 1.0,
                trail_r: None,
                ..s
            },
        ),
        (
            "no-loss + lock, full exit at TP",
            Params {
                tp_fraction: 1.0,
                trail_r: None,
                ..s
            },
        ),
        (
            "smart, no-loss at 75%",
            Params {
                breakeven_at: Some(0.75),
                lock_at: Some(0.9),
                ..s
            },
        ),
        (
            "smart, runner trail 1R",
            Params {
                trail_r: Some(1.0),
                ..s
            },
        ),
        (
            "smart, runner trail 0.25R",
            Params {
                trail_r: Some(0.25),
                ..s
            },
        ),
        (
            "runner only (no early moves)",
            Params {
                breakeven_at: None,
                lock_at: None,
                ..s
            },
        ),
    ]
}

pub const SL_MULTS: &[f64] = &[0.5, 1.0, 2.0];
pub const TP_RS: &[f64] = &[1.5, 2.0, 3.0];
pub const BASE: (f64, f64) = (1.0, 2.0);
pub const UPDATE_SECS: &[i64] = &[1, 5, 30];
pub const BASE_UPDATE: i64 = 5;

#[derive(Debug, Clone)]
pub struct Row {
    pub coin: String,
    pub class: &'static str,
    pub t: i64,
    pub side: Side,
    pub sl_mult: f64,
    pub tp_r: f64,
    pub variant: usize,
    pub update_secs: i64,
    pub r: f64,
    pub exit: Exit,
    pub tp_hit: bool,
    pub max_progress: f64,
    pub hold_secs: i64,
}

/// Tiny deterministic PRNG (splitmix64), so runs are reproducible without extra deps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Market {
    coin: String,
    class: &'static str,
    path: PathBuf,
}

fn run_market(m: &Market, spacing_h: i64, seed: u64) -> Result<Vec<Row>> {
    let (ticks, costs) = if m.class == "crypto" {
        let t = data::load_1s(&m.path)?;
        // Majors are very liquid; alts a bit less. One-way exit cost (spread + slippage).
        let slip = if ["BTC", "ETH"].contains(&m.coin.as_str()) {
            0.5e-4
        } else {
            1.0e-4
        };
        (t, slip)
    } else {
        let (t, c) = data::load_1m_ohlc(&m.path)?;
        (t, c.unwrap_or(2.0e-4).max(0.5e-4))
    };
    let costs = Costs {
        maker_fee: 0.00015,
        taker_fee: 0.00045,
        slippage: costs,
    };
    let (h0, atr) = data::hourly_atr(&ticks, 14);
    let vars = variants();
    let max_hold = 7 * 24 * 3600;

    let first = ticks[0].t + 15 * 24 * 3600; // warm-up for ATR
    let last = ticks[ticks.len() - 1].t - max_hold; // every trade gets its full window
    let mut rng = Rng(seed
        ^ m.coin
            .bytes()
            .fold(0u64, |a, b| a.wrapping_mul(131).wrapping_add(b as u64)));
    let mut rows = Vec::new();
    let mut t = first;
    while t < last {
        let entry_t = t + (rng.unit() * (spacing_h * 3600) as f64) as i64;
        t += spacing_h * 3600;
        let side = if rng.unit() < 0.5 {
            Side::Long
        } else {
            Side::Short
        };
        let idx = ticks.partition_point(|k| k.t < entry_t);
        if idx >= ticks.len() {
            break;
        }
        let entry = ticks[idx];
        // Skip stale prints (market closed): need a tick within 2 minutes of the chosen time.
        if entry.t - entry_t > 120 {
            continue;
        }
        let Some(Some(a)) = atr.get((entry.t / 3600 - h0) as usize).copied() else {
            continue;
        };
        if a / entry.px < 1e-4 {
            continue;
        }
        let path: &[Tick] = &ticks[idx..];
        for &sl_mult in SL_MULTS {
            for &tp_r in TP_RS {
                let risk = sl_mult * a;
                let s = side.sign();
                let Ok(plan) = Plan::new(
                    side,
                    entry.px,
                    entry.px - s * risk,
                    entry.px + s * tp_r * risk,
                ) else {
                    continue;
                };
                let is_base = (sl_mult, tp_r) == BASE;
                for (vi, (_, p)) in vars.iter().enumerate() {
                    // Off the base grid point only plain vs standard are needed.
                    if !is_base && vi > 1 {
                        continue;
                    }
                    let updates: &[i64] = if is_base && vi == 1 {
                        UPDATE_SECS
                    } else {
                        &[BASE_UPDATE]
                    };
                    for &u in updates {
                        let cfg = SimConfig {
                            update_secs: u,
                            max_hold_secs: max_hold,
                            costs,
                        };
                        let o = simulate(&plan, p, path, &cfg);
                        rows.push(Row {
                            coin: m.coin.clone(),
                            class: m.class,
                            t: entry.t,
                            side,
                            sl_mult,
                            tp_r,
                            variant: vi,
                            update_secs: u,
                            r: o.r_net,
                            exit: o.exit,
                            tp_hit: o.tp_hit,
                            max_progress: o.max_progress,
                            hold_secs: o.hold_secs,
                        });
                    }
                }
            }
        }
    }
    Ok(rows)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("trades") {
        let file = args.get(2).context("usage: replay trades <file.json>")?;
        println!("{}", own::run(file)?);
        return Ok(());
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if args.get(1).map(String::as_str) == Some("signals") {
        let file = args
            .get(2)
            .context("usage: replay signals <replay.jsonl> [alert|breakout]")?;
        let kind = args.get(3).map(String::as_str).unwrap_or("alert");
        let dir = PathBuf::from(
            std::env::var("REPLAY_DATA_DIR")
                .unwrap_or_else(|_| format!("{home}/scratchpad/momentum-labs/data/history")),
        );
        println!("{}", signals::run(file, &dir, kind)?);
        return Ok(());
    }
    let data_dir = PathBuf::from(
        std::env::var("REPLAY_DATA_DIR")
            .unwrap_or_else(|_| format!("{home}/scratchpad/momentum-labs/data/history")),
    );
    let out = PathBuf::from(std::env::var("REPLAY_OUT").unwrap_or_else(|_| "replay-out".into()));
    let spacing_h: i64 = std::env::var("REPLAY_SPACING_H")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let seed: u64 = std::env::var("REPLAY_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let threads: usize = std::env::var("REPLAY_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    std::fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;

    let mut markets: Vec<Market> = Vec::new();
    for c in CRYPTO {
        markets.push(Market {
            coin: c.to_string(),
            class: "crypto",
            path: data_dir.join(format!("hyperliquid/bars/{c}_1s.parquet")),
        });
    }
    for c in STOCKS {
        markets.push(Market {
            coin: format!("xyz:{c}"),
            class: "stock",
            path: data_dir.join(format!("bars/xyz_{c}.parquet")),
        });
    }
    markets.retain(|m| {
        let ok = m.path.exists();
        if !ok {
            eprintln!("skip {} (no data at {})", m.coin, m.path.display());
        }
        ok
    });

    let started = Instant::now();
    let queue = Mutex::new(markets.iter().collect::<Vec<_>>());
    let rows = Mutex::new(Vec::<Row>::new());
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1) {
            sc.spawn(|| loop {
                let Some(m) = queue.lock().unwrap().pop() else {
                    break;
                };
                let t0 = Instant::now();
                match run_market(m, spacing_h, seed) {
                    Ok(r) => {
                        eprintln!(
                            "{:>12}: {:>7} rows in {:.1}s",
                            m.coin,
                            r.len(),
                            t0.elapsed().as_secs_f64()
                        );
                        rows.lock().unwrap().extend(r);
                    }
                    Err(e) => eprintln!("{:>12}: FAILED {e:#}", m.coin),
                }
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by(|a, b| {
        (a.t, &a.coin, a.variant, a.update_secs).cmp(&(b.t, &b.coin, b.variant, b.update_secs))
    });
    eprintln!(
        "simulated {} rows in {:.1}s",
        rows.len(),
        started.elapsed().as_secs_f64()
    );

    report::write_csv(&out.join("trades.csv"), &rows)?;
    let md = report::markdown(&rows, &variants(), spacing_h, seed, &data_dir);
    std::fs::write(out.join("report.md"), &md)?;
    println!("{md}");
    eprintln!("wrote {}/report.md and trades.csv", out.display());
    Ok(())
}
