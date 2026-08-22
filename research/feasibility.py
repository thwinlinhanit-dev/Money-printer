"""Economic feasibility gate (serious-research-lab roadmap Phase 4.1).

Before any backtest of a candidate, compute the break-even holding period and
minimum expected move from the ACTUAL proposed legs: entry and exit fees,
bid/ask spread, expected slippage, funding at settlement, borrow,
collateral/margin drag, transfer/inventory constraints, and the cost of a
partial or delayed hedge leg. Report base and stressed cases.

The gate may REJECT an idea without claiming that its signal is false: it
says the proposed implementation cannot afford to discover whether the
signal is true. Funding/basis candidates must use realized post-entry
settlement cash flows (per-settlement rate x cadence), never annualized
peak snapshots.

Pure stdlib, deterministic (no wall clock); research-only (CONV-2).
"""

from __future__ import annotations

import json
import zlib
from dataclasses import dataclass, replace

# Stressed-case multipliers (conservative, not tuned to any venue).
STRESS_FEE_X = 1.25  # worse fee tier
STRESS_SPREAD_X = 1.5  # wider book during adverse conditions
STRESS_SLIPPAGE_X = 2.0  # thin/impacted fills


@dataclass(frozen=True)
class Leg:
    """One proposed execution leg (a perp, a spot hedge, a dated future...).

    Costs are basis points of the leg notional. Funding is signed: positive
    means WE PAY the funding leg, negative means we receive it.
    """

    label: str
    notional_usd: float
    fee_bps: float = 0.0  # taker fee per side (charged on entry AND exit)
    spread_bps: float = 0.0  # full bid/ask cost over the round trip
    slippage_bps: float = 0.0  # expected adverse fill per fill
    transfer_bps: float = 0.0  # one-time collateral/inventory transfer
    funding_bps_per_settlement: float = 0.0  # signed; + = we pay
    settlements_per_day: float = 0.0
    borrow_bps_per_day: float = 0.0
    margin_drag_bps_per_day: float = 0.0

    def round_trip_bps(self) -> float:
        return (
            2.0 * self.fee_bps
            + self.spread_bps
            + 2.0 * self.slippage_bps
            + self.transfer_bps
        )

    def carry_bps_per_day(self) -> float:
        """Net per-day carry cost (positive = cost). Funding uses the recorded
        settlement cadence - never an annualized peak snapshot."""
        return (
            self.funding_bps_per_settlement * self.settlements_per_day
            + self.borrow_bps_per_day
            + self.margin_drag_bps_per_day
        )

    def stressed(self) -> "Leg":
        return replace(
            self,
            fee_bps=self.fee_bps * STRESS_FEE_X,
            spread_bps=self.spread_bps * STRESS_SPREAD_X,
            slippage_bps=self.slippage_bps * STRESS_SLIPPAGE_X,
        )


@dataclass(frozen=True)
class FeasibilityResult:
    legs: tuple[Leg, ...]
    stressed_legs: tuple[Leg, ...]
    max_hold_days: float
    rt_cost_bps: float
    carry_bps_per_day: float
    breakeven_days: float | None  # None = holding never clears the costs
    min_move_bps: float  # directional move needed at max_hold_days
    rt_cost_bps_stressed: float
    carry_bps_per_day_stressed: float
    breakeven_days_stressed: float | None
    min_move_bps_stressed: float
    verdict: str  # CLEARS_BASE_AND_STRESSED | CLEARS_BASE_ONLY | REJECTS_BASE

    @property
    def reason(self) -> str:
        be, rt, car = self.breakeven_days, self.rt_cost_bps, self.carry_bps_per_day
        bes, rts = self.breakeven_days_stressed, self.rt_cost_bps_stressed
        h = self.max_hold_days
        if self.verdict == "REJECTS_BASE":
            if be is None:
                return (
                    "cannot afford to discover whether the signal is true: "
                    f"round-trip costs {rt:.1f} bps with no net carry to clear "
                    f"them - a directional move of >= {self.min_move_bps:.1f} "
                    f"bps within {h:.0f} days is required"
                )
            return (
                "cannot afford to discover whether the signal is true: "
                f"break-even holding period {be:.1f} days > {h:.0f}-day "
                f"horizon (round-trip {rt:.1f} bps at {car:.2f} bps/day carry)"
            )
        if self.verdict == "CLEARS_BASE_ONLY":
            return (
                f"clears base costs (break-even {be:.1f} days <= {h:.0f}-day "
                f"horizon) but fails the stressed case (break-even {bes:.1f} "
                f"days at round-trip {rts:.1f} bps)"
            )
        return (
            f"clears base and stressed costs: break-even {be:.1f}/{bes:.1f} "
            f"days within the {h:.0f}-day horizon; required move at horizon "
            f"{self.min_move_bps:.1f} bps"
        )


