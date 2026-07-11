//! Shared JSON field accessors used by every venue normalizer. Venues encode
//! numbers as strings (mostly) or numbers; these read either.

use crate::normalize::NormError;
use mp_core::{Level, Levels};
use serde_json::Value;
use smallvec::SmallVec;

pub fn f64_field(v: &Value, k: &str) -> Option<f64> {
    match v.get(k) {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

pub fn i64_field(v: &Value, k: &str) -> Option<i64> {
    match v.get(k) {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

pub fn u64_field(v: &Value, k: &str) -> Option<u64> {
    match v.get(k) {
        Some(Value::String(s)) => s.parse().ok(),
        Some(Value::Number(n)) => n.as_u64(),
        _ => None,
    }
}

pub fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}

pub fn ms_to_ns(ms: i64) -> i64 {
    ms.saturating_mul(1_000_000)
}

/// Stable 64-bit FNV-1a hash for string ids (spec 001 Decision: hash string
/// trade ids to `u64` at the collector boundary).
pub fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Parse `[["price","size"], ...]` book levels (string pairs).
pub fn parse_pair_levels(v: Option<&Value>) -> Result<Levels, NormError> {
    let mut out: Levels = SmallVec::new();
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Ok(out);
    };
    for lvl in arr {
        let la = lvl
            .as_array()
            .ok_or_else(|| NormError::Parse("level not an array".into()))?;
        let p = la
            .first()
            .and_then(pair_num)
            .ok_or_else(|| NormError::Parse("bad level price".into()))?;
        let q = la
            .get(1)
            .and_then(pair_num)
            .ok_or_else(|| NormError::Parse("bad level qty".into()))?;
        out.push((p, q) as Level);
    }
    Ok(out)
}

fn pair_num(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Parse `[{"price":..,"qty":..}, ...]` object levels (Kraken/Hyperliquid style).
pub fn parse_obj_levels(
    v: Option<&Value>,
    px_key: &str,
    sz_key: &str,
) -> Result<Levels, NormError> {
    let mut out: Levels = SmallVec::new();
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Ok(out);
    };
    for lvl in arr {
        let p =
            f64_field(lvl, px_key).ok_or_else(|| NormError::Parse("bad obj level price".into()))?;
        let q =
            f64_field(lvl, sz_key).ok_or_else(|| NormError::Parse("bad obj level qty".into()))?;
        out.push((p, q) as Level);
    }
    Ok(out)
}
