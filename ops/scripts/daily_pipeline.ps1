# daily_pipeline.ps1 - Daily integrity gate -> scorecard -> compaction (SPEC-024).
# Windows port of ops/scripts/daily_maintenance.sh, so the promotion gate can
# advance on the native Windows host that actually runs the collectors.
#
# At 00:05 UTC the collectors have rotated yesterday's log (rotation happens at
# UTC midnight), so the 00:05 window is the first moment yesterday is a closed,
# auditable file.
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
#   .\ops\scripts\daily_pipeline.ps1 -RegisterTask  # schedule at 00:05 UTC daily
#   .\ops\scripts\daily_pipeline.ps1 -SkipCompact   # scorecard + verdict only

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
    # gate stream.  Re-add "liquidation" when a venue with a native liq
    # stream (e.g. Bybit, Phase 2) joins the required set.
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
if (Test-Path $guardrails) {
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
}

# ---- task registration ------------------------------------------------------
if ($RegisterTask) {
    # Schedule at the LOCAL wall-clock time that corresponds to 00:05 UTC, so
    # the task really fires right after the collector's UTC-midnight rotation.
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddMinutes(5), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # -At wants a DateTime (the date is ignored); passing the full local time
    # keeps the trigger pinned to the local wall-clock of 00:05 UTC.
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
        Write-Host "     DST note: after a clock change, re-run -RegisterTask to keep the 00:05 UTC window." -ForegroundColor Gray
        Write-Host "     The script self-guards: it only runs the audit when UTC hour is 0-2." -ForegroundColor Gray
    } catch {
        Write-Host "[!!] Register-ScheduledTask failed: $($_.Exception.Message)" -ForegroundColor Red
        Exit 1
    }
    Exit 0
}

# ---- dead-man for the gate itself (OPS-17, runbook pipeline-stale) ----------
# The 00:05 gate is one scheduled task; if THAT job silently dies, the streak
# goes blind (blueprint failure-mode #6). Register a separate 00:15 UTC task
# running `mp-ops pipeline-stale` — a P1 when yesterday's scorecard has not
# landed. It is deliberately NOT part of this script: a watchdog must be
# independent of the thing it watches.
if ($RegisterStaleTask) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddMinutes(15), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $staleArgs = "-ExecutionPolicy Bypass -WindowStyle Hidden -Command & `"$mpOps`" pipeline-stale --telegram --webhook *>> `"$logFile`""
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

# ---- DST / clock-drift guard ------------------------------------------------
# The daily trigger fires at a fixed local wall-clock; after a DST shift that
# can land an hour away from 00:05 UTC. Only audit when we're within the UTC
# midnight window; otherwise no-op (the next day's run catches up).
# An explicit -Date is a manual/backfill run - the guard does not apply.
$utcHour = (Get-Date).ToUniversalTime().Hour
if (-not $Date -and $utcHour -gt 2) {
    Log "Skipped: UTC hour $utcHour is outside the 00:00-02:00 window (clock drift / DST?). Re-run -RegisterTask after a DST change." "WARN"
    Exit 0
}

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
$required = @()
foreach ($rec in $Recordings) { $required += "--required"; $required += $rec }

Log "Running mp-ops scorecard --date $dateDashed ..."
$scoreArgs = @("scorecard", "--date", $dateDashed) + $required + $requireArgs
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
# Pin the replay config (sim/determinism.toml) so the artifact's strategy +
# seed are the reviewed values, not an implicit default (MOD-10).
$detCfg = Join-Path $root "sim\determinism.toml"
$detArgs = @("--date", $dateDashed) + $required + @("--config", $detCfg, "--write")
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

# ---- 2. promotion streak verdict (before any exit, so the daily N/7 line
#         prints even on dirty days - which is when it matters most) ---------
# The verdict comes from the Rust gate (`mp-ops promote` reads
# data/scorecards/*.json and runs the real check_promotion) - single source of
# truth, no PS-side shadow computation.
$promoteArgs = @("promote", "--scorecards-dir", $scoreDir) + $required
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
    $shaOut = (& git rev-parse HEAD 2>&1 | Out-String)
    if ($LASTEXITCODE -eq 0 -and $shaOut.Trim()) { $gitSha = $shaOut.Trim() }
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
        if ($matExit -ne 0) { Log "mp-materialize failed (exit $matExit): $matOut" "ERROR"; Exit 1 }
        Log "  $($matOut | Out-String).Trim()"
    }
} else {
    Log "SkipMaterialize set - no feature-store writes."
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
        Log "  $($compactOut | Out-String).Trim()"
    }
} else {
    Log "SkipCompact set - no cold writes."
}

Log "Daily pipeline complete: $dateDashed"
