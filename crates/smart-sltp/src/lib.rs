//! Smart stop-loss / take-profit engine.
//!
//! The trader sets an entry, a stop-loss (SL) and a take-profit (TP). This engine
//! only ever *tightens* the SL as price moves toward the TP, and optionally keeps a
//! runner open past the TP:
//!
//! | Step | When (progress toward TP, by best price since entry) | SL becomes |
//! |------|-------------------------------------------------------|------------|
//! | 0    | below `breakeven_at`                                  | the trader's SL |
//! | 1    | at `breakeven_at` (default 50%)                       | entry + fee buffer |
//! | 2    | at `lock_at` (default 75%)                            | entry + `lock_frac` × (best − entry) |
//! | 3    | TP reached: `tp_fraction` of the size closes at TP    | max(step-2 SL, best − `trail_r` × R) for the runner |
//!
//! Everything is a pure function of (plan, params, best price, whether TP filled),
//! so a live service can recompute it statelessly from exchange data every tick.

#![forbid(unsafe_code)]

/// +1 for long, −1 for short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn sign(self) -> f64 {
        match self {
            Side::Long => 1.0,
            Side::Short => -1.0,
        }
    }

    /// Is `a` at least as favourable as `b` for this side?
    pub fn better_or_eq(self, a: f64, b: f64) -> bool {
        self.sign() * (a - b) >= 0.0
    }

    /// The more favourable (for a stop: tighter) of two prices.
    pub fn tighter(self, a: f64, b: f64) -> f64 {
        if self.better_or_eq(a, b) {
            a
        } else {
            b
        }
    }
}

/// The trader's original intent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub side: Side,
    pub entry: f64,
    pub sl: f64,
    pub tp: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanError {
    NonPositive,
    StopOnWrongSide,
    TargetOnWrongSide,
}

impl Plan {
    pub fn new(side: Side, entry: f64, sl: f64, tp: f64) -> Result<Self, PlanError> {
        let valid = |x: f64| x.is_finite() && x > 0.0;
        if !(valid(entry) && valid(sl) && valid(tp)) {
            return Err(PlanError::NonPositive);
        }
        let s = side.sign();
        if s * (entry - sl) <= 0.0 {
            return Err(PlanError::StopOnWrongSide);
        }
        if s * (tp - entry) <= 0.0 {
            return Err(PlanError::TargetOnWrongSide);
        }
        Ok(Self {
            side,
            entry,
            sl,
            tp,
        })
    }

    /// Initial risk per unit, in price (always > 0).
    pub fn risk(&self) -> f64 {
        self.side.sign() * (self.entry - self.sl)
    }

    /// Distance from entry to TP, in price (always > 0).
    pub fn reward(&self) -> f64 {
        self.side.sign() * (self.tp - self.entry)
    }

    /// Fraction of the way from entry to TP that `price` represents (can be < 0 or > 1).
    pub fn progress(&self, price: f64) -> f64 {
        self.side.sign() * (price - self.entry) / self.reward()
    }
}

/// Strategy knobs. `None` disables a step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Progress at which SL moves to entry + fee buffer.
    pub breakeven_at: Option<f64>,
    /// Fee buffer as a fraction of entry (0.001 = 0.1%).
    pub fee_buffer: f64,
    /// Progress at which SL starts locking `lock_frac` of the best gain.
    pub lock_at: Option<f64>,
    pub lock_frac: f64,
    /// Fraction of the position closed at TP (1.0 = classic full exit, no runner).
    pub tp_fraction: f64,
    /// Runner trailing distance in multiples of initial risk R. `None` = runner keeps step-2 SL.
    pub trail_r: Option<f64>,
}

impl Params {
    /// The default "smart" profile presented to the user.
    pub const STANDARD: Params = Params {
        breakeven_at: Some(0.5),
        fee_buffer: 0.001,
        lock_at: Some(0.75),
        lock_frac: 0.5,
        tp_fraction: 0.5,
        trail_r: Some(0.5),
    };

    /// Leave the trader's SL/TP untouched (baseline).
    pub const PLAIN: Params = Params {
        breakeven_at: None,
        fee_buffer: 0.0,
        lock_at: None,
        lock_frac: 0.0,
        tp_fraction: 1.0,
        trail_r: None,
    };
}

/// Where the stop should be, given the most favourable price seen since entry.
///
/// Pure and monotone in `best`: a better `best` never yields a looser stop. Callers
/// must still combine with the currently resting stop via [`Side::tighter`] so the
/// live stop never loosens even if inputs are revised.
pub fn stop_target(plan: &Plan, p: &Params, best: f64, tp_filled: bool) -> f64 {
    let side = plan.side;
    let s = side.sign();
    // `best` can never be worse than entry for progress purposes.
    let best = side.tighter(best, plan.entry);
    let prog = plan.progress(best);
    let mut sl = plan.sl;

    if let Some(at) = p.breakeven_at {
        if prog >= at {
            sl = side.tighter(sl, plan.entry + s * p.fee_buffer * plan.entry);
        }
    }
    if let Some(at) = p.lock_at {
        if prog >= at {
            sl = side.tighter(sl, plan.entry + p.lock_frac * (best - plan.entry));
        }
    }
    if tp_filled {
        if let Some(k) = p.trail_r {
            sl = side.tighter(sl, best - s * k * plan.risk());
        }
    }
    sl
}

