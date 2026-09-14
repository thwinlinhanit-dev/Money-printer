# RESEARCH-LAB-STATUS — capability status (2026-09-07; initial 2026-09-04,
# Phase 7 anti-randomness gates closed 2026-09-07 — see plan §10;
# REL-30 paper-mode recording closed same day)

Spec: `specs/054-research-lab-hardening.md` (incl. REL-24..29) · Plan:
`docs/implementation/RESEARCH-LAB-HARDENING-PLAN.md` · Branch:
`feature/signal-catalog-footprint`.

## PASS / FAIL per capability

| Capability | Status | Evidence |
|---|---|---|
| Data integrity — explicit quality state (REL-1..3, REL-27/R-1) | **PASS** | `features/src/data_quality.rs` — `DataQualityState` (Healthy / InsufficientHistory / Stale / Invalid / **Missing / Gap**); never neutral/0.5; screener "never observed" ⇒ Missing, gap through the staleness window ⇒ Gap until fresh min_samples heal it; Parquet codes 0–5, schema ver bumped |
| Identity — canonical + invalidation (REL-4..7) | **PASS** | `features/src/signal_identity.rs` + `signal_catalog.rs`; `apply_grade` refuses mismatched/pre-hardening grades; tests prove params / feature-version / schema / cost-model changes invalidate old evidence |
| Observations — immutable + gated (REL-8..11) | **PASS** | `features/src/observation.rs`; quality-gated recording with blocked-fire counter; deterministic ids; recorder proven write-only (decision-log hash unchanged) |
| Forward outcomes — horizons/gross/net/MFE/MAE (REL-12..14) | **PASS** | `features/src/outcome.rs`; series-coverage guard (REL-13) + no-lookahead tests; integration slice raw → feature → signal → observation → outcome |
| Evaluation & promotion (REL-15..19) | **PASS** | `features/src/evaluation.rs`; sample gate, gross-vs-net, p25/p50/p75, structured `PromotionDecision`, `grade_from_report` bridge |
| Sample-size tiers (REL-24/R-5) | **PASS** | `SampleTier { Insufficient, Preliminary, Research }`, promotion floor `RESEARCH_MIN_N = 100`; `SAMPLE_TIER_PRELIMINARY` refusal tested at 30/99/100 |
| Regime tagging & refusal (REL-25/R-6) | **PASS** | buckets from the fire snapshot's `regime.trend`; `ONLY_WORKS_IN_*` refusal, `REGIME_COVERAGE_SINGLE_*` / `REGIME_SAMPLE_TOO_SMALL_*` flags (`REGIME_MIN_TAGGED = 10`). 2026-09-07: the sim recorder captures last-seen regime per symbol into fire snapshots (`rel_25_recorder_captures_regime_context_into_snapshots`); kill board re-run with context — no `ONLY_WORKS_IN_*` because NO regime is net-positive (orderflow's gross edge concentrates in TREND: win 0.534 vs 0.498 CHOP at 15m — but net-negative in both) |
| Sustained-decay detection (REL-26/R-7) | **PASS** | chronological-window net expectancy (3 windows, ≥5 per window); two consecutive non-positive recent windows ⇒ `DECAY_SUSPECT` refusal |
| Machine-readable reject codes + JSON decision (REL-29/R-8) | **PASS** | every reject carries a `CODE: detail` reason; `PromotionDecision::to_json` = `{"decision":"REJECT","reasons":["INSUFFICIENT_SAMPLE",…]}` |
| Golden dirty fixture (gaps/invalid/insufficient) | **PASS** | `rel_28_golden_dirty_fixture_gaps_invalid_insufficient_history` — fixed dirty feed walks Missing→Insufficient→Healthy→Gap→Healthy→Invalid with frozen FNV hash `17618162958652494096` |
| Paper-mode observation recording (REL-30) | **PASS** | `sim paper` / `sim paper-tail` accept `--params-hash/--obs-dir/--horizons` — the SAME write-only recorder as backtest; `rel_30_paper_recording_is_write_only_and_deterministic` proves the batched hash is unchanged with recording on; live proof 2026-09-07: paper-vs-backtest decision-log hash identical (`17053129571326659044`), 10,740 obs → 4 Parquet files |
| Hit-journal → observation migration (REL-31) | **PASS** | `hit_to_observation` bridges ScreenerHits onto the identity-stamped flow (venue stamped at fire, venue-less hits REFUSED per R-1, Direction::Long default disclosed in params-hash); `footprint` study binary relocated to mp-storage and migrated off the gross-only JSONL backfill — outcomes (gross+net, MFE/MAE) from the Phase-4 engine, Parquet persistence, per-rule tiered reports; legacy JSONL still loads (serde defaults); live proof 2026-09-07 on the 07-19 bybit log (hit → observation → Parquet under identity `f765b9b3…`) |
| Parquet observation store (REL-23) | **PASS** | `storage/src/observation_store.rs`; round-trip, W-6 no-overwrite, deterministic content hash, partitioned write |
| Footprint/accumulation cleanup (REL-20..22) | **PASS** | market-profile approximation documented; `total_cmp` POC determinism tested for POC/VAH/VAL; accumulation evidence fields in hit snapshots, strict AND untouched |
| Determinism / reproducibility | **PASS** | golden observation hash + golden dirty-fixture hash frozen in `sim/tests/observation_engine.rs`; full workspace `cargo test` green (2026-09-07); sim decision-log golden hash (`sim_14`) unchanged |
| Live trading disabled (PD-1) | **PASS** | trading mode `sleep`; no OMS/venue wiring added; observation code is research-only |
| $0 budget / no new database | **PASS** | zero new deps outside the workspace; Parquet only |

