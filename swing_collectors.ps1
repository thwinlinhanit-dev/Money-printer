# swing_collectors.ps1 - Launch + supervise the SWING-ONLY collector set.
#
# A higher-timeframe (daily/4h) trading focus requires only a fraction of the
# full recorder: spec 035 SWG-1 data is trades (OHLCV), funding/mark/OI, and
# liquidations (daily aggregates) + a cross-asset reference. Swing features
# must never depend on L2 order-book depth or tick-tape input (spec 035
# Non-Goals), so this set runs every market-data collector in `swing_only`
# mode (drops the L2 book stream - the dominant disk consumer) and adds the
# low-cadence Tier-2 context collectors (FRED macro + whale census).
#
# The supervised set is HARDCODED here (vs. the Phase-0 gate in
# ops/core_symbols.txt) because a swing_only recording intentionally omits the
# gate-required `book` stream: this corpus is for swing research/backtests,
# NOT the Phase-0 promotion gate. Run it instead of (or alongside) the full
# recorder when you only care about HTF trading.
#
# Usage:
#   .\swing_collectors.ps1               # build + supervise (foreground)
#   .\swing_collectors.ps1 -RegisterTask # register as a Scheduled Task
# Stop: Stop-Process -Name mp-collector,mp-whale,mp-macro -Force
#        Stop-ScheduledTask MoneyPrinterSwingCollectorsWatchdog
#
# Requires FRED_API_KEY in the environment for the macro leg (spec 030 MAC-2,
# PD-2). The market + whale legs need no credentials.

param(
    [switch]$RegisterTask,
    [switch]$AsSystem,
    [int]$CheckIntervalSeconds = 20,
    [int]$CooldownSeconds      = 90,
    [int]$GraceSeconds         = 45
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }

$collectorExe = Join-Path $root "target\release\mp-collector.exe"
$whaleExe     = Join-Path $root "target\release\mp-whale.exe"
$macroExe     = Join-Path $root "target\release\mp-macro.exe"
$swingCfg     = Join-Path $root "collectors\swing"

# ---- supervised process table -----------------------------------------------
# MARKET-DATA COLLECTORS run in swing_only mode. IMPORTANT (Option 1 / Phase-0
# co-existence): hyperliquid:BTC and hyperliquid:ETH are NOT supervised here -
# the Phase-0 recorder (ops/core_symbols.txt + watchdog/VPS mp-hyperliquid@)
# already records them WITH the book stream (a superset of swing's needs:
# trades + activeAssetCtx, plus book), so swing re-recording them would collide
# on .lock_hyperliquid_* / the same log file. Swing consumes the Phase-0 HL
# recordings; this launcher adds only what Phase-0 does NOT cover: Bybit
# (credential-free live liquidations + cross-asset breadth) + whale + macro.
# (Standalone HL swing_only configs remain in collectors/swing/ for an isolated
# host that runs swing WITHOUT the Phase-0 recorder.)
$supervised = @(
    # Tier 1: swing market data (swing_only = trades + funding/mark/OI [+ liq on bybit])
    #   hyperliquid:BTC / ETH -> covered by the Phase-0 recorder (see note above)
    @{ id = "bybit:BTCUSDT";   kind = 'collector'; exe = $collectorExe; cfg = "bybit-btcusdt.toml";
       heartbeat = "mp-collector-bybit-BTCUSDT.heartbeat";      lock = ".lock_bybit_BTCUSDT";      dataLog = "{date}_bybit_BTCUSDT.log" }
    @{ id = "bybit:ETHUSDT";   kind = 'collector'; exe = $collectorExe; cfg = "bybit-ethusdt.toml";
       heartbeat = "mp-collector-bybit-ETHUSDT.heartbeat";      lock = ".lock_bybit_ETHUSDT";      dataLog = "{date}_bybit_ETHUSDT.log" }
    @{ id = "bybit:SOLUSDT";   kind = 'collector'; exe = $collectorExe; cfg = "bybit-solusdt.toml";
       heartbeat = "mp-collector-bybit-SOLUSDT.heartbeat";      lock = ".lock_bybit_SOLUSDT";      dataLog = "{date}_bybit_SOLUSDT.log" }
    # Tier 2: low-cadence context (swing edge builders)
    @{ id = "whale"; kind = 'whale'; exe = $whaleExe; cfg = "whale_positions.toml";
       heartbeat = "mp-collector-hyperliquid_positions.heartbeat"; lock = ".lock_hyperliquid_positions"; dataLog = "{date}_hyperliquid_positions.log" }
    @{ id = "macro"; kind = 'macro'; exe = $macroExe; cfg = "macro.toml";
       heartbeat = "mp-macro.heartbeat"; lock = ".lock_macro"; dataLog = "{date}_fred_macro.log" }
)