def _breakeven(rt_cost_bps: float, carry_bps_per_day: float) -> float | None:
    """Days until cumulative carry clears the round-trip cost. Carry is
    signed (positive = cost, negative = income), so either sign clears it
    eventually; zero carry never does."""
    if carry_bps_per_day == 0.0:
        return None
    return rt_cost_bps / abs(carry_bps_per_day)


def _min_move(rt_cost_bps: float, carry_bps_per_day: float, hold_days: float) -> float:
    return rt_cost_bps + carry_bps_per_day * hold_days


def evaluate(legs: list[Leg], max_hold_days: float) -> FeasibilityResult:
    """Base + stressed economics for the proposed legs over the declared
    holding horizon. ``max_hold_days`` is the hypothesis' holding-period
    budget - it is what the signal claims to afford."""
    stressed = [leg.stressed() for leg in legs]
    rt = sum(leg.round_trip_bps() for leg in legs)
    car = sum(leg.carry_bps_per_day() for leg in legs)
    rts = sum(leg.round_trip_bps() for leg in stressed)
    cars = sum(leg.carry_bps_per_day() for leg in stressed)
    be = _breakeven(rt, car)
    bes = _breakeven(rts, cars)
    if be is not None and be <= max_hold_days:
        if bes is not None and bes <= max_hold_days:
            verdict = "CLEARS_BASE_AND_STRESSED"
        else:
            verdict = "CLEARS_BASE_ONLY"
    else:
        verdict = "REJECTS_BASE"
    return FeasibilityResult(
        legs=tuple(legs),
        stressed_legs=tuple(stressed),
        max_hold_days=max_hold_days,
        rt_cost_bps=rt,
        carry_bps_per_day=car,
        breakeven_days=be,
        min_move_bps=_min_move(rt, car, max_hold_days),
        rt_cost_bps_stressed=rts,
        carry_bps_per_day_stressed=cars,
        breakeven_days_stressed=bes,
        min_move_bps_stressed=_min_move(rts, cars, max_hold_days),
        verdict=verdict,
    )


def from_spec(spec: dict) -> tuple[list[Leg], float]:
    """Parse a feasibility spec JSON object, fail-closed on nonsense.

    Spec shape: ``{"legs": [{...Leg fields...}, ...], "max_hold_days": 30}``.
    A funding leg without a settlement cadence is refused - an annualized
    peak snapshot is not a settlement cash flow (Phase 4.1).
    """
    if not isinstance(spec, dict) or "legs" not in spec:
        raise ValueError("spec must be an object with a 'legs' list")
    legs_raw = spec["legs"]
    if not isinstance(legs_raw, list) or not legs_raw:
        raise ValueError("spec['legs'] must be a non-empty list")
    max_hold = spec.get("max_hold_days")
    if not isinstance(max_hold, (int, float)) or max_hold <= 0:
        raise ValueError("spec['max_hold_days'] must be a positive number")
    legs: list[Leg] = []
    for i, raw in enumerate(legs_raw):
        if not isinstance(raw, dict):
            raise ValueError(f"leg {i}: must be an object")
        unknown = set(raw) - set(Leg.__dataclass_fields__)
        if unknown:
            raise ValueError(f"leg {i}: unknown fields {sorted(unknown)}")
        leg = Leg(**raw)
        if leg.notional_usd <= 0:
            raise ValueError(f"leg {i} ('{leg.label}'): notional_usd must be > 0")
        for name in ("fee_bps", "spread_bps", "slippage_bps", "transfer_bps"):
            if getattr(leg, name) < 0:
                raise ValueError(f"leg {i} ('{leg.label}'): {name} must be >= 0")
        if leg.funding_bps_per_settlement != 0.0 and leg.settlements_per_day <= 0:
            raise ValueError(
                f"leg {i} ('{leg.label}'): funding without a settlement cadence - "
                "annualized peak snapshots are not allowed (Phase 4.1)"
            )
        legs.append(leg)
    return legs, float(max_hold)


def hash_spec(spec: dict) -> int:
    """Deterministic assumptions hash for run records (config-hash analogue)."""
    return zlib.crc32(
        json.dumps(spec, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
