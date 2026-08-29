# start_collectors.ps1 - Launch 24/7 collectors for Phase 0 data recording.
# Usage: .\start_collectors.ps1
# Stop all (task mode):  Stop-ScheduledTask MoneyPrinterCollectorsWatchdog
# Stop all (manual):     Stop-Process -Name mp-collector -Force
#
# If the Scheduled Task MoneyPrinterCollectorsWatchdog is registered, this
# script starts/ensures THAT supervisor (never a duplicate manual watchdog -
# two supervisors would fight over spawns). If the task is not registered,
# disabled, or cannot be started, it falls back to the legacy detached
# PowerShell watchdog. Register the task with:
#   .\ops\watchdog_collectors.ps1 -RegisterTask
#
# The supervised set is derived from ops/core_symbols.txt (venue:SYMBOL lines,
# one per line) - the SAME single source of truth the collector watchdog and
# the daily pipeline use - so the heartbeat names polled here can never drift
# from what the watchdog actually spawns. (audit 08-10: this script used to
# hardcode `mp-collector-binance-BTCUSDT.heartbeat` / `ETHUSDT`, but the live
# recordings moved to hyperliquid:BTC/ETH on 08-08, so it reported 0/2 healthy
# even while both collectors were running.)

$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
$watchdogScript = Join-Path $root "ops\watchdog_collectors.ps1"
$TaskName = "MoneyPrinterCollectorsWatchdog"

# ---- Resolve the recording set from the single source of truth --------------
# Single shared parser: ops/scripts/recordings.ps1 is dot-sourced here, by
# ops/watchdog_collectors.ps1, and by ops/scripts/daily_pipeline.ps1 so the
# venue->symbol parse and the default set can never drift between the three
# (audit 08-10: this script used to hardcode binance heartbeats after the
# live recordings moved to hyperliquid:BTC/ETH on 08-08). A venue:SYMBOL pair
# maps 1:1 to the heartbeat name `mp-collector-{venue}-{symbol}.heartbeat`.
. (Join-Path $root "ops\scripts\recordings.ps1")
try {
    $recordings = @(Resolve-Recordings -CoreFile (Join-Path $root "ops\core_symbols.txt"))
} catch {
    Write-Host "[!!] $_" -ForegroundColor Red
    Exit 1
}
$heartbeats = @($recordings | ForEach-Object { "mp-collector-$($_.venue)-$($_.symbol).heartbeat" })

