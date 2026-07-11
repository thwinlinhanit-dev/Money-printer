"""Screener grading (RES-2) and edge-decay detection (RES-3).

Closes the loop opened by FEA-10: every ``ScreenerHit`` is graded on its
forward returns vs the symbol baseline, per rule, with a leaderboard and a
decay flag so dead rules are caught. Pure stdlib and deterministic — the math
is unit-tested on fixtures; production feeds it Polars frames.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass(frozen=True)
class Hit:
    """One screener hit: which rule fired, on which symbol, at what time."""

    rule: str
    symbol: str
    ts_ns: int


def forward_return(prices: list[tuple[int, float]], ts_ns: int, horizon_ns: int) -> float | None:
    """Return over ``[ts, ts+horizon]`` using the last price at or before each
    end (step lookup). ``None`` if either endpoint has no price yet.

    ``prices`` is ``(ts_ns, price)`` ascending. Deterministic step semantics
    match how a live book would be sampled (no interpolation, no look-ahead).
    """
    entry = _price_asof(prices, ts_ns)
    exit_ = _price_asof(prices, ts_ns + horizon_ns)
    if entry is None or exit_ is None or entry == 0.0:
        return None
    # Require the exit sample to actually be at/after the horizon end — a series
    # that stops early must not silently grade a shorter horizon (PD-5 honesty).
    if prices[-1][0] < ts_ns + horizon_ns:
        return None
    return exit_ / entry - 1.0


def _price_asof(prices: list[tuple[int, float]], ts_ns: int) -> float | None:
    """Last price at or before ``ts_ns`` (binary search, no look-ahead)."""
    lo, hi, found = 0, len(prices) - 1, None
    while lo <= hi:
        mid = (lo + hi) // 2
        if prices[mid][0] <= ts_ns:
            found = prices[mid][1]
            lo = mid + 1
        else:
            hi = mid - 1
    return found


@dataclass
class RuleGrade:
    """Aggregate grade for one rule at one horizon."""

    rule: str
    horizon_ns: int
    n: int = 0
    wins: int = 0
    sum_excess: float = 0.0
    excesses: list[float] = field(default_factory=list)

    @property
    def win_rate(self) -> float:
        return self.wins / self.n if self.n else 0.0

    @property
    def avg_excess(self) -> float:
        return self.sum_excess / self.n if self.n else 0.0


def grade_hits(
    hits: list[Hit],
    prices: dict[str, list[tuple[int, float]]],
    horizon_ns: int,
    baseline: dict[str, float],
) -> dict[str, RuleGrade]:
    """Grade every hit's excess forward return (hit return − symbol baseline)
    at ``horizon_ns``, aggregated per rule. Hits without a computable forward
    return are skipped (not counted as wins or losses — honest denominator).

    ``baseline[symbol]`` is that symbol's average forward return at this horizon
    (the "do nothing" counterfactual). Deterministic iteration order.
    """
    grades: dict[str, RuleGrade] = {}
    for h in sorted(hits, key=lambda x: (x.rule, x.symbol, x.ts_ns)):
        series = prices.get(h.symbol)
        if not series:
            continue
        fwd = forward_return(series, h.ts_ns, horizon_ns)
        if fwd is None:
            continue
        excess = fwd - baseline.get(h.symbol, 0.0)
        g = grades.setdefault(h.rule, RuleGrade(rule=h.rule, horizon_ns=horizon_ns))
        g.n += 1
        g.wins += 1 if excess > 0 else 0
        g.sum_excess += excess
        g.excesses.append(excess)
    return grades


def leaderboard(grades: dict[str, RuleGrade]) -> list[RuleGrade]:
    """Rules ranked by average excess (desc); ties broken by win rate then name
    for a stable, reproducible ordering."""
    return sorted(
        grades.values(),
        key=lambda g: (-g.avg_excess, -g.win_rate, g.rule),
    )


def decay_flag(weekly_avg_excess: list[float]) -> bool:
    """Edge-decay detection (RES-3): flag when the trailing 4-week mean drops
    below half the trailing 12-week mean. Needs >= 12 weeks; fewer ⇒ no flag
    (not enough history to call decay). Only flags a *positive* edge that is
    fading — a rule that was never good is a separate (kill) decision.
    """
    if len(weekly_avg_excess) < 12:
        return False
    window12 = weekly_avg_excess[-12:]
    window4 = weekly_avg_excess[-4:]
    mean12 = sum(window12) / 12.0
    mean4 = sum(window4) / 4.0
    if mean12 <= 0:
        return False
    return mean4 < 0.5 * mean12
