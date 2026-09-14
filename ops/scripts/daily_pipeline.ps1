# daily_pipeline.ps1 - Daily integrity gate -> scorecard -> compaction (SPEC-024).
# Windows port of ops/scripts/daily_maintenance.sh, so the promotion gate can
# advance on the native Windows host that actually runs the collectors.
#
# The gate runs at 07:30 UTC — AFTER the 01:00 VPS drain has landed the closed
# day's files into the master corpus. Since the 2026-08-18 §6 handoff the
# hyperliquid recordings are VPS-canonical: the Windows host no longer records
# them, so the drained VPS copies ARE the audited files. The gate must sit
# past the slow-link transfer's realistic completion (a full closed day is
# ~2.3 GiB and the 08-17 drain of the 08-16 files took ~4 h; the drain task
# allows 8 h before it is killed and the per-file resume re-attempts next
# run). The drain pulls the gate-required hyperliquid files FIRST so they
# land earliest; a night where the drain still runs past the gate audits a
# partially-landed day (honest DIRTY — the VPS gate at 00:05 UTC remains the
# promotion authority, and the drain's resume completes the corpus next run).
#
# Flow (mirrors daily_maintenance.sh):
#   1. scorecard  yesterday's recording matrix (all required streams and
#      recordings) -> data/scorecards/<date>.json
#   2. NOT promotable -> WARN + exit 1 (Task Scheduler flags the run); the
#      scorecard is still archived so the streak verdict stays complete.
#   3. promotable  -> `mp-materialize` the day's logs into the feature store
#      (spec 016 Phase 2: recordings + whale-positions census, FEA-6 footer).
#   4. promotable  -> `mp-ops compact` each recording through the INT-4
#      verified gate (quarantined logs are refused before cold writes).
#   5. Print the promotion streak verdict (longest run of promotable days
#      across data/scorecards/*.json) - the daily "N/7" line.
#
# Usage:
#   .\ops\scripts\daily_pipeline.ps1                # run for yesterday (UTC)
#   .\ops\scripts\daily_pipeline.ps1 -Date 2026-08-04   # backfill/re-check
#   .\ops\scripts\daily_pipeline.ps1 -RegisterTask  # schedule at 07:30 UTC daily
#   .\ops\scripts\daily_pipeline.ps1 -SkipCompact   # scorecard + verdict only
#
# Zero-Cost Mode (docs/ZERO_COST_MODE.md):
#   Set $env:ZERO_COST=1 to arm Zero-Cost Mode (Hyperliquid-only, no book,
#   relaxed gate) AND enable hot-tier raw deletion.
#   Default is 0 (full-mode): an UNSET ZERO_COST must NOT arm destructive
#   semantics on this host. A Windows Scheduled Task does not inherit the
#   shell environment, so unset was silently becoming 1 (A-2, audit
#   2026-09-02) — narrowing recordings and arming deletion on the desktop
#   cold-storage host. Zero-Cost is now an explicit opt-in.
#   $env:RETENTION_DAYS controls hot-tier pruning (default: 14).
#   Use .\ops\scripts\deploy_zero_cost_pipeline.sh / set ZERO_COST=1 in the
#   task env to run the VPS under Zero-Cost.

param(
    [string]$Date,                 # YYYY-MM-DD or YYYYMMDD (default: yesterday UTC)
    [switch]$RegisterTask,         # register the MoneyPrinterDailyPipeline task
    [switch]$RegisterStaleTask,    # register the MoneyPrinterPipelineStale dead-man
    [switch]$SkipCompact,          # audit + scorecard + verdict, no cold writes
    [switch]$SkipMaterialize,      # audit + scorecard + verdict, no feature-store writes
    [string[]]$Recordings  = @(),  # venue:symbol pairs to require; empty = core list
    # 2026-08-08: the gate's required stream set is the one the current
    # Phase-0 venue (hyperliquid) can actually deliver over WS: trades,
    # snapshot books, and activeAssetCtx (mark/funding/OI).  Hyperliquid has
    # no WS liquidation stream by design - liquidation data comes from the
    # on-chain whale census (`mp-whale`, spec 028, recorded separately) and
    # feeds the liq-est band research edge (spec 029), it is not a required
    # gate stream.  Venues WITH a liquidation source are required to show it
    # via venue-scoped requirements below (COL-29: binance/bybit must carry
    # the `liquidation` stream on every recorded day).
    [string[]]$RequiredStreams = @("trade", "book", "funding", "mark_price", "open_interest")
)

$ErrorActionPreference = "Stop"
# Workspace root = the Cargo.toml with a [workspace] section (sub-crates like
# ops/ also carry a Cargo.toml, so we must not stop at those).
$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
while ($true) {
    $manifest = Join-Path $root "Cargo.toml"
    if ((Test-Path $manifest) -and (Select-String -Path $manifest -Pattern '^\[workspace\]' -Quiet)) { break }
    $parent = Split-Path $root -Parent
    if ($parent -eq $root) { Write-Host "[!!] workspace root not found from $PSScriptRoot" -ForegroundColor Red; Exit 1 }
    $root = $parent
}
$TaskName   = "MoneyPrinterDailyPipeline"
$scoreDir   = Join-Path $root "data\scorecards"

# 0. PD guardrails (audit 08-04 #6) run later in the script, after Log and
#    Invoke-Native are defined (audit 08-08): the scheduled task runs hidden,
#    so the guardrails verdict must land in data/scorecards/pipeline.log, not
#    just the lost console.
$logFile    = Join-Path $scoreDir "pipeline.log"
$mpOps      = Join-Path $root "target\release\mp-ops.exe"