# ---- Reset collection state: kill collectors, drop stale locks/heartbeats ----
# Heartbeat files are removed too so the (re)started watchdog sees "down" and
# respawns immediately instead of waiting out its 75s staleness grace.
#
# SCOPE (audit H-5): reset ONLY the Phase-0 recording set. The old global
# `Stop-Process -Name mp-collector` and `Remove-Item .lock_*` also killed the
# independently-supervised swing set (swing_collectors.ps1) and deleted lock
# files still held by live processes - which can admit a SECOND writer to the
# same append-only W-6 log (interleaved/corrupt frames). Phase-0 collectors are
# identified by their spawn command line: the watchdog launches them with
# `--venue/--symbol` and WITHOUT `--config` (ops/watchdog_collectors.ps1:102),
# whereas swing/whale/macro collectors are launched via `--config collectors/...`
# and are left entirely alone.
$phase0C = Get-CimInstance Win32_Process -Filter "Name='mp-collector.exe'" -ErrorAction SilentlyContinue
$killed = @()
foreach ($p in $phase0C) {
    $cl = $p.CommandLine
    if (-not $cl -or $cl -like "*--config*") { continue } # swing/macro/etc use --config
    foreach ($r in $recordings) {
        $sTok = "--symbol `"$($r.symbol)`""
        if ($cl -like "*$sTok*") {
            Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
            $killed += "PID $($p.ProcessId) ($($r.venue):$($r.symbol))"
            break
        }
    }
}
Start-Sleep -Seconds 1
# Clear ONLY the Phase-0 instance locks (a force-killed prior writer may have
# left one); never a glob delete of every .lock_* file.
foreach ($r in $recordings) {
    $lock = ".lock_$($r.venue)_$($r.symbol)"
    Remove-Item -Path (Join-Path $root "data\raw\$lock") -Force -ErrorAction SilentlyContinue
}
if ($killed.Count -gt 0) {
    Write-Host ("[OK] Stopped stale Phase-0 collectors: {0}" -f ($killed -join "; "))
}
foreach ($hb in $heartbeats) {
    Remove-Item -Path (Join-Path $root "data\raw\$hb") -Force -ErrorAction SilentlyContinue
}

# ---- Supervisor: prefer the registered Scheduled Task; fall back to manual ----
$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
$supervisor = "manual"
if ($null -ne $task) {
    if ($task.State -eq 'Disabled') {
        Write-Host ("[!!] Task {0} is Disabled - falling back to manual watchdog" -f $TaskName) -ForegroundColor Yellow
    } elseif ($task.State -eq 'Running') {
        Write-Host ("[OK] Scheduled Task already running: {0}" -f $TaskName) -ForegroundColor Green
        $supervisor = "task"
    } else {
        try {
            Start-ScheduledTask -TaskName $TaskName -ErrorAction Stop
            Write-Host ("[OK] Scheduled Task started: {0}" -f $TaskName) -ForegroundColor Green
            $supervisor = "task"
        } catch {
            Write-Host ("[!!] Failed to start task {0}: {1}" -f $TaskName, $_.Exception.Message) -ForegroundColor Yellow
        }
    }
} else {
    Write-Host ("[!!] Task {0} is not registered (register: .\ops\watchdog_collectors.ps1 -RegisterTask)" -f $TaskName) -ForegroundColor Yellow
}

if ($supervisor -eq "manual") {
    $p = Start-Process -FilePath "powershell.exe" `
        -ArgumentList "-ExecutionPolicy","Bypass","-WindowStyle","Hidden","-File","`"$watchdogScript`"" `
        -WorkingDirectory $root -PassThru
    Write-Host ("[OK] Manual watchdog supervisor started (PID {0})" -f $p.Id) -ForegroundColor Green
}

# ---- Wait for collectors to come back (bounded poll, up to ~90s) ----
$hbDir = Join-Path $root "data\raw"
$fresh = @{}
foreach ($hb in $heartbeats) { $fresh[$hb] = $false }
for ($i = 0; $i -lt 18; $i++) {
    $all = $true
    foreach ($hb in $heartbeats) {
        if ($fresh[$hb]) { continue }
        $hbFile = Join-Path $hbDir $hb
        if (Test-Path $hbFile) {
            $hbAge = ((Get-Date) - (Get-Item $hbFile).LastWriteTime).TotalSeconds
            if ($hbAge -le 90) { $fresh[$hb] = $true }
        }
        if (-not $fresh[$hb]) { $all = $false }
    }
    if ($all) { break }
    Start-Sleep -Seconds 5
}

# ---- Status (heartbeat files only - never process enumeration; see docs/AUDIT-2026-08-03.md) ----
$running = 0
foreach ($hb in $heartbeats) { if ($fresh[$hb]) { $running++ } }
$recLabel = @($recordings | ForEach-Object { "$($_.venue):$($_.symbol)" }) -join ", "

Write-Host ""
Write-Host "=== Collector Status ===" -ForegroundColor Cyan
Write-Host ("Supervisor: {0}" -f $(if ($supervisor -eq "task") { "Scheduled Task ($TaskName)" } else { "manual watchdog" })) -ForegroundColor Gray
Write-Host ("Recordings: {0}" -f $recLabel) -ForegroundColor Gray
Write-Host ("Healthy: {0} / {1} collectors" -f $running, $heartbeats.Count) -ForegroundColor $(if ($running -ge $heartbeats.Count) { "Green" } else { "Yellow" })
Write-Host ("Data dir: {0}\data\raw\" -f $root) -ForegroundColor Gray
Write-Host ""
Write-Host "Monitor:  Get-ChildItem data\raw\*.heartbeat"
if ($supervisor -eq "task") {
    Write-Host "Stop all: Stop-ScheduledTask $TaskName"
} else {
    Write-Host "Stop all: Stop-Process -Name mp-collector -Force"
    Write-Host "Make 24/7: .\ops\watchdog_collectors.ps1 -RegisterTask"
}
