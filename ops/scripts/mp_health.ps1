# mp_health.ps1 - One-shot health report for the VPS-only collectors + pipeline.
#
# Designed for the owner's desktop after the 2026-08-31 zero-cost handoff:
# collectors live on the VPS, data lands here via the nightly VPS drain, and
# backups push to C:\mp-backup + off-host. This script just READS state and
# prints a traffic-light summary - it changes nothing.
#
# Usage:
#   .\ops\scripts\mp_health.ps1            # full report
#   $env:MP_VPS_HOST = "<vps host>"         # optional; enables the SSH
#                                           # reachability check. PD-2: the
#                                           # production host IP never lives
#                                           # in the repo.
#
# Exit codes:
#   0 = all green
#   1 = any warning   (printed as [WARN])
#   2 = any critical  (printed as [CRIT])
#
# Safe to run any time; no side effects.

$ErrorActionPreference = "Continue"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$raw  = Join-Path $root "data\raw"
$manifest = Join-Path $root "data\vps_drain_manifest.jsonl"

$warn = @(); $crit = @()

function Out-Line([string]$mark, [string]$msg) {
    $color = switch ($mark) { '[ OK ]' {'Green'} '[WARN]' {'Yellow'} '[CRIT]' {'Red'} default {'Gray'} }
    Write-Host ("{0,-7} {1}" -f $mark, $msg) -ForegroundColor $color
}

Write-Host ("=== Money-Printer health report  {0}  (VPS-only mode) ===" -f (Get-Date).ToString("yyyy-MM-dd HH:mm"))
Write-Host ("Repo: {0}" -f $root)
Write-Host ""

# ---- 1. Local collectors: must be OFF (VPS-only mode) -----------------------
$local = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match "mp-(collector|whale|macro)" })
if ($local.Count -eq 0) {
    Out-Line "[ OK ]" "local collectors: none running (VPS-only as intended)"
} else {
    Out-Line "[WARN]" "local collectors still running: $($local.Name -join ', ')"
    $warn += "local collectors running"
}

# ---- 2. VPS reachability ----------------------------------------------------
# PD-2: production host IPs never live in the repo. The check runs only when
# MP_VPS_HOST is set in the environment; otherwise it warns and skips.
$vpsHost = $env:MP_VPS_HOST
$vpsReachable = $false
if ([string]::IsNullOrWhiteSpace($vpsHost)) {
    Out-Line "[WARN]" "MP_VPS_HOST not set - skipping VPS reachability check"
    $warn += "MP_VPS_HOST unset"
} else {
    try {
        $tcp = Test-NetConnection -ComputerName $vpsHost -Port 22 -WarningAction SilentlyContinue -InformationLevel Quiet
        $vpsReachable = [bool]$tcp
    } catch { $vpsReachable = $false }
    if ($vpsReachable) {
        Out-Line "[ OK ]" "VPS reachable on SSH port 22 ($vpsHost)"
    } else {
        Out-Line "[CRIT]" "VPS NOT reachable on SSH port 22 ($vpsHost) - drain will fail"
        $crit += "VPS unreachable"
    }
}

# ---- 3. Data freshness: newest closed day in data\raw -----------------------
$logs = @(Get-ChildItem -LiteralPath $raw -Filter "????????_hyperliquid_*.log" -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match "^(\d{8})_hyperliquid_(BTC|ETH)\.log$" } |
    Sort-Object Name)
if ($logs.Count -eq 0) {
    Out-Line "[CRIT]" "no hyperliquid BTC/ETH logs in data\raw at all"
    $crit += "no hyperliquid logs"
} else {
    $newest = ($logs | Select-Object -Last 1).Name.Substring(0,8)
    $yesterday = ((Get-Date).ToUniversalTime().AddDays(-1).ToString("yyyyMMdd"))
    if ($newest -ge $yesterday) {
        Out-Line "[ OK ]" "newest closed hyperliquid day: $newest (yesterday = $yesterday)"
    } else {
        Out-Line "[CRIT]" "newest closed hyperliquid day: $newest; yesterday=$yesterday - drain is BEHIND"
        $crit += "drain behind (newest=$newest)"
    }
    $present = @($logs | Select-Object -Last 3 | ForEach-Object { $_.Name.Substring(0,8) })
    Out-Line "  ..." "last 3 present days: $($present -join ', ')"
}

