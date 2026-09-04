//! Signal research identity (research-lab hardening Phase 2, REL-4..REL-7).
//!
//! A canonical, immutable identity for a signal's EVIDENCE: which code
//! version (feature version), which data schema, which feature-engine params,
//! and which cost model produced a given grade or observation. Any change in
//! any of those dimensions invalidates old evidence — a grade computed under
//! a different identity must never promote a signal (REL-5).
//!
//! Pure: no I/O, no wall clock (PD-3). Hashing uses the same FNV-1a primitives
//! as the rest of the workspace so fingerprints are cheap and deterministic.

use mp_core::{fnv1a_absorb, FNV1A_OFFSET, SCHEMA_VER};
use serde::{Deserialize, Serialize};

/// The immutable evidence identity (REL-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalResearchIdentity {
    /// The signal's canonical id (catalog id, e.g. `accumulation_detector`).
    pub signal_id: String,
    /// Feature version the signal ran on (a `ver=N` bump in FEA-6).
    pub feature_version: u16,
    /// Data-schema version of the raw corpus (default `mp_core::SCHEMA_VER`).
    pub data_schema_version: u16,
    /// `FeaturesConfig::params_hash()` of the feature-engine params (FEA-6/7).
    pub params_hash: String,
    /// Hash of the cost model (fees/slippage) the evidence assumed. Empty
    /// means "cost-free research view" — still a valid identity, but any
    /// NON-empty cost model differs from it.
    pub cost_model_hash: String,
}

impl SignalResearchIdentity {
    /// Build an identity. `data_schema_version` defaults to the corpus schema
    /// (`mp_core::SCHEMA_VER`).
    pub fn new(
        signal_id: impl Into<String>,
        feature_version: u16,
        params_hash: impl Into<String>,
        cost_model_hash: impl Into<String>,
    ) -> Self {
        Self {
            signal_id: signal_id.into(),
            feature_version,
            data_schema_version: SCHEMA_VER,
            params_hash: params_hash.into(),
            cost_model_hash: cost_model_hash.into(),
        }
    }

    /// Two identities match iff EVERY dimension matches (REL-5: a change in
    /// params, feature version, data schema, or cost model breaks the match).
    pub fn matches(&self, other: &SignalResearchIdentity) -> bool {
        self == other
    }

    /// Deterministic fingerprint (FNV-1a over all fields) — the compact form
    /// stored on `GradeSnapshot` and in Parquet footers.
    pub fn fingerprint(&self) -> String {
        let mut h = FNV1A_OFFSET;
        h = fnv1a_absorb(h, self.signal_id.as_bytes());
        h = fnv1a_absorb(h, &self.feature_version.to_le_bytes());
        h = fnv1a_absorb(h, &self.data_schema_version.to_le_bytes());
        h = fnv1a_absorb(h, self.params_hash.as_bytes());
        h = fnv1a_absorb(h, self.cost_model_hash.as_bytes());
        format!("{h:016x}")
    }
}

/// Derive a stable cost-model hash from the sim's cost parameters (REL-7).
/// Any change in fees or slippage produces a different hash, so evidence
/// computed under the old cost model stops matching the new identity.
pub fn cost_model_hash(taker_fee: f64, maker_fee: f64, slip_frac: f64) -> String {
    let mut h = FNV1A_OFFSET;
    h = fnv1a_absorb(h, &taker_fee.to_bits().to_le_bytes());
    h = fnv1a_absorb(h, &maker_fee.to_bits().to_le_bytes());
    h = fnv1a_absorb(h, &slip_frac.to_bits().to_le_bytes());
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> SignalResearchIdentity {
        SignalResearchIdentity::new("whale_imb", 1, "p1", "c1")
    }

    #[test]
    fn rel_4_identity_matches_only_itself() {
        let a = base();
        let b = SignalResearchIdentity::new("whale_imb", 1, "p1", "c1");
        assert!(a.matches(&b));
        assert_eq!(a, b);
        // Identical fingerprints for identical identities (determinism).
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 16);
    }

    #[test]
    fn rel_5_params_change_invalidates_identity() {
        let a = base();
        let b = SignalResearchIdentity::new("whale_imb", 1, "p2", "c1"); // params changed
        assert!(!a.matches(&b));
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn rel_5_feature_version_change_invalidates_identity() {
        let a = base();
        let b = SignalResearchIdentity {
            feature_version: 2,
            ..base()
        };
        assert!(!a.matches(&b));
    }

    #[test]
    fn rel_5_data_schema_change_invalidates_identity() {
        let a = base();
        let b = SignalResearchIdentity {
            data_schema_version: SCHEMA_VER + 1,
            ..base()
        };
        assert!(!a.matches(&b));
    }

    #[test]
    fn rel_5_cost_model_change_invalidates_identity() {
        let a = base();
        let b = SignalResearchIdentity::new("whale_imb", 1, "p1", "c2"); // cost model changed
        assert!(!a.matches(&b));
    }

    #[test]
    fn rel_5_signal_id_change_invalidates_identity() {
        let a = base();
        let b = SignalResearchIdentity::new("other", 1, "p1", "c1");
        assert!(!a.matches(&b));
    }

    #[test]
    fn rel_7_cost_model_hash_is_stable_and_sensitive() {
        let h1 = cost_model_hash(0.00055, 0.0002, 0.0001);
        let h2 = cost_model_hash(0.00055, 0.0002, 0.0001);
        assert_eq!(h1, h2); // deterministic
        assert_ne!(h1, cost_model_hash(0.00056, 0.0002, 0.0001)); // taker fee
        assert_ne!(h1, cost_model_hash(0.00055, 0.0003, 0.0001)); // maker fee
        assert_ne!(h1, cost_model_hash(0.00055, 0.0002, 0.0002)); // slippage
    }

    #[test]
    fn rel_4_fingerprint_roundtrip_stable() {
        let id = base();
        let fp = id.fingerprint();
        // The same identity serialized/deserialized fingerprints identically.
        let json = serde_json::to_string(&id).unwrap();
        let back: SignalResearchIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(fp, back.fingerprint());
    }
}