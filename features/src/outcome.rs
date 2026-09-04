//! Forward outcomes (research-lab hardening Phase 4, REL-12..REL-14).
//!
//! Attaches outcomes to observations AFTER the fact, from a recorded
//! per-symbol mark series — never during signal generation, and never from
//! data the signal could not have known (no lookahead, REL-14):
//!
//! - entry price = the LAST recorded mark at-or-before the observation time
//!   (this is exactly the mark the signal could see when it fired);
//! - for each horizon: exit price = the last recorded mark at-or-before
//!   `ts + horizon`; MFE/MAE = direction-signed best/worst mark return within
//!   `(ts, ts+horizon]`;
//! - an outcome exists ONLY when the recorded series extends at least to
//!   `ts + horizon` AND there is a mark strictly after the entry within the
//!   window — anything else yields NO outcome (a shortened or empty window is
//!   never fabricated into a full-horizon return, REL-13).
//!
//! Pure: no I/O, no wall clock (PD-3). The mark series is whatever the caller
//! recorded (in the sim: the backtester's per-symbol mark history).

use crate::observation::{Direction, ObservationOutcome, SignalObservation};

/// Compute forward outcomes for every observation against ONE symbol's mark
/// series. `marks` must be sorted ascending by timestamp. `horizons` are in
/// nanoseconds. `round_trip_cost` is the fraction subtracted from gross to get
/// net (e.g. `taker_fee + maker_fee`).
///
/// Returns new observations (the inputs are immutable) with `outcomes` filled
/// for every horizon whose window closed within the recorded series.
pub fn attach_outcomes(
    observations: &[SignalObservation],
    marks: &[(i64, f64)],
    horizons: &[i64],
    round_trip_cost: f64,
) -> Vec<SignalObservation> {
    let mut out = Vec::with_capacity(observations.len());
    for obs in observations {
        let mut obs = obs.clone();
        obs.outcomes = outcomes_for(obs.timestamp_ns, obs.direction, marks, horizons, round_trip_cost);
        out.push(obs);
    }
    out
}

/// Outcomes for one (ts, direction) against one mark series.
pub fn outcomes_for(
    ts_ns: i64,
    direction: Direction,
    marks: &[(i64, f64)],
    horizons: &[i64],
    round_trip_cost: f64,
) -> Vec<ObservationOutcome> {
    let mut out = Vec::new();
    // Entry = last mark at-or-before ts (the mark the signal could see).
    let Some(entry_idx) = last_at_or_before(marks, ts_ns) else {
        return out; // no visible mark ⇒ nothing measurable (never fabricated)
    };
    let entry_price = marks[entry_idx].1;
    let s = direction.sign();
    for &h in horizons {
        let exit_ts = ts_ns.saturating_add(h);
        // REL-13: the RECORDED SERIES must extend at least to the horizon
        // boundary. Without this guard, an observation near the end of the
        // series would get an outcome measured to the last mark — a SHORTENED
        // window labeled as the full horizon, which is fabrication.
        if marks.last().is_none_or(|&(t, _)| t < exit_ts) {
            continue;
        }
        let Some(exit_idx) = last_at_or_before(marks, exit_ts) else {
            continue; // no mark at-or-before the horizon end ⇒ no outcome
        };
        if exit_idx <= entry_idx {
            continue; // no mark strictly after entry within the window
        }
        let exit_price = marks[exit_idx].1;
        let gross = s * (exit_price / entry_price - 1.0);
        let net = gross - round_trip_cost;
        // MFE/MAE over marks in (ts, ts+h] — the entry mark itself is the
        // zero-excursion baseline, excluded (REL-14: only post-signal data).
        let mut mfe = 0.0f64;
        let mut mae = 0.0f64;
        for &(t, p) in &marks[entry_idx + 1..=exit_idx] {
            if t <= ts_ns {
                continue;
            }
            let r = s * (p / entry_price - 1.0);
            if r > mfe {
                mfe = r;
            }
            if r < mae {
                mae = r;
            }
        }
        out.push(ObservationOutcome {
            horizon_ns: h,
            entry_price,
            exit_price,
            gross_return: gross,
            net_return: net,
            mfe,
            mae,
            hit: gross > 0.0,
        });
    }
    out
}

