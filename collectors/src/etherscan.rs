//! Etherscan (Ethereum mainnet) exchange-reserve balance normalizer (spec
//! 034, NFL-1..6). The netflow poller (`mp-netflow`) fetches token/ETH
//! balances of known exchange hot wallets and wraps each response before
//! normalization — `{"address": "0x...", "asset": "USDT", "result": "…"}`
//! — because Etherscan's response does not echo the requested address or
//! asset (same wrapping pattern as FRED, spec 030 MAC-3). The normalizer
//! emits [`MarketEvent::NetflowSnapshot`] events; research derives netflows
//! from balance deltas (NFL-5) — the snapshot is the honest primitive.
//!
//! ## Honesty rules
//! - Balance strings that are empty, non-numeric, or non-finite are skipped
//!   (never invent a value, NFL-4 — mirrors FRED's MAC-8).
//! - Balances stay in RAW base units (wei / token base units). Scaling by
//!   the asset's decimals is research-side, where the decimals are explicit
//!   config (NFL-3).
//! - Addresses are opaque identifiers only (WHL-3 style — no labels in
//!   events; labels are poller config for human readability, NFL-3).

use crate::json::str_field;
use crate::normalize::{NormError, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolTable, Venue};
use serde_json::Value;

#[derive(Default)]
pub struct EtherscanNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl EtherscanNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::Ethereum, s)
    }
}

impl Normalizer for EtherscanNormalizer {
    fn venue(&self) -> Venue {
        Venue::Ethereum
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        let address = str_field(&v, "address")
            .ok_or_else(|| NormError::Parse("wrapped Etherscan response lacks address".into()))?;
        let asset = str_field(&v, "asset")
            .ok_or_else(|| NormError::Parse("wrapped Etherscan response lacks asset".into()))?;
        let id = self.sym(asset);
        let Some(result_s) = str_field(&v, "result") else {
            return Ok(()); // Etherscan error shape (status=0, empty result)
        };
        // Raw base units; skip empty/non-numeric honestly (NFL-4).
        let Some(balance) = result_s.parse::<f64>().ok().filter(|b| b.is_finite()) else {
            return Ok(());
        };
        let seq = self.seq();
        out.push(EventEnvelope::new(
            Venue::Ethereum,
            id,
            0, // Etherscan omits an observation timestamp (CONV-4)
            recv_ts_ns,
            seq,
            MarketEvent::NetflowSnapshot {
                address: address.to_owned(),
                balance,
            },
        ));
        Ok(())
    }

    fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    fn reset_books(&mut self) {
        // Balance snapshots — nothing to reset.
    }
}

#[cfg(test)]
mod tests {
    use super::EtherscanNormalizer;
    use crate::normalize::Normalizer;
    use mp_core::{EventEnvelope, MarketEvent, Venue};

    fn norm(n: &mut dyn Normalizer, recv: i64, json: &str) -> Vec<EventEnvelope> {
        let mut out = Vec::new();
        n.normalize(recv, json.as_bytes(), &mut out).unwrap();
        out
    }

    #[test]
    fn nfl_1_wrapped_balance_emits_snapshot() {
        let mut n = EtherscanNormalizer::new();
        let out = norm(
            &mut n,
            1000,
            r#"{"address":"0x1234","asset":"USDT","result":"123456789000000"}"#,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].venue, Venue::Ethereum);
        match &out[0].body {
            MarketEvent::NetflowSnapshot { address, balance } => {
                assert_eq!(address, "0x1234");
                assert_eq!(*balance, 123_456_789_000_000.0);
            }
            other => panic!("expected NetflowSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn nfl_2_asset_is_the_symbol() {
        let mut n = EtherscanNormalizer::new();
        let out = norm(
            &mut n,
            1000,
            r#"{"address":"0x1234","asset":"ETH","result":"42"}"#,
        );
        // First interned asset → symbol 1 (0 is reserved on some paths, but
        // intern order is deterministic: USDT fixtures above already consumed
        // index 0? No — each test has a fresh normalizer, so ETH is index 0.
        assert_eq!(out[0].symbol.0, 0);
    }

    #[test]
    fn nfl_3_empty_or_garbage_result_is_skipped() {
        let mut n = EtherscanNormalizer::new();
        // Etherscan error shape: status 0 with empty result.
        let err = norm(
            &mut n,
            1000,
            r#"{"address":"0x1234","asset":"USDT","result":""}"#,
        );
        assert!(err.is_empty());
        let garbage = norm(
            &mut n,
            1001,
            r#"{"address":"0x1234","asset":"USDT","result":"not-a-number"}"#,
        );
        assert!(garbage.is_empty());
    }

    #[test]
    fn nfl_4_missing_wrap_fields_is_a_parse_error() {
        let mut n = EtherscanNormalizer::new();
        let mut out = Vec::new();
        let err = n.normalize(1000, r#"{"result":"123"}"#.as_bytes(), &mut out);
        assert!(err.is_err(), "missing address must be a parse error");
        assert!(out.is_empty());
    }
}
