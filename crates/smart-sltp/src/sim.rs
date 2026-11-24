//! Trade simulator over a price path, for replaying history.
//!
//! Model (deliberately conservative):
//! - Entry is a filled limit order at `plan.entry` (maker fee).
//! - Exits are checked against the orders resting *before* this tick: a stop fills at
//!   the worse of the stop price and the tick price; a take-profit fills at the TP price.
//!   Both are market-on-trigger (taker fee) and pay `slippage` against us.
//! - The engine recomputes the stop at most every `update_secs`, using the best price
//!   seen so far; a new stop only takes effect from the next tick (no look-ahead).

use crate::{stop_target, Params, Plan};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tick {
    pub t: i64,
    pub px: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Costs {
    /// Fractions of notional, e.g. 0.00045 = 4.5 bps.
    pub maker_fee: f64,
    pub taker_fee: f64,
    pub slippage: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimConfig {
    pub update_secs: i64,
    pub max_hold_secs: i64,
    pub costs: Costs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Whole position closed by the stop (possibly after a partial TP).
    Stop,
    /// Whole position closed at TP (classic exit).
    TakeProfit,
    Timeout,
    EndOfData,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outcome {
    /// Net result in multiples of initial risk R (fees and slippage included).
    pub r_net: f64,
    pub exit: Exit,
    pub tp_hit: bool,
    /// Best progress toward TP reached (1.0 = touched TP).
    pub max_progress: f64,
    pub hold_secs: i64,
    pub stop_moves: u32,
}

/// Simulate one trade. `ticks[0]` is the entry moment; later ticks drive the trade.
pub fn simulate(plan: &Plan, params: &Params, ticks: &[Tick], cfg: &SimConfig) -> Outcome {
    let side = plan.side;
    let s = side.sign();
    let c = cfg.costs;
    let t0 = ticks.first().map_or(0, |t| t.t);

    let mut remaining = 1.0_f64;
    let mut pnl = 0.0_f64; // price units per 1 unit of original size
    let mut fees = c.maker_fee * plan.entry;
    let mut sl = plan.sl;
    let mut tp_filled = false;
    let mut best = plan.entry;
    let mut max_progress = 0.0_f64;
    let mut last_update = t0;
    let mut stop_moves = 0;
    let mut exit = Exit::EndOfData;
    let mut last = Tick {
        t: t0,
        px: plan.entry,
    };

    let close = |qty: f64, px: f64, pnl: &mut f64, fees: &mut f64| {
        let fill = px * (1.0 - s * c.slippage);
        *pnl += qty * s * (fill - plan.entry);
        *fees += qty * c.taker_fee * fill;
    };

    for &tick in ticks.iter().skip(1) {
        last = tick;
        let px = tick.px;

        // 1) Resting stop.
        if s * (px - sl) <= 0.0 {
            // Gapped through the stop: we get the (worse) tick price, not the stop price.
            let fill_at = if s * (px - sl) < 0.0 { px } else { sl };
            close(remaining, fill_at, &mut pnl, &mut fees);
            remaining = 0.0;
            exit = Exit::Stop;
            break;
        }
        // 2) Resting take-profit.
        if !tp_filled && s * (px - plan.tp) >= 0.0 {
            let qty = params.tp_fraction.clamp(0.0, 1.0).min(remaining);
            close(qty, plan.tp, &mut pnl, &mut fees);
            remaining -= qty;
            tp_filled = true;
            if remaining <= 1e-12 {
                remaining = 0.0;
                exit = Exit::TakeProfit;
                break;
            }
        }

        // 3) Track the best price and progress.
        best = side.tighter(best, px);
        max_progress = max_progress.max(plan.progress(px));

        // 4) Engine update (takes effect from the next tick).
        if tick.t - last_update >= cfg.update_secs {
            let target = stop_target(plan, params, best, tp_filled);
            let new_sl = side.tighter(sl, target);
            if new_sl != sl {
                stop_moves += 1;
                sl = new_sl;
            }
            last_update = tick.t;
        }

        if tick.t - t0 >= cfg.max_hold_secs {
            close(remaining, px, &mut pnl, &mut fees);
            remaining = 0.0;
            exit = Exit::Timeout;
            break;
        }
    }
    if remaining > 0.0 {
        close(remaining, last.px, &mut pnl, &mut fees);
    }

    Outcome {
        r_net: (pnl - fees) / plan.risk(),
        exit,
        tp_hit: tp_filled,
        max_progress,
        hold_secs: last.t - t0,
        stop_moves,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Side;

    const FREE: SimConfig = SimConfig {
        update_secs: 0,
        max_hold_secs: i64::MAX,
        costs: Costs {
            maker_fee: 0.0,
            taker_fee: 0.0,
            slippage: 0.0,
        },
    };

    fn path(prices: &[f64]) -> Vec<Tick> {
        prices
            .iter()
            .enumerate()
            .map(|(i, &px)| Tick { t: i as i64, px })
            .collect()
    }

    fn long() -> Plan {
        Plan::new(Side::Long, 60_000.0, 59_000.0, 62_000.0).unwrap()
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn plain_stop_is_minus_one_r() {
        let o = simulate(
            &long(),
            &Params::PLAIN,
            &path(&[60_000.0, 59_500.0, 58_900.0]),
            &FREE,
        );
        assert_eq!(o.exit, Exit::Stop);
        // Gapped through the stop: filled at the worse tick price 58,900.
        approx(o.r_net, -1.1);
    }

    #[test]
    fn plain_tp_is_plus_two_r() {
        let o = simulate(
            &long(),
            &Params::PLAIN,
            &path(&[60_000.0, 61_000.0, 62_100.0]),
            &FREE,
        );
        assert_eq!(o.exit, Exit::TakeProfit);
        approx(o.r_net, 2.0);
    }

    #[test]
    fn smart_turns_reversal_after_halfway_into_breakeven() {
        // Up to 61,200 (60% of the way), then down through the old stop.
        let p = path(&[60_000.0, 61_200.0, 60_500.0, 60_060.0, 58_000.0]);
        let plain = simulate(&long(), &Params::PLAIN, &p, &FREE);
        let smart = simulate(&long(), &Params::STANDARD, &p, &FREE);
        assert!(plain.r_net < -1.0);
        assert_eq!(smart.exit, Exit::Stop);
        approx(smart.r_net, 0.06); // stopped at 60,060

        // If price gaps past the stop between ticks, the fill is the (worse) tick price.
        let gap = path(&[60_000.0, 61_200.0, 60_500.0, 59_900.0]);
        approx(
            simulate(&long(), &Params::STANDARD, &gap, &FREE).r_net,
            -0.1,
        );
    }

    #[test]
    fn smart_runner_beats_plain_tp_in_trend() {
        let p = path(&[
            60_000.0, 61_000.0, 61_500.0, 62_000.0, 63_000.0, 63_500.0, 62_900.0,
        ]);
        let plain = simulate(&long(), &Params::PLAIN, &p, &FREE);
        let smart = simulate(&long(), &Params::STANDARD, &p, &FREE);
        approx(plain.r_net, 2.0);
        // Half at TP (+2R * 0.5 = 1R); the runner's stop trailed to 63,000 and the next
        // tick printed 62,900, so it fills there (+2.9R * 0.5 = 1.45R).
        assert_eq!(smart.exit, Exit::Stop);
        assert!(smart.tp_hit);
        approx(smart.r_net, 2.45);
    }

    #[test]
    fn update_latency_delays_stop_moves() {
        // With a 10 s update cadence the breakeven move has not happened yet.
        let p = path(&[60_000.0, 61_200.0, 58_500.0]);
        let slow = SimConfig {
            update_secs: 10,
            ..FREE
        };
        let o = simulate(&long(), &Params::STANDARD, &p, &slow);
        approx(o.r_net, -1.5);
        assert_eq!(o.stop_moves, 0);
    }

    #[test]
    fn costs_reduce_result() {
        let costs = SimConfig {
            costs: Costs {
                maker_fee: 0.00015,
                taker_fee: 0.00045,
                slippage: 0.0001,
            },
            ..FREE
        };
        let o = simulate(
            &long(),
            &Params::PLAIN,
            &path(&[60_000.0, 62_000.0]),
            &costs,
        );
        assert!(o.r_net < 2.0 && o.r_net > 1.9, "{}", o.r_net);
    }

    #[test]
    fn timeout_and_end_of_data_close_at_market() {
        let p = path(&[60_000.0, 60_300.0, 60_600.0]);
        let o = simulate(
            &long(),
            &Params::PLAIN,
            &p,
            &SimConfig {
                max_hold_secs: 1,
                ..FREE
            },
        );
        assert_eq!(o.exit, Exit::Timeout);
        approx(o.r_net, 0.3);
        let o = simulate(&long(), &Params::PLAIN, &p, &FREE);
        assert_eq!(o.exit, Exit::EndOfData);
        approx(o.r_net, 0.6);
    }

    #[test]
    fn short_mirror() {
        let plan = Plan::new(Side::Short, 100.0, 101.0, 98.0).unwrap();
        let p = path(&[100.0, 99.0, 98.0, 97.0, 97.6]);
        let smart = simulate(&plan, &Params::STANDARD, &p, &FREE);
        // Half at 98 (+1R), runner stop trails to 97.5, hit at 97.6 tick => filled 97.6 (+2.4R * 0.5)
        assert!(smart.tp_hit);
        approx(smart.r_net, 1.0 + 1.2);
    }
}