# Recordings default: empty = load the single source of truth through the
# shared parser (ops/scripts/recordings.ps1, dot-sourced below - the SAME
# resolver start_collectors.ps1 and ops/watchdog_collectors.ps1 use, so the
# scorecard's required set ALWAYS matches what the watchdog records - no
# silent under-scoping, and an invalid line fails the pipeline loudly instead
# of narrowing the gate). Falls back to hyperliquid:BTC+ETH when absent.
. (Join-Path $root "ops\scripts\recordings.ps1")
if ($Recordings.Count -eq 0) {
    try {
        $recPairs = @(Resolve-Recordings -CoreFile (Join-Path $root "ops\core_symbols.txt"))
    } catch {
        Log "core_symbols.txt parse failed: $_" "ERROR"
        Exit 1
    }
    $Recordings = @($recPairs | ForEach-Object { "$($_.venue):$($_.symbol)" })
}

function Log {
    param([string]$Msg, [string]$Level = "INFO")
    $ts = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    New-Item -ItemType Directory -Path $scoreDir -Force | Out-Null
    Add-Content -Path $logFile -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") { "Yellow" } elseif ($Level -eq "ERROR") { "Red" } else { "White" }
    Write-Host $line -ForegroundColor $col
}

# Safe native-call wrapper.  Under $ErrorActionPreference=Stop, a failing native
# command that writes to stderr raises NativeCommandError *during the
# assignment*, so the `$exit = $LASTEXITCODE` line never runs and error handling
# becomes dead code.  We therefore run the command under "Continue" and return
# (output, exitCode) explicitly.  Callers always branch on the exit code.
function Invoke-Native {
    param([string]$FilePath, [string[]]$Arguments)
    $ErrorActionPreference = "Continue"
    $out = & $FilePath @Arguments 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = "Stop"
    return ,@($out, $code)
}

# 0. PD guardrails (audit 08-04 #6): the bash guardrails cannot run on this
#    native Windows host, so the PowerShell port runs first - mechanical
#    rulebook enforcement (PD-1..4, W-7, CONV-21, OPS-4) before any number
#    from the scorecard is trusted. Fail-closed: a violation stops the
#    pipeline with a distinct, grep-able error. Invoked through Invoke-Native
#    so its output is captured and logged (audit 08-08: the scheduled task's
#    window is hidden - a failure must be visible in pipeline.log).
$guardrails = Join-Path $root "ops\ci\guardrails.ps1"
# Fail-closed (audit 2026-08-17): a MISSING guardrails script must not
# silently skip the check - an unverifiable tree is an untrusted tree.
if (-not (Test-Path $guardrails)) {
    Log "guardrails script MISSING at $guardrails - cannot verify PD-1..4/W-7; refusing to trust today's scorecard" "ERROR"
    Exit 1
}
$gr = Invoke-Native -FilePath "powershell.exe" -Arguments @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $guardrails)
foreach ($gline in @($gr[0])) {
    $gs = ($gline | Out-String).Trim()
    if ($gs -ne "") { Log $gs "INFO" }
}
if ($gr[1] -ne 0) {
    Log "guardrails failed - the tree violates the rulebook (PD-1..4/W-7); fix before trusting today's scorecard" "ERROR"
    Exit 1
}
Log "guardrails: all checks passed" "INFO"

# ---- Zero-Cost Mode switch (docs/ZERO_COST_MODE.md) ------------------------
# A-2 (audit 2026-09-02): ZERO_COST must be an EXPLICIT opt-in. Unset now
# defaults to 0 (full mode). A Windows Scheduled Task does not inherit the
# shell env, so silently defaulting to 1 would narrow the required recordings
# AND arm raw deletion on this host. Set ZERO_COST=1 in the task/env (the VPS
# path via deploy_zero_cost_pipeline.sh) to arm Zero-Cost.
if (-not $env:ZERO_COST) { $env:ZERO_COST = "0" }
if ($env:ZERO_COST -eq "1") {
    # Override recordings to Hyperliquid-only (permissionless, no geo-blocks),
    # BTC + ETH only (storage bounded). No book stream.
    if ($Recordings.Count -eq 0) {
        $Recordings = @("hyperliquid:BTC", "hyperliquid:ETH")
    }
    # Drop 'book' from required streams — Zero-Cost does not record L2 book.
    $RequiredStreams = @("trade", "funding", "mark_price", "open_interest")
    Log "Zero-Cost Mode ARMED: recordings=$($Recordings -join ',') streams=$($RequiredStreams -join ',') - hot-tier raw deletion is ENABLED (A-2)" "WARN"
} else {
    Log "Full-mode: recordings=$($Recordings -join ',') streams=$($RequiredStreams -join ',') (ZERO_COST unset -> 0, raw deletion OFF)"
}

