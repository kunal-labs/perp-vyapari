//! Aggregation and markdown/CSV output.

use std::{collections::HashMap, fmt::Write as _, io::Write as _, path::Path};

use anyhow::Result;
use smart_sltp::{sim::Exit, Params, Side};

use crate::{Row, BASE, BASE_UPDATE, SL_MULTS, TP_RS, UPDATE_SECS};

/// 2026-07-01T00:00:00Z: trades before are "first 8 months", after are "last 4 months".
const SPLIT_T: i64 = 1_782_864_000;

pub fn write_csv(path: &Path, rows: &[Row]) -> Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(f, "coin,class,entry_t,side,sl_atr_mult,tp_r,variant,update_secs,r_net,exit,tp_hit,max_progress,hold_secs")?;
    for r in rows {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{:.5},{:?},{},{:.4},{}",
            r.coin,
            r.class,
            r.t,
            if r.side == Side::Long {
                "long"
            } else {
                "short"
            },
            r.sl_mult,
            r.tp_r,
            r.variant,
            r.update_secs,
            r.r,
            r.exit,
            r.tp_hit,
            r.max_progress,
            r.hold_secs
        )?;
    }
    Ok(())
}

#[derive(Default, Clone, Copy)]
struct Stats {
    n: usize,
    win: f64,
    avg: f64,
    median: f64,
    pf: f64,
    max_dd: f64,
    hold_h: f64,
}

/// `rows` must be in entry-time order (for drawdown).
fn stats(rows: &[&Row]) -> Stats {
    if rows.is_empty() {
        return Stats::default();
    }
    let rs: Vec<f64> = rows.iter().map(|r| r.r).collect();
    let n = rs.len();
    let mut sorted = rs.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let (pos, neg): (f64, f64) =
        rs.iter().fold(
            (0.0, 0.0),
            |(p, q), &x| if x > 0.0 { (p + x, q) } else { (p, q - x) },
        );
    let (mut cum, mut peak, mut dd) = (0.0_f64, 0.0_f64, 0.0_f64);
    for &x in &rs {
        cum += x;
        peak = peak.max(cum);
        dd = dd.max(peak - cum);
    }
    Stats {
        n,
        win: rs.iter().filter(|&&x| x > 0.0).count() as f64 / n as f64,
        avg: rs.iter().sum::<f64>() / n as f64,
        median: sorted[n / 2],
        pf: if neg > 0.0 { pos / neg } else { f64::INFINITY },
        max_dd: dd,
        hold_h: rows.iter().map(|r| r.hold_secs as f64).sum::<f64>() / n as f64 / 3600.0,
    }
}

type Key = (String, i64, u64, u64);
fn key(r: &Row) -> Key {
    (r.coin.clone(), r.t, r.sl_mult.to_bits(), r.tp_r.to_bits())
}

/// Paired difference vs plain: mean, 2 x standard error, share better, share worse.
fn paired(rows: &[&Row], plain: &HashMap<Key, f64>) -> (f64, f64, f64, f64) {
    let d: Vec<f64> = rows
        .iter()
        .filter_map(|r| plain.get(&key(r)).map(|p| r.r - p))
        .collect();
    if d.len() < 2 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let n = d.len() as f64;
    let mean = d.iter().sum::<f64>() / n;
    let var = d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let better = d.iter().filter(|&&x| x > 1e-9).count() as f64 / n;
    let worse = d.iter().filter(|&&x| x < -1e-9).count() as f64 / n;
    (mean, 2.0 * (var / n).sqrt(), better, worse)
}

fn pct(x: f64) -> String {
    format!("{:.1}%", 100.0 * x)
}

fn r2(x: f64) -> String {
    format!("{x:+.3}")
}

fn select(rows: &[Row], f: impl Fn(&Row) -> bool) -> Vec<&Row> {
    rows.iter().filter(|r| f(r)).collect()
}

