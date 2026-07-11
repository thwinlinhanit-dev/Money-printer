"""Event-study harness (RES-4): average cumulative excess returns (CAR) around
event times, with seeded bootstrap CIs (CONV-11) and regime slicing.

Powers both ad-hoc questions ("what happens 30 min after liq.cluster > $5M?")
and scheduled studies. Pure stdlib + deterministic RNG so a study is
reproducible from its seed — same inputs, same CI, every run.
"""

from __future__ import annotations

import random
from dataclasses import dataclass


@dataclass(frozen=True)
class Event:
    """An event occurrence: when it happened and (optionally) its regime tag."""

    ts_ns: int
    regime: str | None = None


def _window_excess(
    excess_returns: dict[int, float],
    event_ts: int,
    bar_ns: int,
    pre: int,
    post: int,
) -> list[float] | None:
    """Per-bar excess returns over offsets ``[-pre, +post]`` around an event.

    ``excess_returns`` maps bar-start ns → that bar's excess return (asset −
    benchmark). Returns ``None`` if any bar in the window is missing (no silent
    partial windows — SIM-6 spirit); the caller drops incomplete events.
    """
    out: list[float] = []
    for k in range(-pre, post + 1):
        ts = event_ts + k * bar_ns
        if ts not in excess_returns:
            return None
        out.append(excess_returns[ts])
    return out


def car(
    events: list[Event],
    excess_returns: dict[int, float],
    bar_ns: int,
    pre: int,
    post: int,
) -> list[float]:
    """Average cumulative excess return across events, per offset from ``-pre``
    to ``+post``. Averages the per-bar excess across all complete-window events,
    then cumulatively sums — the standard CAR construction.
    """
    windows = [
        w
        for e in events
        if (w := _window_excess(excess_returns, e.ts_ns, bar_ns, pre, post)) is not None
    ]
    n_off = pre + post + 1
    if not windows:
        return [0.0] * n_off
    avg = [sum(w[i] for w in windows) / len(windows) for i in range(n_off)]
    cum, running = [], 0.0
    for a in avg:
        running += a
        cum.append(running)
    return cum


def car_by_regime(
    events: list[Event],
    excess_returns: dict[int, float],
    bar_ns: int,
    pre: int,
    post: int,
) -> dict[str, list[float]]:
    """CAR computed separately per regime tag (RES-4 regime slicing).
    Deterministic key order via sorted tags."""
    regimes = sorted({e.regime or "_all" for e in events})
    out: dict[str, list[float]] = {}
    for r in regimes:
        subset = [e for e in events if (e.regime or "_all") == r]
        out[r] = car(subset, excess_returns, bar_ns, pre, post)
    return out


def bootstrap_ci(
    events: list[Event],
    excess_returns: dict[int, float],
    bar_ns: int,
    pre: int,
    post: int,
    *,
    seed: int,
    n_boot: int = 1000,
    alpha: float = 0.05,
) -> tuple[float, float]:
    """Percentile bootstrap CI for the terminal CAR (offset ``+post``).

    Resamples events with replacement using a seeded RNG (CONV-11) so the CI is
    reproducible. Returns ``(lo, hi)`` at the ``alpha`` two-sided level.
    """
    terminals: list[float] = []
    for e in events:
        w = _window_excess(excess_returns, e.ts_ns, bar_ns, pre, post)
        if w is not None:
            terminals.append(sum(w))  # cumulative excess to +post for this event
    if not terminals:
        return (0.0, 0.0)

    rng = random.Random(seed)
    boot_means: list[float] = []
    n = len(terminals)
    for _ in range(n_boot):
        sample = [terminals[rng.randrange(n)] for _ in range(n)]
        boot_means.append(sum(sample) / n)
    boot_means.sort()
    lo_idx = int((alpha / 2) * n_boot)
    hi_idx = min(n_boot - 1, int((1 - alpha / 2) * n_boot))
    return (boot_means[lo_idx], boot_means[hi_idx])


@dataclass
class StudyRecord:
    """Tracker-style run record for a study (SIM-10 pattern, RES-4)."""

    name: str
    n_events: int
    pre: int
    post: int
    bar_ns: int
    seed: int
    car: list[float]
    ci_lo: float
    ci_hi: float

    def summary(self) -> str:
        return (
            f"study '{self.name}': n={self.n_events} "
            f"CAR[+{self.post}]={self.car[-1]:+.4f} "
            f"CI95=[{self.ci_lo:+.4f},{self.ci_hi:+.4f}] seed={self.seed}"
        )


def run_study(
    name: str,
    events: list[Event],
    excess_returns: dict[int, float],
    bar_ns: int,
    pre: int,
    post: int,
    *,
    seed: int,
    n_boot: int = 1000,
) -> StudyRecord:
    """Run a full study: CAR curve + terminal bootstrap CI + a run record."""
    curve = car(events, excess_returns, bar_ns, pre, post)
    lo, hi = bootstrap_ci(
        events, excess_returns, bar_ns, pre, post, seed=seed, n_boot=n_boot
    )
    complete = sum(
        1
        for e in events
        if _window_excess(excess_returns, e.ts_ns, bar_ns, pre, post) is not None
    )
    return StudyRecord(
        name=name,
        n_events=complete,
        pre=pre,
        post=post,
        bar_ns=bar_ns,
        seed=seed,
        car=curve,
        ci_lo=lo,
        ci_hi=hi,
    )