# ---- task registration ------------------------------------------------------
if ($RegisterTask) {
    # Schedule at the LOCAL wall-clock time that corresponds to 07:30 UTC.
    # Handoff 2026-08-18: the audit moved from 00:05 to AFTER the 01:00 VPS
    # drain (hyperliquid is VPS-canonical now; the drained files are what the
    # gate audits), and a full day's transfer takes hours on the slow link,
    # so the gate sits past its realistic completion (drain task limit 8 h).
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(7).AddMinutes(30), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): -Daily anchors StartBoundary to the
    # DATE passed in -At (verified empirically 2026-09-06), so registering
    # AT/AFTER the target time puts the boundary in the past and Task Scheduler
    # can fire the task immediately into its own registration
    # (MoneyPrinterDataBackup 00:07Z launch failure, 0xFFFD0000). Pin the first
    # trigger to tomorrow in that case.
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $action  = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Hours 6) `
        -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 5)
    try {
        Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
            -Settings $settings -User $env:USERNAME -Force -ErrorAction Stop | Out-Null
        Write-Host "[OK] Registered $TaskName daily at $($localAt.ToString('HH:mm')) local (=$($utcTarget.ToString('HH:mm')) UTC)." -ForegroundColor Green
        Write-Host "     DST note: after a clock change, re-run -RegisterTask to keep the 07:30 UTC window." -ForegroundColor Gray
        Write-Host "     The script self-guards: it only runs the audit when UTC hour is 0-9." -ForegroundColor Gray
    } catch {
        Write-Host "[!!] Register-ScheduledTask failed: $($_.Exception.Message)" -ForegroundColor Red
        Exit 1
    }
    Exit 0
}

# ---- dead-man for the gate itself (OPS-17, runbook pipeline-stale) ----------
# The 02:30 gate is one scheduled task; if THAT job silently dies, the streak
# goes blind (blueprint failure-mode #6). Register a separate 00:15 UTC task
# running `mp-ops pipeline-stale` — a P1 when yesterday's scorecard has not
# landed. It is deliberately NOT part of this script: a watchdog must be
# independent of the thing it watches.
if ($RegisterStaleTask) {
    # Probe (audit 2026-08-17): a dead-man task registered against an mp-ops
    # binary that predates the pipeline-stale subcommand (OPS-17) would error
    # hourly while the P1 stays silent - the exact hole pipeline_stale_check.sh
    # probes for. Mirror it: skip registration with a log line (safe to
    # register once the tree is rebuilt).
    if (-not (Test-Path $mpOps)) {
        Log "pipeline-stale task NOT registered: mp-ops.exe missing (build it, then re-run -RegisterStaleTask)" "WARN"
        Exit 0
    }
    $ErrorActionPreference = "Continue"
    $staleUsage = (& $mpOps 2>&1 | Out-String)
    $ErrorActionPreference = "Stop"
    if ($staleUsage -notmatch "pipeline-stale") {
        Log "pipeline-stale task NOT registered: mp-ops.exe predates the pipeline-stale subcommand (OPS-17) - rebuild, then re-run -RegisterStaleTask" "WARN"
        Exit 0
    }
    # 08:15 UTC — after the 07:30 gate (handoff 2026-08-18: the gate moved
    # after the 01:00 drain, so the stale deadline moved with it).
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(8).AddMinutes(15), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): pin the first trigger to tomorrow
    # when registering at/after the target time (see the -RegisterTask block).
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
        # Absolute --scorecards-dir (audit 2026-08-18): the task has no working
    # directory, and pipeline-stale's default is CWD-relative — the dead-man
    # false-fired a P1 every day even when the gate landed (reproduced from
    # the task's default CWD: stale=true "no scorecard for YYYY-MM-DD").
    $staleArgs = "-ExecutionPolicy Bypass -WindowStyle Hidden -Command & `"$mpOps`" pipeline-stale --scorecards-dir `"$scoreDir`" --telegram --webhook *>> `"$logFile`""
    $action  = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $staleArgs
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 10)
    try {
        Register-ScheduledTask -TaskName "MoneyPrinterPipelineStale" -Action $action -Trigger $trigger `
            -Settings $settings -User $env:USERNAME -Force -ErrorAction Stop | Out-Null
        Write-Host "[OK] Registered MoneyPrinterPipelineStale daily at $($localAt.ToString('HH:mm')) local (=$($utcTarget.ToString('HH:mm')) UTC)." -ForegroundColor Green
        Write-Host "     Fires a P1 (pipeline-stale) when yesterday's scorecard is missing by the deadline." -ForegroundColor Gray
    } catch {
        Write-Host "[!!] Register-ScheduledTask failed: $($_.Exception.Message)" -ForegroundColor Red
        Exit 1
    }
    Exit 0
}

# ---- LAB-7: data home must not be Downloads ----------------------------------
# If MP_DATA_HOME is set, it must not contain "Downloads" (case-insensitive).
# The scheduled task on the host may inherit no env; when the data dir defaults
# to a Downloads path, that is a hard stop (W-6: data lives off Downloads).
if ($env:MP_DATA_HOME) {
    if ($env:MP_DATA_HOME -imatch 'Downloads') {
        Log "LAB-7: MP_DATA_HOME contains 'Downloads' ($($env:MP_DATA_HOME)) - data must live off Downloads (W-6). Hard stop." "ERROR"
        Exit 1
    }
}

# ---- LAB-1: no DST/hour-window skip; catch-up scores missing days ----------
# The pipeline MUST attempt a scorecard for every UTC calendar day (LAB-1).
# A skip because the hour is outside 00:00-09:00 is a defect. If the
# scheduled task wakes late, it still scores the missing UTC day (catch-up),
# then today's. The DST guard is removed per spec 050 LAB-1.

# ---- date resolution --------------------------------------------------------
if (-not $Date) {
    $Date = (Get-Date).ToUniversalTime().AddDays(-1).ToString("yyyyMMdd")
} else {
    Log "Manual run: explicit -Date $Date (DST guard bypassed)"
}
$dateFlat = $Date.Replace("-", "")
if ($dateFlat.Length -ne 8) { Log "Invalid date: $Date (expected YYYYMMDD or YYYY-MM-DD)" "ERROR"; Exit 1 }
$dateDashed = "{0}-{1}-{2}" -f $dateFlat.Substring(0,4), $dateFlat.Substring(4,2), $dateFlat.Substring(6,2)

Log "Daily pipeline start: date=$dateDashed recordings=$($Recordings -join ',') skip_compact=$SkipCompact"

# ---- build mp-ops if missing or stale ---------------------------------------
# Stale-guard: a release binary that predates a subcommand would fail with
# "unknown subcommand" mid-pipeline, so verify `scorecard` exists too.
function Invoke-Build {
    # Route through Invoke-Native (audit 08-08): cargo writes its progress to
    # stderr, and under $ErrorActionPreference=Stop PowerShell 5.1 raises a
    # terminating NativeCommandError on stderr output - the old `2>&1 |`
    # pipe killed the whole pipeline silently (task result=1, no log entry)
    # whenever a rebuild was needed (also explains the 08-07/08-08 00:05 UTC
    # silent failures: no scorecards were ever archived).
    Push-Location $root
    $res = Invoke-Native -FilePath "cargo" -Arguments @("build", "-p", "mp-ops", "--release")
    Pop-Location
    foreach ($line in @($res[0])) {
        $s = ($line | Out-String).Trim()
        if ($s -match "error|warning: unused|Finished") { Log $s "WARN" }
    }
    if ($res[1] -ne 0) { Log "mp-ops build failed (exit $($res[1]))" "ERROR"; Exit 1 }
}