/// Index of the last `(t, _)` with `t <= target`; `None` when none qualifies.
fn last_at_or_before(marks: &[(i64, f64)], target: i64) -> Option<usize> {
    if marks.is_empty() {
        return None;
    }
    // Binary search: first index with t > target, then step back one.
    let mut lo = 0usize;
    let mut hi = marks.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if marks[mid].0 <= target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        None
    } else {
        Some(lo - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic synthetic series: 1s marks at prices 100, 102, 101, 104, 99.
    fn marks() -> Vec<(i64, f64)> {
        vec![
            (1_000_000_000, 100.0),
            (2_000_000_000, 102.0),
            (3_000_000_000, 101.0),
            (4_000_000_000, 104.0),
            (5_000_000_000, 99.0),
        ]
    }

    #[test]
    fn rel_12_horizon_math_and_direction_symmetry() {
        // Long at t=1s, entry 100. Horizon 2s ⇒ exit at t=3s = 101 ⇒ gross +1%.
        let out = outcomes_for(1_000_000_000, Direction::Long, &marks(), &[2_000_000_000], 0.0);
        assert_eq!(out.len(), 1);
        let o = &out[0];
        assert_eq!(o.entry_price, 100.0);
        assert_eq!(o.exit_price, 101.0);
        assert!((o.gross_return - 0.01).abs() < 1e-12);
        assert!(o.hit);
        // Short symmetric: −1%.
        let out_s = outcomes_for(1_000_000_000, Direction::Short, &marks(), &[2_000_000_000], 0.0);
        assert!((out_s[0].gross_return - (-0.01)).abs() < 1e-12);
        assert!(!out_s[0].hit);
    }

    #[test]
    fn rel_13_net_return_subtracts_cost_and_open_window_is_omitted() {
        // Cost 0.003 ⇒ net = 0.01 − 0.003 = 0.007.
        let out = outcomes_for(1_000_000_000, Direction::Long, &marks(), &[2_000_000_000], 0.003);
        assert!((out[0].net_return - 0.007).abs() < 1e-12);
        // Observation at the END of the series (t=5s, the last mark): no mark
        // strictly after the entry within ANY horizon ⇒ NO outcome, never
        // fabricated from the entry price itself.
        let out_open = outcomes_for(5_000_000_000, Direction::Long, &marks(), &[10_000_000_000], 0.0);
        assert!(out_open.is_empty());
        // No mark at-or-before the observation time ⇒ nothing measurable.
        let out_no_entry = outcomes_for(0, Direction::Long, &marks(), &[2_000_000_000], 0.0);
        assert!(out_no_entry.is_empty());
    }

    #[test]
    fn rel_14_mfe_mae_use_only_post_signal_marks() {
        // Long at t=1s entry 100. Window (1s, 3s] = {102, 101}.
        // MFE = 2% (102), MAE = 0 (101 is still > 100) — the pre-entry mark
        // history must never contribute.
        let out = outcomes_for(1_000_000_000, Direction::Long, &marks(), &[2_000_000_000], 0.0);
        assert!((out[0].mfe - 0.02).abs() < 1e-12);
        assert!((out[0].mae).abs() < 1e-12);
        // Short at t=1s: MFE = +1% (99? no — window (1s,3s] only, = {102,101};
        // adverse 2% ⇒ MAE = −0.02, MFE = 0).
        let out_s = outcomes_for(1_000_000_000, Direction::Short, &marks(), &[2_000_000_000], 0.0);
        assert!((out_s[0].mfe).abs() < 1e-12);
        assert!((out_s[0].mae + 0.02).abs() < 1e-12);
    }

    #[test]
    fn rel_14_entry_is_last_mark_at_or_before_signal_time() {
        // Fire at t=2.5s: the visible marks are t≤2.5s = {100@1s, 102@2s} ⇒
        // entry must be 102 (the newest VISIBLE mark), not 100.
        let out = outcomes_for(2_500_000_000, Direction::Long, &marks(), &[2_000_000_000], 0.0);
        assert_eq!(out[0].entry_price, 102.0);
        // Exit = last mark ≤ 4.5s = 104.
        assert_eq!(out[0].exit_price, 104.0);
        // Fire at t=1.5s: the t=2s mark is in the FUTURE — the signal cannot
        // see it (no lookahead) ⇒ entry is the t=1s mark (100), NOT 102.
        let out_early = outcomes_for(1_500_000_000, Direction::Long, &marks(), &[2_000_000_000], 0.0);
        assert_eq!(out_early[0].entry_price, 100.0);
        assert_eq!(out_early[0].exit_price, 101.0);
    }

    #[test]
    fn rel_12_attach_outcomes_is_pure_and_idempotent() {
        let obs = crate::observation::SignalObservation {
            observation_id: 1,
            identity: crate::signal_identity::SignalResearchIdentity::new(
                "sig", 1, "p", "c",
            ),
            timestamp_ns: 1_000_000_000,
            symbol: mp_core::SymbolId(1),
            venue: mp_core::Venue::Bybit,
            direction: Direction::Long,
            feature_snapshot: Default::default(),
            quality: crate::data_quality::DataQualityState::Healthy,
            created_at_ns: 0,
            outcomes: Vec::new(),
        };
        let attached = attach_outcomes(&[obs.clone()], &marks(), &[2_000_000_000], 0.0);
        assert_eq!(attached[0].outcomes.len(), 1);
        // Input is untouched (immutability) and re-attachment is deterministic.
        assert!(obs.outcomes.is_empty());
        let again = attach_outcomes(&[obs], &marks(), &[2_000_000_000], 0.0);
        assert_eq!(attached, again);
    }
}