# daily_paper.ps1 - Closed-day paper rehearsal (spec 051, PAP-1..PAP-10).
#
# Runs a paper session over the previous UTC day's drained/complete Hyperliquid
# log using sim fills only (PAP-2). No OMS venue adapter is constructed.
#
# The task MoneyPrinterPaper is scheduled AFTER the scorecard attempt
# (PAP-1: even if compact failed, if the raw log decodes).
#
# Usage:
#   .\ops\scripts\daily_paper.ps1                # run for yesterday (UTC)
#   .\ops\scripts\daily_paper.ps1 -Date 2026-08-30   # explicit date
#   .\ops\scripts\daily_paper.ps1 -RegisterTask  # schedule daily
#
# Exit codes:
#   0 = paper session completed (or skipped due to latch/mode)
#   1 = paper session faulted
#   2 = config error (missing binary, missing log, etc.)

param(
    [string]$Date,                 # YYYY-MM-DD or YYYYMMDD (default: yesterday UTC)
    [switch]$RegisterTask,         # register the MoneyPrinterPaper task
    [string]$Strategy = "",        # paper strategy id (default: swing-range-reclaim-v1)
    [int]$Seed = 42,               # deterministic seed (fixed, recorded in journal)
    [string]$ScorecardsDir = "",   # override scorecards dir
    [string]$PipelineLog = "",     # override pipeline log
    [string]$RunsDir = ""          # override runs dir
)

$ErrorActionPreference = "Stop"

# ---- workspace root resolution ------------------------------------------------
$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
while ($true) {
    $manifest = Join-Path $root "Cargo.toml"
    if ((Test-Path $manifest) -and (Select-String -Path $manifest -Pattern '^\[workspace\]' -Quiet)) { break }
    $parent = Split-Path $root -Parent
    if ($parent -eq $root) { Write-Host "[!!] workspace root not found from $PSScriptRoot" -ForegroundColor Red; Exit 2 }
    $root = $parent
}

$TaskName = "MoneyPrinterPaper"
$scoreDir = if ($ScorecardsDir) { $ScorecardsDir } else { Join-Path $root "data\scorecards" }
$runsDir = if ($RunsDir) { $RunsDir } else { Join-Path $root "runs" }
$logFile = Join-Path $scoreDir "paper.log"