function Invoke-Build-Materialize {
    # Same native-stderr discipline as Invoke-Build (audit 08-08).
    Push-Location $root
    $res = Invoke-Native -FilePath "cargo" -Arguments @("build", "-p", "mp-storage", "--release", "--bin", "mp-materialize")
    Pop-Location
    foreach ($line in @($res[0])) {
        $s = ($line | Out-String).Trim()
        if ($s -match "error|warning: unused|Finished") { Log $s "WARN" }
    }
    if ($res[1] -ne 0) { Log "mp-materialize build failed (exit $($res[1]))" "ERROR"; Exit 1 }
}

function Invoke-Build-Determinism {
    # Same native-stderr discipline as Invoke-Build (audit 08-08).  mp-sim
    # depends on mp-storage, so this also exercises the shared log loader
    # (spec 018 MOD-9..11: replay inputs == materializer inputs).
    Push-Location $root
    $res = Invoke-Native -FilePath "cargo" -Arguments @("build", "-p", "mp-sim", "--release", "--bin", "mp-determinism")
    Pop-Location
    foreach ($line in @($res[0])) {
        $s = ($line | Out-String).Trim()
        if ($s -match "error|warning: unused|Finished") { Log $s "WARN" }
    }
    if ($res[1] -ne 0) { Log "mp-determinism build failed (exit $($res[1]))" "ERROR"; Exit 1 }
}
if (-not (Test-Path $mpOps)) {
    Log "mp-ops.exe not found - building release binary" "WARN"
    Invoke-Build
} else {
    # Usage text goes to stderr; a nonzero exit would otherwise throw under
    # ErrorActionPreference=Stop. Toggle to Continue just for the probe.
    $ErrorActionPreference = "Continue"
    $usage = (& $mpOps 2>&1 | Out-String)
    $ErrorActionPreference = "Stop"
    if ($usage -notmatch "scorecard" -or $usage -notmatch "promote") {
        Log "mp-ops.exe is stale (missing scorecard/promote subcommands) - rebuilding" "WARN"
        Invoke-Build
    } else {
        # Schema staleness guard (audit 08-08): the subcommand probe cannot see
        # a schema-version mismatch - an older mp-ops reads schema-v3 raw logs
        # as unreadable/malformed (which audits as non-promotable, not an
        # error, silently blocking the gate).  Rebuild whenever mp-ops
        # predates the newest raw recording.
        $newestRaw = Get-ChildItem (Join-Path $root "data\raw") -Filter "*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if ($newestRaw -and (Get-Item $mpOps).LastWriteTime -lt $newestRaw.LastWriteTime) {
            Log "mp-ops.exe predates the newest raw recording - rebuilding (schema staleness guard)" "WARN"
            Invoke-Build
        }
    }
}

# ---- 1. scorecard (silence tracing: env-filter respects RUST_LOG=off) ------
$requireArgs = @()
foreach ($stream in $RequiredStreams) { $requireArgs += "--require-stream"; $requireArgs += $stream }
# COL-29 (spec 024): venues with a liquidation source must also show the
# `liquidation` stream on every recorded day. Venue-scoped so Hyperliquid
# (no native liq stream by design) is unaffected. Bybit delivers it via its
# public WS liquidation topic (the live leg). Binance's REST allForceOrders
# leg is USER_DATA — only delivers once MP_BINANCE_API_KEY/SECRET exist
# (dead-until-creds); the requirement is still correct either way: a Binance
# recording without liquidations is not promotable.
$liquidationVenues = @("binance", "bybit")
foreach ($rec in $Recordings) {
    $venue = ($rec -split ':')[0]
    if ($venue -in $liquidationVenues) {
        $requireArgs += "--require-stream"; $requireArgs += "$venue`:liquidation"
    }
}
$required = @()
foreach ($rec in $Recordings) { $required += "--required"; $required += $rec }

Log "Running mp-ops scorecard --date $dateDashed ..."
$scoreArgs = @("scorecard", "--date", $dateDashed) + $required + $requireArgs
if ($env:ZERO_COST -eq "1") {
    $scoreArgs += "--zero-cost"
}
$env:RUST_LOG = "off"
Push-Location $root
try {
    $scoreResult = Invoke-Native -FilePath $mpOps -Arguments $scoreArgs
} finally {
    Pop-Location
    Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
}
$scoreOut = $scoreResult[0]; $scoreExit = $scoreResult[1]
if ($scoreExit -ne 0) { Log "mp-ops scorecard failed (exit $scoreExit): $scoreOut" "ERROR"; Exit 1 }

# Parse the JSON: mp-ops prints only the pretty JSON when tracing is off.
$card = $scoreOut | Out-String | ConvertFrom-Json
if ($null -eq $card.promotable) {
    Log "scorecard output did not parse as expected: $scoreOut" "ERROR"
    Exit 1
}
New-Item -ItemType Directory -Path $scoreDir -Force | Out-Null
$cardPath = Join-Path $scoreDir "$dateDashed.json"
$scoreOut | Out-String | Set-Content -Path $cardPath -Encoding UTF8
Log "Scorecard archived: $cardPath (promotable=$($card.promotable))"

$promotable = $card.promotable

