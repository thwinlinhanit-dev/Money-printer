//! Retention policy for Zero-Cost Mode (docs/RETENTION_POLICY.md).
//!
//! Enforces age-based data lifecycle on the cold store:
//! - Hot: last 7-14 days of tick-level data (raw logs)
//! - Warm: last 30-60 days of bar aggregates (features)
//! - Cold: older data as daily/4h bars or feature snapshots
//!
//! The retention check is purely advisory in the storage crate — it reports
//! what SHOULD be cleaned up but never deletes without human confirmation
//! (W-6: never delete recorded data without explicit human instruction).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default retention windows for Zero-Cost Mode.
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// Hot tier: keep raw tick-level data for this many days (default: 14).
    pub hot_days: u64,
    /// Warm tier: keep bar aggregates for this many days (default: 60).
    pub warm_days: u64,
    /// Cold tier: keep daily/4h bars indefinitely (manual cleanup only).
    /// This is tracked but never auto-deleted.
    pub cold_days: Option<u64>,
    /// Estimated daily bytes for budget projection (default: 400 MB).
    pub daily_bytes: u64,
    /// Total budget in bytes (default: 10 GB for 30 GB disk with headroom).
    pub budget_bytes: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            hot_days: 14,
            warm_days: 60,
            cold_days: None,
            daily_bytes: 400 * 1024 * 1024,       // 400 MB
            budget_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
        }
    }
}

/// A raw log file that exceeds the retention window.
#[derive(Debug, Clone)]
pub struct RetentionViolation {
    pub path: PathBuf,
    pub age_days: u64,
    pub tier: String,
}

/// Check raw log files against the hot-tier retention window.
///
/// Returns files older than `hot_days` that are candidates for cleanup.
/// This is advisory only — the daily pipeline should report these but
/// never auto-delete (W-6).
pub fn check_hot_tier(
    raw_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
) -> Vec<RetentionViolation> {
    let mut violations = Vec::new();
    let Ok(now_dur) = now.duration_since(UNIX_EPOCH) else {
        return violations;
    };
    let now_secs = now_dur.as_secs();

    let Ok(entries) = std::fs::read_dir(raw_dir) else {
        return violations;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Match {YYYYMMDD}_{venue}_{symbol}.log
        if !name.ends_with(".log") || name.len() < 20 {
            continue;
        }
        let date_str = &name[..8];
        let Ok((y, m, d)) = parse_ymd(date_str) else {
            continue;
        };
        let file_secs = date_to_secs(y, m, d);
        if file_secs == 0 {
            continue;
        }
        let age_secs = now_secs.saturating_sub(file_secs);
        let age_days = age_secs / 86400;

        if age_days > config.hot_days {
            violations.push(RetentionViolation {
                path,
                age_days,
                tier: "hot".to_string(),
            });
        }
    }

    violations.sort_by(|a, b| a.age_days.cmp(&b.age_days));
    violations
}

/// Check feature store files against the warm-tier retention window.
pub fn check_warm_tier(
    features_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
) -> Vec<RetentionViolation> {
    let mut violations = Vec::new();
    let Ok(now_dur) = now.duration_since(UNIX_EPOCH) else {
        return violations;
    };
    let now_secs = now_dur.as_secs();

    let Ok(entries) = std::fs::read_dir(features_dir) else {
        return violations;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        let Ok(mtime_dur) = mtime.duration_since(UNIX_EPOCH) else {
            continue;
        };
        let age_secs = now_secs.saturating_sub(mtime_dur.as_secs());
        let age_days = age_secs / 86400;

        if age_days > config.warm_days {
            violations.push(RetentionViolation {
                path,
                age_days,
                tier: "warm".to_string(),
            });
        }
    }

    violations.sort_by(|a, b| a.age_days.cmp(&b.age_days));
    violations
}

/// Project total storage usage over `horizon_days` given current usage.
pub fn project_usage(
    current_bytes: u64,
    config: &RetentionConfig,
    horizon_days: u64,
) -> StorageProjection {
    let projected = current_bytes + config.daily_bytes * horizon_days;
    let days_to_budget = if config.daily_bytes > 0 {
        (config.budget_bytes.saturating_sub(current_bytes)) / config.daily_bytes
    } else {
        u64::MAX
    };
    StorageProjection {
        current_bytes,
        projected_bytes: projected,
        budget_bytes: config.budget_bytes,
        days_to_budget,
        over_budget: projected > config.budget_bytes,
    }
}

/// Storage usage projection.
#[derive(Debug, Clone)]
pub struct StorageProjection {
    pub current_bytes: u64,
    pub projected_bytes: u64,
    pub budget_bytes: u64,
    pub days_to_budget: u64,
    pub over_budget: bool,
}

fn parse_ymd(s: &str) -> Result<(u32, u32, u32), ()> {
    if s.len() != 8 {
        return Err(());
    }
    let y = s[0..4].parse::<u32>().map_err(|_| ())?;
    let m = s[4..6].parse::<u32>().map_err(|_| ())?;
    let d = s[6..8].parse::<u32>().map_err(|_| ())?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(());
    }
    Ok((y, m, d))
}

/// Convert year/month/day to seconds since epoch (simplified, no leap year).
fn date_to_secs(y: u32, m: u32, d: u32) -> u64 {
    let days = (y - 1970) * 365 + (y - 1970) / 4 + month_day_to_yday(m, d);
    (days as u64) * 86400
}

fn month_day_to_yday(m: u32, d: u32) -> u32 {
    let cum = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    cum[(m - 1) as usize] + d - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_config_defaults() {
        let c = RetentionConfig::default();
        assert_eq!(c.hot_days, 14);
        assert_eq!(c.warm_days, 60);
        assert_eq!(c.daily_bytes, 400 * 1024 * 1024);
        assert_eq!(c.budget_bytes, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn project_usage_within_budget() {
        let c = RetentionConfig::default();
        let p = project_usage(1_000_000_000, &c, 14);
        assert!(!p.over_budget);
        assert!(p.days_to_budget > 20);
    }

    #[test]
    fn project_usage_over_budget() {
        let c = RetentionConfig::default();
        let p = project_usage(9_500_000_000, &c, 14);
        assert!(p.over_budget);
    }

    #[test]
    fn parse_ymd_valid() {
        assert_eq!(parse_ymd("20260831"), Ok((2026, 8, 31)));
        assert_eq!(parse_ymd("20260101"), Ok((2026, 1, 1)));
    }

    #[test]
    fn parse_ymd_invalid() {
        assert!(parse_ymd("20260001").is_err()); // month 0
        assert!(parse_ymd("20261301").is_err()); // month 13
        assert!(parse_ymd("abc").is_err());
        assert!(parse_ymd("2026").is_err());
    }
}
