//! Pre-trade risk gate (EXE-1, spec 007). Paranoid on purpose: the strategy is
//! smart, the gate is dumb and unforgiving. Ordered checks RG-1..11; the first
//! failure rejects with a reason, and every verdict is meant to be journaled.
//! ~200 lines, zero dependency on strategy code.

use crate::killswitch::KillSwitches;
use mp_core::{Side, StrategyId, SymbolId, Venue};

/// Execution mode. Only `Live`/`Paper` may place orders (RG-1). Agents never
/// construct `Live` (PD-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Backtest,
    Shadow,
    Paper,
    Live,
}

impl Mode {
    fn allows_orders(self) -> bool {
        matches!(self, Mode::Paper | Mode::Live)
    }
}

/// Static risk limits (from `risk.toml`; changing defaults is owner-only).
#[derive(Debug, Clone, Copy)]
pub struct RiskLimits {
    pub max_order_notional: f64,
    pub max_position_notional: f64,
    pub max_gross_portfolio: f64,
    pub max_px_dev_frac: f64,
    pub max_orders_per_min: u32,
    pub strategy_daily_loss_budget: f64,
    pub portfolio_daily_loss_budget: f64,
    /// SWG-6 (spec 035): max distinct symbols with an open position at once
    /// (RG-12). Swing portfolios run several positions concurrently — this
    /// caps the book's breadth, not its depth.
    pub max_concurrent_positions: u32,
    /// SWG-6 (spec 035): correlation-adjusted portfolio exposure cap (RG-13).
    /// Default `> max_gross_portfolio` so the plain gross check binds first
    /// unless the owner configures a tighter correlation number.
    pub max_corr_adjusted_portfolio: f64,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_order_notional: 500.0, // live-small
            max_position_notional: 2_000.0,
            max_gross_portfolio: 300_000.0, // 3× on 100k
            max_px_dev_frac: 0.02,
            max_orders_per_min: 30,
            strategy_daily_loss_budget: 1_000.0,
            portfolio_daily_loss_budget: 3_000.0,
            max_concurrent_positions: 32,
            max_corr_adjusted_portfolio: 500_000.0,
        }
    }
}

/// One order presented to the gate (already sized to contracts).
#[derive(Debug, Clone)]
pub struct GateInput<'a> {
    pub mode: Mode,
    pub venue: Venue,
    pub symbol: SymbolId,
    pub strategy: StrategyId,
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub mark: f64,
    pub current_position_qty: f64,
    pub gross_exposure_notional: f64,
    pub orders_last_min: u32,
    pub strategy_daily_pnl: f64,
    pub portfolio_daily_pnl: f64,
    pub reconciler_clean: bool,
    /// Reducing/closing order (drops `current_position_qty` toward zero).
    /// When true the order contributes no additional gross exposure (RG-4/RG-5
    /// treat it as a reduction, not an add).
    pub reduce_only: bool,
    /// Venue contract multiplier: notional per contract ($). 1.0 = spot/linear
    /// contract whose qty is already in base units. Without this the gate
    /// silently assumes linear semantics for inverse contracts.
    pub contract_multiplier: f64,
    /// Allow-list of tradeable (venue, symbol) pairs (RG-2).
    pub allowed: &'a [(Venue, SymbolId)],
    /// SWG-6 (spec 035): number of distinct symbols currently holding a
    /// position (RG-12 counts an order that OPENS a new symbol slot).
    pub open_positions: u32,
    /// SWG-6 (spec 035): correlation-adjusted portfolio exposure AFTER this
    /// order would fill (`portfolio::correlation_adjusted_exposure`). The gate
    /// stays dumb — the caller owns the correlation math (RG-13).
    pub corr_adjusted_exposure_notional: f64,
}

/// Why the gate rejected an order (the check that failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    ModeDisallows,        // RG-1
    NotAllowlisted,       // RG-2
    OrderTooLarge,        // RG-3
    PositionTooLarge,     // RG-4
    GrossTooLarge,        // RG-5
    PriceOutOfBand,       // RG-6
    RateLimited,          // RG-7
    StrategyLossBudget,   // RG-8
    PortfolioLossBudget,  // RG-9
    KillSwitchTripped,    // RG-10
    ReconcilerDiverged,   // RG-11
    TooManyPositions,     // RG-12: new symbol slot beyond max_concurrent_positions
    CorrAdjustedTooLarge, // RG-13: correlation-adjusted exposure cap breached
    InvalidInput,         // RG-0: non-finite / negative qty, price, multiplier, or state inputs
}

/// Gate verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Reject(RejectReason),
}

impl Verdict {
    pub fn is_pass(self) -> bool {
        matches!(self, Verdict::Pass)
    }
}

/// Kill-switch scope the gate wants tripped by an RG-8/9 breach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TripRequest {
    /// Portfolio daily-loss budget breached → trip the owning strategy.
    Strategy(StrategyId),
    /// Portfolio-wide breach → trip everything.
    Global,
}

