# start_collectors.ps1 - Launch 24/7 collectors for Phase 0 data recording under Supervisor Watchdog.
# Usage: .\start_collectors.ps1
# Stop:  Stop-Process -Name mp-collector -Force

$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
$watchdogScript = Join-Path $root "ops\watchdog_collectors.ps1"

# Kill any existing collectors and clean stale locks
Stop-Process -Name "mp-collector" -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
$lockPattern = Join-Path $root "data\raw\.lock_*"
Remove-Item -Path $lockPattern -Force -ErrorAction SilentlyContinue

# Launch 24/7 watchdog supervisor
$p = Start-Process -FilePath "powershell.exe" -ArgumentList "-ExecutionPolicy","Bypass","-WindowStyle","Hidden","-File","`"$watchdogScript`"" -WorkingDirectory $root -PassThru
Write-Host ("[OK] 24/7 Collector Watchdog Supervisor started (PID {0})" -f $p.Id) -ForegroundColor Green
Start-Sleep -Seconds 4

# Status via heartbeat files, NOT process enumeration (Get-CimInstance
# Win32_Process + CommandLine was observed to terminate collectors on this
# box — see docs/AUDIT-2026-08-03.md).
$running = 0
$hbDir = Join-Path $root "data\raw"
foreach ($sym in @("BTCUSDT", "ETHUSDT")) {
    $hbFile = Join-Path $hbDir "mp-collector-binance-$sym.heartbeat"
    if (Test-Path $hbFile) {
        $hbAge = ((Get-Date) - (Get-Item $hbFile).LastWriteTime).TotalSeconds
        if ($hbAge -le 90) { $running++ }
    }
}

Write-Host ""
Write-Host "=== Collector Status ===" -ForegroundColor Cyan
Write-Host ("Healthy: {0} / 2 collectors under 24/7 Watchdog" -f $running) -ForegroundColor $(if ($running -ge 2) { "Green" } else { "Yellow" })
Write-Host ("Data dir: {0}\data\raw\" -f $root) -ForegroundColor Gray
Write-Host ""
Write-Host "Monitor:  Get-ChildItem data\raw\*.heartbeat"
Write-Host "Stop all: Stop-Process -Name mp-collector -Force"
