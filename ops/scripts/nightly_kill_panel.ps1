# nightly_kill_panel.ps1 - Nightly signal-decay kill panel (POWER-GATES §7.3 bridge,
# owner-approved 2026-09-11; feeds §7.1 via the fault-free streak).
#
# What it does, per night:
#   1. Picks the FRESHEST closed hyperliquid BTC raw day log (>= 16h of data).
#   2. Replays all 5 registered strategies + the coinflip-any noise control
#      through the production observation engine (params-hash killpanel-nightly,
#      seed 1, write-only recording into data/observations).
#   3. Grades the gate verdict lines:
#        - ANY "GATE PASS" on a non-control strategy  -> P2 telegram (the rarest,
#          most important event in the system; the retest mandate fires).
#        - "GATE PASS" on coinflip-any               -> P1 (R-8: pipeline broken).
#        - New "DECAY_SUSPECT" on a strategy          -> P2 telegram.
#      Routine refusals are journaled only (no alert noise).
#   4. Tracks a fault-free streak (data/killpanel/streak.json) - 14 consecutive
#      clean nights is a POWER-GATES Next-Review trigger.
#
# Usage:
#   .\nightly_kill_panel.ps1               # run for yesterday (UTC)
#   .\nightly_kill_panel.ps1 -Date 20260909
#   .\nightly_kill_panel.ps1 -RegisterTask # schedule daily 09:15 UTC (after paper)
#
# Exit codes: 0 = panel ran to verdict; 1 = panel faulted (missing sim/log);
#             2 = config error.
param(
    [string]$Date,
    [switch]$RegisterTask
)
$ErrorActionPreference = "Continue"

# ---- workspace root resolution (same walk as daily_paper.ps1) -----------------
$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
while ($true) {
    $manifest = Join-Path $root "Cargo.toml"
    if ((Test-Path $manifest) -and (Select-String -Path $manifest -Pattern '^\[workspace\]' -Quiet)) { break }
    $parent = Split-Path $root -Parent
    if ($parent -eq $root) { Write-Host "[!!] workspace root not found" -ForegroundColor Red; Exit 2 }
    $root = $parent
}
$TaskName = "MoneyPrinterKillPanel"
$outDir   = Join-Path $root "data\killpanel"
$runsDir  = Join-Path $root "data\runs"
$mpOps    = Join-Path $root "target\release\mp-ops.exe"
$sim      = Join-Path $root "target\release\sim.exe"
$streakF  = Join-Path $outDir "streak.json"

function Log {
    param([string]$Msg, [string]$Level = "INFO")
    $ts = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    Add-Content -Path (Join-Path $outDir "killpanel.log") -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") { "Yellow" } elseif ($Level -eq "ERROR") { "Red" } else { "White" }
    Write-Host $line -ForegroundColor $col
}

function Send-Tg {
    param([string]$Severity, [string]$Detail)
    if (Test-Path $mpOps) {
        $r = & $mpOps telegram-send --id nightly-kill-panel --severity $Severity --detail $Detail 2>&1
        if ($LASTEXITCODE -ne 0) { Log "telegram send failed (exit $LASTEXITCODE): $r" "WARN" }
        else { Log "telegram $Severity sent: $Detail" }
    } else {
        Log "telegram skipped - mp-ops.exe missing (PD-honest, never a fake send)" "WARN"
    }
}

function Update-Streak {
    param([bool]$Clean)
    $streak = 0
    if (Test-Path $streakF) {
        try { $streak = (Get-Content $streakF -Raw | ConvertFrom-Json).consecutive } catch { $streak = 0 }
    }
    if ($Clean) { $streak++ } else { $streak = 0 }
    @{ consecutive = $streak; last_run = (Get-Date).ToUniversalTime().ToString("o") } |
        ConvertTo-Json -Compress | Set-Content -Path $streakF -Encoding UTF8
    if ($streak -ge 14) {
        Log "fault-free streak: $streak nights - POWER-GATES Next-Review trigger (a) is MET" "WARN"
    } else {
        Log "fault-free streak: $streak (target 14)"
    }
    return $streak
}

# ---- task registration --------------------------------------------------------
if ($RegisterTask) {
    # After the daily pipeline (07:30) and paper rehearsal (08:30): 09:15 UTC.
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(9).AddMinutes(15), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }   # registration-race guard
    $trigger  = New-ScheduledTaskTrigger -Daily -At $localAt
    $action   = New-ScheduledTaskAction -Execute "powershell.exe" `
                    -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    $settings = New-ScheduledTaskSettingsSet `
                    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
                    -StartWhenAvailable -MultipleInstances IgnoreNew `
                    -ExecutionTimeLimit (New-TimeSpan -Hours 2) `
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
if (-not $Date) { $Date = (Get-Date).ToUniversalTime().AddDays(-1).ToString("yyyyMMdd") }
$dateFlat = $Date.Replace("-", "")