# ---- PAP-10: refuse MONEY_PRINTER_MODE=live during paper ---------------------
# If the env says "live", sleep and fire a P1 — paper MUST NOT run in live mode.
$envMode = $env:MONEY_PRINTER_MODE
if ($envMode -eq "live") {
    $ts = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $msg = "[$ts][ERROR] PAP-10: MONEY_PRINTER_MODE=live during paper task - refusing to run, firing P1"
    New-Item -ItemType Directory -Path $scoreDir -Force | Out-Null
    Add-Content -Path $logFile -Value $msg -Encoding UTF8
    Write-Host $msg -ForegroundColor Red
    Exit 1
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

function Invoke-Native {
    param([string]$FilePath, [string[]]$Arguments)
    $ErrorActionPreference = "Continue"
    $out = & $FilePath @Arguments 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = "Stop"
    return ,@($out, $code)
}

# ---- task registration --------------------------------------------------------
if ($RegisterTask) {
    # PAP-1: schedule AFTER the daily pipeline (07:30 UTC), at 08:30 UTC.
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(8).AddMinutes(30), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): pin the first trigger to tomorrow
    # when registering at/after the target time (see daily_pipeline.ps1
    # -RegisterTask; MoneyPrinterDataBackup 00:07Z launch failure, 0xFFFD0000).
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $action  = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 30) `
        -RestartCount 2 -RestartInterval (New-TimeSpan -Minutes 5)
    try {
        Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
            -Settings $settings -User $env:USERNAME -Force -ErrorAction Stop | Out-Null
        Write-Host "[OK] Registered $TaskName daily at $($localAt.ToString('HH:mm')) local (=$($utcTarget.ToString('HH:mm')) UTC)." -ForegroundColor Green
    } catch {
        Write-Host "[!!] Register-ScheduledTask failed: $($_.Exception.Message)" -ForegroundColor Red
        Exit 1
    }
    Exit 0
}

# ---- date resolution ----------------------------------------------------------
if (-not $Date) {
    $Date = (Get-Date).ToUniversalTime().AddDays(-1).ToString("yyyyMMdd")
} else {
    Log "Manual run: explicit -Date $Date"
}
$dateFlat = $Date.Replace("-", "")
if ($dateFlat.Length -ne 8) { Log "Invalid date: $Date (expected YYYYMMDD or YYYY-MM-DD)" "ERROR"; Exit 2 }
$dateDashed = "{0}-{1}-{2}" -f $dateFlat.Substring(0,4), $dateFlat.Substring(4,2), $dateFlat.Substring(6,2)

# ---- PAP-4: load kill-latch (fail-closed) ------------------------------------
$latchPath = $env:MP_OPS_KILL_LATCH
if (-not $latchPath) {
    $pd = $env:PROGRAMDATA
    if (-not $pd) { $pd = "C:\ProgramData" }
    $latchPath = Join-Path $pd "money-printer\kill.json"
}
$latched = $false
if (Test-Path $latchPath) {
    try {
        $latchText = Get-Content -Path $latchPath -Raw
        $latch = ConvertFrom-Json $latchText
        if ($latch.scopes -and $latch.scopes.Count -gt 0) {
            $latched = $true
            Log "PAP-4: kill-latch tripped ($($latch.scopes.Count) scope(s)): $($latch.reason) - paper will run with zero intents" "WARN"
        }
    } catch {
        Log "PAP-4: kill-latch file corrupt at $latchPath - treating as latched (fail-closed)" "ERROR"
        $latched = $true
    }
}

# ---- find the raw log for the day --------------------------------------------
# Paper runs on the drained/complete HL log (PAP-1). Primary symbol is BTC
# (spec 051 rehearsal symbol); fall back to ETH — the other Zero-Cost gate
# recording — so a symbol-specific recording gap does not silently skip the
# day (A-12, audit 2026-09-02: a missing log must not masquerade as success).
$rawLog = $null
foreach ($sym in @("BTC", "ETH")) {
    $candidate = Join-Path $root "data\raw\${dateFlat}_hyperliquid_$sym.log"
    if (Test-Path -LiteralPath $candidate) { $rawLog = $candidate; $paperSymbol = $sym; break }
}
if ($null -eq $rawLog) {
    Log "PAP-1: no hyperliquid BTC or ETH raw log found for $dateDashed in $($root)\data\raw - config/data error, FAILING (exit 2; a closed day without a recording is a data hole, not a skip)" "ERROR"
    Exit 2
}
Log "PAP-1: found raw log for ${dateDashed}: $rawLog"

# ---- PAP-3: risk gate configuration ------------------------------------------
# Paper runs on sim fills with the sim's BUILT-IN risk gate (check_funding on
# close, finite RG budgets). No external risk config is wired into
# `mp-sim paper` — do not pretend one is copied or loaded (A-12, audit
# 2026-09-02: the previous comment described a paper-risk.toml copy that
# never happened).
$riskCfg = Join-Path $root "risk\risk.example.toml"
if (-not (Test-Path $riskCfg)) {
    Log "PAP-3: note - $riskCfg not present; sim built-in risk gate defaults apply (no external risk config is wired into paper)" "WARN"
}

# ---- strategy resolution (default: swing-range-reclaim-v1) -------------------
$papStrategy = if ($Strategy) { $Strategy } else { "swing-range-reclaim-v1" }
# PAP-10: never use liq-fade-v1 (SWG-8 frozen).
if ($papStrategy -eq "liq-fade-v1") {
    Log "PAP-10: liq-fade-v1 is frozen (SWG-8) - refusing to use as paper strategy" "ERROR"
    Exit 2
}
# Explicit paper.strategy = "null" is allowed for plumbing; null strategy
# never silently replaces a registered one.
Log "Paper strategy: $papStrategy (seed=$Seed)"

# ---- build mp-sim if missing or stale ----------------------------------------
# The mp-sim package's paper binary is `sim` (sim/src/bin/sim.rs); `mp-sim`
# is the package name, not the artifact name. The old check pointed at
# mp-sim.exe, which NEVER exists - so every rehearsal "built" (a cached
# no-op) and then failed to launch: the perpetual PAP-1 WARN (fix 2026-09-06).
$simBinCandidates = @(
    (Join-Path $root "target\release\sim.exe"),
    (Join-Path $root "target\release\mp-sim.exe")   # fallback if the bin is ever renamed
)
$simBin = $simBinCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $simBin) {
    Log "sim.exe not found - building release binary" "WARN"
    Push-Location $root
    $res = Invoke-Native -FilePath "cargo" -Arguments @("build", "-p", "mp-sim", "--release")
    Pop-Location
    if ($res[1] -ne 0) { Log "mp-sim build failed (exit $($res[1]))" "ERROR"; Exit 2 }
    $simBin = $simBinCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $simBin) {
        Log "mp-sim built but no binary found at $($simBinCandidates -join ' or ')" "ERROR"; Exit 2
    }
}

# ---- run the paper session ----------------------------------------------------
# PAP-4: if latched, run with zero intents (the sim still journals for
# operational visibility). PAP-2: sim fills only, no OMS adapter.
# run_id is generated ONCE here and passed to the sim (--run-id / --runs-dir
# are mandatory; `sim paper` refuses to run without them - fix 2026-09-06,
# the missing args faulted every rehearsal) and reused by the PAP-6 journal
# entry so both records share the identity.
$runId = "paper-${dateFlat}-$(Get-Date -UFormat %s)"
# PAP-12 (spec 054 REL-30): the primary leg records observations nightly —
# write-only recording under the stable `pap1-primary` identity. The research
# corpus grows from the daily schedule (date-partitioned Parquet under one
# fingerprint). Same-date re-runs follow the W-6 guard: identical = no-op,
# divergent = loud fault (never a silent overwrite). Recording does not
# touch the decision path or the primary verdict.
$logArgs = @("paper", "--log", $rawLog, "--strategy", $papStrategy, "--seed", "$Seed", "--run-id", $runId, "--runs-dir", $runsDir,
    "--params-hash", "pap1-primary", "--obs-dir", (Join-Path $root "data\observations"))
if ($latched) {
    $logArgs += @("--zero-intents")
}

Log "Running paper session: $(Split-Path $simBin -Leaf) $($logArgs -join ' ')"
$env:RUST_LOG = "off"
Push-Location $root
try {
    $paperResult = Invoke-Native -FilePath $simBin -Arguments $logArgs
} finally {
    Pop-Location
    Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
}
$paperOut = ($paperResult[0] | Out-String).Trim()
$paperExit = $paperResult[1]

# ---- PAP-6: journal to runs/index.jsonl --------------------------------------
New-Item -ItemType Directory -Path $runsDir -Force | Out-Null
$indexFile = Join-Path $runsDir "index.jsonl"
$journalEntry = [ordered]@{
    run_id    = $runId
    kind      = "paper"
    date      = $dateDashed
    venue     = "hyperliquid"
    symbol    = $paperSymbol
    strategy  = $papStrategy
    seed      = $Seed
    latched   = $latched
    exit_code = $paperExit
    ts_utc    = (Get-Date).ToUniversalTime().ToString("o")
}
# Parse paper output for expectancy and fault count if available.
if ($paperExit -eq 0) {
    if ($paperOut -match 'expectancy[:\s]+([-\d.]+)') {
        $journalEntry["expectancy"] = [double]$Matches[1]
    }
    if ($paperOut -match 'trades[:\s]+(\d+)') {
        $journalEntry["trades"] = [int]$Matches[1]
    }
    if ($paperOut -match 'observations:\s*(\d+) recorded') {
        $journalEntry["observations"] = [int]$Matches[1]
    }
    if ($paperOut -match 'faults[:\s]+(\d+)') {
        $journalEntry["faults"] = [int]$Matches[1]
    }
    $journalEntry["faults"] = if ($journalEntry.Contains("faults")) { $journalEntry["faults"] } else { 0 }
} else {
    $journalEntry["faults"] = 1
    $journalEntry["error"] = $paperOut.Substring(0, [Math]::Min(500, $paperOut.Length))
}
Add-Content -Path $indexFile -Value ($journalEntry | ConvertTo-Json -Compress) -Encoding UTF8
Log "PAP-6: journaled paper run to $indexFile (run_id=$runId)"

# ---- PAP-11: noise-baseline leg (spec 051 + spec 054 REL-32) ------------------
# Run the venue-generic noise control (coinflip-any) over the SAME log and
# seed, with observation recording enabled so the spec 054 gates grade it.
# Expected daily outcome: control FIRES on the hyperliquid log and is REFUSED
# at every horizon (NET_EXPECTANCY_NEGATIVE at minimum). Control GATE PASS at
# any horizon means the gate chain is broken - noise survived the net gate -
# which is a P1 pipeline fault (R-8), not a strategy result. The control leg
# never gates the primary strategy's verdict; it certifies the evaluation
# pipeline itself. Latch-independent: the control grades the PIPELINE, not a
# strategy, and paper sim never touches a live path (PAP-2).
$pap11Fault = $false
$noiseRunId = "${runId}-noise"
$obsDir = Join-Path $root "data\observations"
$noiseArgs = @("paper", "--log", $rawLog, "--strategy", "coinflip-any", "--seed", "$Seed", "--run-id", $noiseRunId, "--runs-dir", $runsDir, "--params-hash", "pap11-noise-baseline", "--obs-dir", $obsDir)

Log "PAP-11: running noise-baseline leg (coinflip-any, run_id=$noiseRunId)"
$env:RUST_LOG = "off"
Push-Location $root
try {
    $noiseResult = Invoke-Native -FilePath $simBin -Arguments $noiseArgs
} finally {
    Pop-Location
    Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue
}
$noiseOut = ($noiseResult[0] | Out-String).Trim()
$noiseExit = $noiseResult[1]

$noiseEntry = [ordered]@{
    run_id    = $noiseRunId
    kind      = "paper-noise-baseline"
    date      = $dateDashed
    venue     = "hyperliquid"
    strategy  = "coinflip-any"
    seed      = $Seed
    latched   = $false
    exit_code = $noiseExit
    ts_utc    = (Get-Date).ToUniversalTime().ToString("o")
}
if ($noiseExit -ne 0) {
    $noiseEntry["faults"] = 1
    $noiseEntry["error"] = $noiseOut.Substring(0, [Math]::Min(500, $noiseOut.Length))
    Add-Content -Path $indexFile -Value ($noiseEntry | ConvertTo-Json -Compress) -Encoding UTF8
    Log "PAP-11: noise-baseline leg FAULTED (exit $noiseExit) - counts as a session fault" "ERROR"
    $pap11Fault = $true
} elseif ($noiseOut -match 'GATE PASS') {
    # R-8 violation: the evaluation pipeline let noise through the net gate.
    $noiseEntry["faults"] = 1
    $noiseEntry["gate_pass"] = $true
    Add-Content -Path $indexFile -Value ($noiseEntry | ConvertTo-Json -Compress) -Encoding UTF8
    Log "PAP-11: noise control GATE PASS - evaluation pipeline is BROKEN (R-8). Firing P1; session faults." "ERROR"
    $mpOps11 = Join-Path $root "target\release\mp-ops.exe"
    if (Test-Path $mpOps11) {
        $tgArgs11 = @("telegram-send", "--id", "pap11-noise-baseline", "--severity", "p1", "--detail", "PAP-11 $dateDashed : coinflip-any GATE PASS - gate chain broken (noise survived net gate)")
        $tg11 = Invoke-Native -FilePath $mpOps11 -Arguments $tgArgs11
        if ($tg11[1] -ne 0) { Log "PAP-11: P1 telegram send failed (exit $($tg11[1]))" "WARN" }
    } else {
        Log "PAP-11: P1 telegram skipped - mp-ops.exe not found" "WARN"
    }
    $pap11Fault = $true
} else {
    $noiseFires = $null
    if ($noiseOut -match 'observations:\s*(\d+) recorded') { $noiseFires = [int]$Matches[1] }
    $noiseEntry["observations"] = $noiseFires
    $noiseEntry["faults"] = 0
    Add-Content -Path $indexFile -Value ($noiseEntry | ConvertTo-Json -Compress) -Encoding UTF8
    if (-not $noiseFires -or $noiseFires -eq 0) {
        Log "PAP-11: noise control fired 0 on this log - baseline VOID today (P3), nothing certified" "WARN"
    } else {
        Log "PAP-11: noise baseline OK - control fired $noiseFires and was refused by the gates (expected)"
    }
}

# ---- PAP-9: fault-free streak tracking --------------------------------------
$streakFile = Join-Path $runsDir "paper_streak.json"
$faults = $journalEntry["faults"]
if ($faults -eq 0) {
    $currentStreak = 0
    if (Test-Path $streakFile) {
        try {
            $streakData = Get-Content -Path $streakFile -Raw | ConvertFrom-Json
            $currentStreak = $streakData.consecutive
        } catch { }
    }
    $currentStreak++
    $streakData = [ordered]@{
        consecutive = $currentStreak
        last_date  = $dateDashed
        last_run   = $runId
    }
    Set-Content -Path $streakFile -Value ($streakData | ConvertTo-Json -Compress) -Encoding UTF8
    Log "PAP-9: fault-free streak: $currentStreak (target: 14)"
    if ($currentStreak -ge 14) {
        Log "PAP-9: ROADMAP Phase 4 evidence row may be checked - $currentStreak consecutive fault-free sessions" "WARN"
    }
} else {
    # One fault resets the count (PAP-9).
    $streakData = [ordered]@{
        consecutive = 0
        last_date  = $dateDashed
        last_run   = $runId
        reset_reason = "fault(s)=$faults"
    }
    Set-Content -Path $streakFile -Value ($streakData | ConvertTo-Json -Compress) -Encoding UTF8
    Log "PAP-9: streak reset to 0 (faults=$faults)" "WARN"
}

# ---- PAP-7: Telegram summary -------------------------------------------------
$severity = if ($faults -gt 0) { "p2" } else { "p3" }
$tgDetail = "paper ${dateDashed}: strategy=$papStrategy seed=$Seed latched=$latched faults=$faults"
if ($journalEntry.Contains("expectancy")) {
    $tgDetail += " expectancy=$($journalEntry['expectancy'])"
}
if ($journalEntry.Contains("trades")) {
    $tgDetail += " trades=$($journalEntry['trades'])"
}
$tgArgs = @("telegram-send", "--id", "daily-paper", "--detail", $tgDetail, "--severity", $severity)
$mpOps = Join-Path $root "target\release\mp-ops.exe"
if (Test-Path $mpOps) {
    $tgResult = Invoke-Native -FilePath $mpOps -Arguments $tgArgs
    $tgOut = ($tgResult[0] | Out-String).Trim()
    if ($tgResult[1] -ne 0) {
        Log "Telegram paper summary send failed (exit $($tgResult[1])): $tgOut" "WARN"
    } else {
        Log "Telegram paper summary sent: $tgOut"
    }
} else {
    Log "Telegram send skipped: mp-ops.exe not found" "WARN"
}

# ---- verdict -----------------------------------------------------------------
if ($paperExit -ne 0) {
    Log "Paper session FAULTED for $dateDashed (exit $paperExit)" "ERROR"
    Exit 1
}
if ($pap11Fault) {
    Log "Paper session FAULTED for $dateDashed (PAP-11 noise-baseline leg)" "ERROR"
    Exit 1
}
Log "Paper session complete: $dateDashed (faults=$faults)"
Exit 0
