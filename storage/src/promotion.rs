//! Seven-day promotion gate (Phase 0 validation gate).
//!
//! Promotion from raw to research-ready requires 7 consecutive clean days
//! across all required venue/symbol pairs — not just process uptime.  This is
//! deliberately a separate check from the daily scorecard because promotion
//! is a cumulative decision while the scorecard is a point-in-time verdict.

use crate::audit::DailyScorecard;
use crate::determinism::DeterminismArtifact;
use serde::Serialize;

/// Minimum consecutive clean days required for promotion (ROADMAP Phase 0).
pub const REQUIRED_CONSECUTIVE_CLEAN_DAYS: usize = 7;

/// Zero-Cost Mode: relaxed promotion requirements (docs/ZERO_COST_MODE.md).
/// Longer streak (14 days) compensates for lower per-day bar (0.95 vs 0.995).
pub const ZERO_COST_REQUIRED_CONSECUTIVE_CLEAN_DAYS: usize = 14;

/// Zero-Cost Mode: minimum coverage threshold (down from 0.995).
pub const ZERO_COST_MIN_COVERAGE: f64 = 0.95;

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
    /// Days inside the best qualifying run that carried stale bursts on some
    /// required recording — the `why` when a full streak is held back by the
    /// Phase-0 window condition (spec 024, amendment 2026-08-12). Empty when
    /// promoted (the qualifying window is burst-free by construction).
    pub burst_days: Vec<BurstDay>,
    /// Determinism condition (spec 018 MOD-9, 2026-08-13): every day in the
    /// qualifying window must also carry a PASSING determinism artifact. Set
    /// only by [`check_promotion_determinism`]; the plain [`check_promotion`]
    /// leaves it `true` (the base gate does not know about determinism).
    #[serde(default)]
    pub determinism_ok: bool,
    /// Days in the qualifying run whose determinism artifact is missing,
    /// corrupt, or failed — the `why` when determinism holds a window back.
    #[serde(default)]
    pub determinism_failures: Vec<String>,
}

/// One bursty day inside the best qualifying run, with the recordings that
/// carried stale bursts (venue:symbol).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BurstDay {
    pub date: String,
    pub recordings: Vec<String>,
}

