//! Price-path loaders for the momentum-labs history layout (read-only).
//!
//! - Crypto (Hyperliquid main dex): `hyperliquid/bars/<COIN>_1s.parquet`, columns `s` (unix s), `close`.
//! - Stocks/commodities (HIP-3 `xyz`): `bars/xyz_<COIN>.parquet`, 1-minute OHLC, `t` (unix ms).
//!   Each bar becomes four ticks (open, first extreme, second extreme, close) using the usual
//!   heuristic: an up bar visits the low first, a down bar visits the high first.

use std::{fs::File, path::Path};

use anyhow::{bail, Context, Result};
use arrow_array::{Array, Float64Array, Int64Array, RecordBatch};
use parquet::arrow::{arrow_reader::ParquetRecordBatchReaderBuilder, ProjectionMask};
use smart_sltp::sim::Tick;

fn batches(path: &Path, cols: &[&str]) -> Result<Vec<RecordBatch>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let mask = ProjectionMask::columns(builder.parquet_schema(), cols.iter().copied());
    let reader = builder
        .with_projection(mask)
        .with_batch_size(1 << 16)
        .build()?;
    reader.map(|b| b.map_err(Into::into)).collect()
}

fn f64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Float64Array> {
    b.column_by_name(name)
        .with_context(|| format!("missing column {name}"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .with_context(|| format!("column {name} is not float64"))
}

fn i64_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int64Array> {
    b.column_by_name(name)
        .with_context(|| format!("missing column {name}"))?
        .as_any()
        .downcast_ref::<Int64Array>()
        .with_context(|| format!("column {name} is not int64"))
}

/// 1-second closes. Skips nulls / non-positive prices.
pub fn load_1s(path: &Path) -> Result<Vec<Tick>> {
    let mut out = Vec::new();
    for b in batches(path, &["close", "s"])? {
        let (px, t) = (f64_col(&b, "close")?, i64_col(&b, "s")?);
        for i in 0..b.num_rows() {
            if px.is_null(i) || t.is_null(i) {
                continue;
            }
            let p = px.value(i);
            if p > 0.0 && p.is_finite() {
                out.push(Tick {
                    t: t.value(i),
                    px: p,
                });
            }
        }
    }
    out.sort_by_key(|k| k.t);
    if out.is_empty() {
        bail!("no ticks in {}", path.display());
    }
    Ok(out)
}

/// 1-minute OHLC bars -> 4 ticks per bar. Also returns the median one-way exit cost
/// (half of `cost_rt_bps`) as a fraction, if the file has it.
pub fn load_1m_ohlc(path: &Path) -> Result<(Vec<Tick>, Option<f64>)> {
    let mut out = Vec::new();
    let mut costs = Vec::new();
    for b in batches(path, &["t", "open", "high", "low", "close", "cost_rt_bps"])? {
        let t = i64_col(&b, "t")?;
        let (o, h, l, c) = (
            f64_col(&b, "open")?,
            f64_col(&b, "high")?,
            f64_col(&b, "low")?,
            f64_col(&b, "close")?,
        );
        let cost = f64_col(&b, "cost_rt_bps").ok();
        for i in 0..b.num_rows() {
            if [
                o.is_null(i),
                h.is_null(i),
                l.is_null(i),
                c.is_null(i),
                t.is_null(i),
            ]
            .iter()
            .any(|x| *x)
            {
                continue;
            }
            let (ts, o, h, l, c) = (
                t.value(i) / 1000,
                o.value(i),
                h.value(i),
                l.value(i),
                c.value(i),
            );
            if !(o > 0.0 && h > 0.0 && l > 0.0 && c > 0.0) {
                continue;
            }
            let (first, second) = if c >= o { (l, h) } else { (h, l) };
            for (k, px) in [o, first, second, c].into_iter().enumerate() {
                out.push(Tick {
                    t: ts + 15 * k as i64,
                    px,
                });
            }
            if let Some(cc) = cost {
                if !cc.is_null(i) && cc.value(i).is_finite() && cc.value(i) > 0.0 {
                    costs.push(cc.value(i));
                }
            }
        }
    }
    out.sort_by_key(|k| k.t);
    if out.is_empty() {
        bail!("no bars in {}", path.display());
    }
    let median = if costs.is_empty() {
        None
    } else {
        costs.sort_by(|a, b| a.total_cmp(b));
        Some(costs[costs.len() / 2] / 2.0 / 1e4)
    };
    Ok((out, median))
}

/// Hourly ATR(14) from ticks, indexed by hour bucket (unix s / 3600). No look-ahead:
/// `atr[h]` uses only hours strictly before `h`.
pub fn hourly_atr(ticks: &[Tick], period: usize) -> (i64, Vec<Option<f64>>) {
    let h0 = ticks[0].t / 3600;
    let h1 = ticks[ticks.len() - 1].t / 3600;
    let n = (h1 - h0 + 1) as usize;
    let mut hi = vec![f64::NAN; n];
    let mut lo = vec![f64::NAN; n];
    let mut cl = vec![f64::NAN; n];
    for t in ticks {
        let i = (t.t / 3600 - h0) as usize;
        if hi[i].is_nan() || t.px > hi[i] {
            hi[i] = t.px;
        }
        if lo[i].is_nan() || t.px < lo[i] {
            lo[i] = t.px;
        }
        cl[i] = t.px;
    }
    let mut atr = vec![None; n];
    let mut trs: Vec<f64> = Vec::new();
    let mut prev_close = f64::NAN;
    for i in 0..n {
        // ATR known at the start of hour i uses the true ranges of completed hours < i.
        if trs.len() >= period {
            let w = &trs[trs.len() - period..];
            atr[i] = Some(w.iter().sum::<f64>() / period as f64);
        }
        if hi[i].is_nan() {
            continue; // no trades this hour (e.g. market closed): carry on
        }
        let mut tr = hi[i] - lo[i];
        if prev_close.is_finite() {
            tr = tr
                .max((hi[i] - prev_close).abs())
                .max((lo[i] - prev_close).abs());
        }
        trs.push(tr);
        prev_close = cl[i];
    }
    (h0, atr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atr_has_no_look_ahead() {
        // Hour 0: range 1, hour 1: range 3, hour 2: range 5.
        let mut ticks = vec![];
        for (h, (lo, hi)) in [(100.0, 101.0), (100.0, 103.0), (100.0, 105.0)]
            .iter()
            .enumerate()
        {
            ticks.push(Tick {
                t: h as i64 * 3600,
                px: *lo,
            });
            ticks.push(Tick {
                t: h as i64 * 3600 + 10,
                px: *hi,
            });
        }
        let (_, atr) = hourly_atr(&ticks, 2);
        assert_eq!(atr[0], None);
        assert_eq!(atr[1], None);
        // Hour 2's ATR uses hours 0 and 1 only: TRs 1 and max(3, |103-101|) = 3.
        assert_eq!(atr[2], Some(2.0));
    }
}
