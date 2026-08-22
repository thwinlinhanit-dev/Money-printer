//! Storage-budget watch (OPS-15): projects when `data/raw` growth reaches the
//! budget cap so the operator hears about it *before* the disk fills — the
//! forward-looking counterpart to `watch::disk_alert` (OPS-7), which only
//! sees current usage. Operationalizes spec 001's appendix revisit trigger
//! ("disk budget — raw data approaching available storage").
//!
//! The trend is derived from the corpus itself, read-only (W-6 — no state
//! file): the daily raw logs are named `{YYYYMMDD}_{venue}_{SYMBOL}.log`, so
//! each day's size is the sum of that day's file sizes. The current (partial)
//! day is excluded; the corpus is append-only, so the CURRENT size is the sum
//! of all sampled days and the GROWTH RATE is the trailing window's mean
//! daily addition; days-to-cap = remaining budget / rate. Pure decision
//! functions — the CLI passes samples/cap in, nothing here reads the clock
//! (PD-3) or touches the filesystem.

use crate::alert::{Alert, Severity};
use std::collections::BTreeMap;
use std::path::Path;

const NS_PER_DAY: i64 = 86_400_000_000_000;

/// Days since 1970-01-01 for a `YYYYMMDD` integer (Howard Hinnant's civil
/// calendar algorithm). Returns `None` for a malformed date. Used to turn the
/// `{YYYYMMDD}_*.log` file-name prefix into a linear day axis — the raw
/// `YYYYMMDD` integer itself jumps by ~70 at month boundaries, which would
/// corrupt the slope.
pub fn days_from_yyyymmdd(yyyymmdd: u32) -> Option<i64> {
    let y = (yyyymmdd / 10000) as i64;
    let m = ((yyyymmdd / 100) % 100) as i64;
    let d = (yyyymmdd % 100) as i64;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146097 + doe - 719468)
}

/// Sum per-day corpus sizes from a directory of `{YYYYMMDD}_*.log` files
/// (read-only, W-6). Non-conforming names (`trace_*`, `.lock_*`,
/// `collected.eventlog`, …) are skipped; the current (partial) day —
/// `now_ns`'s day — is excluded so the trend never understates growth.
/// Returns `(days_since_epoch, bytes)` sorted ascending, or `Err` when the
/// directory is unreadable (fail-closed, CONV-8: a check that cannot see the
/// corpus must not pretend it did). `now_ns` is injected (PD-3).
pub fn sample_daily_sizes(dir: &Path, now_ns: i64) -> Result<Vec<(i64, u64)>, String> {
    let today = now_ns.div_euclid(NS_PER_DAY);
    let mut per_day: BTreeMap<i64, u64> = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for ent in entries {
        let ent = ent.map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        let name = ent.file_name().to_string_lossy().into_owned();
        let Some(prefix) = name.split('_').next() else {
            continue;
        };
        // Exactly the 8-digit `YYYYMMDD` prefix (a 7-digit parse would slip
        // through `parse::<u32>`), and a plausible date.
        if prefix.len() != 8 {
            continue;
        }
        let Some(day_num) = prefix.parse::<u32>().ok() else {
            continue;
        };
        let Some(day) = days_from_yyyymmdd(day_num) else {
            continue;
        };
        if day >= today {
            continue; // the current day is still being written — partial
        }
        let len = ent
            .metadata()
            .map_err(|e| format!("metadata {}: {e}", ent.path().display()))?
            .len();
        *per_day.entry(day).or_insert(0) += len;
    }
    Ok(per_day.into_iter().collect())
}

/// The growth projection for a budget cap: current corpus size, the
/// trailing-window growth rate, and how many days until the cap is hit.
/// `None` when there is no data at all. The corpus is append-only, so the
/// current size is the SUM of every sampled day (a deletion just lowers the
/// total); `growth_bytes_per_day` is the trailing window's MEAN daily
/// addition — the run rate, `Some` whenever the window is non-empty. Using
/// the rate rather than the rate's trend matters: a corpus adding a constant
/// 1 GB/day never trends (slope 0) yet fills the disk in a predictable
/// number of days. `days_to_cap` is `Some(0.0)` when the corpus already
/// meets/exceeds the cap, `Some(remaining / rate)` when the rate is positive,
/// and `None` when nothing is being added (rate ≤ 0) — never an alert.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StorageProjection {
    pub current_bytes: u64,
    pub growth_bytes_per_day: Option<f64>,
    pub days_to_cap: Option<f64>,
}

pub fn project_storage(
    samples: &[(i64, u64)],
    cap_bytes: u64,
    trend_days: usize,
) -> Option<StorageProjection> {
    if samples.is_empty() {
        return None;
    }
    let current_bytes = samples.iter().map(|(_, b)| *b).sum::<u64>();
    let window_start = samples.len().saturating_sub(trend_days.max(1));
    let rate = growth_rate_per_day(&samples[window_start..]);
    let days_to_cap = match rate {
        Some(r) if r > 0.0 => {
            if current_bytes >= cap_bytes {
                Some(0.0)
            } else {
                Some((cap_bytes - current_bytes) as f64 / r)
            }
        }
        _ => {
            if current_bytes >= cap_bytes {
                Some(0.0)
            } else {
                None
            }
        }
    };
    Some(StorageProjection {
        current_bytes,
        growth_bytes_per_day: rate,
        days_to_cap,
    })
}