# ---- 1.5 decision determinism check (spec 018 MOD-9..11) ---------------------
# Replay yesterday's recorded session through the PRODUCTION runtime (features
# -> strategy -> risk, SIM-5) and require the decision log to be byte-identical
# across two fresh runs. The promotion gate reads the artifact this writes
# (data/scorecards/<date>.determinism.json): a window day without a PASSING
# artifact does not promote, and a divergence is determinism-diff (P2,
# ops/runbooks/determinism-diff.md). Fail-closed: on divergence the run exits 1
# BEFORE any cold writes. Same build/staleness discipline as mp-ops above.
$detBin = Join-Path $root "target\release\mp-determinism.exe"
if (-not (Test-Path $detBin)) {
    Log "mp-determinism.exe not found - building release binary" "WARN"
    Invoke-Build-Determinism
} else {
    $newestRaw = Get-ChildItem (Join-Path $root "data\raw") -Filter "*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($newestRaw -and (Get-Item $detBin).LastWriteTime -lt $newestRaw.LastWriteTime) {
        Log "mp-determinism.exe predates the newest raw recording - rebuilding (schema staleness guard)" "WARN"
        Invoke-Build-Determinism
    }
}
Log "Running mp-determinism --date $dateDashed ..."
# Deferred-input guard (2026-08-25): a raw file still in flight from the
# overnight drain made mp-determinism exit 2 on a missing file, which logged
# as a scary "FAILED - promotion blocked" while the truth was "inputs not
# here YET" (the drain was landing files as the gate ran). Check presence
# first: a missing input DEFERS the artifact (the backfill pass below heals
# it once landed); only an actual replay divergence is determinism-diff.
# Deferral still exits non-zero AFTER verdict+telegram so Task Scheduler
# flags the run without blinding the streak line.
$detDeferred = $false
foreach ($rec in $Recordings) {
    $p = $rec -split ':'
    $rawPath = Join-Path $root "data\raw\${dateFlat}_$($p[0])_$($p[1]).log"
    if (-not (Test-Path -LiteralPath $rawPath)) {
        Log "Determinism DEFERRED for ${dateDashed}: required input not landed: $($p[0])/$($p[1]) (backfill will retry once the drain lands it)" "WARN"
        $detDeferred = $true
        break
    }
}
# Pin the replay config (sim/determinism.toml) so the artifact's strategy +
# seed are the reviewed values, not an implicit default (MOD-10).
$detCfg = Join-Path $root "sim\determinism.toml"
$detArgs = @("--date", $dateDashed) + $required + @("--config", $detCfg, "--write")
if (-not $detDeferred) {
$env:RUST_LOG = "off"
Push-Location $root
try {
    $detResult = Invoke-Native -FilePath $detBin -Arguments $detArgs
} finally {
    Pop-Location
    Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
}
$detOut = $detResult[0]; $detExit = $detResult[1]
if ($detExit -ne 0) {
    Log "Determinism check FAILED for $dateDashed (exit $detExit) - determinism-diff, promotion blocked. $detOut" "ERROR"
    Exit 1
}
Log "Determinism check: passed for $dateDashed"
} else {
    Log "Determinism run skipped for ${dateDashed} (deferred inputs)" "WARN"
}

# ---- 1.6 backfill pass (2026-08-25): heal late-landed days ------------------
# The gate is only as continuous as its scorecards. A day whose raw files
# landed AFTER that morning's run (slow-link drain backlog) used to stay
# unscored or stuck dirty forever - a permanent streak break caused by
# logistics, not data quality; likewise a promotable day whose determinism
# artifact was never written silently blocks MOD-9 later (08-19/20/21).
# Walk backwards over trailing closed days and:
#   - re-score any day whose inputs are complete (missing card, OR an
#     archived card that is no longer promotable but whose sources changed -
#     the --reuse-unchanged cache makes unchanged days return instantly, so
#     only genuinely re-auditable days cost a full pass);
#   - write the determinism artifact for any PROMOTABLE scored day missing
#     one.
# Bounded (14 days), idempotent, never touches today's open files. A failure
# logs WARN and moves on: backfill is opportunistic repair - the main path
# above stays authoritative, and nothing here can mark a clean day dirty
# that the auditor itself does not.
$backfillScored = 0; $backfillDet = 0
for ($i = 2; $i -le 15; $i++) {
    try {
        $bfDate = (Get-Date).ToUniversalTime().Date.AddDays(-$i)
        $bfDash = $bfDate.ToString("yyyy-MM-dd"); $bfFlat = $bfDate.ToString("yyyyMMdd")
        # Inputs complete? A still-unlanded day stays pending - skip quietly.
        $inputsComplete = $true
        foreach ($rec in $Recordings) {
            $p = $rec -split ':'
            if (-not (Test-Path -LiteralPath (Join-Path $root "data\raw\${bfFlat}_$($p[0])_$($p[1]).log"))) { $inputsComplete = $false; break }
        }
        if (-not $inputsComplete) { continue }
        $cardFile = Join-Path $scoreDir "$bfDash.json"
        $needScore = $true; $wasPromotable = $false
        if (Test-Path -LiteralPath $cardFile) {
            $old = Get-Content -LiteralPath $cardFile -Raw | ConvertFrom-Json
            $wasPromotable = [bool]$old.promotable
            # Re-score only when the archived day is not promotable (it may
            # have been poisoned by a mid-gate landing); promotable days are
            # final unless their sources changed, which --reuse-unchanged
            # makes free to check anyway via the scorecard call below.
            $needScore = -not $wasPromotable
        }
        if ($needScore) {
            Log "Backfill: scoring late/changed day ${bfDash} ..."
            $env:RUST_LOG = "off"
            Push-Location $root
            try {
                $bfArgs = @("scorecard", "--date", $bfDash, "--reuse-unchanged") + $required + $requireArgs
                $bfRes = Invoke-Native -FilePath $mpOps -Arguments $bfArgs
                if ($bfRes[1] -eq 0) {
                    $bfOut = $bfRes[0] | Out-String
                    $bfCard = $bfOut | ConvertFrom-Json
                    if ($null -ne $bfCard.promotable) {
                        Set-Content -Path $cardFile -Value $bfOut -Encoding UTF8
                        $wasPromotable = [bool]$bfCard.promotable
                        $backfillScored++
                        Log ("Backfill scorecard {0}: promotable={1}" -f $bfDash, $wasPromotable)
                    } else {
                        Log "Backfill scorecard ${bfDash}: unparseable output, skipped" "WARN"
                    }
                } else {
                    Log "Backfill scorecard ${bfDash} failed (exit $($bfRes[1]))" "WARN"
                }
            } finally {
                Pop-Location
                Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
            }
        }
        if ($wasPromotable -and -not (Test-Path -LiteralPath (Join-Path $scoreDir "$bfDash.determinism.json"))) {
            Log "Backfill: writing missing determinism artifact for ${bfDash} ..."
            $env:RUST_LOG = "off"
            Push-Location $root
            try {
                $bfDet = @("--date", $bfDash) + $required + @("--config", $detCfg, "--write")
                $bfDetRes = Invoke-Native -FilePath $detBin -Arguments $bfDet
                if ($bfDetRes[1] -eq 0) { $backfillDet++; Log "Backfill determinism ${bfDash}: passed" }
                else { Log "Backfill determinism ${bfDash} failed (exit $($bfDetRes[1]) - inputs may predate the replay config)" "WARN" }
            } finally {
                Pop-Location
                Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
            }
        }
    } catch {
        Log "Backfill iteration failed: $_" "WARN"
    }
}
if ($backfillScored -gt 0 -or $backfillDet -gt 0) {
    Log "Backfill complete: $($backfillScored) day(s) scored, $($backfillDet) artifact(s) written"
}

