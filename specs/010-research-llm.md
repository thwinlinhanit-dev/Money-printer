# 010 — Research Workflow & LLM Intelligence Agents

## Purpose
The layer that turns recorded data and journals into *understanding*: the
weekly research ritual, screener grading, event studies, and the LLM agents
that draft briefs and explanations — grounded in this system's own data,
never in vibes.

## Scope
In: research environment, screener grading, event-study harness, LLM agent
jobs (daily brief, anomaly explainer, monthly report prose), grounding rules.
Out: ML model training (future BACKLOG spec), feature definitions (004),
report *numbers* (009 generates them; 010 only drafts prose around them).

## Design

### Research environment
`research/` (Python 3.12, CONV-2): Polars + DuckDB over the cold store and
feature store. A tiny `mp_data` package wraps the Dataset layout so notebooks
never hand-roll paths: `mp_data.events(...)`, `mp_data.features(...)`,
`mp_data.coverage(...)` (manifest-aware — refuses silent gaps like SIM-6).

### Screener grading (closes the loop opened by FEA-10)
Weekly job grades every `ScreenerHit` older than the horizon set:
forward returns at +15m/+1h/+4h/+1d/+3d vs symbol baseline, win rate and
avg excess move per rule, trend over time (edge decay detection). Output:
`research/grades/{week}.parquet` + a rules leaderboard. Rules with stable
post-hit drift ≥ threshold for ≥ 8 weeks are flagged "graduate candidate"
→ human decides whether it becomes a hypothesis (006).

