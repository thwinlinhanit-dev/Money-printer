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
#   3. promotable  -> `mp-ops compact` each recording through the INT-4
#      verified gate (quarantined logs are refused before cold writes).
#   4. Print the promotion streak verdict (longest run of promotable days
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
    [switch]$SkipCompact,          # audit + scorecard + verdict, no cold writes
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

# 0. PD guardrails (audit 08-04 #6): the bash guardrails cannot run on this
#    native Windows host, so the PowerShell port runs first - mechanical
#    rulebook enforcement (PD-1..4, W-7, CONV-21, OPS-4) before any number
#    from the scorecard is trusted. Fail-closed: a guardrails violation stops
#    the pipeline with a distinct, grep-able error.
$guardrails = Join-Path $root "ops\ci\guardrails.ps1"
if (Test-Path $guardrails) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $guardrails
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[!!] guardrails failed - the tree violates the rulebook (PD-1..4/W-7); fix before trusting today's scorecard" -ForegroundColor Red
        Exit 1
    }
    Write-Host "[ok] guardrails: all checks passed" -ForegroundColor Green
}
$logFile    = Join-Path $scoreDir "pipeline.log"
$mpOps      = Join-Path $root "target\release\mp-ops.exe"

# Recordings default: empty = load the single source of truth (ops/core_symbols.txt)
# as `venue:symbol` lines (a plain SYMBOL line means binance:<sym>) - so the
# scorecard's required set ALWAYS matches what the watchdog records (no silent
# under-scoping of the promotion gate). Falls back to hyperliquid:BTC+ETH when absent.
if ($Recordings.Count -eq 0) {
    $coreFile = Join-Path $root "ops\core_symbols.txt"
    if (Test-Path $coreFile) {
        # Trim BEFORE filtering so a line with leading whitespace is not
        # silently dropped (audit 08-08).
        $coreLines = @(Get-Content $coreFile | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '^[A-Za-z0-9]' -and $_ -notmatch '^\s*#' } | Where-Object { $_ -ne '' })
        $Recordings = @()
        foreach ($line in $coreLines) {
            if ($line -match '^([a-z][a-z0-9]*):([A-Z0-9]{2,20})$') { $Recordings += "$($Matches[1]):$($Matches[2])" }
            elseif ($line -match '^[A-Z0-9]{2,20}$') { $Recordings += "binance:$line" }
        }
    }
    if ($Recordings.Count -eq 0) { $Recordings = @("hyperliquid:BTC", "hyperliquid:ETH") }
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
    Push-Location $root
    cargo build -p mp-ops --release 2>&1 | Where-Object { $_ -match "error|Finished" } | ForEach-Object { Log $_ "WARN" }
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { Log "mp-ops build failed (exit $code)" "ERROR"; Exit 1 }
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
        $why = if ($null -ne $pv.first_failure) { "first break $($pv.first_failure)" } else { "no clean days yet" }
        Log "Promotion gate: $($pv.consecutive_clean) consecutive clean day(s), required $($pv.required) ($why)"
    }
} else {
    # No scorecards yet (first run) - nothing to compute; not an error.
    Log "Promotion gate: no scorecards yet - streak starts with the first archived scorecard."
}

if (-not $promotable) {
    foreach ($rec in $card.recordings) {
        $clean = if ($rec.clean) { "clean" } else { "DIRTY" }
        $block = if ($null -ne $rec.blocking_findings) { $rec.blocking_findings } else { 0 }
        Log ("  {0}/{1}: {2} (blocking={3})" -f $rec.venue, $rec.symbol, $clean, $block)
    }
    Log "NOT promotable - day $dateDashed fails the INT-4 gate. No cold writes." "WARN"
    # Exit 1 so Task Scheduler records a failed run (the outage is worth a flag).
    Exit 1
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