/// Which step the position is in (for messages).
pub fn step(plan: &Plan, p: &Params, best: f64, tp_filled: bool) -> u8 {
    let prog = plan.progress(plan.side.tighter(best, plan.entry));
    if tp_filled {
        3
    } else if p.lock_at.is_some_and(|a| prog >= a) {
        2
    } else if p.breakeven_at.is_some_and(|a| prog >= a) {
        1
    } else {
        0
    }
}

pub mod sim;

#[cfg(test)]
mod tests {
    use super::*;

    fn long() -> Plan {
        // The worked example: long 60,000, SL 59,000, TP 62,000 (R = 1,000, TP = 2R).
        Plan::new(Side::Long, 60_000.0, 59_000.0, 62_000.0).unwrap()
    }

    fn short() -> Plan {
        Plan::new(Side::Short, 100.0, 101.0, 98.0).unwrap()
    }

    #[test]
    fn plan_validation() {
        assert_eq!(
            Plan::new(Side::Long, 100.0, 101.0, 105.0),
            Err(PlanError::StopOnWrongSide)
        );
        assert_eq!(
            Plan::new(Side::Long, 100.0, 99.0, 95.0),
            Err(PlanError::TargetOnWrongSide)
        );
        assert_eq!(
            Plan::new(Side::Short, 100.0, 99.0, 95.0),
            Err(PlanError::StopOnWrongSide)
        );
        assert_eq!(
            Plan::new(Side::Long, 0.0, 99.0, 105.0),
            Err(PlanError::NonPositive)
        );
        assert_eq!(long().risk(), 1000.0);
        assert_eq!(long().reward(), 2000.0);
    }

    #[test]
    fn worked_example_long() {
        let (pl, p) = (long(), Params::STANDARD);
        assert_eq!(
            stop_target(&pl, &p, 60_500.0, false),
            59_000.0,
            "step 0: untouched"
        );
        assert_eq!(
            stop_target(&pl, &p, 61_000.0, false),
            60_060.0,
            "step 1: entry + 0.1%"
        );
        assert_eq!(
            stop_target(&pl, &p, 61_500.0, false),
            60_750.0,
            "step 2: lock half of +1,500"
        );
        // Step 3 after TP: trail 0.5R = 500 below best.
        assert_eq!(stop_target(&pl, &p, 62_000.0, true), 61_500.0);
        assert_eq!(stop_target(&pl, &p, 63_500.0, true), 63_000.0);
    }

    #[test]
    fn mirrored_for_short() {
        let (pl, p) = (short(), Params::STANDARD);
        assert_eq!(stop_target(&pl, &p, 99.5, false), 101.0);
        assert!(
            (stop_target(&pl, &p, 99.0, false) - 99.9).abs() < 1e-9,
            "breakeven = entry - 0.1%"
        );
        assert!(
            (stop_target(&pl, &p, 98.5, false) - 99.25).abs() < 1e-9,
            "lock half of 1.5"
        );
        assert!(
            (stop_target(&pl, &p, 97.0, true) - 97.5).abs() < 1e-9,
            "trail 0.5R above best"
        );
    }

    #[test]
    fn plain_never_moves() {
        let pl = long();
        for best in [59_500.0, 61_000.0, 70_000.0] {
            assert_eq!(stop_target(&pl, &Params::PLAIN, best, false), 59_000.0);
            assert_eq!(stop_target(&pl, &Params::PLAIN, best, true), 59_000.0);
        }
    }

    #[test]
    fn adverse_best_is_ignored() {
        // A "best" worse than entry (bad input) must not loosen anything.
        assert_eq!(
            stop_target(&long(), &Params::STANDARD, 50_000.0, false),
            59_000.0
        );
    }

    #[test]
    fn monotone_never_loosens_as_best_improves() {
        for pl in [long(), short()] {
            for tp_filled in [false, true] {
                let mut prev: Option<f64> = None;
                for i in 0..=400 {
                    // Sweep best from entry to 2x the TP distance beyond.
                    let best = pl.entry + pl.side.sign() * pl.reward() * (i as f64 / 200.0);
                    let sl = stop_target(&pl, &Params::STANDARD, best, tp_filled);
                    if let Some(p) = prev {
                        assert!(
                            pl.side.better_or_eq(sl, p),
                            "{:?} loosened {p} -> {sl} at best {best}",
                            pl.side
                        );
                    }
                    // The stop must stay on the protective side of best.
                    assert!(pl.side.sign() * (best - sl) > 0.0 || best == pl.entry);
                    prev = Some(sl);
                }
            }
        }
    }

    #[test]
    fn steps_for_messages() {
        let (pl, p) = (long(), Params::STANDARD);
        assert_eq!(step(&pl, &p, 60_100.0, false), 0);
        assert_eq!(step(&pl, &p, 61_000.0, false), 1);
        assert_eq!(step(&pl, &p, 61_600.0, false), 2);
        assert_eq!(step(&pl, &p, 62_000.0, true), 3);
    }
}
