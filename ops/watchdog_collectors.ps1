# watchdog_collectors.ps1  (SPEC-019)
# 24/7 supervisor: restarts mp-collector if it crashes, stops writing, or its
# heartbeat goes stale.
#
# Run in foreground:
#   .\ops\watchdog_collectors.ps1
#
# Register as a Scheduled Task (survives reboot; starts at user logon):
#   .\ops\watchdog_collectors.ps1 -RegisterTask
#   (registers for the logged-on user, no elevation needed. The task is
#    interactive: it starts when that user logs on. -AsSystem needs an
#    elevated prompt and is NOT recommended here - the watchdog runs
#    `cargo build` and rustup/cargo are installed per-user under
#    $env:USERPROFILE, so a SYSTEM-context run has no cargo on PATH.)
#   .\ops\watchdog_collectors.ps1 -RegisterTask -AsSystem
#
# Recordings come from ops/core_symbols.txt as `venue:SYMBOL` lines (a plain
# SYMBOL line means the binance venue).  One live `mp-collector` per line.
# The Hyperliquid on-chain whale census (`mp-whale`, spec 028) is supervised
# alongside them - its log feeds the liq-est band research edge (spec 029).

param(
    [switch]$RegisterTask,
    [switch]$AsSystem,
    [string[]]$Recordings      = @(),  # venue:symbol pairs; empty = ops/core_symbols.txt
    [int]$CheckIntervalSeconds = 20,
    [int]$CooldownSeconds      = 90,
    [int]$GraceSeconds         = 45
)

# Recordings default: empty = load the single source of truth from the
# shared parser (ops/scripts/recordings.ps1, dot-sourced below — the SAME
# resolver start_collectors.ps1 and scripts/daily_pipeline.ps1 use, so
# recorded == required can never drift; audit 08-10).  `venue:SYMBOL` per
# line, a plain SYMBOL means binance; falls back to hyperliquid:BTC+ETH when
# the file is absent.  An explicit -Recordings override goes through the same
# parser (which emits one object per entry — no array double-wrap).
. (Join-Path $PSScriptRoot "scripts\recordings.ps1")

$recPairs = @()
try {
    if ($Recordings.Count -eq 0) {
        $recPairs = @(Resolve-Recordings -CoreFile (Join-Path $PSScriptRoot "core_symbols.txt"))
    } else {
        $recPairs = @(Resolve-Recordings -Raw $Recordings)
    }
} catch {
    Write-Host "[!!] $_" -ForegroundColor Red
    Exit 1
}

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$collectorExe = Join-Path $root "target\release\mp-collector.exe"
$whaleExe     = Join-Path $root "target\release\mp-whale.exe"
$whaleConfig  = Join-Path $root "collectors\whale_positions.toml"

# ---- supervised process table -------------------------------------------------
# Each entry is a hashtable-ish object: kind (collector|whale), venue, symbol,
# exe + base args, and the filesystem names used for liveness.  Note the two
# binaries name their artifacts differently: mp-collector writes its heartbeat
# as mp-collector-{venue}-{symbol} (dash) but its instance lock as
# .lock_{venue}_{symbol} (underscore); mp-whale uses binutil's underscore
# naming throughout - kept explicit here so a name change in one binary cannot
# silently break supervision.
$supervised = @()
foreach ($r in $recPairs) {
    $supervised += [pscustomobject]@{
        id        = "$($r.venue):$($r.symbol)"
        kind      = 'collector'
        venue     = $r.venue
        symbol    = $r.symbol
        exe       = $collectorExe
        args      = $null
        heartbeat = "mp-collector-$($r.venue)-$($r.symbol).heartbeat"
        lock      = ".lock_$($r.venue)_$($r.symbol)"
        dataLog   = "{date}_$($r.venue)_$($r.symbol).log"
        trace     = "trace_{date}_$($r.venue)_$($r.symbol).log"
    }
}
# spec 028 whale census (Hyperliquid public REST, no auth).  Log:
# {date}_hyperliquid_positions.log; heartbeat mp-collector-hyperliquid_positions.heartbeat.
$supervised += [pscustomobject]@{
    id        = 'whale'
    kind      = 'whale'
    venue     = 'hyperliquid'
    symbol    = 'positions'
    exe       = $whaleExe
    args      = "--config `"$whaleConfig`""
    heartbeat = 'mp-collector-hyperliquid_positions.heartbeat'
    lock      = '.lock_hyperliquid_positions'
    dataLog   = '{date}_hyperliquid_positions.log'
    trace     = $null
}

# Spawn args per venue.  Binance keeps the spec 024 REST mitigations
# (trade_source/mark_source=rest); other venues (hyperliquid) are pure WS -
# trade_source=rest requires a Binance normalizer, so it must NOT be passed.
foreach ($s in $supervised) {
    if ($s.kind -eq 'collector' -and $s.venue -eq 'binance') {
        $s.args = "--symbol `"$($s.symbol)`" --trade-source rest --mark-source rest"
    } elseif ($s.kind -eq 'collector') {
        $s.args = "--venue `"$($s.venue)`" --symbol `"$($s.symbol)`""
    }
}