/// Check whether a sequence of daily scorecards (sorted by date) contains at
/// least `REQUIRED_CONSECUTIVE_CLEAN_DAYS` consecutive promotable days.
///
/// Returns the most recent qualifying window found anywhere in the sequence
/// (on equal-length runs, the latest is preferred — a fresh burst-free tail
/// after an old bursty streak is the window a live gate should promote on).
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
            burst_days: vec![],
            determinism_ok: true,
            determinism_failures: vec![],
        };
    }

    let mut best_run = 0usize;
    let mut best_start = 0usize;
    let mut current_run = 0usize;
    let mut current_start = 0usize;
    let mut first_failure: Option<String> = None;

    for (i, card) in scorecards.iter().enumerate() {
        // Adjacency gate: consecutive scorecards must be adjacent
        // UTC calendar days (exactly 1 day apart). A missing
        // scorecard file breaks the streak even if every present
        // file is clean — filesystem listing is not proof of
        // calendar continuity.
        if i > 0 && !dates_are_adjacent(&scorecards[i - 1].date, &card.date) {
            current_run = 0;
            if first_failure.is_none() {
                first_failure = Some(card.date.clone());
            }
        }
        if card.promotable {
            if current_run == 0 {
                current_start = i;
            }
            current_run += 1;
            if current_run >= best_run {
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

    // Phase-0 window condition (spec 024, amendment 2026-08-12): promotion
    // additionally requires a contiguous burst-free window of `required` days
    // inside the best run. A bursty day never breaks the streak (it still
    // audits clean and counts) — but `PROMOTED` needs a window where every
    // required recording on every day has zero stale bursts. The best run is
    // scanned for its longest burst-free sub-run, so a clean streak that
    // merely contains an isolated early burst still qualifies on the
    // burst-free tail (that tail is the qualifying window).
    let run = &scorecards[best_start..best_start + best_run];
    let (burst_free_start, burst_free_len) = longest_burst_free_run(run);
    let promoted = best_run >= required && burst_free_len >= required;
    let (window_start, window_end) = if promoted {
        (
            Some(run[burst_free_start].date.clone()),
            Some(run[burst_free_start + burst_free_len - 1].date.clone()),
        )
    } else {
        (None, None)
    };
    // The `why` when a full streak is held back: the bursty days inside the
    // best run, named with the recordings that carried bursts.
    let burst_days = if promoted {
        Vec::new()
    } else {
        burst_days_in(run)
    };

    PromotionVerdict {
        consecutive_clean: best_run,
        required,
        promoted,
        first_failure: if promoted { None } else { first_failure },
        window_start,
        window_end,
        burst_days,
        determinism_ok: true,
        determinism_failures: vec![],
    }
}

/// Promotion verdict enriched with the spec 018 determinism condition:
/// every day in the qualifying window must carry a PASSING determinism
/// artifact (MOD-9 "a diff MUST block promotion"). The base verdict is
/// computed first; when the numeric window qualifies, the window's days are
/// checked against `artifacts` — a day without a passing artifact (missing,
/// corrupt, or failed) is named in `determinism_failures` and the verdict is
/// held back. `window_start`/`window_end` stay set (the numeric window is
/// real; determinism is a separate, orthogonal condition — the same shape as
/// the burst-days window condition).
pub fn check_promotion_determinism(
    scorecards: &[DailyScorecard],
    artifacts: &[DeterminismArtifact],
) -> PromotionVerdict {
    let mut verdict = check_promotion(scorecards);
    if !verdict.promoted {
        // The numeric gate already failed — determinism changes nothing and
        // must not muddy the `why`.
        return verdict;
    }
    let (Some(ws), Some(we)) = (
        verdict.window_start.as_deref(),
        verdict.window_end.as_deref(),
    ) else {
        return verdict;
    };
    let failures: Vec<String> = scorecards
        .iter()
        .filter(|c| c.date.as_str() >= ws && c.date.as_str() <= we)
        .filter(|c| !artifacts.iter().any(|a| a.date == c.date && a.passed))
        .map(|c| c.date.clone())
        .collect();
    verdict.determinism_failures = failures.clone();
    verdict.determinism_ok = failures.is_empty();
    if !failures.is_empty() {
        verdict.promoted = false;
    }
    verdict
}

/// A day carries a burst for the window condition when any recording's
/// stale-burst count is nonzero.
fn day_has_bursts(card: &DailyScorecard) -> bool {
    card.recording_bursts.iter().any(|r| r.stale_bursts > 0)
}

/// Check whether two date strings (`YYYYMMDD`) are adjacent UTC calendar
/// days (exactly 1 day apart). Returns `false` for equal dates, non-adjacent
/// dates, or malformed input.
fn dates_are_adjacent(a: &str, b: &str) -> bool {
    let Ok((ya, ma, da)) = parse_ymd(a) else {
        return false;
    };
    let Ok((yb, mb, db)) = parse_ymd(b) else {
        return false;
    };
    days_since_epoch(yb, mb, db) - days_since_epoch(ya, ma, da) == 1
}

fn parse_ymd(s: &str) -> Result<(u32, u32, u32), ()> {
    // Accept both YYYYMMDD (8 chars) and YYYY-MM-DD (10 chars)
    let (y, m, d) = if s.len() == 8 {
        (s[0..4].parse::<u32>().map_err(|_| ())?,
         s[4..6].parse::<u32>().map_err(|_| ())?,
         s[6..8].parse::<u32>().map_err(|_| ())?)
    } else if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        (s[0..4].parse::<u32>().map_err(|_| ())?,
         s[5..7].parse::<u32>().map_err(|_| ())?,
         s[8..10].parse::<u32>().map_err(|_| ())?)
    } else {
        return Err(());
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(());
    }
    Ok((y, m, d))
}

/// Convert a (year, month, day) triple to days since a fixed epoch (2000-01-01).
fn days_since_epoch(y: u32, m: u32, d: u32) -> i64 {
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let adjusted_month = m - 3;
    let year_offset = if adjusted_month < 0 { y - 1 } else { y };
    let month_offset = if adjusted_month < 0 {
        adjusted_month + 12
    } else {
        adjusted_month
    };
    let era = year_offset / 400;
    let year_of_era = year_offset - era * 400;
    let doe =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + (month_offset * 306 + 5) / 10 + d
            - 1;
    era * 146097 + doe - 719468
}

/// Longest contiguous run of burst-free days in `cards`; returns
/// `(start_index, length)` of the latest longest run (consistent with the
/// streak scan above, which also prefers the most recent on ties).
fn longest_burst_free_run(cards: &[DailyScorecard]) -> (usize, usize) {
    let mut best_start = 0usize;
    let mut best_len = 0usize;
    let mut current_start = 0usize;
    let mut current_len = 0usize;
    for (i, card) in cards.iter().enumerate() {
        if day_has_bursts(card) {
            current_len = 0;
        } else {
            if current_len == 0 {
                current_start = i;
            }
            current_len += 1;
            if current_len >= best_len {
                best_len = current_len;
                best_start = current_start;
            }
        }
    }
    (best_start, best_len)
}

/// All bursty days in `cards`, each with the recordings (venue:symbol) that
/// carried stale bursts.
fn burst_days_in(cards: &[DailyScorecard]) -> Vec<BurstDay> {
    cards
        .iter()
        .filter(|card| day_has_bursts(card))
        .map(|card| BurstDay {
            date: card.date.clone(),
            recordings: card
                .recording_bursts
                .iter()
                .filter(|r| r.stale_bursts > 0)
                .map(|r| format!("{}:{}", r.venue, r.symbol))
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{scorecard, AuditFinding, RawLogAudit};
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
            stale_bursts: vec![],
            stale_silences_ms: vec![],
            worst_gap_ns: 0,
            findings: vec![],
        }
    }

    fn dirty_audit() -> RawLogAudit {
        RawLogAudit {
            findings: vec![AuditFinding {
                code: "test".into(),
                detail: "fail".into(),
            }],
            ..clean_audit()
        }
    }

    fn card(date: &str, clean: bool) -> DailyScorecard {
        let audit = if clean { clean_audit() } else { dirty_audit() };
        scorecard(date, vec![(Venue::BinanceFutures, "BTCUSDT".into(), audit)])
    }

    /// A still-promotable day (clean audit, no blockers) that carries one
    /// stale burst — the exact shape the window condition must hold back
    /// without breaking the streak (spec 024, amendment 2026-08-12).
    fn burst_card(date: &str) -> DailyScorecard {
        let mut audit = clean_audit();
        audit.stale_bursts = vec![crate::audit::TimeRange {
            start_ns: 0,
            end_ns: 0,
        }];
        scorecard(date, vec![(Venue::BinanceFutures, "BTCUSDT".into(), audit)])
    }

    #[test]
    fn promotion_requires_seven_consecutive_clean_days() {
        let cards: Vec<_> = (1..=7)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
        let v = check_promotion(&cards);
        assert!(v.promoted);
        assert_eq!(v.consecutive_clean, 7);
        assert_eq!(v.window_start.as_deref(), Some("20260701"));
        assert_eq!(v.window_end.as_deref(), Some("20260707"));
    }

    #[test]
    fn promotion_rejects_non_adjacent_clean_scorecard_dates() {
        // A scorecard file can be absent when the pipeline fails. Seven files
        // must never be mistaken for seven calendar days of evidence.
        let cards = [
            "20260701", "20260702", "20260703", "20260705", "20260706", "20260707", "20260708",
        ]
        .into_iter()
        .map(|date| card(date, true))
        .collect::<Vec<_>>();

        let v = check_promotion(&cards);

        assert!(!v.promoted);
        assert_eq!(v.consecutive_clean, 4);
    }

    #[test]
    fn promotion_fails_with_six_clean_days() {
        let cards: Vec<_> = (1..=6)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
        let v = check_promotion(&cards);
        assert!(!v.promoted);
        assert_eq!(v.consecutive_clean, 6);
    }

    #[test]
    fn promotion_resets_on_dirty_day() {
        let mut cards: Vec<_> = (1..=4)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
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
        assert!(v.burst_days.is_empty());
    }

    #[test]
    fn promotion_window_condition_holds_back_bursty_full_streak() {
        // Seven clean days, one of them bursty: the streak stands at 7 (a
        // burst day still audits clean and counts) but promotion waits, and
        // the verdict names the burst day and recording as the why.
        let mut cards: Vec<_> = (1..=7)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
        cards[3] = burst_card("20260704");
        let v = check_promotion(&cards);
        assert!(
            !v.promoted,
            "a bursty day in the window must hold promotion"
        );
        assert_eq!(v.consecutive_clean, 7, "the streak itself is intact");
        assert!(
            v.first_failure.is_none(),
            "no clean-day break — bursts are the why"
        );
        assert_eq!(v.burst_days.len(), 1);
        assert_eq!(v.burst_days[0].date, "20260704");
        assert_eq!(
            v.burst_days[0].recordings,
            vec!["binance:BTCUSDT".to_string()]
        );
    }

    #[test]
    fn promotion_promotes_on_burst_free_subwindow() {
        // Ten clean days, one isolated burst on day 3: the burst-free tail
        // (days 4..10) is a 7-day window, so promotion succeeds there — the
        // condition is window-level, never a per-day veto.
        let mut cards: Vec<_> = (1..=10)
            .map(|d| card(&format!("202607{d:02}"), true))
            .collect();
        cards[2] = burst_card("20260703");
        let v = check_promotion(&cards);
        assert!(v.promoted, "a burst-free 7-day tail must qualify");
        assert_eq!(v.consecutive_clean, 10);
        assert_eq!(v.window_start.as_deref(), Some("20260704"));
        assert_eq!(v.window_end.as_deref(), Some("20260710"));
        assert!(v.burst_days.is_empty(), "qualifying window is burst-free");
    }

    #[test]
    fn promotion_bursts_on_window_edges_block() {
        // Seven clean days with bursts on the first and last day: no
        // burst-free 7-day window exists (longest is 5), so promotion waits
        // despite the full streak, and both burst days are named.
        let mut cards: Vec<_> = (1..=7)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
        cards[0] = burst_card("20260701");
        cards[6] = burst_card("20260707");
        let v = check_promotion(&cards);
        assert!(!v.promoted);
        assert_eq!(v.consecutive_clean, 7);
        assert_eq!(v.burst_days.len(), 2);
        assert_eq!(v.burst_days[0].date, "20260701");
        assert_eq!(v.burst_days[1].date, "20260707");
    }

    /// MOD-9 (spec 018, 2026-08-13): a full clean window is held back when a
    /// day in it lacks a PASSING determinism artifact — and the missing day
    /// is named as the why. The plain check_promotion (no determinism) stays
    /// orthogonal and passes.
    #[test]
    fn mod_11_determinism_blocks_promotion_and_names_failures() {
        use crate::determinism::DeterminismArtifact;
        let cards: Vec<_> = (1..=7)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
        let plain = check_promotion(&cards);
        assert!(plain.promoted, "the numeric gate passes on its own");
        assert!(
            plain.determinism_ok,
            "plain gate leaves determinism_ok true"
        );

        // No artifacts at all ⇒ every window day lacks proof ⇒ blocked.
        let v = check_promotion_determinism(&cards, &[]);
        assert!(
            !v.promoted,
            "a window with no determinism proof must not promote"
        );
        assert_eq!(v.consecutive_clean, 7, "the streak itself is intact");
        assert!(!v.determinism_ok);
        assert_eq!(v.determinism_failures.len(), 7, "all window days named");
        assert!(v.determinism_failures.contains(&"20260703".to_string()));

        // Artifacts for every day but one failing ⇒ still blocked, one name.
        let mut artifacts: Vec<DeterminismArtifact> = (1..=7)
            .map(|d| DeterminismArtifact {
                date: format!("2026070{d}"),
                passed: true,
                ..Default::default()
            })
            .collect();
        artifacts[3].passed = false;
        let v2 = check_promotion_determinism(&cards, &artifacts);
        assert!(!v2.promoted);
        assert_eq!(v2.determinism_failures, vec!["20260704".to_string()]);

        // All passing ⇒ the window promotes, determinism_ok true.
        for a in artifacts.iter_mut() {
            a.passed = true;
        }
        let v3 = check_promotion_determinism(&cards, &artifacts);
        assert!(v3.promoted);
        assert!(v3.determinism_ok);
        assert!(v3.determinism_failures.is_empty());
    }

    #[test]
    fn promotion_bursts_do_not_mask_clean_day_break() {
        // A bursty day is still a clean day for the streak, but a genuinely
        // dirty day still breaks it: 6 clean days, day 7 bursty, day 8 dirty,
        // then 7 more clean. Best promotable run is 7 (days 9..15) and it is
        // burst-free, so promotion succeeds there — the dirty day is the
        // first failure, not the burst day.
        let mut cards: Vec<_> = (1..=6)
            .map(|d| card(&format!("2026070{d}"), true))
            .collect();
        cards.push(burst_card("20260707"));
        cards.push(card("20260708", false));
        cards.extend((9..=15).map(|d| card(&format!("202607{d:02}"), true)));
        let v = check_promotion(&cards);
        assert!(v.promoted);
        assert_eq!(v.window_start.as_deref(), Some("20260709"));
        assert_eq!(v.window_end.as_deref(), Some("20260715"));
    }
}