## How to run a research evaluation

```bash
# Replay a recorded log through the production stack with observations on
# (recording is enabled by passing --params-hash); attach forward outcomes,
# persist Parquet to --obs-dir, and print per-horizon evaluation reports:
cargo run -p mp-sim --bin sim -- backtest \
    --log data/merged_hyperliquid_BTC_4d.log \
    --strategy carry-v1 --seed 1 --run-id eval-2026-09-04 --runs-dir data/runs \
    --params-hash <features.toml params hash> \
    --obs-dir data/observations --horizons "15m,1h,4h,1d"
# → per-horizon lines: n / gross_exp / net_exp / win / p25/p50/p75 and
#   "GATE PASS …" or "GATE REFUSED — <explicit reasons>" (REL-15..18)
# Verified 2026-09-04 on the 4-day hyperliquid BTC log: 10,740 observations,
# 4 date-partitioned Parquet files, both horizons honestly refused
# (no positive net edge after costs — REL-16).
```

Programmatic path (libraries):
1. `mp_sim::Backtester` + `enable_observations(params_hash, feature_version,
   created_at_ns)` → run the replay.
2. `bt.attach_outcomes(&[15m, 1h, 4h, 1d].map(…))`.
3. `mp_features::evaluate(bt.observations(), horizon, min_n)` →
   `EvaluationReport` → `decide()` → `PromotionDecision`.
4. `mp_features::grade_from_report(fp, run_id, now, &report)` → catalog
   `GradeSnapshot` → `signals --file … grade …` (identity-stamped).

## Known limitations (honest)

- **Hit journal** (`hit_journal.rs`) JSONL is now the WRITE-side fire log
  only (REL-31): grading/evaluation reads the identity-stamped observation
  store. The legacy gross-only backfill is retired; old journal files still
  parse (serde defaults) and never block. GRD-4's execution-shaped entry
  convention remains binding for fill studies (spec 017 amendment
  2026-09-07); research artifacts use REL-14's measurement-shaped entry.
- The backtester's observation marks are event-sampled (trades/marks/mid), so
  MFE/MAE are sampled excursions, not continuous-path excursions. Deterministic
  and honest, but documented as an approximation.
