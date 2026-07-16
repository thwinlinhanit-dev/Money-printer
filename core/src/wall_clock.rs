//! The ONE sanctioned wall-clock reader (CONV-5). Every other decision-path
//! reads time via an injected [`Clock`](crate::Clock); this type is what gets
//! injected in live/paper mode. Isolated in its own file so the rulebook
//! guardrail (`ops/ci/guardrails.sh`) can allow exactly this file and reject
//! `SystemTime::now` everywhere else in the decision-path crates.

use crate::time::{Clock, Nanos};
use std::time::{SystemTime, UNIX_EPOCH};

/// Reads the operating-system clock. Use only at the live/paper edge; never on
/// a replay/backtest path (that is what [`SimClock`](crate::SimClock) is for).
#[derive(Debug, Clone, Copy, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ns(&self) -> Nanos {
        // SAFETY(CONV-13): the epoch is always <= now on a sane host; on the
        // impossible pre-1970 clock we saturate to 0 rather than panic.
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i64,
            Err(_) => 0,
        }
    }
}
