//! Cross-venue gap detector (spec 026, CVG-1..12). An OFFLINE batch job that
//! classifies every per-venue manifest gap as venue-side / market-wide /
//! isolated-unknown, using the *other* venues' recordings of the same
//! underlying. Pure function of (recorded data, manifests, config); no network
//! (CVG-1); deterministic (CVG-2); never recovers or backfills missing data
//! (CVG-6) and never relaxes the INT-5 promotion gate (CVG-7).

use crate::{compactor, layout, Dataset, StorageError};
use mp_core::{EventEnvelope, MarketEvent, Venue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Findings artifact schema version (CONV-20). v2 adds the whole-day
/// value-level veracity section (`Findings::veracity`, spec 026 amendment
/// 2026-08-13) — old v1 artifacts still parse (`veracity` defaults empty).
pub const FINDINGS_SCHEMA_VER: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    CorroboratedVenueSide,
    MarketWide,
    IsolatedUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CohortMember {
    pub venue: String,
    pub symbol: String,
    pub corroborates: bool,
    pub trade_count: u64,
    /// None when no trades in the window — NaN is never serialized (CVG-10:
    /// serde_json rejects non-finite; fail-closed instead of a silent default).
    pub vwp: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub venue: String,
    pub symbol: String,
    pub gap_index: usize,
    pub from_ns: i64,
    pub to_ns: i64,
    pub kind: String,
    pub classification: Classification,
    pub cohort: Vec<CohortMember>,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Findings {
    pub schema_ver: u16,
    pub date: String,
    pub detector_version: String,
    pub config_hash: String,
    pub findings: Vec<Finding>,
    /// Whole-day value-level veracity findings (spec 026 amendment
    /// 2026-08-13): presence looked fine but the VALUE of a feed diverged
    /// from its cohort — the failure mode coverage structurally cannot see.
    #[serde(default)]
    pub veracity: Vec<VeracityFinding>,
}

/// One whole-day value-level veracity finding. `kind` is `price_divergence`
/// (VWP left the cohort median by more than `max_price_band_pct`) or
/// `trade_drought` (trade count collapsed below `veracity_trade_ratio` × the
/// cohort median while the cohort stayed liquid) over a fixed window
/// (default 1h). Evidence, never a gate (CVG-7) — and the same fail-closed
/// discipline as gap findings: a non-finite VWP is never serialized and never
/// silently defaulted (CVG-10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeracityFinding {
    pub venue: String,
    pub symbol: String,
    /// Fixed window (UTC-day aligned) the finding covers.
    pub window_from_ns: i64,
    pub window_to_ns: i64,
    /// `price_divergence` | `trade_drought`.
    pub kind: String,
    /// The venue's own metrics over the window.
    pub trade_count: u64,
    /// None when the venue had no trades — NaN is never serialized (CVG-10).
    pub vwp: Option<f64>,
    /// Cohort reference it diverged from (median of the liquid members).
    pub cohort_vwp: Option<f64>,
    pub cohort_median_trades: u64,
    /// Liquid cohort members the reference was computed over.
    pub cohort_size: usize,
    pub evidence: String,
}

/// Tunables + per-venue symbol cohort mapping (CVG-9). `deny_unknown_fields` so
/// a typo errors instead of silently defaulting (CONV-16).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossVenueConfig {
    #[serde(default = "default_min_cohort")]
    pub min_cohort: usize,
    #[serde(default = "default_min_corroborators")]
    pub min_corroborators: usize,
    #[serde(default = "default_min_trades")]
    pub min_trades: u64,
    #[serde(default = "default_cohort_gap_overlap")]
    pub cohort_gap_overlap: f64,
    #[serde(default = "default_max_price_band_pct")]
    pub max_price_band_pct: f64,
    /// Veracity check (2026-08-13 amendment): fixed window length in minutes
    /// over which each member's trade count + VWP is compared to the cohort.
    #[serde(default = "default_veracity_window_min")]
    pub veracity_window_min: u64,
    /// Minimum per-window trade count for a member to count as liquid (and
    /// for its VWP to be meaningful) in the veracity reference.
    #[serde(default = "default_veracity_min_trades")]
    pub veracity_min_trades: u64,
    /// A member whose window trade count falls below this fraction of the
    /// cohort's median (while the cohort is liquid) is a `trade_drought`.
    #[serde(default = "default_veracity_trade_ratio")]
    pub veracity_trade_ratio: f64,
    pub symbol_cohorts: Vec<SymbolCohort>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolCohort {
    pub underlying: String,
    /// venue slug → per-venue ticker. BTreeMap for deterministic order (CONV-10).
    pub members: BTreeMap<String, String>,
}

impl CrossVenueConfig {
    pub fn defaults() -> Self {
        Self {
            min_cohort: default_min_cohort(),
            min_corroborators: default_min_corroborators(),
            min_trades: default_min_trades(),
            cohort_gap_overlap: default_cohort_gap_overlap(),
            max_price_band_pct: default_max_price_band_pct(),
            veracity_window_min: default_veracity_window_min(),
            veracity_min_trades: default_veracity_min_trades(),
            veracity_trade_ratio: default_veracity_trade_ratio(),
            symbol_cohorts: Vec::new(),
        }
    }
}

fn default_min_cohort() -> usize {
    2
}
fn default_min_corroborators() -> usize {
    1
}
fn default_min_trades() -> u64 {
    1
}
fn default_cohort_gap_overlap() -> f64 {
    0.5
}
fn default_max_price_band_pct() -> f64 {
    5.0
}
fn default_veracity_window_min() -> u64 {
    60
}
fn default_veracity_min_trades() -> u64 {
    50
}
fn default_veracity_trade_ratio() -> f64 {
    0.5
}

/// Parse a config TOML (CVG-9). Unknown fields error (deny_unknown_fields).
pub fn parse_config(toml_str: &str) -> Result<CrossVenueConfig, toml::de::Error> {
    toml::from_str(toml_str)
}

/// `cold/cross_venue/date={d}/findings.json` (CVG-8). Separate, append-only.
pub fn findings_file(root: &Path, date: &str) -> PathBuf {
    root.join("cross_venue")
        .join(format!("date={date}"))
        .join("findings.json")
}

/// Load a manifest iff it exists for (venue, date); None ⇒ no recording.
fn manifest_opt(
    root: &Path,
    venue: Venue,
    date: &str,
) -> Result<Option<crate::manifest::QualityManifest>, StorageError> {
    if !layout::manifest_file(root, venue, date).exists() {
        return Ok(None);
    }
    Ok(Some(compactor::load_manifest(root, venue, date)?))
}
/// Run the detector (CVG-1..7). Pure: same (root, date, cfg, detector_version,
/// config_hash) ⇒ identical Findings (CVG-2). Reads manifests + trades Parquet
/// only; never writes raw/cold trades, never edits per-venue manifests (CVG-6).
pub fn detect(
    root: &Path,
    date: &str,
    cfg: &CrossVenueConfig,
    detector_version: &str,
    config_hash: &str,
) -> Result<Findings, StorageError> {
    let ds = Dataset::open(root);
    let mut out: Vec<Finding> = Vec::new();
    for cohort in &cfg.symbol_cohorts {
        for (vslug, symbol) in &cohort.members {
            let Some(venue) = Venue::from_slug(vslug) else {
                tracing::warn!(venue = %vslug, "cvg: unknown venue slug in cohort, skipping");
                continue;
            };
            let Some(m) = manifest_opt(root, venue, date)? else {
                continue; // no recording for this venue/date ⇒ no gaps to classify
            };
            let key = format!("trades:{symbol}");
            for (gi, gap) in m.gaps(&key).iter().enumerate() {
                let (class, members, evidence) =
                    classify_gap(root, &ds, venue, symbol, gap, cohort, cfg, date)?;
                if class == Classification::MarketWide {
                    tracing::warn!(
                        venue = %venue.slug(), symbol = %symbol,
                        from = gap.from_ns, to = gap.to_ns,
                        "cvg: market-wide gap (synchronized outage) — raise alert (spec 009)"
                    );
                }
                out.push(Finding {
                    venue: layout::venue_slug(venue).to_owned(),
                    symbol: symbol.clone(),
                    gap_index: gi,
                    from_ns: gap.from_ns,
                    to_ns: gap.to_ns,
                    kind: format!("{:?}", gap.kind).to_lowercase(),
                    classification: class,
                    cohort: members,
                    evidence,
                });
            }
        }
    }
    // Whole-day value-level veracity pass (2026-08-13 amendment): for every
    // cohort, compare each member's per-window trade count + VWP against the
    // cohort's median — catching silent corruption, a wrong feed, or dropped
    // frames that presence (coverage/manifest gaps) structurally cannot see.
    let mut veracity: Vec<VeracityFinding> = Vec::new();
    for cohort in &cfg.symbol_cohorts {
        veracity.extend(detect_veracity(root, &ds, date, cohort, cfg)?);
    }
    Ok(Findings {
        schema_ver: FINDINGS_SCHEMA_VER,
        date: date.to_owned(),
        detector_version: detector_version.to_owned(),
        config_hash: config_hash.to_owned(),
        findings: out,
        veracity,
    })
}

/// Whole-day value-level veracity check (spec 026 amendment 2026-08-13).
/// Presence looked fine — the member's manifest shows no gap in a window, and
/// the aggregate coverage passed — but the VALUE of the feed diverged from
/// its cohort. Two kinds:
///
/// - `price_divergence` — the member's window VWP leaves the cohort's median
///   VWP by more than `max_price_band_pct`;
/// - `trade_drought` — the member's window trade count collapses below
///   `veracity_trade_ratio` × the cohort's median count while the cohort
///   itself is liquid (≥ `veracity_min_trades` per window).
///
/// Windows are fixed-length (`veracity_window_min`), aligned to the UTC day.
/// The reference is the median of the LIQUID members only (a venue that
/// barely trades is no reference for anyone). Same discipline as the gap
/// detector: offline + read-only (CVG-1/6), deterministic (CVG-2, BTreeMap
/// iteration + sorted output), evidence not a gate (CVG-7), non-finite VWP
/// never serialized (CVG-10).
fn detect_veracity(
    root: &Path,
    ds: &Dataset,
    date: &str,
    cohort: &SymbolCohort,
    cfg: &CrossVenueConfig,
) -> Result<Vec<VeracityFinding>, StorageError> {
    let win_ns = cfg.veracity_window_min.saturating_mul(60_000_000_000) as i64;
    if win_ns <= 0 {
        return Ok(Vec::new());
    }
    // Per-venue trade streams for members that recorded the date (BTreeMap
    // iteration ⇒ deterministic, CONV-10). No recording ⇒ nothing to verify.
    let mut streams: Vec<(String, String, Vec<EventEnvelope>)> = Vec::new();
    for (vslug, symbol) in &cohort.members {
        let Some(venue) = Venue::from_slug(vslug) else {
            continue;
        };
        if manifest_opt(root, venue, date)?.is_none() {
            continue;
        }
        let trades = ds.trades_day(venue, symbol, date)?;
        if trades.is_empty() {
            continue;
        }
        streams.push((venue.slug().to_owned(), symbol.clone(), trades));
    }
    if streams.len() < cfg.min_cohort {
        return Ok(Vec::new()); // no cohort to diverge from (CVG-3 analog)
    }
    let first = streams
        .iter()
        .map(|(_, _, t)| t[0].recv_ts_ns)
        .min()
        .unwrap_or(0);
    let last = streams
        .iter()
        .map(|(_, _, t)| t[t.len() - 1].recv_ts_ns)
        .max()
        .unwrap_or(0);
    let day_start = first.div_euclid(86_400_000_000_000) * 86_400_000_000_000;

    let mut out: Vec<VeracityFinding> = Vec::new();
    let mut cursors = vec![0usize; streams.len()];
    let mut ws = day_start;
    while ws < last {
        let we = ws + win_ns;
        // Per-member metrics over [ws, we) via monotonic cursors (streams are
        // sorted by recv_ts, windows advance forward ⇒ O(n) total).
        let mut rows: Vec<(usize, u64, f64)> = Vec::with_capacity(streams.len());
        for (i, (_, _, trades)) in streams.iter().enumerate() {
            let mut count = 0u64;
            let mut notional = 0.0_f64;
            let mut qty = 0.0_f64;
            while cursors[i] < trades.len() && trades[cursors[i]].recv_ts_ns < we {
                let e = &trades[cursors[i]];
                if e.recv_ts_ns >= ws {
                    if let MarketEvent::Trade { price, qty: q, .. } = &e.body {
                        notional += price * q;
                        qty += q;
                        count += 1;
                    }
                }
                cursors[i] += 1;
            }
            let v = if qty > 0.0 { notional / qty } else { f64::NAN };
            rows.push((i, count, v));
        }
        // Reference from the liquid members only; non-finite VWP is excluded
        // and never propagated (CVG-10).
        let liquid: Vec<&(usize, u64, f64)> = rows
            .iter()
            .filter(|(_, c, v)| *c >= cfg.veracity_min_trades && v.is_finite() && *v != 0.0)
            .collect();
        if liquid.len() >= cfg.min_cohort {
            let med_vwp = median(liquid.iter().map(|(_, _, v)| *v));
            let med_trades =
                median(liquid.iter().map(|(_, c, _)| *c as f64)).round() as u64;
            let band = cfg.max_price_band_pct / 100.0;
            for (i, count, v) in &rows {
                let (vslug, symbol, _) = &streams[*i];
                // Price divergence: only meaningful when the member itself is
                // liquid enough for its VWP to be a real number.
                if *count >= cfg.veracity_min_trades && v.is_finite() {
                    let dev = (v - med_vwp).abs();
                    if dev > band * med_vwp.abs() {
                        out.push(VeracityFinding {
                            venue: vslug.clone(),
                            symbol: symbol.clone(),
                            window_from_ns: ws,
                            window_to_ns: we,
                            kind: "price_divergence".into(),
                            trade_count: *count,
                            vwp: Some(*v),
                            cohort_vwp: Some(med_vwp),
                            cohort_median_trades: med_trades,
                            cohort_size: liquid.len(),
                            evidence: format!(
                                "vwp {v:.4} deviates {:.2}% from cohort median {med_vwp:.4} (band {:.1}%)",
                                (dev / med_vwp.abs()) * 100.0,
                                cfg.max_price_band_pct
                            ),
                        });
                    }
                }
                // Trade drought: the member traded (so presence is fine) but
                // collapsed vs a liquid cohort — dropped frames coverage
                // cannot see.
                if *count > 0
                    && med_trades >= cfg.veracity_min_trades
                    && (*count as f64) < cfg.veracity_trade_ratio * (med_trades as f64)
                {
                    out.push(VeracityFinding {
                        venue: vslug.clone(),
                        symbol: symbol.clone(),
                        window_from_ns: ws,
                        window_to_ns: we,
                        kind: "trade_drought".into(),
                        trade_count: *count,
                        vwp: if v.is_finite() { Some(*v) } else { None },
                        cohort_vwp: Some(med_vwp),
                        cohort_median_trades: med_trades,
                        cohort_size: liquid.len(),
                        evidence: format!(
                            "{} trades vs cohort median {med_trades} (ratio {:.2} < {:.2})",
                            count,
                            (*count as f64) / (med_trades as f64),
                            cfg.veracity_trade_ratio
                        ),
                    });
                }
            }
        }
        if we <= ws {
            break; // window width clamped to 0 (defensive; never loops forever)
        }
        ws = we;
    }
    // Explicit deterministic order (CONV-10): venue, then window, then kind.
    out.sort_by(|a, b| {
        a.venue
            .cmp(&b.venue)
            .then(a.window_from_ns.cmp(&b.window_from_ns))
            .then(a.kind.cmp(&b.kind))
    });
    Ok(out)
}

/// Median of an f64 iterator (sorted copy; empty ⇒ NaN).
fn median<I: IntoIterator<Item = f64>>(vals: I) -> f64 {
    let mut v: Vec<f64> = vals.into_iter().collect();
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}
/// Classify one gap window on (venue, symbol) (CVG-3..5, CVG-10).
#[allow(clippy::too_many_arguments)]
fn classify_gap(
    root: &Path,
    ds: &Dataset,
    venue: Venue,
    _symbol: &str,
    gap: &crate::manifest::Gap,
    cohort: &SymbolCohort,
    cfg: &CrossVenueConfig,
    date: &str,
) -> Result<(Classification, Vec<CohortMember>, String), StorageError> {
    let mut members: Vec<CohortMember> = Vec::new();
    let mut all_cohort_gapped = true;
    for (slug, sym) in &cohort.members {
        let Some(other) = Venue::from_slug(slug) else {
            continue;
        };
        if other == venue {
            continue; // exclude the gapped venue itself
        }
        let Some(om) = manifest_opt(root, other, date)? else {
            // No recording for this cohort venue ⇒ not part of the cohort this
            // date; can neither corroborate nor prove market-wide.
            all_cohort_gapped = false;
            continue;
        };
        let okey = format!("trades:{sym}");
        let overlap = gap_overlap_fraction(om.gaps(&okey), gap.from_ns, gap.to_ns);
        let itself_gapped = overlap > cfg.cohort_gap_overlap;
        if !itself_gapped {
            all_cohort_gapped = false;
        }
        let trades = ds.trades_day(other, sym, date)?;
        let in_win: Vec<&EventEnvelope> = trades
            .iter()
            .filter(|e| e.recv_ts_ns >= gap.from_ns && e.recv_ts_ns < gap.to_ns)
            .collect();
        let trade_count = in_win.len() as u64;
        let (vwp_win, _) = vwp(&in_win);
        let gap_len = (gap.to_ns - gap.from_ns).max(1);
        let pre: Vec<&EventEnvelope> = trades
            .iter()
            .filter(|e| e.recv_ts_ns >= gap.from_ns - gap_len && e.recv_ts_ns < gap.from_ns)
            .collect();
        let (vwp_pre, _) = vwp(&pre);

        // Corroboration (CVG-4); non-finite vwp ⇒ fail-closed (CVG-10).
        let band = cfg.max_price_band_pct / 100.0;
        let corroborates = !itself_gapped
            && trade_count >= cfg.min_trades
            && vwp_win.is_finite()
            && vwp_pre.is_finite()
            && vwp_pre.abs() > 0.0
            && (vwp_win - vwp_pre).abs() <= band * vwp_pre.abs();
        if !vwp_win.is_finite() || !vwp_pre.is_finite() {
            tracing::warn!(
                venue = %other.slug(), symbol = %sym,
                "cvg: non-finite vwp, fail-closed to non-corroborating (CVG-10)"
            );
        }
        members.push(CohortMember {
            venue: layout::venue_slug(other).to_owned(),
            symbol: sym.clone(),
            corroborates,
            trade_count,
            vwp: if vwp_win.is_finite() {
                Some(vwp_win)
            } else {
                None
            },
        });
    }

    // CVG-3: too few cohort venues ⇒ isolated_unknown.
    if members.len() < cfg.min_cohort {
        let e = ev("isolated", &members);
        return Ok((Classification::IsolatedUnknown, members, e));
    }
    // CVG-5: cohort non-empty and every cohort venue itself gapped ⇒ market_wide.
    if all_cohort_gapped {
        let e = ev("market_wide", &members);
        return Ok((Classification::MarketWide, members, e));
    }
    let corrs = members.iter().filter(|m| m.corroborates).count();
    if corrs >= cfg.min_corroborators {
        let e = ev("corroborated", &members);
        return Ok((Classification::CorroboratedVenueSide, members, e));
    }
    let e = ev("isolated", &members);
    Ok((Classification::IsolatedUnknown, members, e))
}
/// Volume-weighted price + count over a slice of trade envelopes.
fn vwp(trades: &[&EventEnvelope]) -> (f64, u64) {
    let mut notional = 0.0_f64;
    let mut qty = 0.0_f64;
    let mut n = 0u64;
    for e in trades {
        if let MarketEvent::Trade { price, qty: q, .. } = &e.body {
            notional += price * q;
            qty += q;
            n += 1;
        }
    }
    let v = if qty > 0.0 { notional / qty } else { f64::NAN };
    (v, n)
}

/// Fraction of `[from, to)` covered by any gap interval.
fn gap_overlap_fraction(gaps: &[crate::manifest::Gap], from: i64, to: i64) -> f64 {
    let span = (to - from).max(1) as f64;
    let mut covered = 0i64;
    let mut cur = from;
    for g in gaps {
        if g.to_ns <= cur || g.from_ns >= to {
            continue;
        }
        let lo = g.from_ns.max(cur);
        let hi = g.to_ns.min(to);
        if hi > lo {
            covered += hi - lo;
            cur = cur.max(hi);
        }
    }
    (covered as f64 / span).clamp(0.0, 1.0)
}

fn ev(kind: &str, members: &[CohortMember]) -> String {
    let corrs = members.iter().filter(|m| m.corroborates).count();
    format!(
        "{kind} ({corrs}/{} cohort venues corroborate)",
        members.len()
    )
}

/// Write the findings artifact (CVG-8). Creates parents; never edits manifests.
pub fn write_findings(root: &Path, f: &Findings) -> Result<PathBuf, StorageError> {
    let path = findings_file(root, &f.date);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // SAFETY: Findings is derived-Serialize over plain data (numbers, strings,
    // vecs, options); serde_json cannot fail on it (CONV-13).
    let json = serde_json::to_string_pretty(f).expect("findings serializes");
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Stable FNV-1a hash of a config's text (CVG-8 `config_hash`), sharing the
/// collector's trade-id hashing primitive so digests stay comparable across
/// crates (spec 001 Decision). Same config text ⇒ same hash.
pub fn config_hash(toml_str: &str) -> String {
    format!("{:016x}", mp_core::fnv1a_64_str(toml_str))
}

/// Build SHA embedded at build time (CONV-18). `MP_GIT_SHA` is set by the
/// release build; `dev` otherwise. Honest, never fabricated.
const GIT_SHA: &str = match option_env!("MP_GIT_SHA") {
    Some(s) => s,
    None => "dev",
};

/// `--version` string for any storage binary (CONV-18). Lives in the lib so it
/// is unit-testable; each CLI passes its own name (e.g. `mp-bootstrap`).
pub fn app_version(app: &str) -> String {
    format!("{app} {} ({GIT_SHA})", env!("CARGO_PKG_VERSION"))
}

/// `--version` string (CONV-18). mp-cross-venue's canonical form.
pub fn version_string() -> String {
    app_version("mp-cross-venue")
}
