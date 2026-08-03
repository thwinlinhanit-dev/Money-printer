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

$procs = Get-CimInstance Win32_Process -Filter "Name='mp-collector.exe'" -ErrorAction SilentlyContinue
$running = if ($procs) { @($procs).Count } else { 0 }

Write-Host ""
Write-Host "=== Collector Status ===" -ForegroundColor Cyan
Write-Host ("Running: {0} / 2 collectors under 24/7 Watchdog" -f $running) -ForegroundColor $(if ($running -ge 2) { "Green" } else { "Yellow" })
Write-Host ("Data dir: {0}\data\raw\" -f $root) -ForegroundColor Gray
Write-Host ""
Write-Host "Monitor:  Get-ChildItem data\raw\*.heartbeat"
Write-Host "Stop all: Stop-Process -Name mp-collector -Force"