- Bar-based market profile is an approximation (REL-20); trade-level footprint
  requires full L2 and is out of scope.
- Bootstrap CIs / FDR correction and multi-regime taxonomy: out of scope
  (deferred).
- Paper-mode recording (REL-30) covers the REPLAYED/tailed feed path. A
  true real-time collector-integrated recorder (in the collector process,
  not the sim binary) remains a follow-up; the write-only pattern and
  artifacts are identical.

## Cross-tape kill panel (2026-09-09) — do refusals replicate?

Round 3: all 5 strategies on `data/swing_hyperliquid_BTC.log` (789 MB,
2026-07-19 → 2026-08-18, ~31.6 days — the daily-paper rehearsal tape) plus a
`coinflip-any` re-run on the 4-day tape. params-hash `rel32-swing-baseline`;
runs journaled as `eval-20260909-*-swing` and `eval-20260909-control-4d-rerun`;
script archived at `data/panel-swing-r2.sh`.

| Strategy | 4d tape (08-18→22) | swing tape (07-19→08-18) | Replicates? |
|---|---|---|---|
| coinflip-any (REL-32 control) | REJECT ×4 (NET_NEG+DECAY) — despite a POSITIVE raw journal expectancy (+26.4/trade) | REJECT ×4 (NET_NEG+DECAY), n≈23k | YES — noise killed on both tapes |
| carry-v1 | REJECT ×4 (NET_NEG+DECAY) | REJECT ×4 (NET_NEG+DECAY), n≈19k; 1d gross +0.000194 but net −0.000556 | YES |
| orderflow-v1 | REJECT ×4 (NET_NEG+DECAY) | REJECT at 15m/1d; **GATE PASS at 1h (net +0.000281) and 4h (+0.000566)**, net-positive in BOTH regimes | **NO — the pass is tape-dependent** |
| liq-fade-v1 | n=0 → INSUFFICIENT_SAMPLE | n=0 → INSUFFICIENT_SAMPLE | YES (honest refusal) |
| swing-range-reclaim-v1 | n=0 → INSUFFICIENT_SAMPLE | n=0 → INSUFFICIENT_SAMPLE | YES (honest refusal) |

Findings:
- The R-8 control loop is closed: `coinflip-any` fires on hyperliquid
  (prefix `cvd.` subscription) and the pipeline refuses it at every horizon on
  every tape — even when the raw run journal looks profitable. The net gate
  (REL-16) and decay gate (REL-26) are what kill noise; gross-only grading
  would have PASSED the 4d control.
- orderflow-v1's swing-window 1h/4h PASS reproduces the regime structure from
  round 2 (TREND and CHOP both net-positive at those horizons) but does NOT
  survive the 4-day window (REFUSED + DECAY_SUSPECT there). Per R-7 this is a
  retest mandate, not a promotion: one passing window is preliminary evidence;
  the same identity flipping between PASS and REFUSED across adjacent tapes is
  exactly the instability the multi-window process exists to catch.
- Zero-fire strategies are refused honestly at n=0 on both tapes (no
  fabrication, R-1).
- Determinism cross-checks: carry-v1 on the swing tape reproduces the REL-30
  paper run bit-for-bit (12,006 trades, identical expectancy −8.0816); the 4d
  control rerun journaled identical to its first pass (136,277 trades).
- Artifacts: 3 new identity dirs, 41 new Parquet files. Both control tapes
  share ONE identity (`8e415159…`) — the fingerprint is tape-independent by
  design; date partitions separate the tapes and the W-6 guard accepted both
  writes (no symbols_hash drift: same venue/symbol universe).

## See also

`docs/research/POWER-GATES.md` — the owner's power-gate checklist, audited
2026-09-09 against this document's evidence (23/31 green; §2 and §3 complete;
§4 edge honestly red pending the orderflow-v1 retest).

`docs/research/RESEARCH-RECAP-2026-09-09.md` — the narrative: cross-tape
kill board, REL-30/31 wiring, and the recommended next experiment (bybit
hot days).

