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


def forward_return(
    prices: list[tuple[int, float]], ts_ns: int, horizon_ns: int
) -> float | None:
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
    # Identity stamp (spec 054 REL-34): when grading observation-store
    # outcomes, the rule IS a signal_id under a full research identity —
    # carried on the grade so journal rows are comparable only
    # like-for-like (R-3). Empty for legacy screener grading.
    signal_id: str = ""
    identity_fingerprint: str = ""

    @property
    def win_rate(self) -> float:
        return self.wins / self.n if self.n else 0.0

    @property
    def avg_excess(self) -> float:
        return self.sum_excess / self.n if self.n else 0.0

    @property
    def sharpe(self) -> float:
        """Per-period Sharpe of the "buy the hit" strategy (spec 017 GRD-3),
        computed on per-hit excess returns — consistent with ``avg_excess``
        and ``win_rate``; not annualized. Degenerate samples (n < 2 or zero
        variance) score 0.0: a rule with no variance evidence must never be
        promoted on a made-up Sharpe (PD-5)."""
        if self.n < 2:
            return 0.0
        mean = self.avg_excess
        var = sum((x - mean) ** 2 for x in self.excesses) / (self.n - 1)
        if var == 0.0:
            return 0.0
        return mean / var**0.5


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


def grade_observation_returns(
    rows: list,
    horizon_ns: int,
    *,
    rule: str | None = None,
    signal_id: str | None = None,
    identity_fingerprint: str | None = None,
) -> RuleGrade:
    """Grade PRECOMPUTED net outcomes from the observation store (REL-34).

    ``rows`` are ``observation_store.ObservationRow``s with a closed outcome
    at ``horizon_ns`` (``load_observation_store(...).net_outcomes(h)``).
    An EMPTY row list grades to an honest n=0 ``RuleGrade`` (the tier gate
    speaks for it — the Rust evaluator does the same); pass ``signal_id``/
    ``identity_fingerprint`` then, since they cannot come from rows.

    R-4: net expectancy is the primary metric — the store's
    ``outcome_net_return`` is already cost-adjusted under the identity's
    cost model, so the grader consumes it as-is and never re-derives "net"
    from gross with different assumptions.

    Semantics match the Rust evaluator (features/src/evaluation.rs):
    expectancy = mean(net_return); win = ``outcome_hit`` (gross > 0).
    Non-finite outcomes REFUSE the grade (REL-1: a corrupt store must not
    silently shrink the sample — raise, never impute, never skip quietly).
    Deterministic: rows are graded in the loader's order.
    """
    sid = rule or signal_id or (rows[0].signal_id if rows else "")
    if not sid:
        raise ValueError("grade target unknown: no rows and no rule/signal_id given")
    g = RuleGrade(rule=sid, horizon_ns=horizon_ns)
    g.signal_id = signal_id or sid
    g.identity_fingerprint = identity_fingerprint or (
        rows[0].identity_fingerprint if rows else ""
    )
    if not rows:
        return g
    for r in rows:
        net = r.outcome_net_return
        if net is None or net != net or net in (float("inf"), float("-inf")):
            raise ValueError(
                f"non-finite outcome_net_return for observation {r.observation_id} "
                "(REL-1: refusing to grade a corrupt sample)"
            )
        g.n += 1
        g.wins += 1 if r.outcome_hit == 1 else 0
        g.sum_excess += net
        g.excesses.append(net)
    return g


def leaderboard(grades: dict[str, RuleGrade]) -> list[RuleGrade]:
    """Rules ranked by average excess (desc); ties broken by win rate then name
    for a stable, reproducible ordering."""
    return sorted(
        grades.values(),
        key=lambda g: (-g.avg_excess, -g.win_rate, g.rule),
    )


def recommendation(g: RuleGrade) -> str:
    """Next-stage recommendation for the funnel (spec 017 GRD-3): promote /
    demote / kill / hold, per the spec's thresholds. ``win_rate`` is the hit
    rate on the honest denominator (wins / hits with computable returns —
    total evaluations are not tracked; see spec 017 Decisions 2026-08-17).
    Kill is evaluated before demote so the stricter verdict wins (a <20% hit
    rate rule is killed, not demoted)."""
    if g.win_rate >= 0.60 and g.sharpe > 1.0:
        return "promote"
    if g.win_rate < 0.20 or (g.sharpe < 0.0 and g.win_rate < 0.50):
        return "kill"
    if g.win_rate < 0.40:
        return "demote"
    return "hold"


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
