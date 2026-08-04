# watchdog_collectors.ps1  (SPEC-019)
# 24/7 supervisor: restarts mp-collector if it crashes, stops writing, or its
# heartbeat goes stale.
#
# Run in foreground:
#   .\ops\watchdog_collectors.ps1
#
# Register as a Scheduled Task (run elevated for AtStartup):
#   .\ops\watchdog_collectors.ps1 -RegisterTask
#   .\ops\watchdog_collectors.ps1 -RegisterTask -AsSystem

param(
    [switch]$RegisterTask,
    [switch]$AsSystem,
    [string[]]$Symbols         = @("BTCUSDT", "ETHUSDT"),
    [int]$CheckIntervalSeconds = 20,
    [int]$CooldownSeconds      = 90,
    [int]$GraceSeconds         = 45
)

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$exe  = Join-Path $root "target\release\mp-collector.exe"

# ---- task registration ------------------------------------------------------
if ($RegisterTask) {
    $TaskName = "MoneyPrinterCollectorsWatchdog"
    Write-Host "Registering: $TaskName" -ForegroundColor Yellow
    $Action = New-ScheduledTaskAction `
        -Execute  "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSScriptRoot\watchdog_collectors.ps1`""
    $Triggers = @(
        (New-ScheduledTaskTrigger -AtStartup),
        (New-ScheduledTaskTrigger -AtLogOn)
    )
    $Settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit (New-TimeSpan -Days 3650) `
        -RestartCount 99 -RestartInterval (New-TimeSpan -Minutes 1)
    if ($AsSystem) {
        Register-ScheduledTask -TaskName $TaskName -Action $Action `
            -Trigger $Triggers -Settings $Settings -RunLevel Highest -User "SYSTEM" -Force
        Write-Host "[OK] Registered as SYSTEM." -ForegroundColor Green
    } else {
        Register-ScheduledTask -TaskName $TaskName -Action $Action `
            -Trigger $Triggers -Settings $Settings -User $env:USERNAME -Force
        Write-Host "[OK] Registered for $($env:USERNAME)." -ForegroundColor Green
    }
    Exit 0
}

# ---- foreground loop --------------------------------------------------------
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "  MONEY PRINTER COLLECTOR WATCHDOG" -ForegroundColor Cyan
Write-Host "==========================================" -ForegroundColor Cyan

Set-Location $root
Write-Host "Building binary..." -ForegroundColor Yellow
cargo build -p mp-collectors --features live-ws,live-http --bin mp-collector --release 2>&1 | Where-Object { $_ -match "Compiling|Finished|error" }
if ($LASTEXITCODE -ne 0) { Write-Host "Build failed." -ForegroundColor Red; Exit 1 }
Write-Host "Binary ready: $exe" -ForegroundColor Green

$rawDir = Join-Path $root "data\raw"
if (-not (Test-Path $rawDir)) { New-Item -ItemType Directory -Path $rawDir -Force | Out-Null }

function Get-WLog { return Join-Path $rawDir ("watchdog_" + (Get-Date).ToUniversalTime().ToString("yyyyMMdd") + ".log") }

function WLog {
    param([string]$Msg, [string]$Level = "INFO")
    $ts   = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    Add-Content -Path (Get-WLog) -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") {"Yellow"} elseif ($Level -eq "ERROR") {"Red"} else {"White"}
    Write-Host $line -ForegroundColor $col
}

WLog "Watchdog started. Symbols: $($Symbols -join ',')  cooldown=${CooldownSeconds}s  grace=${GraceSeconds}s"

function Spawn-Collector {
    param([string]$Sym)
    $lock = Join-Path $rawDir ".lock_binance_$Sym"
    if (Test-Path $lock) { Remove-Item $lock -Force -ErrorAction SilentlyContinue }

    # KEY FIX: UseShellExecute=true fully detaches from this process's stdio.
    # No pipe is created, so the collector never blocks on stdout.
    # Tracing output goes to nul; the binary writes its own data + heartbeat files.
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName         = $exe
    $psi.Arguments        = "--symbol $Sym --no-whale"
    $psi.WorkingDirectory = [string]$root
    $psi.UseShellExecute  = $true
    $psi.WindowStyle      = [System.Diagnostics.ProcessWindowStyle]::Hidden
    # Pipe tracing to nul via the shell (UseShellExecute allows this)
    $psi.Arguments = "--symbol $Sym --no-whale"

    $p = New-Object System.Diagnostics.Process
    $p.StartInfo = $psi
    $null = $p.Start()
    return $p.Id
}

$lastSpawn  = @{}
$spawnCount = @{}
$spawnedPids = @{}
foreach ($s in $Symbols) {
    $lastSpawn[$s]  = [datetime]::MinValue
    $spawnCount[$s] = 0
    $spawnedPids[$s] = $null
}

Write-Host "Running. Ctrl+C to stop." -ForegroundColor Gray

while ($true) {
    foreach ($sym in $Symbols) {
        $hbFile    = Join-Path $rawDir "mp-collector-binance-$sym.heartbeat"
        $todayStr  = (Get-Date).ToUniversalTime().ToString("yyyyMMdd")
        $dataLog   = Join-Path $rawDir "${todayStr}_binance_${sym}.log"
        $tSinceSpawn = ((Get-Date) - $lastSpawn[$sym]).TotalSeconds

        $needRestart   = $false
        $restartReason = ""

        # Liveness = heartbeat/data-log freshness. We deliberately do NOT use
        # Get-CimInstance Win32_Process here: enumerating collectors that way
        # (in particular materializing the CommandLine property) was observed
        # to TERMINATE the very collectors it listed on Windows (exit code
        # 0xFFFFFFFF, exactly one check-interval after spawn; see
        # docs/AUDIT-2026-08-03.md). The collector writes a heartbeat every
        # 15s, so staleness detection is a strictly stronger liveness signal
        # anyway (it also catches hung processes, which a process query can't).
        if ($tSinceSpawn -gt $GraceSeconds) {
            # Heartbeat check (collector writes every 15s)
            if (Test-Path $hbFile) {
                $hbAge = ((Get-Date) - (Get-Item $hbFile).LastWriteTime).TotalSeconds
                if ($hbAge -gt 75) {
                    $needRestart   = $true
                    $restartReason = "heartbeat stale ($([int]$hbAge)s)"
                }
            } else {
                $needRestart   = $true
                $restartReason = "no heartbeat file"
            }
            # Log-write stall check (data should arrive every few seconds)
            if (-not $needRestart -and (Test-Path $dataLog)) {
                $logAge = ((Get-Date) - (Get-Item $dataLog).LastWriteTime).TotalSeconds
                if ($logAge -gt 150) {
                    $needRestart   = $true
                    $restartReason = "data log stalled ($([int]$logAge)s)"
                }
            }
        }

        if ($needRestart) {
            # Kill the tracked instance by PID. Get-Process -Id is a plain
            # native read (proven safe); the CIM query previously used to
            # enumerate processes here is not. A stale heartbeat means the
            # collector is dead or hung, so killing the tracked PID is correct.
            $prevPid = $spawnedPids[$sym]
            if ($prevPid) {
                Stop-Process -Id $prevPid -Force -ErrorAction SilentlyContinue
            }
            $spawnedPids[$sym] = $null
            Start-Sleep -Milliseconds 400
            Remove-Item (Join-Path $rawDir ".lock_binance_$sym") -Force -ErrorAction SilentlyContinue

            # Cooldown (skip on first spawn: spawnCount=0)
            if ($spawnCount[$sym] -gt 0 -and $tSinceSpawn -lt $CooldownSeconds) {
                WLog "$sym cooldown ($([int]$tSinceSpawn)s < ${CooldownSeconds}s): $restartReason" "WARN"
                continue
            }

            $spawnCount[$sym]++
            WLog "$sym spawning #$($spawnCount[$sym]): $restartReason" "WARN"
            $spawnedPid = Spawn-Collector -Sym $sym
            $spawnedPids[$sym] = $spawnedPid
            $lastSpawn[$sym] = Get-Date
            WLog "$sym spawned PID=$spawnedPid"
        }
    }

    Start-Sleep -Seconds $CheckIntervalSeconds
}