## REL-30 incremental-tail live proof (2026-09-09)

`paper-tail --params-hash` proven against a GROWING log slice (a real 20 MB
`head` of the 4d tape grown to 30 MB by five 2 MB appends mid-run, 3s polls):

- Poll-line observability added: `obs=<n> obs_blocked=<n>` now prints every
  poll (read-only counters from the write-only recorder). Live trace shows
  incremental recording mid-stream: `obs=0 → 9 → 139 → 146` as chunks landed,
  with dedup consuming only new frames (`dup=` monotone).
- Negative control (accidental, then kept): same command against a
  non-growing log idles out after 5 polls with 0 observations, 0 Parquet, and
  an honest `INSUFFICIENT_SAMPLE` refusal — a tail that sees no data
  fabricates nothing.
- Regression test `rel_30_paper_tail_records_observations_incrementally`:
  observations grow strictly per poll while the log grows; fires before
  sufficient history are BLOCKED not recorded (R-1 — the block count is
  captured once and must never grow); the final tailed observation set is
  EQUAL to a one-shot paper replay of the completed log, decision-log hash
  included.
-  Artifacts: `data/tail-rel30-live-negative-control.out`,
  `data/tail-rel30-live2-incremental-proof.out`; runs journaled as
  `tail-rel30-live{,2}`; observation Parquet under identity `9dc922a8…`.
  The live close refused on three coded reasons (n=63 < Research floor,
  NET_NEG, DECAY) — the tier gate visibly biting on a small live sample.

## PAP-11 paper noise baseline (2026-09-09)

Spec 051 gained **PAP-11**: every closed-day paper rehearsal now also runs
`coinflip-any` over the SAME log and seed with recording enabled
(`--params-hash pap11-noise-baseline`), journaled as
`kind=paper-noise-baseline`. Expected daily outcome: fires and is REFUSED at
every horizon. **Control `GATE PASS` at any horizon ⇒ evaluation pipeline
broken ⇒ P1 + session fault (exit 1)** — the R-8 alarm is now wired into the
ops surface, not just the research panel. Control fires 0 ⇒ P3 (baseline
void, nothing certified); control-leg crash ⇒ session fault. The leg never
gates the primary strategy's verdict; it certifies the pipeline. Paper-layer
test `pap_11_coinflip_any_fires_on_hyperliquid_log_via_resolver` proves the
control fires with Hyperliquid-venued observations through the real resolver
while legacy `coinflip` starves on the same log; live smoke on a real 4d-tape
slice confirmed fire → regime-tagged refusal → exit 0.

## PAP-12 nightly observation recording + REL-33 W-6 idempotency fix (2026-09-09)

**PAP-12** (spec 051): the PRIMARY paper rehearsal leg now records
observations nightly (`--params-hash pap1-primary --obs-dir data/observations`),
so the research corpus grows from the daily schedule, not just from noise.
W-6 semantics make this safe: each night processes a new date partition;
an identical same-date re-run is a no-op; a divergent same-date re-run
faults loudly.

**REL-33** (spec 054): PAP-12's smoke test exposed a genuine determinism
defect in the observation store — feature snapshots are stored as a JSON
text column, and the serde_json f64→text→f64 cycle is not bit-exact
(1-ULP drift, e.g. `11.439860000000003` → `…05`). The read-back content
hash never matched the in-memory hash, so EVERY identical re-run W-6-faulted
(a nightly schedule would have faulted on its first re-run). Additionally,
the guard was name-based: divergent content on the same date could land as
a duplicate sibling file.