# ---- 2. promotion streak verdict (before any exit, so the daily N/7 line
#         prints even on dirty days - which is when it matters most) ---------
# The verdict comes from the Rust gate (`mp-ops promote` reads
# data/scorecards/*.json and runs the real check_promotion) - single source of
# truth, no PS-side shadow computation.
$promoteArgs = @("promote", "--scorecards-dir", $scoreDir) + $required
if ($env:ZERO_COST -eq "1") {
    $promoteArgs += "--zero-cost"
}
$env:RUST_LOG = "off"
Push-Location $root
try {
    $promoteResult = Invoke-Native -FilePath $mpOps -Arguments $promoteArgs
} finally {
    Pop-Location
    Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
}
$promoteOut = $promoteResult[0]; $promoteExit = $promoteResult[1]
if ($promoteExit -eq 0) {
    $pv = $promoteOut | Out-String | ConvertFrom-Json
    if ($pv.promoted) {
        Log "PROMOTION GATE: PASSED - $($pv.consecutive_clean) consecutive clean days (required $($pv.required)). Window $($pv.window_start)..$($pv.window_end)!" "WARN"
    } else {
        # The `why` (2026-08-12): a full streak can be held back by the
        # Phase-0 window condition even with no clean-day break - name the
        # burst days from the verdict's burst_days field when that is the case.
        $burstDates = @($pv.burst_days) | ForEach-Object { $_.date }
        if ($burstDates.Count -gt 0) {
            $why = "window carries stale bursts on $($burstDates -join ', ')"
        } elseif ($null -ne $pv.first_failure) {
            $why = "first break $($pv.first_failure)"
        } else {
            $why = "no clean days yet"
        }
        Log "Promotion gate: $($pv.consecutive_clean) consecutive clean day(s), required $($pv.required) ($why)"
    }
} else {
    # No scorecards yet (first run) - nothing to compute; not an error.
    Log "Promotion gate: no scorecards yet - streak starts with the first archived scorecard."
}

# ---- 2.1 Telegram verdict (2026-08-10): a non-promotable day must reach
#         the owner's phone, not just exit 1 for Task Scheduler. Best-effort
#         by design: a delivery failure is logged (WARN) and does NOT change
#         the pipeline's exit code - the scorecard file + pipeline.log remain
#         the evidence, and a dead alert channel must never mask the gate.
#         `telegram-send` breaks through quiet hours by construction: the
#         pipeline runs at 00:05 UTC (inside 22:00-07:00), so a batched P3
#         would sit in the ledger until the next flush - useless for a
#         verdict that must land now.
$tgDetail = "day ${dateDashed}: $($card.recordings.Count) recording(s)"
if ($null -ne $pv) {
    $tgDetail += " | streak $($pv.consecutive_clean)/$($pv.required)"
}
# Per-recording margin line (2026-08-12): the numeric gate facts that make
# a DIRTY day self-explaining - blocking count, coverage vs the 0.995 bar,
# stale burst count, and the worst single gap in seconds.  `coverage`,
# `stale_bursts` and `worst_gap_ns` are margin fields, never vetoes.
function Get-RecordingLine {
    param($Rec)
    $clean = if ($Rec.clean) { "clean" } else { "DIRTY" }
    $block = if ($null -ne $Rec.blocking_findings) { $Rec.blocking_findings } else { 0 }
    $sb = if ($null -ne $Rec.stale_bursts) { $Rec.stale_bursts } else { 0 }
    $wg = if ($null -ne $Rec.worst_gap_ns) { [Math]::Round($Rec.worst_gap_ns / 1e9) } else { 0 }
    return ("  {0}/{1}: {2} (blocking={3} coverage={4} stale_bursts={5} worst_gap_s={6})" -f $Rec.venue, $Rec.symbol, $clean, $block, $Rec.coverage, $sb, $wg)
}
foreach ($rec in $card.recordings) {
    $tgDetail += "`n" + (Get-RecordingLine $rec)
}
$tgSeverity = if ($promotable) { "p3" } else { "p2" }
$tgArgs = @("telegram-send", "--id", "daily-pipeline", "--detail", $tgDetail, "--severity", $tgSeverity)
$tgResult = Invoke-Native -FilePath $mpOps -Arguments $tgArgs
# NOTE: materialize the trimmed output first - inside a PS string,
# `$($x | Out-String).Trim()` would print the literal text '.Trim()'.
$tgOut = ($tgResult[0] | Out-String).Trim()
if ($tgResult[1] -ne 0) {
    Log "Telegram verdict send failed (exit $($tgResult[1])): $tgOut" "WARN"
} else {
    Log "Telegram verdict sent: $tgOut"
}