### Event-study harness
`study run --event <feature-condition> --window <pre,post>` computes average
cumulative excess returns around event times with bootstrap CIs, regime-
sliced. Same engine powers ad-hoc questions ("what happens 30 min after
liq.cluster > $5M?") and scheduled studies.

### LLM agents (all follow the grounding contract below)
1. **Daily brief (07:00 UTC):** input = last 24h of: regime states, funding
   z-scores, OI quadrant shifts, screener hits + grades, liq clusters,
   position/P&L summary (if any live), data-quality warnings. Output =
   markdown brief to Telegram + `journal/briefs/`. Sections fixed:
   *Regime / Flows worth knowing / Your book / Data health / Watch today*.
2. **Anomaly explainer (on P2 detector alerts, rate-limited):** given the
   alert snapshot + recent events, drafts a 5-line "what likely happened,
   what to check" with links to the runbook. Clearly marked as hypothesis.
3. **Monthly report prose (009 hook):** drafts commentary *around* the
   generated tables; every numeric claim must quote a table cell verbatim.

### Grounding contract (normative for every agent job)
- Inputs are structured exports from this system (SQL/Parquet), passed in
  the prompt; agents MUST NOT be given tools to fetch external
  news/web in v1 (deterministic inputs, auditable outputs).
- Every brief/explanation is archived with its full input bundle hash —
  a brief is reproducible or it doesn't ship.
- Agents never see or produce: API keys, order-placement capability,
  risk-limit changes. LLM output is *read by humans*, never parsed into
  decisions (no LLM on any decision path — extends CONV-2's spirit).

## Requirements
- **RES-1** `mp_data` package MUST wrap dataset/feature/manifest access with
  coverage checks (refuse-or-warn semantics mirroring SIM-6, default warn in
  research, refuse in scheduled jobs).
- **RES-2** Screener grading job MUST run weekly (ops timer), be idempotent,
  and journal a leaderboard; grading math unit-tested on fixtures.
- **RES-3** Edge-decay detection: each rule's grade trend over trailing 12
  weeks with a flag when the 4-week mean drops below half the 12-week mean.
- **RES-4** Event-study harness MUST support feature-condition event
  definitions, bootstrap CIs (seeded — CONV-11), regime slicing, and write
  tracker-style run records (SIM-10 pattern).
- **RES-5** Daily brief job MUST implement the fixed sections, the grounding
  contract, and degrade gracefully: missing input section ⇒ "no data" line,
  never invented content; failure to generate ⇒ P3 alert, not silence.
- **RES-6** Every LLM job archives {input bundle, prompt version, model id,
  output} under `journal/briefs/` (append-only, W-6).
- **RES-7** LLM jobs MUST have no write access to configs, funnel state,
  risk limits, or order paths — enforced by process user/permissions, not
  politeness (Lever 2).
- **RES-8** Prompt templates live in `research/prompts/*.md`, versioned;
  changing one bumps its version header (CONV-20 spirit) so brief archives
  remain interpretable.

## Acceptance criteria
- [ ] Grading job on fixture hits produces hand-verified forward-return grades (RES-2).
- [ ] Decay flag fires on a synthetic decaying rule (RES-3).
- [ ] Event study on fixture data reproduces hand-computed CAR ± CI (RES-4).
- [ ] Brief generated from fixture inputs contains all sections, quotes only input numbers (spot-check test on numeric tokens ⊆ input), archives its bundle (RES-5/6).
- [ ] Permission test: brief job user cannot write to `risk.toml`/funnel state (RES-7).

## Decisions
- 2026-07-10: no external news/web tools for v1 agents — determinism and
  auditability first; narrative/news ingestion is a BACKLOG item with its own
  spec when wanted.
- 2026-07-10: LLM output is human-read only; anything "LLM → decision path"
  requires a new spec and owner sign-off.
- 2026-07-11: LLM provider abstraction implemented as a Rust crate `mp-llm`
  (not Python) so the intelligence agents share the workspace's error/logging
  conventions and the grounding types are compile-checked. Nine providers:
  Anthropic (default model `claude-opus-4-8`), OpenAI, Gemini, Mistral, Cohere,
  DeepSeek, Groq, xAI, and local Ollama. Each is a pure translator
  (`build_request`/`parse_response`); the six OpenAI-compatible providers share
  one code path. Live network I/O (blocking `reqwest`+rustls) is behind the
  `live-http` feature so all request-shaping/parsing is offline-testable — same
  gating pattern as collectors' `live-ws`. This is the single new external
  dependency-with-network for spec 010; it is feature-off by default and never
  reads or logs keys.
- 2026-07-11: RES-6 grounding realised in code: `InputBundle` content-hashes
  (FNV-1a, non-crypto, documented) the exact prompt inputs; `ArchiveRecord`
  emits a deterministic one-line JSON for the append-only `journal/briefs/`
  archive `{provider, model_id, prompt_version, bundle_hash, output, ts}`.
  RES-7 "no LLM on a decision path" is made structural: completions are wrapped
  in `HumanReadOnly` which has no method producing an `OrderIntent`/decision
  type — the guarantee is a type, not a comment. Keys resolve only from env
  vars (PD-2); `providers.example.toml` documents the model/env map with no
  secrets.
- 2026-07-11: Python research layer implemented under `research/` (CONV-2,
  research-only — no Python on a decision path). `mp_data.Coverage` does the
  refuse-or-warn gap check (RES-1, mirrors SIM-6); `grading.py` does forward-
  return screener grading with an honest denominator + a rules leaderboard
  (RES-2) and the 4-wk-vs-half-12-wk edge-decay flag (RES-3); `event_study.py`
  computes CAR with seeded (CONV-11) percentile bootstrap CIs and regime
  slicing (RES-4), plus SIM-10-style run records. All math is pure stdlib and
  deterministic so the 15 pytest fixtures (hand-verified numbers, RES-2/3/4)
  run without Polars/DuckDB — production readers add those for the heavy
  frames. `research/prompts/daily-brief.md` is the versioned template (RES-8)
  with the fixed sections + degrade-to-"no data" rule (RES-5). Still deferred:
  the scheduled brief/anomaly/monthly-report *jobs* wiring these to `mp-llm`
  and the ops timer, and the `mp_data` Polars/DuckDB frame readers — status
  stays `implementing`.

## Open questions
- Which model/provider and monthly token budget — owner picks (cost knob).