pub fn markdown(
    rows: &[Row],
    vars: &[(&str, Params)],
    spacing_h: i64,
    seed: u64,
    data_dir: &Path,
) -> String {
    let base = |r: &Row| (r.sl_mult, r.tp_r) == BASE && r.update_secs == BASE_UPDATE;
    let plain: HashMap<Key, f64> = rows
        .iter()
        .filter(|r| r.variant == 0 && r.update_secs == BASE_UPDATE)
        .map(|r| (key(r), r.r))
        .collect();
    let mut md = String::new();

    let base_plain = select(rows, |r| base(r) && r.variant == 0);
    let (t_min, t_max) = (
        base_plain.iter().map(|r| r.t).min().unwrap_or(0),
        base_plain.iter().map(|r| r.t).max().unwrap_or(0),
    );
    let markets: std::collections::BTreeSet<&str> =
        base_plain.iter().map(|r| r.coin.as_str()).collect();
    let _ = writeln!(md, "# Smart SL/TP replay\n");
    let _ = writeln!(
        md,
        "- **Trades:** {} synthetic trades (base setup) across {} markets, one every ~{spacing_h} h per market, random long/short, seed {seed}",
        base_plain.len(),
        markets.len()
    );
    let _ = writeln!(md, "- **Period:** entries from unix {t_min} to {t_max}");
    let _ = writeln!(
        md,
        "- **Base setup:** stop = 1.0 x hourly ATR(14) from entry; target = 2R; stop recomputed every {BASE_UPDATE} s; max hold 7 days"
    );
    let _ = writeln!(
        md,
        "- **Costs:** maker 1.5 bps on entry; taker 4.5 bps plus spread/slippage on every exit (crypto 0.5–1 bp, stocks: median of the recorded round-trip cost / 2)"
    );
    let _ = writeln!(
        md,
        "- **Data:** `{}` (crypto 1-second closes; stocks 1-minute OHLC)",
        data_dir.display()
    );
    let _ = writeln!(md, "- **R:** results are in multiples of the initial risk. −1R = full stop-out, +2R = classic target hit.\n");

    // 1) Headline.
    let _ = writeln!(md, "## 1. Headline (base setup, all markets)\n");
    let _ = writeln!(md, "| Strategy | Trades | Win rate | Avg R | Median R | Profit factor | Max drawdown (R) | Avg hold (h) | Δ avg R vs plain (±2 SE) | Better / worse than plain |");
    let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|---|");
    for (vi, (name, _)) in vars.iter().enumerate() {
        let sel = select(rows, |r| base(r) && r.variant == vi);
        let s = stats(&sel);
        let (d, se, b, w) = paired(&sel, &plain);
        let delta = if vi == 0 {
            "—".into()
        } else {
            format!("{} ± {:.3}", r2(d), se)
        };
        let bw = if vi == 0 {
            "—".into()
        } else {
            format!("{} / {}", pct(b), pct(w))
        };
        let _ = writeln!(
            md,
            "| {name} | {} | {} | {} | {} | {:.2} | {:.1} | {:.1} | {delta} | {bw} |",
            s.n,
            pct(s.win),
            r2(s.avg),
            r2(s.median),
            s.pf,
            s.max_dd,
            s.hold_h
        );
    }

    // 2) Robustness splits for plain vs standard.
    let _ = writeln!(
        md,
        "\n## 2. Does it hold up? (plain vs smart standard, base setup)\n"
    );
    let _ = writeln!(
        md,
        "| Slice | Trades | Plain avg R | Smart avg R | Δ (±2 SE) | Plain win | Smart win |"
    );
    let _ = writeln!(md, "|---|---|---|---|---|---|---|");
    type Slice<'a> = (&'a str, Box<dyn Fn(&Row) -> bool>);
    let slices: Vec<Slice> = vec![
        ("Crypto", Box::new(|r: &Row| r.class == "crypto")),
        ("Stocks (HIP-3 xyz)", Box::new(|r: &Row| r.class == "stock")),
        ("Longs", Box::new(|r: &Row| r.side == Side::Long)),
        ("Shorts", Box::new(|r: &Row| r.side == Side::Short)),
        (
            "First 8 months (before 2026-07-01)",
            Box::new(|r: &Row| r.t < SPLIT_T),
        ),
        (
            "Last 4 months (from 2026-07-01)",
            Box::new(|r: &Row| r.t >= SPLIT_T),
        ),
    ];
    for (name, f) in &slices {
        let p = stats(&select(rows, |r| base(r) && r.variant == 0 && f(r)));
        let sel = select(rows, |r| base(r) && r.variant == 1 && f(r));
        let s = stats(&sel);
        let (d, se, _, _) = paired(&sel, &plain);
        let _ = writeln!(
            md,
            "| {name} | {} | {} | {} | {} ± {:.3} | {} | {} |",
            s.n,
            r2(p.avg),
            r2(s.avg),
            r2(d),
            se,
            pct(p.win),
            pct(s.win)
        );
    }

    // 3) Per market.
    let _ = writeln!(md, "\n## 3. Per market (base setup)\n");
    let _ = writeln!(
        md,
        "| Market | Trades | Plain avg R | Smart avg R | Δ | Plain win | Smart win |"
    );
    let _ = writeln!(md, "|---|---|---|---|---|---|---|");
    for m in &markets {
        let p = stats(&select(rows, |r| base(r) && r.variant == 0 && r.coin == *m));
        let s = stats(&select(rows, |r| base(r) && r.variant == 1 && r.coin == *m));
        let _ = writeln!(
            md,
            "| {m} | {} | {} | {} | {} | {} | {} |",
            s.n,
            r2(p.avg),
            r2(s.avg),
            r2(s.avg - p.avg),
            pct(p.win),
            pct(s.win)
        );
    }

    // 4) Stop / target grid.
    let _ = writeln!(
        md,
        "\n## 4. Different stop and target distances (plain → smart avg R)\n"
    );
    let mut hdr = String::from("| Stop distance \\ Target |");
    let mut sep = String::from("|---|");
    for tp in TP_RS {
        let _ = write!(hdr, " {tp}R |");
        sep.push_str("---|");
    }
    let _ = writeln!(md, "{hdr}\n{sep}");
    for sl in SL_MULTS {
        let mut line = format!("| {sl} x ATR |");
        for tp in TP_RS {
            let g = |r: &Row| r.sl_mult == *sl && r.tp_r == *tp && r.update_secs == BASE_UPDATE;
            let p = stats(&select(rows, |r| g(r) && r.variant == 0));
            let s = stats(&select(rows, |r| g(r) && r.variant == 1));
            let _ = write!(
                line,
                " {} → {} ({}) |",
                r2(p.avg),
                r2(s.avg),
                r2(s.avg - p.avg)
            );
        }
        let _ = writeln!(md, "{line}");
    }

    // 5) Update cadence.
    let _ = writeln!(
        md,
        "\n## 5. How fast must the bot react? (smart standard, base setup)\n"
    );
    let _ = writeln!(md, "| Stop recomputed every | Avg R | Win rate |");
    let _ = writeln!(md, "|---|---|---|");
    for u in UPDATE_SECS {
        let s = stats(&select(rows, |r| {
            (r.sl_mult, r.tp_r) == BASE && r.variant == 1 && r.update_secs == *u
        }));
        let _ = writeln!(md, "| {u} s | {} | {} |", r2(s.avg), pct(s.win));
    }

    // 6) Where smart helps and hurts.
    let _ = writeln!(
        md,
        "\n## 6. Where smart helps and where it hurts (smart standard vs plain, base setup)\n"
    );
    let sm = select(rows, |r| base(r) && r.variant == 1);
    let n = sm.len().max(1) as f64;
    let mut saved = 0usize;
    let mut cut = 0usize;
    let mut cut_cost = 0.0;
    let mut runner = 0usize;
    let mut runner_gain = 0.0;
    for r in &sm {
        let Some(&p) = plain.get(&key(r)) else {
            continue;
        };
        if p <= -0.9 && r.r > -0.5 {
            saved += 1;
        }
        if p > 1.5 && r.r < p - 0.5 {
            cut += 1;
            cut_cost += p - r.r;
        }
        if r.tp_hit && r.r > p + 1e-9 {
            runner += 1;
            runner_gain += r.r - p;
        }
    }
    let _ = writeln!(md, "- **Losses avoided:** {} of trades were full stop-outs with plain but closed near breakeven or better with smart.", pct(saved as f64 / n));
    let _ = writeln!(
        md,
        "- **Winners cut short:** {} of trades hit the target with plain but were stopped out earlier by smart (average cost {:.2}R each).",
        pct(cut as f64 / n),
        if cut > 0 { cut_cost / cut as f64 } else { 0.0 }
    );
    let _ = writeln!(
        md,
        "- **Runner paid off:** {} of trades made more than the plain target thanks to the runner (average extra {:.2}R).",
        pct(runner as f64 / n),
        if runner > 0 { runner_gain / runner as f64 } else { 0.0 }
    );
    let exits = |vi: usize, e: Exit| {
        select(rows, |r| base(r) && r.variant == vi && r.exit == e).len() as f64 / n
    };
    let _ = writeln!(
        md,
        "- **Exit mix (plain → smart):** stop {} → {}, full target {} → {}, 7-day timeout {} → {}.",
        pct(exits(0, Exit::Stop)),
        pct(exits(1, Exit::Stop)),
        pct(exits(0, Exit::TakeProfit)),
        pct(exits(1, Exit::TakeProfit)),
        pct(exits(0, Exit::Timeout)),
        pct(exits(1, Exit::Timeout))
    );

    let _ = writeln!(md, "\n## Caveats\n");
    let _ = writeln!(md, "- Entries are random, so the plain baseline is roughly break-even minus costs. That's intended: it isolates the effect of *management*, not entry skill. Real entries with an edge will shift both columns up.");
    let _ = writeln!(md, "- Trades overlap in time and markets are correlated, so ±2 SE understates the true uncertainty. Treat small differences as noise.");
    let _ = writeln!(md, "- Stock bars are 1-minute OHLC with an assumed path inside each bar, which is coarser than the 1-second crypto data. Fills assume no queue or partial-fill effects.");
    let _ = writeln!(
        md,
        "- Fees use Hyperliquid's base tier. HIP-3 markets can charge different fees."
    );
    md
}
