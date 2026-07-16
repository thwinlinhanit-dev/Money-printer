//! Vol-targeted position sizing (RSK-1, RSK-8). Strategies emit risk units;
//! this converts them to contracts. Every term is recorded in a [`SizingTrace`]
//! so a surprising size can always be explained.

use serde::{Deserialize, Serialize};

/// Global sizing parameters (from `risk.toml`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SizingParams {
    /// Fraction of a strategy's risk capital risked per standard risk unit.
    pub per_trade_risk_pct: f64,
}

impl Default for SizingParams {
    fn default() -> Self {
        Self {
            per_trade_risk_pct: 0.005, // 0.5%
        }
    }
}

/// Per-intent sizing inputs.
#[derive(Debug, Clone, Copy)]
pub struct SizingInputs {
    /// `u` — risk units requested by the strategy.
    pub risk_units: f64,
    /// Account equity.
    pub equity: f64,
    /// Allocation weight for the owning strategy (from the allocator).
    pub alloc_weight: f64,
    /// Instrument volatility as a *fraction* of price over the strategy horizon
    /// (e.g. realized vol). Must be > 0 to size.
    pub instrument_vol_frac: f64,
    /// Current mark price (for the vol→dollar conversion and notional floor).
    pub mark_price: f64,
    /// Stop distance in vol units.
    pub k_stop: f64,
    /// Contract size increment (round down to a multiple).
    pub step_size: f64,
    /// Minimum notional; below this, no trade (not a tiny trade).
    pub min_notional: f64,
    /// Contract multiplier (e.g. 0.001 for Bitcoin futures where 1 contract
    /// = 0.001 BTC). Default 1.0 for spot/linear contracts.
    pub contract_multiplier: f64,
}

/// Every term of the sizing formula, for journaling (RSK-8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SizingTrace {
    pub risk_capital: f64,
    pub per_unit_risk: f64,
    pub dollar_vol_per_contract: f64,
    pub raw_contracts: f64,
    pub rounded_contracts: f64,
    /// True when the rounded size fell below `min_notional` and was zeroed.
    pub floored_to_zero: bool,
}

/// Sizing result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizedOrder {
    pub qty_contracts: f64,
    pub trace: SizingTrace,
}

fn round_down(x: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return x;
    }
    (x / step).floor() * step
}

/// Compute the contract quantity for an intent (RSK-1). Fail-closed: any
/// non-finite or non-positive volatility/price yields a zero-size trade rather
/// than a NaN (CONV-8).
pub fn size(params: &SizingParams, inp: &SizingInputs) -> SizedOrder {
    let risk_capital = (inp.equity * inp.alloc_weight).max(0.0);
    let per_unit_risk = risk_capital * params.per_trade_risk_pct;
    let dollar_vol = inp.instrument_vol_frac * inp.mark_price * inp.contract_multiplier;

    let denom = inp.k_stop * dollar_vol;
    let raw = if denom > 0.0 && inp.risk_units.is_finite() && per_unit_risk.is_finite() {
        (inp.risk_units.max(0.0) * per_unit_risk) / denom
    } else {
        0.0
    };
    let raw = if raw.is_finite() { raw } else { 0.0 };

    let rounded = round_down(raw, inp.step_size);
    let notional = rounded * inp.mark_price * inp.contract_multiplier;
    let floored = notional < inp.min_notional;
    let qty = if floored { 0.0 } else { rounded };

    SizedOrder {
        qty_contracts: qty,
        trace: SizingTrace {
            risk_capital,
            per_unit_risk,
            dollar_vol_per_contract: dollar_vol,
            raw_contracts: raw,
            rounded_contracts: rounded,
            floored_to_zero: floored,
        },
    }
}