Fix (storage-canonical hashing): `observations_content_hash` now hashes
snapshot f64 values in their storage-canonical form (`to_string` then
parse — the exact writer+parser pair the Parquet column uses), so in-memory
and read-back hashes agree by construction. `partitioned_write` gained a
content-based sibling scan: identical content anywhere in the date
partition ⇒ no-op; divergent content ⇒ hard error, never a duplicate.
Regression tests `rel_33_…` (62/62 in mp-storage) + end-to-end proof with
the release binary: identical re-run exit 0 with 1 Parquet (no sibling);
same-identity divergent content exit 2 with explicit W-6 refusal and no
file written. Probe scaffolding removed; suites green (28 sim lib, 62
storage, strategies).

## REL-34 Python grading arm reads the observation store (2026-09-09)

One artifact store, two consumers: `research/observation_store.py` reads the
SAME identity-stamped Parquet the Rust store writes — no parallel export.
Every load verifies (a) the Arrow schema against declared constants, (b) the
identity fingerprint CROSS-LANGUAGE (Python FNV-1a re-derives the on-disk
directory name from every row's identity columns — mismatch = hard error,
R-3), and (c) single-identity (a load spanning >1 fingerprint refuses).
Grading consumes precomputed `outcome_net_return` (R-4 net-primary) at the
engine's horizons; open windows are skipped AND counted (honest denominator,
R-1); `RuleGrade` carries `signal_id`/`identity_fingerprint` so journal rows
are comparable only like-for-like. `run_observation_grading` journals per
identity + corpus stamp, idempotent (W-6).Decisive test: Python re-derived ALL 10 real identity directory names from their Parquet bytes; the live run on `9dc922a8…` (tail-live2) independently reproduced the Rust verdict — n=63, win 0.508, net −0.000771, PRELIMINARY + NET_EXPECTANCY_NONPOSITIVE. Research suite: 210 passed (11 new).

## orderflow-v1 retest — candidate killed (2026-09-11)

Owner-approved execution of POWER-GATES §4's retest mandate (report:
`docs/research/RETEST-orderflow-v1-2026-09-11.md`). Venue generalization:
18 binance BTCUSDT days replayed per-day (the merged 12 GB log OOMs
`read_log` on this box — a documented sim limitation); 5/18 days trade, all
net-negative at 2× cost (worst −172.2 on 08-07, heaviest −52.6 @ n=1541 on
07-28). Walk-forward: 27-combo grid, purged + 1h embargo — swing 8 windows
(5 vacuous, 3 selected, ALL OOS-negative, incl. in-sample +12.04 → OOS
−12.46); 4d tape 2/2 OOS-negative. 3/3 hypothesis kill criteria met; registry
row `killed`. Zero standing candidates.

Same day, owner-approved ops changes: `MoneyPrinterKillPanel` scheduled
(09:15 UTC nightly; 5 strategies + control on the freshest closed HL day;
P2 on strategy PASS/DECAY, P1 on control PASS; streak file feeds the §7.1
14-night trigger); §6 capital pre-commitment written into POWER-GATES
(§4=5/5 AND 6+ months clean paper before ANY capital decision).

Housekeeping fixed en route: registry checker validated run_ids against the
funnel-era journal only (`runs/index.jsonl`) — now checks both journals;
dead run_id references (experiment-tracker ULIDs with no surviving artifacts,
`backlog-event-studies-*` pseudo-ids never journaled) replaced with the real
evidence markdowns. Open owner-governance item flagged, not silently fixed:
ALP-1 WIP limit exceeded (4 active candidates vs. max 1) — deciding the sole
active slot is the next owner call now that orderflow-v1 is killed.
## B1 delete + panel followup (2026-09-12)

- **B1 chain re-armed (pid 8196, 36h deadline).** The original chain's 12h
  wait deadline would have expired (~11:52 local) *before* tonight's backup
  can finish (~18:00–19:00 UTC; the full 53 GiB hash verify alone is ~21h at
  measured 0.7 MiB/s), silently aborting the owner-approved delete for a day.
  Re-armed with a 36h deadline; fail-closed gates unchanged (task not Running +
  TONIGHT's verify-green line in a freshly-written log + same-night 38/38
  mirror re-verify). Chain log: `data/retest/b1_chain.log`.
