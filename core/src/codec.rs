//! Binary serialization for the event log (EVT-3). bincode is the internal
//! format (spec 001 Decisions); Parquet (spec 003) is the research interchange.

use crate::event::EventEnvelope;
use crate::symbol::SymbolMeta;

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
    bincode::serialize(e).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Decode an envelope from bytes.
pub fn decode_event(bytes: &[u8]) -> Result<EventEnvelope, CodecError> {
    bincode::deserialize(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}

/// Encode a symbol-table snapshot (EVT-8).
pub fn encode_symbols(metas: &[SymbolMeta]) -> Result<Vec<u8>, CodecError> {
    bincode::serialize(metas).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Decode a symbol-table snapshot.
pub fn decode_symbols(bytes: &[u8]) -> Result<Vec<SymbolMeta>, CodecError> {
    bincode::deserialize(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}
