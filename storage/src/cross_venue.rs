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

/// Findings artifact schema version (CONV-20).
pub const FINDINGS_SCHEMA_VER: u16 = 1;

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
    Ok(Findings {
        schema_ver: FINDINGS_SCHEMA_VER,
        date: date.to_owned(),
        detector_version: detector_version.to_owned(),
        config_hash: config_hash.to_owned(),
        findings: out,
    })
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
