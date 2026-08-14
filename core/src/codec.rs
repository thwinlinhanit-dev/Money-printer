//! Binary serialization for the event log (EVT-3). bincode is the internal
//! format (spec 001 Decisions); Parquet (spec 003) is the research interchange.
//!
//! Wire format (spec 001 codec amendment): `bincode-next` (the maintained
//! bincode-2 line)
//! with `config::legacy()`, which is byte-identical to bincode 1.3.3's
//! default encoding — the format every recorded log has used since 2026-07.
//! Golden-bytes tests in `core/tests/bincode_migration.rs` (BDC-1/BDC-2)
//! pin that identity.

use crate::event::EventEnvelope;
use crate::log::EnvelopeV1;
use crate::symbol::SymbolMeta;
use bincode_next::config::legacy;
use bincode_next::serde::{decode_from_slice, encode_to_vec};

/// The wire format for every serialized event-log value (spec 001 BDC-3):
/// bincode-1-compatible — little-endian, fixed-width integers, unlimited
/// size. Do NOT change without amending spec 001: every recorded log since
/// 2026-07 and the schema-1 legacy decode path use this exact config.
pub(crate) fn wire_config() -> impl bincode_next::config::Config {
    legacy()
}

/// Serialization errors.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("bincode encode failed: {0}")]
    Encode(String),
    #[error("bincode decode failed: {0}")]
    Decode(String),
}

/// Encode an envelope to bytes.
pub fn encode_event(e: &EventEnvelope) -> Result<Vec<u8>, CodecError> {
    encode_to_vec(e, wire_config()).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Decode an envelope from bytes.
pub fn decode_event(bytes: &[u8]) -> Result<EventEnvelope, CodecError> {
    decode_from_slice(bytes, wire_config())
        .map(|(v, _)| v)
        .map_err(|e| CodecError::Decode(e.to_string()))
}

/// Encode a symbol-table snapshot (EVT-8).
pub fn encode_symbols(metas: &[SymbolMeta]) -> Result<Vec<u8>, CodecError> {
    encode_to_vec(metas, wire_config()).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Decode a symbol-table snapshot.
pub fn decode_symbols(bytes: &[u8]) -> Result<Vec<SymbolMeta>, CodecError> {
    decode_from_slice(bytes, wire_config())
        .map(|(v, _)| v)
        .map_err(|e| CodecError::Decode(e.to_string()))
}

// ---- schema-1 legacy layout (spec 001 / CONV-20) ---------------------------

/// Encode the pre-provenance envelope layout (`EnvelopeV1`). Only the legacy
/// reader path and the migration fixtures that reproduce historical frames
/// use these — normal writes go through [`encode_event`].
pub fn encode_envelope_v1(e: &EnvelopeV1) -> Result<Vec<u8>, CodecError> {
    encode_to_vec(e, wire_config()).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Decode the pre-provenance envelope layout (`EnvelopeV1`).
pub fn decode_envelope_v1(bytes: &[u8]) -> Result<EnvelopeV1, CodecError> {
    decode_from_slice(bytes, wire_config())
        .map(|(v, _)| v)
        .map_err(|e| CodecError::Decode(e.to_string()))
}
