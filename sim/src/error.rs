//! Backtester run errors (SIM-4/SIM-6). The sim refuses to produce numbers it
//! knows are wrong rather than silently reporting on bad data (PD-5).

use mp_core::SymbolId;

#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum SimError {
    /// SIM-4: a perp position was held past the funding-check interval with no
    /// Funding event ever recorded for it. Funding cost would be silently zero,
    /// which the sim refuses to report as if it were measured.
    #[error("funding data missing for symbol {0:?} while holding a position past the check interval (SIM-4)")]
    MissingFunding(SymbolId),

    /// SIM-6: the consumed stream's manifest coverage is below `min_coverage`.
    /// A gap that isn't refused is a gap that silently poisons the numbers.
    #[error("stream coverage {actual:.6} < required {required:.6} (SIM-6)")]
    LowCoverage { actual: f64, required: f64 },
}