- **Kill panel updated**: the killed orderflow-v1 removed from the nightly
  replay list (it only produced standing DECAY/KILL noise); now replays
  control + carry-v1, liq-fade-v1, swing-range-reclaim-v1. Parse-checked.
- **ALP-1 candidate grading (facts as of 09-12):** swing-range-reclaim-v1 is
  the only hypothesis-state candidate with a live sim harness (nightly panel
  replay since 09-11); funding-arb-v1 is the only candidate that has CLEARED
  cost math and now has the >=3-overlap-day bybit+hyperliquid corpus on disk
  (18+ bybit BTCUSDT days, 07-19..09-xx); carry-v1 and trend-breadth-v1 are
  blocked on uncollected data (spot leg; 50–100-symbol panel). Decision and
  registry move are the owner's.

## B1 gate redesign: scoped off-host verify (2026-09-12 evening)

- **Machine rebooted 20:31 local**, killing the overnight backup mid-verify
  (push of 2943 artifacts DID complete; only the ~21h full-corpus hash verify
  was cut short) and blocking the kill panel's 15:45 fire (caught up at 21:03
  after boot: 20260911 day, control refused 4/4, carry-v1 DECAY x6 -> real P2,
  streak 2/14; orderflow-v1 absent post-kill).
- **Design change:** waiting on a full-corpus backup verify to unlock B1 is
  fragile (20h+ tail, execution limits, reboot exposure). B1's off-host term
  needs only the 38 signed binance artifacts byte-verified off-host, so the
  gate is now a **scoped decrypt-hash verify** of exactly those artifacts
  against the live staging manifest: remote ciphertext -> age decrypt -> gzip
  -> plaintext sha256 == manifest sha256 == master sha256. New tool:
  `data/retest/verify_offhost_scope.ps1` (resumable; `-Only` smoke mode; the
  6-second single-file smoke passed before the full run). The 8 small
  binance cold artifacts stay on master (B1 signs 38 raw logs = 19,975,043,408
  bytes exactly).
- **gdrive rate-limit incident (first full run):** unbounded-tps rclone
  downloads wedged at 0-byte partials on `rateLimitExceeded` (probe: pacer
  sleeping 16s+; with `--tpslimit 1` data flows). Verifier hardened: every
  rclone call tps-limited (`--tpslimit 2`) with rclone-internal retries
  (`--retries 5 --low-level-retries 20`), resumable from a prior report.
- **Chain rewritten** (`data/retest/owner_delete_binance.ps1`): gate 0 waits
  for the scoped verify's RESULT line (36h deadline), gate 1 requires
  `RESULT ok=38 fail=0` + marker <12h old + cross-check of every OK line
  against the exact master bytes on disk, gate 2 is the fresh same-night
  mirror verify, then the 38 journaled owner_approved entries + delete.
  Fail-closed throughout; the abortive backup-log-gate (and its UTC-midnight
  freshness bug) is gone. Verifier + chain both armed detached; logs:
  `data/retest/offhost_scope_verify.log`, `data/retest/b1_chain.log`.
- The morning backup's remote state (all 2943 artifacts pushed) is what makes
  tonight's scoped verify meaningful; its own full verify is expected to
  complete on the next scheduled run (09:00 local daily).

### Offhost hardening + VPS collector audit (2026-09-12 late)

- `ops/scripts/offhost_backup.ps1`: all 3 rclone call sites (push, check,
  verify download) now tps-limited `--tpslimit 2` + `--retries 5
  --low-level-retries 20` — same rateLimitExceeded wedge class as the scoped
  verifier hit. Next scheduled run (09:00 local) exercises it.