# ---- 4. VPS drain scheduled task --------------------------------------------
$drainTask = Get-ScheduledTask -TaskName "MoneyPrinterVpsDrain" -ErrorAction SilentlyContinue
if ($null -eq $drainTask) {
    Out-Line "[CRIT]" "MoneyPrinterVpsDrain task NOT registered"
    $crit += "drain task missing"
} else {
    $drainInfo = Get-ScheduledTaskInfo -TaskName "MoneyPrinterVpsDrain" -ErrorAction SilentlyContinue
    $lastTs = $drainInfo.LastRunTime
    if ($drainTask.State -eq "Running") {
        Out-Line "[WARN]" "MoneyPrinterVpsDrain task is RUNNING right now"
        $warn += "drain running now"
    } elseif ($lastTs -lt (Get-Date).AddDays(-2)) {
        Out-Line "[CRIT]" "MoneyPrinterVpsDrain last ran $lastTs (> 2 days ago)"
        $crit += "drain stale"
    } else {
        Out-Line "[ OK ]" "MoneyPrinterVpsDrain last ran $lastTs (next $($drainInfo.NextRunTime))"
    }
    if (Test-Path $manifest) {
        $lastEntryTs = Get-Content -LiteralPath $manifest -Tail 1 -ErrorAction SilentlyContinue | ForEach-Object {
            if ($_ -match '"ts_utc":"([^"]+)"') { [datetime]$Matches[1] }
        }
        if ($lastEntryTs) { Out-Line "  ..." "drain manifest last success entry: $lastEntryTs" }
    }
}
# ---- 5. Backup tasks -------------------------------------------------------------
foreach ($tt in @("MoneyPrinterVpsBackup","MoneyPrinterOffhostBackup","MoneyPrinterDailyPipeline")) {
    $task = Get-ScheduledTask -TaskName $tt -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        Out-Line "[CRIT]" "$tt not registered"
        $crit += "$tt missing"
        continue
    }
    $info = Get-ScheduledTaskInfo -TaskName $tt -ErrorAction SilentlyContinue
    $bad = ($null -ne $info.LastTaskResult -and $info.LastTaskResult -ne 0 -and $info.LastTaskResult -ne 267009 -and $info.LastTaskResult -ne 267011 -and $info.LastTaskResult -ne 267010)
    if ($task.State -eq "Running") {
        Out-Line "[ OK ]" "$tt currently running"
    } elseif ($bad) {
        Out-Line "[WARN]" "$tt last result=$($info.LastTaskResult) at $($info.LastRunTime)"
        $warn += "$tt result=$($info.LastTaskResult)"
    } else {
        Out-Line "[ OK ]" "$tt last result=0 at $($info.LastRunTime) (next $($info.NextRunTime))"
    }
}

# ---- 6. Disk --------------------------------------------------------------------------
$drive = Get-PSDrive C
$freeGB = $drive.Free / 1GB
$rawSizeGB = ((Get-ChildItem -LiteralPath $raw -File -ErrorAction SilentlyContinue |
    Measure-Object Length -Sum).Sum) / 1GB
Out-Line "  ..." ("C: free {0:N1} GB | data\raw {1:N2} GB" -f $freeGB, $rawSizeGB)
if ($freeGB -lt 20) { Out-Line "[WARN]" "C: free under 20 GB"; $warn += "low disk" }

Write-Host ""
if ($crit.Count -gt 0) {
    Out-Line "[CRIT]" "CRITICAL: $($crit -join '; ')"
    exit 2
} elseif ($warn.Count -gt 0) {
    Out-Line "[WARN]" "WARNINGS: $($warn -join '; ')"
    exit 1
} else {
    Out-Line "[ OK ]" "All green"
    exit 0
}