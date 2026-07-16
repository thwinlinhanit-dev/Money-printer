//! Canonical 64-bit FNV-1a (spec 001 Decision: hash string trade ids at the
//! collector boundary; decision-log / config / grounding digests share the same
//! primitive so hashes stay comparable across crates).

/// FNV-1a offset basis.
pub const FNV1A_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a prime.
pub const FNV1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One-shot FNV-1a over a byte slice.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = FNV1A_OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV1A_PRIME);
    }
    h
}

/// One-shot FNV-1a over a string.
pub fn fnv1a_64_str(s: &str) -> u64 {
    fnv1a_64(s.as_bytes())
}

/// Rolling FNV-1a: absorb one byte into `h`.
pub fn fnv1a_absorb_byte(h: u64, b: u8) -> u64 {
    (h ^ u64::from(b)).wrapping_mul(FNV1A_PRIME)
}

/// Rolling FNV-1a: absorb a full byte slice into `h`.
pub fn fnv1a_absorb(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h = fnv1a_absorb_byte(h, *b);
    }
    h
}
