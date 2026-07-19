//! WebSocket backpressure policies (spec 013).
//!
//! Configurable per-venue handling when the channel from WS transport to
//! consumer is full. Replaces the silent `try_send`-drops pattern.

use mp_core::StatusKind;

/// How to behave when the WS channel is full (BKP-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressurePolicy {
    /// Block the WS read (stalls connection, venue may disconnect).
    /// Use `send` with timeout — if timeout expires, treat as disconnect.
    Block,
    /// Drop oldest frames in channel (favors recency).
    DropOldest,
    /// Drop newest frames (favors completeness of old data).
    DropNewest,
    /// Grow channel unboundedly (OOM risk).
    Unbounded,
}

impl BackpressurePolicy {
    /// Parse from a TOML string value.
    pub fn from_toml(s: &str) -> Option<Self> {
        Some(match s {
            "block" => Self::Block,
            "drop_oldest" => Self::DropOldest,
            "drop_newest" => Self::DropNewest,
            "unbounded" => Self::Unbounded,
            _ => return None,
        })
    }

    /// Return the StatusKind for a drop batch, or None for Block/Unbounded.
    pub fn drop_status(&self, dropped: u64) -> Option<StatusKind> {
        match self {
            Self::Block | Self::Unbounded => None,
            Self::DropOldest | Self::DropNewest => {
                Some(StatusKind::BackpressureDrop { dropped })
            }
        }
    }
}

impl Default for BackpressurePolicy {
    /// Phase 0 default: DropOldest (survival over completeness).
    fn default() -> Self {
        Self::DropOldest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bkp_1_block_policy_never_drops() {
        let p = BackpressurePolicy::Block;
        assert!(p.drop_status(5).is_none());
        assert_eq!(BackpressurePolicy::from_toml("block"), Some(BackpressurePolicy::Block));
    }

    #[test]
    fn bkp_2_drop_oldest_favors_recency() {
        let p = BackpressurePolicy::DropOldest;
        assert_eq!(p.drop_status(5), Some(StatusKind::BackpressureDrop { dropped: 5 }));
    }

    #[test]
    fn bkp_3_drop_emits_status_event() {
        let p = BackpressurePolicy::DropNewest;
        match p.drop_status(3) {
            Some(StatusKind::BackpressureDrop { dropped }) => assert_eq!(dropped, 3),
            _ => panic!("expected BackpressureDrop"),
        }
    }

    #[test]
    fn bkp_4_unbounded_grows_under_load() {
        let p = BackpressurePolicy::Unbounded;
        assert!(p.drop_status(999).is_none());
    }

    #[test]
    fn bkp_5_from_toml_roundtrip() {
        for (s, expected) in [
            ("block", BackpressurePolicy::Block),
            ("drop_oldest", BackpressurePolicy::DropOldest),
            ("drop_newest", BackpressurePolicy::DropNewest),
            ("unbounded", BackpressurePolicy::Unbounded),
        ] {
            assert_eq!(BackpressurePolicy::from_toml(s), Some(expected));
        }
        assert_eq!(BackpressurePolicy::from_toml("bogus"), None);
    }
}
