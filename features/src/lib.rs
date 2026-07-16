//! mp-features — streaming feature engine (spec 004).
//!
//! Turns raw events into named, versioned, timestamped features consumed by
//! strategies (live) and research (offline) from the SAME code — the
//! one-code-path pillar. Everything here is a pure function of events: no wall
//! clock, no I/O, no unseeded randomness (PD-3/FEA-2).
//!
//! v1 slice: the engine + bar builder + a representative catalog + screener.
//! The full catalog and offline Parquet materialization are the same pattern,
//! tracked in spec 004 Decisions.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod bar;
pub mod catalog;
pub mod config;
pub mod engine;
pub mod screener;

pub use bar::{Bar, BarBuilder};
pub use config::{ConfigError, FeaturesConfig};
pub use engine::{BarFeature, FeatureEngine, FeatureUpdate, Locality, TickFeature};
pub use screener::{Cond, Op, Rule, Screener, ScreenerHit};