/// Evaluate the ordered checks RG-1..11 (EXE-1). Returns the first failure.
/// RG-8/9 breaches should additionally trip the corresponding kill switch —
/// that is the caller's responsibility (fail-closed, one-way).
pub fn evaluate(limits: &RiskLimits, kills: &KillSwitches, i: &GateInput) -> Verdict {
    use RejectReason::*;

    // RG-0 conv-8: reject non-finite or negative inputs up front, before any
    // comparison. A NaN qty/price/mark must never be *executed*; and NaN state
    // inputs (position / gross / PNL) must never be allowed to silently pass a
    // limit check — without this guard every `>` / `<` check below (RG-3..9) is
    // trivially bypassed by NaN, because `NaN > x` and `NaN < x` are both false
    // (audit C2; 08-04 hardening extended the check to the loss-stop state).
    if !i.qty.is_finite()
        || !i.price.is_finite()
        || !i.mark.is_finite()
        || !i.current_position_qty.is_finite()
        || !i.gross_exposure_notional.is_finite()
        || !i.strategy_daily_pnl.is_finite()
        || !i.portfolio_daily_pnl.is_finite()
        || !i.corr_adjusted_exposure_notional.is_finite()
        || i.qty < 0.0
        || i.price <= 0.0
        || i.contract_multiplier <= 0.0
        || !i.contract_multiplier.is_finite()
        || i.gross_exposure_notional < 0.0
        || i.corr_adjusted_exposure_notional < 0.0
    {
        return Verdict::Reject(InvalidInput);
    }

    // RG-1 mode allows orders.
    if !i.mode.allows_orders() {
        return Verdict::Reject(ModeDisallows);
    }
    // RG-2 venue+symbol on the allow-list.
    if !i
        .allowed
        .iter()
        .any(|(v, s)| *v == i.venue && *s == i.symbol)
    {
        return Verdict::Reject(NotAllowlisted);
    }
    // Notional is contracts × price × multiplier (conv-6 contract semantics).
    let order_notional = i.qty.abs() * i.price * i.contract_multiplier;
    // RG-3 order notional.
    if order_notional > limits.max_order_notional {
        return Verdict::Reject(OrderTooLarge);
    }
    // RG-4 resulting position notional.
    let signed = match i.side {
        Side::Buy => i.qty.abs(),
        Side::Sell => -i.qty.abs(),
    };
    let resulting_qty = i.current_position_qty + signed;
    let resulting = resulting_qty.abs() * i.mark * i.contract_multiplier;
    // A reduction that does not flip direction can never grow the position.
    if resulting > limits.max_position_notional
        && !(i.reduce_only && resulting_qty.abs() <= i.current_position_qty.abs())
    {
        return Verdict::Reject(PositionTooLarge);
    }
    // RG-5 gross portfolio exposure. A reducing order subtracts its reduction
    // from gross rather than adding its full notional.
    let gross_delta = if i.reduce_only && resulting_qty.abs() <= i.current_position_qty.abs() {
        -(i.current_position_qty.abs() - resulting_qty.abs()) * i.price * i.contract_multiplier
    } else {
        order_notional
    };
    if i.gross_exposure_notional + gross_delta > limits.max_gross_portfolio {
        return Verdict::Reject(GrossTooLarge);
    }
    // RG-6 price sanity: apply only when we actually have a trustworthy mark.
    if i.mark > 0.0 && ((i.price - i.mark).abs() / i.mark) > limits.max_px_dev_frac {
        return Verdict::Reject(PriceOutOfBand);
    }
    // RG-7 rate limit.
    if i.orders_last_min >= limits.max_orders_per_min {
        return Verdict::Reject(RateLimited);
    }
    // RG-8 strategy daily loss.
    if i.strategy_daily_pnl < -limits.strategy_daily_loss_budget {
        return Verdict::Reject(StrategyLossBudget);
    }
    // RG-9 portfolio daily loss.
    if i.portfolio_daily_pnl < -limits.portfolio_daily_loss_budget {
        return Verdict::Reject(PortfolioLossBudget);
    }
    // RG-10 kill switches.
    if kills.blocks(i.venue, &i.strategy) {
        return Verdict::Reject(KillSwitchTripped);
    }
    // RG-11 reconciler must be clean for this venue.
    if !i.reconciler_clean {
        return Verdict::Reject(ReconcilerDiverged);
    }
    // RG-12 (SWG-6) max concurrent positions: only an order that OPENS a NEW
    // symbol slot (no existing qty, not reduce_only) consumes a breadth slot.
    // Adds to a held symbol and reduces/closes never count against the cap.
    if !i.reduce_only
        && i.current_position_qty.abs() < f64::EPSILON
        && i.open_positions >= limits.max_concurrent_positions
    {
        return Verdict::Reject(TooManyPositions);
    }
    // RG-13 (SWG-6) correlation-adjusted portfolio exposure cap. The caller
    // precomputes the RESULTING value via `portfolio::correlation_adjusted_exposure`
    // (gate stays dumb; identity correlation ⇒ equals the gross notional).
    if i.corr_adjusted_exposure_notional > limits.max_corr_adjusted_portfolio {
        return Verdict::Reject(CorrAdjustedTooLarge);
    }
    Verdict::Pass
}

/// Companion to [`evaluate`]: when a verdict is an RG-8/RG-9 daily-loss reach,
/// return the kill-switch scope the caller must trip (fail-closed, one-way —
/// spec 007). Callers feed this into `KillSwitches::trip` and re-journal.
pub fn trip_on_breach(verdict: Verdict, i: &GateInput) -> Option<TripRequest> {
    match verdict {
        Verdict::Reject(RejectReason::StrategyLossBudget) => {
            Some(TripRequest::Strategy(i.strategy.clone()))
        }
        Verdict::Reject(RejectReason::PortfolioLossBudget) => Some(TripRequest::Global),
        _ => None,
    }
}