/// Mean daily addition over the given window, in bytes per day. `None` for an
/// empty window (no data yet — never a rate of zero that implies "flat").
pub fn growth_rate_per_day(samples: &[(i64, u64)]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let n = samples.len() as f64;
    Some(samples.iter().map(|(_, b)| *b as f64).sum::<f64>() / n)
}

/// Storage-budget alert (OPS-15): raises `storage-budget` (P2) when the
/// projection puts the corpus at the cap within `alert_at_days`, or when the
/// corpus already meets/exceeds it (the growth rate is irrelevant once you
/// are there). Corpora adding nothing and projections beyond the horizon stay
/// silent. P2 breaks through quiet hours (OPS-9). `label` names the corpus in
/// the detail (e.g. `data/raw`).
pub fn storage_budget_alert(
    samples: &[(i64, u64)],
    cap_bytes: u64,
    trend_days: usize,
    alert_at_days: f64,
    dedupe_ns: i64,
    label: &str,
) -> Option<Alert> {
    let p = project_storage(samples, cap_bytes, trend_days)?;
    let days = p.days_to_cap?; // None = not growing toward the cap — never an alert
    if days > alert_at_days {
        return None;
    }
    let cur_gb = p.current_bytes as f64 / 1e9;
    let cap_gb = cap_bytes as f64 / 1e9;
    let detail = if p.current_bytes >= cap_bytes {
        format!("{label} {cur_gb:.1} GB used — at/over the {cap_gb:.0} GB budget cap")
    } else {
        match p.growth_bytes_per_day {
            Some(g) => format!(
                "{label} {cur_gb:.1} GB used, growing {:.2} GB/day; projected to hit the {cap_gb:.0} GB cap in {days:.1} days (alert horizon {alert_at_days:.0} days)",
                g / 1e9
            ),
            None => format!(
                "{label} {cur_gb:.1} GB used; at the {cap_gb:.0} GB cap in {days:.1} days"
            ),
        }
    };
    Some(Alert::new(
        "storage-budget",
        Severity::P2,
        dedupe_ns,
        detail,
    ))
}

/// One entry from the drain manifest (`data/vps_drain_manifest.jsonl`, appended
/// by `ops/scripts/vps_drain.ps1`): the per-file disposition of a nightly
/// relay drain. `release` is the VPS copy's outcome — `released` |
/// `skipped: <reason>` | `ssh_failed` | `no_release` | `kept` — and is
/// ABSENT from entries written before the release tracking shipped
/// (2026-08-16).
#[derive(Debug, Clone, PartialEq)]
pub struct DrainManifestEntry {
    pub ts_utc: String,
    pub file: String,
    pub action: String,
    pub release: String, // "" when the entry predates the release field
}

/// Parse one JSONL manifest line into an entry. Tolerant: missing/unknown
/// fields parse to "" — so an entry written before the `release` field
/// existed is never mistaken for a held file (only an explicit non-released
/// value is).
pub fn parse_drain_manifest_line(line: &str) -> Option<DrainManifestEntry> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(DrainManifestEntry {
        ts_utc: s("ts_utc"),
        file: s("file"),
        action: s("action"),
        release: s("release"),
    })
}

/// The drain manifest's "VPS is still holding it" state: a file whose LATEST
/// manifest entry is `action=landed` but `release` not in {released,
/// no_release} — the byte-verified copy never left the relay (skipped by the
/// re-hash guard, an ssh failure, …) and re-attempts next run. The manifest
/// is append-only, so per-file LATEST-entry resolution is required: a file
/// that failed one night but was released on a later run must not keep
/// firing. `kept` (A-B collision — never released by design) and entries
/// predating the `release` field are never flagged. Returns (file, reason)
/// pairs sorted by file. Pure — the CLI owns the file I/O (PD-3, W-6).
pub fn held_drain_files(entries: &[DrainManifestEntry]) -> Vec<(String, String)> {
    let mut latest: BTreeMap<&str, &DrainManifestEntry> = BTreeMap::new();
    for e in entries {
        // ts_utc is the ISO-8601 "o" format; same-form strings compare
        // lexicographically, which equals append order across runs.
        match latest.get(e.file.as_str()) {
            Some(prev) if prev.ts_utc > e.ts_utc => {}
            _ => {
                latest.insert(e.file.as_str(), e);
            }
        }
    }
    let mut held: Vec<(String, String)> = latest
        .into_iter()
        .filter(|(_, e)| {
            e.action == "landed" && !matches!(e.release.as_str(), "released" | "no_release" | "")
        })
        .map(|(f, e)| (f.to_string(), e.release.clone()))
        .collect();
    held.sort();
    held
}