# Deferred day (2026-08-25): verdict + telegram are out, so the scheduler
# flag is the only remaining duty - exit non-zero AFTER the streak line, not
# before it. The backfill pass heals this day once the drain lands its files.
if ($detDeferred) {
    Log "Deferred day ${dateDashed}: exiting non-zero (inputs pending); backfill will score it when they land." "WARN"
    Exit 1
}

# ---- 2.2 storage-budget watch (2026-08-14, spec 009 OPS-15) --------------
# The spec 001 appendix's first revisit trigger, wired: projects when
# data/raw growth hits the budget cap (--cap-bytes or MP_STORAGE_BUDGET_BYTES)
# and sends the storage-budget P2 via Telegram. Best-effort by design: a
# missing budget config or a failed send is logged (WARN) and NEVER changes
# the pipeline's exit code - the gate verdict is the pipeline's job. Runs
# even on non-promotable days (this block sits before the promotable exit).
$sbCap = $env:MP_STORAGE_BUDGET_BYTES
if ($sbCap) {
    $sbArgs = @("storage-budget", "--dir", (Join-Path $root "data\raw"), "--cap-bytes", $sbCap, "--telegram")
    # Drain-manifest scan (2026-08-16): the watch also fires the storage-budget
    # P2 when the relay is silently holding files (action=landed but release
    # not in {released, no_release}) - a byte-verified file the drain never
    # released. Windows-side artifact (the VPS timer has no manifest); passed
    # only when the manifest exists (no drain runs yet = nothing to scan).
    $sbManifest = Join-Path $root "data\vps_drain_manifest.jsonl"
    if (Test-Path $sbManifest) { $sbArgs += @("--manifest", $sbManifest) }
    $sbResult = Invoke-Native -FilePath $mpOps -Arguments $sbArgs
    $sbOut = ($sbResult[0] | Out-String).Trim()
    if ($sbResult[1] -ne 0) {
        Log "storage-budget check failed (exit $($sbResult[1])): $sbOut" "WARN"
    } else {
        Log "storage-budget: $sbOut"
    }
} else {
    Log "storage-budget: skipped (no MP_STORAGE_BUDGET_BYTES - set the budget to arm the OPS-15 watch)" "WARN"
}

if (-not $promotable) {
    foreach ($rec in $card.recordings) {
        Log (Get-RecordingLine $rec)
    }
    Log "NOT promotable - day $dateDashed fails the INT-4 gate. No cold writes." "WARN"
    # Exit 1 so Task Scheduler records a failed run (the outage is worth a flag).
    Exit 1
}

# ---- 2.5 materialize features for the approved day (spec 016 Phase 2) -------
# The feature store is the research substrate: it is only written for days that
# passed the INT-4 gate (dirty days would bake gaps/staleness into features).
# Log set = every required recording + the whale-positions census for venues
# that have one (spec 028 feeds whale.net/delta). Engine provenance is the git
# sha of the tree that ran the pipeline (FEA-6 footer).
if (-not $SkipMaterialize) {
    $matBin = Join-Path $root "target\release\mp-materialize.exe"
    $newestRaw = Get-ChildItem (Join-Path $root "data\raw") -Filter "*.log" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not (Test-Path $matBin) -or ($newestRaw -and (Get-Item $matBin -ErrorAction SilentlyContinue).LastWriteTime -lt $newestRaw.LastWriteTime)) {
        Log "mp-materialize.exe missing or stale - building release binary" "WARN"
        Invoke-Build-Materialize
    }
    $cfgPath = Join-Path $root "features\features.toml"
    $matArgs = @()
    $matLogCount = 0
    $seenVenues = @{}
    foreach ($rec in $Recordings) {
        $p = $rec.Split(":")
        $logPath = Join-Path $root ("data\raw\{0}_{1}_{2}.log" -f $dateFlat, $p[0], $p[1])
        if (Test-Path $logPath) {
            $matArgs += "--log"; $matArgs += $logPath; $matLogCount++
        }
        if (-not $seenVenues.ContainsKey($p[0])) {
            $seenVenues[$p[0]] = $true
            $posPath = Join-Path $root ("data\raw\{0}_{1}_positions.log" -f $dateFlat, $p[0])
            if (Test-Path $posPath) {
                $matArgs += "--log"; $matArgs += $posPath; $matLogCount++
            }
        }
    }
    $gitSha = "unknown"
    # Audit 2026-08-17: `git rev-parse HEAD 2>&1` under EAP=Stop turns a git
    # failure (non-repo cwd, no git on PATH, corrupt HEAD) into an unhandled
    # terminating error mid-materialize. Route through Invoke-Native (EAP is
    # Continue inside) and default to "unknown" on failure; run from $root so
    # the sha resolves to this tree.
    Push-Location $root
    try {
        $gitRes = Invoke-Native -FilePath "git" -Arguments @("rev-parse", "HEAD")
    } finally {
        Pop-Location
    }
    $gitOut = ($gitRes[0] | Out-String).Trim()
    if ($gitRes[1] -eq 0 -and $gitOut) { $gitSha = $gitOut }
    $matArgs += "--config"; $matArgs += $cfgPath
    $matArgs += "--out";  $matArgs += (Join-Path $root "data\features")
    $matArgs += "--git-sha"; $matArgs += $gitSha

    if ($matLogCount -eq 0) {
        Log "materialize: no raw logs found for $dateDashed - skipping" "WARN"
    } else {
        Log "Running mp-materialize over $matLogCount log(s) for $dateDashed ..."
        $env:RUST_LOG = "off"
        Push-Location $root
        try {
            $matResult = Invoke-Native -FilePath $matBin -Arguments $matArgs
        } finally {
            Pop-Location
            Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
        }
        $matOut = $matResult[0]; $matExit = $matResult[1]
        # LAB-6: materialize symbols_hash conflict is P2, not pipeline abort.
        # The scorecard is already archived; overwrite refusal (W-6) stays, but
        # the pipeline continues to compaction. A non-conflict failure is still
        # a hard stop.
        $matLog = ($matOut | Out-String).Trim()
        if ($matExit -ne 0) {
            if ($matLog -match 'symbols snapshot hash collision') {
                Log "LAB-6: materialize hash conflict (P2, pipeline continues): $matLog" "WARN"
            } else {
                Log "mp-materialize failed (exit $matExit): $matLog" "ERROR"
                Exit 1
            }
        }
        if ($matLog) { Log "  $matLog" }
    }
} else {
    Log "SkipMaterialize set - no feature-store writes."
}