function HLog {
    param([string]$Msg, [string]$Level = "INFO")
    $raw = Join-Path $root "data\raw"
    $log = Join-Path $raw ("swing_watchdog_" + (Get-Date).ToUniversalTime().ToString("yyyyMMdd") + ".log")
    $ts  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    Add-Content -Path $log -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") {"Yellow"} elseif ($Level -eq "ERROR") {"Red"} else {"White"}
    Write-Host $line -ForegroundColor $col
}

# ---- single-instance guard ---------------------------------------------------
$created = $false
$mutex = [System.Threading.Mutex]::new($true, 'Local\MoneyPrinterSwingCollectorsWatchdog', [ref]$created)
if (-not $created) {
    Write-Host "[INFO] MoneyPrinterSwingCollectorsWatchdog already running; exiting duplicate invocation." -ForegroundColor Yellow
    Exit 0
}
# ---- task registration -------------------------------------------------------
if ($RegisterTask) {
    $TaskName = "MoneyPrinterSwingCollectorsWatchdog"
    Write-Host "Registering: $TaskName" -ForegroundColor Yellow
    $Action = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSScriptRoot\swing_collectors.ps1`""
    $Triggers = @(
        (New-ScheduledTaskTrigger -AtStartup),
        (New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME)
    )
    $Settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit (New-TimeSpan -Days 3650) `
        -RestartCount 99 -RestartInterval (New-TimeSpan -Minutes 1) `
        -MultipleInstances IgnoreNew
    if ($AsSystem) {
        Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Triggers -Settings $Settings -RunLevel Highest -User "SYSTEM" -Force
        Write-Host "[OK] Registered as SYSTEM." -ForegroundColor Green
    } else {
        try {
            Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Triggers -Settings $Settings -User $env:USERNAME -Force -ErrorAction Stop
            Write-Host "[OK] Registered for $($env:USERNAME) (startup + logon)." -ForegroundColor Green
        } catch {
            Write-Host "AtStartup trigger denied (not elevated) - falling back to logon-only." -ForegroundColor Yellow
            $LogonOnly = @(New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME)
            Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $LogonOnly -Settings $Settings -User $env:USERNAME -Force -ErrorAction Stop
            Write-Host "[OK] Registered for $($env:USERNAME) (logon-only)." -ForegroundColor Green
        }
    }
    Exit 0
}

# ---- build binaries (best-effort; running processes lock their exe) ---------
Write-Host "Building binaries..." -ForegroundColor Yellow
Set-Location $root
foreach ($b in @(
    @{ exe = $collectorExe; args = @("--features","live-ws,live-http","--bin","mp-collector","--release") },
    @{ exe = $whaleExe;     args = @("--features","live-http","--bin","mp-whale","--release") },
    @{ exe = $macroExe;     args = @("--features","live-http","--bin","mp-macro","--release") }
)) {
    cargo build -p mp-collectors @($b.args) 2>&1 | Where-Object { $_ -match "Compiling|Finished|error" }
    if ($LASTEXITCODE -ne 0) {
        if (Test-Path $b.exe) {
            Write-Host "Build failed for $(Split-Path $b.exe -Leaf) (exe likely locked by a running process) - continuing with existing binary." -ForegroundColor Yellow
        } else {
            Write-Host "Build failed and no binary exists for $(Split-Path $b.exe -Leaf) - cannot supervise." -ForegroundColor Red
            Exit 1
        }
    }
}
Write-Host "Binaries ready." -ForegroundColor Green

$rawDir = Join-Path $root "data\raw"
if (-not (Test-Path $rawDir)) { New-Item -ItemType Directory -Path $rawDir -Force | Out-Null }

function Spawn-Process {
    param([string]$ExePath, [string]$ArgString)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName         = $ExePath
    $psi.Arguments        = $ArgString
    $psi.WorkingDirectory = [string]$root
    $psi.UseShellExecute  = $true
    $psi.WindowStyle      = [System.Diagnostics.ProcessWindowStyle]::Hidden
    $p = New-Object System.Diagnostics.Process
    $p.StartInfo = $psi
    $null = $p.Start()
    return $p.Id
}

$ids = @($supervised | ForEach-Object { $_.id })
HLog "Swing-only watchdog started. Set: $($ids -join ',')"
if (-not $env:FRED_API_KEY) { HLog "WARNING: FRED_API_KEY not set - the macro leg will fail to start (spec 030 MAC-2)." "WARN" }

$lastSpawn  = @{}; $spawnCount = @{}; $spawnedPids = @{}; $lastDataLen = @{}; $lastDataLenTs = @{}
foreach ($s in $supervised) {
    $lastSpawn[$s.id] = [datetime]::MinValue; $spawnCount[$s.id] = 0; $spawnedPids[$s.id] = $null
    $lastDataLen[$s.id] = 0; $lastDataLenTs[$s.id] = [datetime]::MinValue
}

Write-Host "Running. Ctrl+C to stop." -ForegroundColor Gray
while ($true) {
    $todayStr = (Get-Date).ToUniversalTime().ToString("yyyyMMdd")
    foreach ($s in $supervised) {
        $hbFile      = Join-Path $rawDir $s.heartbeat
        $dataLog     = Join-Path $rawDir ($s.dataLog.Replace('{date}', $todayStr))
        $tSinceSpawn = ((Get-Date) - $lastSpawn[$s.id]).TotalSeconds

        $needRestart = $false; $restartReason = ""

        if ($tSinceSpawn -gt $GraceSeconds) {
            if (Test-Path $hbFile) {
                $hbAge = ((Get-Date) - (Get-Item $hbFile).LastWriteTime).TotalSeconds
                if ($hbAge -gt 75) { $needRestart = $true; $restartReason = "heartbeat stale ($([int]$hbAge)s)" }
            } else {
                $needRestart = $true; $restartReason = "no heartbeat file"
            }
            if (-not $needRestart -and (Test-Path $dataLog)) {
                $logAge = ((Get-Date) - (Get-Item $dataLog).LastWriteTime).TotalSeconds
                if ($logAge -gt 150) { $needRestart = $true; $restartReason = "data log stalled ($([int]$logAge)s)" }
            }
        }

        if ($needRestart) {
            if ($spawnedPids[$s.id]) { Stop-Process -Id $spawnedPids[$s.id] -Force -ErrorAction SilentlyContinue }
            $spawnedPids[$s.id] = $null
            Start-Sleep -Milliseconds 400
            Remove-Item (Join-Path $rawDir $s.lock) -Force -ErrorAction SilentlyContinue
            $lastDataLen[$s.id] = 0; $lastDataLenTs[$s.id] = [datetime]::MinValue

            if ($spawnCount[$s.id] -gt 0 -and $tSinceSpawn -lt $CooldownSeconds) {
                HLog "$($s.id) cooldown ($([int]$tSinceSpawn)s < ${CooldownSeconds}s): $restartReason" "WARN"
                continue
            }
            $spawnCount[$s.id]++
            $cfgPath = Join-Path $swingCfg $s.cfg
            $argStr = "--config `"$cfgPath`""
            $spawnedPids[$s.id] = Spawn-Process -ExePath $s.exe -ArgString $argStr
            $lastSpawn[$s.id] = Get-Date
            HLog "$($s.id) spawned #$($spawnCount[$s.id]) (PID $($spawnedPids[$s.id])): $restartReason"
        }
    }
    Start-Sleep -Seconds $CheckIntervalSeconds
}