# ---- task registration --------------------------------------------------------
if ($RegisterTask) {
    $TaskName = "MoneyPrinterCollectorsWatchdog"
    Write-Host "Registering: $TaskName" -ForegroundColor Yellow
    $Action = New-ScheduledTaskAction `
        -Execute  "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSScriptRoot\watchdog_collectors.ps1`""
    # AtLogOn scoped to the registering user; for an interactive task the
    # AtStartup trigger only fires once that user is logged on. Setting
    # MultipleInstances=IgnoreNew collapses the boot+logon trigger pair into
    # a single watchdog instance (no duplicate supervisors).
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
        Register-ScheduledTask -TaskName $TaskName -Action $Action `
            -Trigger $Triggers -Settings $Settings -RunLevel Highest -User "SYSTEM" -Force
        Write-Host "[OK] Registered as SYSTEM." -ForegroundColor Green
    } else {
        # No -RunLevel Highest: the collectors need no elevation (they only
        # write under data\raw), and registering with RunLevel Highest would
        # require an elevated prompt. Standard users cannot create tasks with
        # an AtStartup trigger (access denied - verified 2026-08-04), so we
        # try Startup+Logon first and fall back to Logon-only when denied.
        # Either way the task is interactive: it starts when $env:USERNAME
        # logs on, which is the right model for a per-user cargo/rustup
        # install.
        try {
            Register-ScheduledTask -TaskName $TaskName -Action $Action `
                -Trigger $Triggers -Settings $Settings -User $env:USERNAME -Force -ErrorAction Stop
            Write-Host "[OK] Registered for $($env:USERNAME) (startup + logon)." -ForegroundColor Green
        } catch {
            Write-Host "AtStartup trigger denied (not elevated) - falling back to logon-only." -ForegroundColor Yellow
            $LogonOnly = @(New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME)
            Register-ScheduledTask -TaskName $TaskName -Action $Action `
                -Trigger $LogonOnly -Settings $Settings -User $env:USERNAME -Force -ErrorAction Stop
            Write-Host "[OK] Registered for $($env:USERNAME) (logon-only)." -ForegroundColor Green
        }
    }
    Exit 0
}

# One watchdog owns collector supervision. Task Scheduler's IgnoreNew setting
# only protects duplicate task launches; it cannot prevent a foreground/manual
# invocation from racing the scheduled task and restarting the same processes.
$watchdogCreated = $false
$watchdogMutex = [System.Threading.Mutex]::new(
    $true,
    'Local\MoneyPrinterCollectorsWatchdog',
    [ref]$watchdogCreated
)
if (-not $watchdogCreated) {
    Write-Host "[INFO] MoneyPrinterCollectorsWatchdog is already running; exiting duplicate invocation." -ForegroundColor Yellow
    Exit 0
}

# ---- foreground loop ----------------------------------------------------------
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "  MONEY PRINTER COLLECTOR WATCHDOG" -ForegroundColor Cyan
Write-Host "==========================================" -ForegroundColor Cyan

Set-Location $root
Write-Host "Building binaries..." -ForegroundColor Yellow
# Best-effort rebuild (audit 08-08): a running collector holds its exe open on
# Windows, so ANY crate change makes cargo's relink fail with "Access is
# denied" (os error 5). The rebuild is a convenience, not a liveness
# requirement - supervision only needs *some* binary on disk. Fail soft:
# warn and continue with the existing binary (the running processes are
# already executing it). Only abort if there is no binary at all.
foreach ($b in @(
    @{ name = "mp-collector"; exe = $collectorExe; args = @("--features", "live-ws,live-http", "--bin", "mp-collector", "--release") },
    @{ name = "mp-whale";    exe = $whaleExe;    args = @("--features", "live-http", "--bin", "mp-whale", "--release") }
)) {
    # A bare string variable would be passed as ONE argument (PowerShell
    # never splits it); array-splat instead so each flag stays separate.
    $buildArgs = $b.args
    cargo build -p mp-collectors @buildArgs 2>&1 | Where-Object { $_ -match "Compiling|Finished|error" }
    if ($LASTEXITCODE -ne 0) {
        if (Test-Path $b.exe) {
            Write-Host "$($b.name) rebuild failed (exe likely locked by a running process) - continuing with existing binary." -ForegroundColor Yellow
        } else {
            Write-Host "$($b.name) build failed and no binary exists - cannot supervise." -ForegroundColor Red
            Exit 1
        }
    }
}
Write-Host "Binaries ready: $collectorExe / $whaleExe" -ForegroundColor Green

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

$ids = @($supervised | ForEach-Object { $_.id })
WLog "Watchdog started. Recordings: $($ids -join ',')  cooldown=${CooldownSeconds}s  grace=${GraceSeconds}s"

function Spawn-Process {
    param([string]$ExePath, [string]$ArgString)
    # KEY FIX: UseShellExecute=true fully detaches from this process's stdio.
    # No pipe is created, so the collector never blocks on stdout.
    # Args are built from validated alphanumeric venue/symbol values and
    # double-quoted here (audit 08-04 anti-injection).
    # --trace-file: the collector writes its tracing logs itself (append-only,
    # per-day file) because stderr is discarded by the detachment - keeps the
    # freeze diagnostics from a respawn cycle instead of losing them.
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

$lastSpawn  = @{}
$spawnCount = @{}
$spawnedPids = @{}
$lastDataLen   = @{}
$lastDataLenTs = @{}
foreach ($s in $supervised) {
    $lastSpawn[$s.id]  = [datetime]::MinValue
    $spawnCount[$s.id] = 0
    $spawnedPids[$s.id] = $null
    $lastDataLen[$s.id]   = 0
    $lastDataLenTs[$s.id] = [datetime]::MinValue
}

Write-Host "Running. Ctrl+C to stop." -ForegroundColor Gray

while ($true) {
    $todayStr = (Get-Date).ToUniversalTime().ToString("yyyyMMdd")
    foreach ($s in $supervised) {
        $hbFile    = Join-Path $rawDir $s.heartbeat
        $dataLog   = Join-Path $rawDir ($s.dataLog.Replace('{date}', $todayStr))
        $tSinceSpawn = ((Get-Date) - $lastSpawn[$s.id]).TotalSeconds

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
            # Heartbeat check (collectors write every 15s)
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
            # Log-write stall check (data should arrive every few seconds;
            # the whale census writes at least once per 60s top-N poll)
            if (-not $needRestart -and (Test-Path $dataLog)) {
                $logAge = ((Get-Date) - (Get-Item $dataLog).LastWriteTime).TotalSeconds
                if ($logAge -gt 150) {
                    $needRestart   = $true
                    $restartReason = "data log stalled ($([int]$logAge)s)"
                }
            }
            # Data-flow check (audit 08-08): a collector whose WS is dead but
            # keeps emitting Status::Stale grows its log by one tiny frame per
            # ~15s (~5 B/s), while a healthy HL feed grows it by hundreds of
            # B/s (trades + activeAssetCtx).  Neither the heartbeat (written
            # regardless of data) nor the log-stall check above (Stale events
            # keep LastWriteTime fresh) can see this - today's 10h stale
            # window (2026-08-08) is exactly that case - so track the growth
            # rate over the last window and restart on a sustained stall.
            # The collector's --trace-file sink is unbuffered, so its latest
            # reconnect/error line survives a watchdog restart for diagnosis.
            if (-not $needRestart -and $s.kind -eq 'collector' -and (Test-Path $dataLog)) {
                $curLen = (Get-Item $dataLog).Length
                if ($lastDataLenTs[$s.id] -ne [datetime]::MinValue) {
                    $elapsed = ((Get-Date) - $lastDataLenTs[$s.id]).TotalSeconds
                    if ($elapsed -ge 60) {
                        $growth = $curLen - $lastDataLen[$s.id]
                        $rate   = $growth / $elapsed
                        # Negative growth = torn-tail truncation on reopen;
                        # re-baseline without judging this window.
                        if ($growth -lt 0) {
                            $lastDataLen[$s.id]   = $curLen
                            $lastDataLenTs[$s.id] = Get-Date
                        } elseif ($rate -lt 20) {
                            $needRestart   = $true
                            $restartReason = "data flow stalled ({0:N1} B/s over {1:N0}s)" -f $rate, $elapsed
                        } else {
                            $lastDataLen[$s.id]   = $curLen
                            $lastDataLenTs[$s.id] = Get-Date
                        }
                    }
                } else {
                    $lastDataLen[$s.id]   = $curLen
                    $lastDataLenTs[$s.id] = Get-Date
                }
            }
        }

        if ($needRestart) {
            # Kill the tracked instance by PID. Get-Process -Id is a plain
            # native read (proven safe); the CIM query previously used to
            # enumerate processes here is not. A stale heartbeat means the
            # collector is dead or hung, so killing the tracked PID is correct.
            $prevPid = $spawnedPids[$s.id]
            if ($prevPid) {
                Stop-Process -Id $prevPid -Force -ErrorAction SilentlyContinue
            }
            $spawnedPids[$s.id] = $null
            Start-Sleep -Milliseconds 400
            Remove-Item (Join-Path $rawDir $s.lock) -Force -ErrorAction SilentlyContinue
            # A fresh process must get a fresh data-flow baseline: the old
            # process's baseline would otherwise make the first evaluation
            # after a restart judge the new process against stale growth
            # (audit 08-08: one false 'data flow stalled' on restart).
            $lastDataLen[$s.id]   = 0
            $lastDataLenTs[$s.id] = [datetime]::MinValue

            # Cooldown (skip on first spawn: spawnCount=0)
            if ($spawnCount[$s.id] -gt 0 -and $tSinceSpawn -lt $CooldownSeconds) {
                WLog "$($s.id) cooldown ($([int]$tSinceSpawn)s < ${CooldownSeconds}s): $restartReason" "WARN"
                continue
            }

            $spawnCount[$s.id]++
            WLog "$($s.id) spawning #$($spawnCount[$s.id]): $restartReason" "WARN"
            $traceArg = ""
            if ($s.kind -eq 'collector') {
                $tracePath = Join-Path $rawDir ($s.trace.Replace('{date}', $todayStr))
                $traceArg  = " --trace-file `"$tracePath`""
            }
            $spawnedPid = Spawn-Process -ExePath $s.exe -ArgString ($s.args + $traceArg)
            $spawnedPids[$s.id] = $spawnedPid
            $lastSpawn[$s.id] = Get-Date
            WLog "$($s.id) spawned PID=$spawnedPid"
        }
    }

    Start-Sleep -Seconds $CheckIntervalSeconds
}
