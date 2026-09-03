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

# ---- 5b. VPS backup corpus (C:\mp-backup) --------------------------------------------
# The nightly VPS backup mirrors /opt/money-printer/data into
# C:\mp-backup\vps-data. Since the 2026-09-04 hardening, .last_backup_ts only
# advances after a verified-complete run, so its age IS the staleness of the
# last good backup; the manifest's last entry records that run's integrity.
$bkRoot     = Join-Path "C:\mp-backup" "vps-data"
$bkMarker   = Join-Path $bkRoot ".last_backup_ts"
$bkManifest = Join-Path $bkRoot "backup_manifest.jsonl"
if (-not (Test-Path $bkRoot)) {
    Out-Line "[CRIT]" "backup corpus missing: $bkRoot"
    $crit += "backup corpus missing"
} else {
    $nowUtc = [DateTime]::UtcNow
    if (Test-Path $bkMarker) {
        $markerEpoch = [int64](Get-Content -LiteralPath $bkMarker -Raw).Trim()
        $markerUtc = [DateTimeOffset]::FromUnixTimeSeconds($markerEpoch).UtcDateTime
        $ageH = ($nowUtc - $markerUtc).TotalHours
        if ($ageH -gt 50) {
            Out-Line "[CRIT]" ("backup marker {0}Z is {1:N1}h old - backup STALE (> 2 days)" -f $markerUtc.ToString("yyyy-MM-dd HH:mm"), $ageH)
            $crit += "backup stale"
        } elseif ($ageH -gt 26) {
            Out-Line "[WARN]" ("backup marker {0}Z is {1:N1}h old - missed a night?" -f $markerUtc.ToString("yyyy-MM-dd HH:mm"), $ageH)
            $warn += "backup missed a night"
        } else {
            Out-Line "[ OK ]" ("backup marker {0}Z ({1:N1}h old)" -f $markerUtc.ToString("yyyy-MM-dd HH:mm"), $ageH)
        }
    } else {
        Out-Line "[CRIT]" "backup marker missing: $bkMarker"
        $crit += "backup marker missing"
    }
    if (Test-Path $bkManifest) {
        $last = Get-Content -LiteralPath $bkManifest -Tail 1 -ErrorAction SilentlyContinue | ConvertFrom-Json
        if ($null -ne $last) {
            $mUtc = [DateTimeOffset]::Parse($last.ts_utc).UtcDateTime
            $mAge = ($nowUtc - $mUtc).TotalHours
            $skipIt = $false
            if ($null -ne $last.skip_integrity) { $skipIt = [bool]$last.skip_integrity }
            if ($skipIt) {
                Out-Line "[WARN]" ("last backup ran with -SkipIntegrity (ts_utc={0}) - integrity not verified" -f $last.ts_utc)
                $warn += "backup skipped integrity"
            } elseif ($null -eq $last.dst_delta_files -or $last.dst_delta_files -ne $last.src_delta_files) {
                Out-Line "[CRIT]" ("last manifest entry (ts_utc={0}) dst_delta_files={1} vs src_delta_files={2} - integrity NOT green" -f $last.ts_utc, $last.dst_delta_files, $last.src_delta_files)
                $crit += "backup manifest integrity not green"
            } elseif ($mAge -gt 50) {
                Out-Line "[CRIT]" ("last verified-good backup {0}Z is {1:N1}h old" -f $mUtc.ToString("yyyy-MM-dd HH:mm"), $mAge)
                $crit += "backup manifest stale"
            } else {
                Out-Line "[ OK ]" ("last verified-good backup {0}Z: {1} files / {2:N1} MiB (delta), elapsed {3}s" -f $last.ts_utc.Substring(0, 19), $last.dst_delta_files, ($last.dst_delta_bytes / 1MB), $last.elapsed_sec)
            }
        }
    } else {
        Out-Line "[WARN]" "backup manifest missing: $bkManifest"
        $warn += "backup manifest missing"
    }
    $bkFiles = @(Get-ChildItem -LiteralPath $bkRoot -Recurse -File -ErrorAction SilentlyContinue)
    $bkBytes = ($bkFiles | Measure-Object Length -Sum).Sum
    Out-Line "  ..." ("corpus: {0} files / {1:N2} GiB under {2}" -f $bkFiles.Count, ($bkBytes / 1GB), $bkRoot)
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