- VPS collector audit (34.135.127.147, read-only ssh as mp-egress): 6/6 legs
  active (mp-hyperliquid@BTC/@ETH, mp-swing@bybit-{btc,eth,sol}usdt,
  mp-swing-macro FRED, mp-whale census), 7/7 heartbeats <60s, zero journal
  errors 24h, live growth confirmed on bybit BTC. mp-whale writes
  `data/raw/{date}_hyperliquid_positions.log` (spec 028) — the 512M/swap
  numbers are cgroup accounting, process stable since Sep 03. Hot tier holds
  30 raw files, retention+drain by design (mirror pull integrity 0 missing).
  mp-netflow disabled by design (PD-2, awaits MP_ETHERSCAN_KEY). Cosmetic:
  systemd "unit file changed on disk" warnings on mp-whale/mp-netflow —
  daemon-reload on next deploy window.

## 2026-09-13 — funding-arb-v1 killed by its own pre-registered backtest; ALP-1 restored

- **Registry governance (spec 053 ALP-1):** the 4-candidate WIP violation is
  resolved — funding-arb-v1 entered as sole active candidate per 053's own
  written pick, carry-v1 / trend-breadth-v1 / swing-range-reclaim-v1 →
  `held` (data-blocked / alpha bet parked; swing keeps its nightly
  kill-panel replay as 051 plumbing). `run_registry.py check` green.
- **Backtest (record `farb2-backtest-2026-09-13`):** pre-registered protocol
  (`research/run_funding_arb_backtest.py`): signal = FARB-3 annualized
  spread on prior bar (no lookahead), accrual = HL hourly + bybit 8h
  settlements (raw rates, strict-before-boundary), costs = registry 29/42.5
  bps RT + 2× kill column. Verdict: **KILLED** — both configs, expectancy
  −28.1 bps at BASE costs, 0/58 episodes profitable, carry collected ≤ 4.3
  bps vs 29 bps cost leg; breadth fine (6–8 windows), WF clean (0 flips) —
  the edge is uniformly negative, not curve-fit. §4 standing candidates:
  **zero** (swing-range-reclaim-v1 remains 051 paper plumbing, parked).
- **B1 delete chain:** last night's 17:31Z abort was a gate-parsing bug —
  the scoped verify wrote `RESULT ok=38 fail=0 bytes=…` and the chain
  compared the line for exact equality. Off-host evidence was actually
  GREEN (38/38 decrypt-hash, exactly 19,975,043,408 bytes). Chain rewritten
  to gate in dependency order (backup Ready → THIS run's full-corpus
  verify-green → master drift re-hash → fresh mirror verify → race guard →
  journal+delete) and re-armed behind this morning's backup run.

## 2026-09-13 — Pre-registered next-candidate slate (all held, none active)

With §4 at zero standing candidates, three hypotheses were pre-registered
(falsification written BEFORE any backtest, spec 053 template; each carries
its own data-gate prefix — the FARB pattern). All enter registry as `held`
per ALP-1 (WIP=1; the slot reopens by owner pick):

1. **whale-shadow-v1** (`strategies/whale-shadow-v1/hypothesis.md`, WSH-n) —
   HL leaderboard whale-census flow (`whale.delta`) predicts venue-relative
   basis drift; cross-venue basis trade, funding-arb's cost bar. Gates: ≥10
   event days, same-day both legs, cohort-epoch control (WSH-3).
2. **oi-purge-v1** (`strategies/oi-purge-v1/hypothesis.md`, OPG-n) —
   OI-purge exhaustion reversion; the ALP-6-compliant revival of the parked
   backlog idea (n=1 then; corpus since tripled). Fixed 2×2 grid, ≥12 events
   or NOT-GRADABLE-stays-held.
3. **footprint-exhaustion-v1** (`strategies/footprint-exhaustion-v1/hypothesis.md`,
   FPX-n) — volume-climax exhaustion fade on the trade tapes (spec 049
   features); carries the orderflow-v1 overfit lessons; scalp cost bar
   declares the most likely honest kill up front.

Each gates on an event study FIRST (seeded bootstrap CI95, partial windows
omitted per SIM-6, excess-return beta control where directional). None may
enter a backtest or the active slot before its gates pass and the owner
picks.