# ---- 2.7 Hot-tier retention enforcement (docs/RETENTION_POLICY.md) ---------
# Under ZERO_COST=1, raw tick data older than 14 days MAY be deleted — but
# ONLY through the hash-verified `mp-ops prune` gate (AUDIT-2026-09-02
# A-1/A-12): removed only when (a) it is a gate recording, (b) its day had a
# PASSING scorecard (INT-4), and (c) `mp-ops prune` confirms compaction
# manifest + Parquet whose source_log_hash still matches the CURRENT raw bytes
# (W-6/C-2). Deletions are journaled to data\retention_delete_manifest.jsonl.
# A day that never compacted is NEVER auto-deleted. Compacted Parquet + features always kept.
if ($env:ZERO_COST -eq "1") {
    if (-not $env:RETENTION_DAYS) { $env:RETENTION_DAYS = "14" }
    $retentionDays = [int]$env:RETENTION_DAYS
    $rawDir = Join-Path $root "data\raw"
    $cutoff = (Get-Date).ToUniversalTime().AddDays(-$retentionDays)
    Log "Hot-tier retention: scanning raw logs older than $retentionDays days (before $($cutoff.ToString('yyyy-MM-dd')) UTC) - hash-verified gate only (A-1)"
    $deleted = 0; $refused = 0; $skipped = 0
    $candidates = @()
    foreach ($rec in $Recordings) {
        $p = $rec -split ':'
        $v = $p[0]; $s = $p[1]
        Get-ChildItem -Path $rawDir -Filter "*_${v}_${s}.log" -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^\d{8}_' -and $_.LastWriteTime.ToUniversalTime() -lt $cutoff } |
            ForEach-Object { $candidates += ,@{ File = $_; Rec = $rec } }
    }
    foreach ($c in $candidates) {
        $base = $c.File.Name
        $dateFlat = $base.Substring(0,8)
        $dash = "{0}-{1}-{2}" -f $dateFlat.Substring(0,4), $dateFlat.Substring(4,2), $dateFlat.Substring(6,2)
        $p = $c.Rec -split ':'
        $cardFile = Join-Path $scoreDir "$dash.json"
        # INT-4 gate: never auto-delete a day that did not pass the gate.

        if (-not (Test-Path -LiteralPath $cardFile)) {

            Log "Retention: SKIP $base (no scorecard $dash.json - unclean/never-compacted day is kept)" "WARN"
            $skipped++; continue
        }
        $card = Get-Content -LiteralPath $cardFile -Raw | ConvertFrom-Json
        if (-not [bool]$card.promotable) {

            Log "Retention: SKIP $base (day $dash did not pass the gate)" "WARN"
            $skipped++; continue
        }
        $pruneArgs = @("prune", "--date", $dash, "--venue", $p[0], "--symbol", $p[1])
        $env:RUST_LOG = "off"
        Push-Location $root
        try {
            $pruneRes = Invoke-Native -FilePath $mpOps -Arguments $pruneArgs
        } finally {
            Pop-Location
            Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
        }
        $pruneOut = ($pruneRes[0] | Out-String).Trim()
        if ($pruneRes[1] -ne 0) {
            Log "Retention: REFUSED $base (compact proof incomplete): $pruneOut" "WARN"
            $refused++
        } else {
            Log "Retention: pruned $base (hash-verified): $pruneOut"
            $deleted++
        }
    }
    Log "Hot-tier retention: deleted $deleted verified raw log(s), $refused refused (kept), $skipped skipped (ungated)"
}

# ---- 3. compact through the INT-4 verified gate -----------------------------
if (-not $SkipCompact) {
    foreach ($rec in $Recordings) {
        $parts = $rec.Split(":")
        $venue = $parts[0]; $sym = $parts[1]
        Log "Compacting $venue/$sym (INT-4 verified gate)..."
        $compactArgs = @("compact", "--date", $dateDashed, "--venue", $venue, "--symbol", $sym) + $requireArgs
        $env:RUST_LOG = "off"
        Push-Location $root
        try {
            $compactResult = Invoke-Native -FilePath $mpOps -Arguments $compactArgs
        } finally {
            Pop-Location
            Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
        }
        $compactOut = $compactResult[0]; $compactExit = $compactResult[1]
        if ($compactExit -ne 0) { Log "compact failed for $venue/$sym (exit $compactExit): $compactOut" "ERROR"; Exit 1 }
        $compactLog = ($compactOut | Out-String).Trim()
        Log "  $compactLog"
    }
} else {
    Log "SkipCompact set - no cold writes."
}

# ---- 4. paper rehearsal (spec 051, PAP-1) ----------------------------------
# Run AFTER the scorecard attempt (even if compact failed, if the raw log
# decodes). The paper script handles its own error reporting and Telegram.
$paperScript = Join-Path $root "ops\scripts\daily_paper.ps1"
if (Test-Path $paperScript) {
    Log "Running paper rehearsal for $dateDashed (PAP-1)..."
    $paperArgs = @("-ExecutionPolicy", "Bypass", "-File", $paperScript, "-Date", $dateDashed)
    $paperResult = Invoke-Native -FilePath "powershell.exe" -Arguments $paperArgs
    $paperOut = ($paperResult[0] | Out-String).Trim()
    if ($paperResult[1] -ne 0) {
        Log "Paper rehearsal exit $($paperResult[1]): $paperOut" "WARN"
    } else {
        Log "Paper rehearsal completed for $dateDashed"
    }
} else {
    Log "Paper script not found at $paperScript - skipping paper rehearsal" "WARN"
}

Log "Daily pipeline complete: $dateDashed"