# ---- 1. freshest closed hyperliquid BTC log at/before $Date --------------------
$candidates = Get-ChildItem (Join-Path $root "data\raw") -Filter "*_hyperliquid_BTC.log" |
    Where-Object { $_.Name -notlike "trace_*" -and $_.Name.Substring(0,8) -le $dateFlat } |
    Sort-Object Name -Descending
$rawLog = $null
foreach ($c in $candidates) {
    if ($c.Length -ge 16MB) { $rawLog = $c; break }   # >= ~16h of data (a 500MB day ≈ 24h)
}
if ($null -eq $rawLog) {
    Log "no hyperliquid BTC day log >= 16h at/before $dateFlat - nothing to panel tonight (P3, not a fault)" "WARN"
    Send-Tg "p3" "kill-panel $dateFlat : no fresh closed HL BTC log - night skipped"
    Exit 0
}
$day = $rawLog.Name.Substring(0, 8)
Log "PANEL target log: $($rawLog.Name) ($([math]::Round($rawLog.Length/1MB,0)) MB)"

if (-not (Test-Path $sim)) { Log "sim.exe missing - config error" "ERROR"; Exit 2 }

# ---- 2. run the panel ----------------------------------------------------------
# orderflow-v1 removed 2026-09-12: KILLED by the perturbation + venue retest
# (docs/research/RETEST-orderflow-v1-2026-09-11.md); replaying it nightly only
# produced standing DECAY/KILL noise.
$strategies = @("coinflip-any", "carry-v1", "liq-fade-v1", "swing-range-reclaim-v1")
$stamp = Get-Date -UFormat %s
$panelFaults = 0
$verdicts = @()

foreach ($s in $strategies) {
    $rid = "killpanel-$day-$s-$stamp"
    $out = & $sim backtest --log $rawLog.FullName --strategy $s --seed 1 `
        --run-id $rid --runs-dir $runsDir --git-sha killpanel-nightly `
        --params-hash "killpanel-nightly" --obs-dir (Join-Path $root "data\observations") `
        --horizons "15m,1h,4h,1d" 2>&1
    $exit = $LASTEXITCODE
    $text = ($out | Out-String)
    Set-Content -Path (Join-Path $outDir "$day-$s.out") -Value $text -Encoding UTF8

    if ($exit -ne 0) {
        Log "RUN $s FAULTED (exit $exit)" "ERROR"
        $panelFaults++
        $verdicts += "$s=FAULT"
        continue
    }
    $passes  = ([regex]::Matches($text, "GATE PASS")).Count
    $decays  = ([regex]::Matches($text, "DECAY_SUSPECT")).Count
    $refused = ([regex]::Matches($text, "GATE REFUSED")).Count

    if ($s -eq "coinflip-any") {
        if ($passes -gt 0) {
            # R-8: noise survived the net gate - the evaluation pipeline is broken.
            Log "R-8 ALARM: coinflip-any control GATE PASS - evaluation pipeline BROKEN" "ERROR"
            Send-Tg "p1" "kill-panel $day : R-8 ALARM - coinflip-any GATE PASS ($passes); evaluation pipeline broken"
            $panelFaults++
        } elseif ($refused -eq 0) {
            Log "control fired 0 / no verdicts - baseline VOID tonight (P3)" "WARN"
        } else {
            Log "control OK: refused at all $refused graded horizons (noise killed, pipeline certified)"
        }
        $verdicts += "control=refused:$refused"
    } else {
        if ($passes -gt 0) {
            Log "ALERT: $s GATE PASS x$passes on $day - retest mandate (rarest event)" "WARN"
            Send-Tg "p2" "kill-panel $day : $s GATE PASS x$passes (horizons graded: n>0) - retest mandate"
        }
        if ($decays -gt 0) {
            Log "ALERT: $s DECAY_SUSPECT x$decays on $day" "WARN"
            Send-Tg "p2" "kill-panel $day : $s DECAY_SUSPECT x$decays"
        }
        $verdicts += "$s=pass:$passes/decay:$decays/refused:$refused"
    }
}

# ---- 4. streak + verdict -------------------------------------------------------
$clean = ($panelFaults -eq 0)
$null = Update-Streak $clean
$summary = "kill-panel $day : " + ($verdicts -join " | ")
Log $summary
if ($clean) { Log "panel complete: no faults" }
else        { Log "panel complete WITH FAULTS ($panelFaults)" "ERROR"; Exit 1 }
Exit 0
