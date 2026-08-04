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

$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
$watchdogScript = Join-Path $root "ops\watchdog_collectors.ps1"
$TaskName = "MoneyPrinterCollectorsWatchdog"

# ---- Reset collection state: kill collectors, drop stale locks/heartbeats ----
# Heartbeat files are removed too so the (re)started watchdog sees "down" and
# respawns immediately instead of waiting out its 75s staleness grace.
Stop-Process -Name "mp-collector" -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
Remove-Item -Path (Join-Path $root "data\raw\.lock_*") -Force -ErrorAction SilentlyContinue
Remove-Item -Path (Join-Path $root "data\raw\mp-collector-binance-*.heartbeat") -Force -ErrorAction SilentlyContinue

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
foreach ($sym in @("BTCUSDT", "ETHUSDT")) { $fresh[$sym] = $false }
for ($i = 0; $i -lt 18; $i++) {
    $all = $true
    foreach ($sym in @("BTCUSDT", "ETHUSDT")) {
        if ($fresh[$sym]) { continue }
        $hbFile = Join-Path $hbDir "mp-collector-binance-$sym.heartbeat"
        if (Test-Path $hbFile) {
            $hbAge = ((Get-Date) - (Get-Item $hbFile).LastWriteTime).TotalSeconds
            if ($hbAge -le 90) { $fresh[$sym] = $true }
        }
        if (-not $fresh[$sym]) { $all = $false }
    }
    if ($all) { break }
    Start-Sleep -Seconds 5
}

# ---- Status (heartbeat files only - never process enumeration; see docs/AUDIT-2026-08-03.md) ----
$running = 0
foreach ($sym in @("BTCUSDT", "ETHUSDT")) { if ($fresh[$sym]) { $running++ } }

Write-Host ""
Write-Host "=== Collector Status ===" -ForegroundColor Cyan
Write-Host ("Supervisor: {0}" -f $(if ($supervisor -eq "task") { "Scheduled Task ($TaskName)" } else { "manual watchdog" })) -ForegroundColor Gray
Write-Host ("Healthy: {0} / 2 collectors" -f $running) -ForegroundColor $(if ($running -ge 2) { "Green" } else { "Yellow" })
Write-Host ("Data dir: {0}\data\raw\" -f $root) -ForegroundColor Gray
Write-Host ""
Write-Host "Monitor:  Get-ChildItem data\raw\*.heartbeat"
if ($supervisor -eq "task") {
    Write-Host "Stop all: Stop-ScheduledTask $TaskName"
} else {
    Write-Host "Stop all: Stop-Process -Name mp-collector -Force"
    Write-Host "Make 24/7: .\ops\watchdog_collectors.ps1 -RegisterTask"
}
