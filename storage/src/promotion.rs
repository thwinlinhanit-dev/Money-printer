//! Seven-day promotion gate (Phase 0 validation gate).
//!
//! Promotion from raw to research-ready requires 7 consecutive clean days
//! across all required venue/symbol pairs — not just process uptime.  This is
//! deliberately a separate check from the daily scorecard because promotion
//! is a cumulative decision while the scorecard is a point-in-time verdict.

use crate::audit::DailyScorecard;
use serde::Serialize;

/// Minimum consecutive clean days required for promotion (ROADMAP Phase 0).
pub const REQUIRED_CONSECUTIVE_CLEAN_DAYS: usize = 7;

/// Result of the promotion check.
#[derive(Debug, Clone, Serialize)]
pub struct PromotionVerdict {
    /// Number of consecutive clean days ending at the most recent scorecard.
    pub consecutive_clean: usize,
    /// The required threshold.
    pub required: usize,
    /// Whether the gate passes.
    pub promoted: bool,
    /// If not promoted, the date that broke the streak (or the first date if
    /// no clean days exist).
    pub first_failure: Option<String>,
    /// If promoted, the start date of the qualifying clean window.
    pub window_start: Option<String>,
    /// If promoted, the end date of the qualifying clean window.
    pub window_end: Option<String>,
}

/// Check whether a sequence of daily scorecards (sorted by date) contains at
/// least `REQUIRED_CONSECUTIVE_CLEAN_DAYS` consecutive promotable days.
///
/// Returns the longest qualifying window found anywhere in the sequence.
pub fn check_promotion(scorecards: &[DailyScorecard]) -> PromotionVerdict {
    check_promotion_n(scorecards, REQUIRED_CONSECUTIVE_CLEAN_DAYS)
}

/// Same as [`check_promotion`] but with a configurable threshold (for testing).
pub fn check_promotion_n(scorecards: &[DailyScorecard], required: usize) -> PromotionVerdict {
    if scorecards.is_empty() {
        return PromotionVerdict {
            consecutive_clean: 0,
            required,
            promoted: false,
            first_failure: None,
            window_start: None,
            window_end: None,
        };
    }

    let mut best_run = 0usize;
    let mut best_start = 0usize;
    let mut current_run = 0usize;
    let mut current_start = 0usize;
    let mut first_failure: Option<String> = None;

    for (i, card) in scorecards.iter().enumerate() {
        if card.promotable {
            if current_run == 0 {
                current_start = i;
            }
            current_run += 1;
            if current_run > best_run {
                best_run = current_run;
                best_start = current_start;
            }
        } else {
            current_run = 0;
            if first_failure.is_none() {
                first_failure = Some(card.date.clone());
            }
        }
    }

    let promoted = best_run >= required;
    let (window_start, window_end) = if promoted {
        (
            Some(scorecards[best_start].date.clone()),
            Some(scorecards[best_start + best_run - 1].date.clone()),
        )
    } else {
        (None, None)
    };

    PromotionVerdict {
        consecutive_clean: best_run,
        required,
        promoted,
        first_failure: if promoted { None } else { first_failure },
        window_start,
        window_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{scorecard, RawLogAudit, AuditFinding};
    use mp_core::Venue;
    use std::collections::BTreeMap;

    fn clean_audit() -> RawLogAudit {
        RawLogAudit {
            event_count: 100,
            first_recv_ts_ns: Some(1),
            last_recv_ts_ns: Some(1_000_000_000),
            coverage: 1.0,
            streams: BTreeMap::new(),
            gaps: vec![],
            stale_periods: vec![],
            findings: vec![],
        }
    }

    fn dirty_audit() -> RawLogAudit {
        RawLogAudit {
            findings: vec![AuditFinding { code: "test".into(), detail: "fail".into() }],
            ..clean_audit()
        }
    }

    fn card(date: &str, clean: bool) -> DailyScorecard {
        let audit = if clean { clean_audit() } else { dirty_audit() };
        scorecard(date, vec![(Venue::BinanceFutures, "BTCUSDT".into(), audit)])
    }

    #[test]
    fn promotion_requires_seven_consecutive_clean_days() {
        let cards: Vec<_> = (1..=7).map(|d| card(&format!("2026070{d}"), true)).collect();
        let v = check_promotion(&cards);
        assert!(v.promoted);
        assert_eq!(v.consecutive_clean, 7);
        assert_eq!(v.window_start.as_deref(), Some("20260701"));
        assert_eq!(v.window_end.as_deref(), Some("20260707"));
    }

    #[test]
    fn promotion_fails_with_six_clean_days() {
        let cards: Vec<_> = (1..=6).map(|d| card(&format!("2026070{d}"), true)).collect();
        let v = check_promotion(&cards);
        assert!(!v.promoted);
        assert_eq!(v.consecutive_clean, 6);
    }

    #[test]
    fn promotion_resets_on_dirty_day() {
        let mut cards: Vec<_> = (1..=4).map(|d| card(&format!("2026070{d}"), true)).collect();
        cards.push(card("20260705", false)); // breaks streak
        cards.extend((6..=9).map(|d| card(&format!("2026070{d}"), true)));
        let v = check_promotion(&cards);
        assert!(!v.promoted); // best run is 4, not 7
        assert_eq!(v.consecutive_clean, 4);
        assert_eq!(v.first_failure.as_deref(), Some("20260705"));
    }

    #[test]
    fn promotion_empty_input() {
        let v = check_promotion(&[]);
        assert!(!v.promoted);
        assert_eq!(v.consecutive_clean, 0);
    }
}
