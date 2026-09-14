# bybit_retention_check.ps1 - Nightly watchdog for the bybit hot-tier retention chain.
#
# Auto-tracks EVERY bybit day (present raws + journaled history) and verifies
# each day completes the W-6 retention chain as it crosses the 14-day hot-tier
# line (RETENTION_POLICY.md, daily_pipeline.ps1 section 2.7):
#
#   compact (INT-4 gate) -> cold proof (manifest + Parquet) -> mp-ops prune
#   (hash-verified, journaled to data/retention_delete_manifest.jsonl)
#
# On Windows ZERO_COST=1 is NOT set, so the pipeline's retention block never
# runs here: nothing auto-prunes the bybit era. This check is the forcing
# function - it reports, nightly, exactly which crossed days are waiting on
# the chain and which never compacted at all.
#
# Day states (auto-track window = [earliest bybit day, today UTC]):
#   HOT     date still inside the retention window (no action expected yet;
#           the first night the check demands action is day+RetentionDays+1).
#   OK      crossed + journaled in retention_delete_manifest.jsonl (pruned).
#           Reported as a summary count (history grows unbounded).
#   WARN    crossed + cold proof exists (manifest + Parquet) but prune has
#           not run - one `mp-ops prune` away from leaving raw. Also: a
#           window day absent from BOTH raw and journal within the trailing
#           horizon (see -AbsentHorizonDays) - a landing gap or a deletion
#           outside the journaled gate.
#   CRIT    crossed + no proof, no journal, no ack - never compacted; the
#           retention chain is stalled for it (P1).
#   ACKED   crossed + an ack entry in data/bybit_retention_acks.jsonl - the
#           owner documented why this day can never complete the chain (e.g.
#           bybit 2026-08-22 backpressure_loss quarantine, pre-VPS stubs).
#           Silent by design - the ack converts the nag into a counted line.
#
# Evidence mirrors the gate exactly (storage/src/layout.rs, prune.rs):
#   manifest  data/cold/manifests/venue=bybit/date={d}.json
#   parquet   data/cold/trades/venue=bybit/symbol={s}/date={d}/part-000.parquet
#   journal   data/retention_delete_manifest.jsonl (fields date/venue/symbol)
#
# Crossing rule: the day's filename date must be strictly older than
# (UTC-today - RetentionDays). Deterministic from the {YYYYMMDD} prefix -
# deliberately NOT LastWriteTime (the pipeline's mtime test is fine for its
# scan; a watchdog should not depend on touch-times).
#
# Usage:
#   .\ops\scripts\bybit_retention_check.ps1                      # auto-track all bybit days
#   .\ops\scripts\bybit_retention_check.ps1 -Telegram            # also notify on WARN/CRIT
#   .\ops\scripts\bybit_retention_check.ps1 -Register            # nightly 02:15 UTC task (with -Telegram)
#   .\ops\scripts\bybit_retention_check.ps1 -EarliestDate 2026-08-25 -LatestDate 2026-09-06
#                                                               # explicit window (tests / investigations)
#   .\ops\scripts\bybit_retention_check.ps1 -Ack 2026-08-22 -Reason "quarantined: backpressure_loss"
#                                                               # append an ack entry (human-invoked)
#   .\ops\scripts\bybit_retention_check.ps1 -RawDir X -ColdRoot Y -JournalPath Z [-AckPath W]
#                                                               # test overrides (scratch tree)
#
# Exit codes (mirrors mp_health.ps1 / data_backup_check.ps1):
#   0 = no WARN/CRIT state (acked days and pruned history do not count)
#   1 = warning   (prune pending / recent absent day)
#   2 = critical  (crossed day never compacted and not acknowledged)
#
# Telegram edge (2026-09-07, matches the storage-budget --telegram pattern):
#   with -Telegram, a WARN verdict dispatches severity p2 and a CRIT verdict
#   dispatches severity p1 through the framework's `mp-ops telegram-send`
#   (fail-closed: no TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID => honest log line,
#   never a fake send; always sends now - no quiet-hours batching, same as
#   the pipeline wrapper verdicts). Best-effort: a send failure is logged but
#   never changes the retention verdict/exit code. The -Register task action
#   includes -Telegram so the nightly run notifies.
#
# Safe to run any time; read-only (no mp-ops invocation, no deletes; the
# -Ack mode appends one line to the ack ledger).

param(
    [switch]$Register,                                   # register (or refresh) the nightly MoneyPrinterBybitRetentionCheck task
    [switch]$Telegram,                                   # notify via Telegram on WARN (p2) / CRIT (p1) - see header
    [string]$EarliestDate = "",                          # override window start (default: auto - earliest bybit day)
    [string]$LatestDate   = "",                          # override window end (default: auto - today UTC)
    [int]$RetentionDays   = 14,                          # hot-tier window (RETENTION_POLICY.md)
    [int]$AbsentHorizonDays = 0,                         # trailing horizon for absent-day WARN (0 = 2*RetentionDays)
    [string]$Ack          = "",                          # append an ack entry for a day (-Ack YYYY-MM-DD -Reason "...")
    [string]$Reason       = "",                          # required with -Ack
    [string]$RawDir       = "",                          # override for tests (default: <root>\data\raw)
    [string]$ColdRoot     = "",                          # override for tests (default: <root>\data\cold)
    [string]$JournalPath  = "",                          # override for tests (default: <root>\data\retention_delete_manifest.jsonl)
    [string]$AckPath      = ""                           # override for tests (default: <root>\data\bybit_retention_acks.jsonl)
)

$ErrorActionPreference = "Continue"
$TaskName = "MoneyPrinterBybitRetentionCheck"
$root     = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$rawDir   = if ($RawDir)       { $RawDir }       else { Join-Path $root "data\raw" }
$coldRoot = if ($ColdRoot)     { $ColdRoot }     else { Join-Path $root "data\cold" }
$journal  = if ($JournalPath)  { $JournalPath }  else { Join-Path $root "data\retention_delete_manifest.jsonl" }
$ackFile  = if ($AckPath)      { $AckPath }      else { Join-Path $root "data\bybit_retention_acks.jsonl" }
$logFile  = Join-Path $PSScriptRoot "bybit_retention_check.log"

$warn = @(); $crit = @()

function Out-Line([string]$mark, [string]$msg) {
    $color = switch ($mark) { '[ OK ]' {'Green'} '[WARN]' {'Yellow'} '[CRIT]' {'Red'} '[HOT ]' {'Gray'} '[ACK ]' {'Cyan'} default {'Gray'} }
    Write-Host ("{0,-7} {1}" -f $mark, $msg) -ForegroundColor $color
}

# ---- ack-ledger mode (append-only, human-invoked) -----------------------------
if ($Ack) {
    $day = $null
    try { $day = [DateTime]::ParseExact($Ack, "yyyy-MM-dd", $null) } catch { }
    if ($null -eq $day) {
        Write-Host "[CRIT] -Ack requires YYYY-MM-DD" -ForegroundColor Red
        exit 2
    }
    if (-not $Reason) {
        Write-Host "[CRIT] -Ack requires -Reason (why this day can never complete the chain)" -ForegroundColor Red
        exit 2
    }
    $line = @{ date = $Ack; reason = $Reason; ts_utc = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ") } | ConvertTo-Json -Compress
    Add-Content -Path $ackFile -Value $line -Encoding UTF8
    Write-Host ("[ACK ] appended {0}: {1}" -f $Ack, $Reason) -ForegroundColor Cyan
    Write-Host "       The day will report as acknowledged (silent, counted) instead of CRIT from the next run."
    exit 0
}

# ---- scheduled-task registration (mirrors data_backup_check.ps1) ------------
if ($Register) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(2).AddMinutes(15), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): pin the first trigger to tomorrow
    # when registering at/after the target time (0xFFFD0000 launch failure -
    # see daily_pipeline.ps1 -RegisterTask).
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument (
        "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`" -Telegram *> `"$logFile`"")
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Hours 1)
    Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
        -Settings $settings -User $env:USERNAME -Force | Out-Null
    Write-Host ("[OK] Registered {0} daily at {1} local (= {2} UTC, 15 min after the 02:00 drain window closes; 75 min before the 07:30 gate) with -Telegram" -f `
        $TaskName, $localAt.ToString("HH:mm"), $utcTarget.ToString("HH:mm")) -ForegroundColor Green
    Write-Host "     Re-run -Register after a DST change to re-pin the UTC window." -ForegroundColor Gray
    exit 0
}

# ---- load the retention journal once (date|venue|symbol -> entry) -----------
$journaled = @{}
if (Test-Path -LiteralPath $journal) {
    Get-Content -LiteralPath $journal -ErrorAction SilentlyContinue | ForEach-Object {
        try {
            $e = $_ | ConvertFrom-Json
            if ($e.date -and $e.venue -and $e.symbol) { $journaled["$($e.date)|$($e.venue)|$($e.symbol)"] = $true }
        } catch { }
    }
}

# ---- load the ack ledger (date -> reason; latest entry wins) -----------------
$acks = @{}
if (Test-Path -LiteralPath $ackFile) {
    Get-Content -LiteralPath $ackFile -ErrorAction SilentlyContinue | ForEach-Object {
        try {
            $e = $_ | ConvertFrom-Json
            if ($e.date) { $acks[$e.date] = $e.reason }
        } catch { }
    }
}

# ---- scan the watch window ----------------------------------------------------
$raws = @(Get-ChildItem -Path $rawDir -Filter "*_bybit_*.log" -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^(\d{8})_bybit_([A-Z0-9]+)\.log$' })
$hot = @(); $okDays = @{}; $pending = @(); $stalled = @(); $ackedDays = @{}
$heldBytes = 0L
$today = (Get-Date).ToUniversalTime()
$todayFlat = $today.ToString("yyyyMMdd")

# Auto-track window bounds: earliest bybit day across raws + journal, today's end.
$allDays = @()
foreach ($f in $raws) { $allDays += [regex]::Match($f.Name, '^(\d{8})').Groups[1].Value }
foreach ($k in $journaled.Keys) { if ($k -match '\|bybit\|') { $allDays += ($k -split '\|')[0].Replace('-', '') } }
if ($EarliestDate) {
    $windowLo = [DateTime]::ParseExact($EarliestDate, "yyyy-MM-dd", $null)
} elseif ($allDays.Count -gt 0) {
    $windowLo = [DateTime]::ParseExact(($allDays | Sort-Object | Select-Object -First 1), "yyyyMMdd", $null)
} else {
    $windowLo = $today.Date
}
$windowHi = if ($LatestDate) { [DateTime]::ParseExact($LatestDate, "yyyy-MM-dd", $null) } else { $today.Date }
$cutoff   = $today.Date.AddDays(-$RetentionDays)
$horizon  = if ($AbsentHorizonDays -gt 0) { $AbsentHorizonDays } else { 2 * $RetentionDays }
$horizonStart = $today.Date.AddDays(-$horizon)

Write-Host ("=== bybit retention check  {0}  (auto window {1}..{2}, {3}-day hot tier, absent horizon {4}d) ===" -f `
    $today.ToString("yyyy-MM-dd HH:mm"), $windowLo.ToString("yyyy-MM-dd"), $windowHi.ToString("yyyy-MM-dd"), $RetentionDays, $horizon)
Write-Host ""

foreach ($f in $raws) {
    $m = [regex]::Match($f.Name, '^(\d{8})_bybit_([A-Z0-9]+)\.log$')
    $dateFlat = $m.Groups[1].Value
    $symbol   = $m.Groups[2].Value
    $dash     = "{0}-{1}-{2}" -f $dateFlat.Substring(0,4), $dateFlat.Substring(4,2), $dateFlat.Substring(6,2)
    $day      = [DateTime]::ParseExact($dash, "yyyy-MM-dd", $null)
    if ($day -lt $windowLo -or $day -gt $windowHi) { continue }

    if ($day -ge $cutoff) {
        # Strict-< rule: hot while today-day <= RetentionDays; the first night
        # the check demands action is day+RetentionDays+1.
        $hot += ,@{
            Dash    = $dash; Symbol = $symbol
            HotThru = $day.AddDays($RetentionDays).ToString("yyyy-MM-dd")
            Demand  = $day.AddDays($RetentionDays + 1).ToString("yyyy-MM-dd")
        }
        continue
    }

    # ---- crossed the line: verify the chain -----------------------------------
    if ($journaled["$dash|bybit|$symbol"]) {
        $okDays[$dash] = $true
        continue
    }
    if ($acks.ContainsKey($dash)) {
        $ackedDays[$dash] = $acks[$dash]
        $heldBytes += $f.Length
        continue
    }
    $manifest = Join-Path $coldRoot "manifests\venue=bybit\date=$dash.json"
    $parquet  = Join-Path $coldRoot "trades\venue=bybit\symbol=$symbol\date=$dash\part-000.parquet"
    $proof    = (Test-Path -LiteralPath $manifest) -and (Test-Path -LiteralPath $parquet)
    if ($proof) {
        $pending += ,@{ Dash = $dash; Symbol = $symbol; Size = $f.Length }
    } else {
        $stalled += ,@{ Dash = $dash; Symbol = $symbol; Size = $f.Length }
    }
    $heldBytes += $f.Length
}

# ---- absent days: neither raw nor journal (nor ack) ---------------------------
# Within the trailing horizon: WARN (landing gap / unjournaled deletion).
# Older than the horizon: historical fact - one summary count, never nagged.
$rawDays = @{}; $journalDays = @{}
foreach ($f in $raws) { $rawDays[[regex]::Match($f.Name, '^(\d{8})').Groups[1].Value] = $true }
foreach ($k in $journaled.Keys) {
    if ($k -match '\|bybit\|') {
        $jDate = ($k -split '\|')[0].Replace('-', '')
        $journalDays[$jDate] = $true
        # A journaled bybit day IS the pruned-history record even when its raw
        # is long gone (the raw loop never sees it) - count it as OK.
        $okDays[[DateTime]::ParseExact($jDate, "yyyyMMdd", $null).ToString("yyyy-MM-dd")] = $true
    }
}
$historicalGaps = 0; $absent = @()
for ($d = $windowLo; $d -le $windowHi; $d = $d.AddDays(1)) {
    $flat = $d.ToString("yyyyMMdd")
    if ($flat -ge $todayFlat) { break }            # future/partial days not expected yet
    if ($rawDays[$flat] -or $journalDays[$flat]) { continue }
    $dashD = $d.ToString("yyyy-MM-dd")
    if ($acks.ContainsKey($dashD)) { $ackedDays[$dashD] = $acks[$dashD]; continue }
    if ($d -ge $horizonStart) {
        $absent += ,@{ Dash = $d.ToString("yyyy-MM-dd") }
    } else {
        $historicalGaps++
    }
}

# ---- report ------------------------------------------------------------------
foreach ($h in $hot) { Out-Line "[HOT ]" ("{0} {1} hot through {2} - prune expected from the {3} run" -f $h.Dash, $h.Symbol, $h.HotThru, $h.Demand) }
foreach ($a in $ackedDays.GetEnumerator() | Sort-Object Name) {
    Out-Line "[ACK ]" ("{0} acknowledged: {1}" -f $a.Key, $a.Value)
}
foreach ($p in $pending) {
    Out-Line "[WARN]" ("{0} {1} COMPACTED but prune pending - proof verified, run: mp-ops prune --date {0} --venue bybit --symbol {1}" -f $p.Dash, $p.Symbol)
    $warn += "prune pending: $($p.Dash) $($p.Symbol)"
}
foreach ($a in $absent) {
    Out-Line "[WARN]" ("{0}: no bybit raw and no retention journal entry - never landed or deleted outside the journaled gate (check drain manifest + mirrors)" -f $a.Dash)
    $warn += "absent unjournaled: $($a.Dash)"
}
foreach ($s in $stalled) {
    Out-Line "[CRIT]" ("{0} {1} crossed the {2}-day line and was NEVER compacted - retention stalled (gate refusal or chain not run; ack with -Ack {0} -Reason ... if intentional)" -f $s.Dash, $s.Symbol, $RetentionDays)
    $crit += "never compacted: $($s.Dash) $($s.Symbol)"
}

if ($okDays.Count -gt 0) {
    $newest = ($okDays.Keys | Sort-Object | Select-Object -Last 1)
    Out-Line "[ OK ]" ("{0} pruned day(s) journaled (newest {1})" -f $okDays.Count, $newest)
}
if ($historicalGaps -gt 0) {
    Out-Line "  ..." ("{0} historical bybit gap day(s) before {1} (pre-recording or long-explained - not nagged)" -f $historicalGaps, $horizonStart.ToString("yyyy-MM-dd"))
}
if ($heldBytes -gt 0) {
    Out-Line "  ..." ("{0:N2} GiB still held by crossed days not yet pruned (WARN/CRIT/ACK above)" -f ($heldBytes / 1GB))
}

# ---- persist a timestamped verdict line (gitignored *.log) -------------------
$verdict = if ($crit.Count -gt 0) { "CRIT" } elseif ($warn.Count -gt 0) { "WARN" } else { "OK" }
$detail  = if ($crit.Count -gt 0) { $crit -join "; " } elseif ($warn.Count -gt 0) { $warn -join "; " } else {
    "clean: {0} hot, {1} pruned, {2} acked, {3} historical gaps" -f $hot.Count, $okDays.Count, $ackedDays.Count, $historicalGaps
}
Add-Content -Path $logFile -Value ("[{0}][{1}] {2}" -f $today.ToString("yyyy-MM-ddTHH:mm:ssZ"), $verdict, $detail) -Encoding UTF8

# ---- Telegram edge (2026-09-07) ------------------------------------------------
# WARN => p2, CRIT => p1 via the framework's telegram-send (fail-closed: no
# credentials => honest log line, never a fake send). Best-effort: a send
# failure is logged and surfaced as a WARN detail, never changes the verdict.
$telegramNote = ""
if ($Telegram -and ($verdict -eq "WARN" -or $verdict -eq "CRIT")) {
    $sev = if ($verdict -eq "CRIT") { "p1" } else { "p2" }
    $mpOps = Join-Path $root "target\release\mp-ops.exe"
    if (-not (Test-Path -LiteralPath $mpOps)) {
        $telegramNote = "mp-ops not found ($mpOps) - telegram skipped"
        Out-Line "[WARN]" $telegramNote
    } elseif (-not $env:TELEGRAM_BOT_TOKEN -or -not $env:TELEGRAM_CHAT_ID) {
        $telegramNote = "telegram unconfigured (TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID unset) - no notification sent"
        Out-Line "[WARN]" $telegramNote
    } else {
        # Bounded detail: first 8 items + tallies so the message stays short.
        $items = @($crit + $warn | Select-Object -First 8)
        $held = if ($heldBytes -gt 0) { "; crossed days hold {0:N2} GiB" -f ($heldBytes / 1GB) } else { "" }
        $text = "bybit retention {0} (window {1}..{2}): {3}{4}" -f $verdict, $windowLo.ToString("yyyy-MM-dd"), $windowHi.ToString("yyyy-MM-dd"), ($items -join "; "), $held
        $sendArgs = @("telegram-send", "--id", "bybit-retention-check", "--severity", $sev, "--detail", $text)
        # PS 5.1 native-stderr trap (audit 2026-09-07): a failing native
        # command's stderr becomes ErrorRecords that garble `2>&1` captures
        # and even `2> file` renders. mp-ops prints its ERROR line to STDOUT,
        # so: suppress stderr, capture stdout, override EAP around the call
        # (the project's ssh-call pattern).
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = "SilentlyContinue"
        $sendOut = (& $mpOps $sendArgs 2> $null | Out-String).Trim()
        $ErrorActionPreference = $prevEap
        if ($LASTEXITCODE -eq 0) {
            $telegramNote = "telegram sent ($sev)"
            Out-Line "[ OK ]" "telegram $sev dispatch sent"
        } else {
            $telegramNote = "telegram send FAILED (exit $LASTEXITCODE): $sendOut"
            Out-Line "[WARN]" $telegramNote
        }
    }
    if ($telegramNote) {
        Add-Content -Path $logFile -Value ("[{0}][TG ] {1}" -f $today.ToString("yyyy-MM-ddTHH:mm:ssZ"), $telegramNote) -Encoding UTF8
    }
}

Write-Host ""
if ($crit.Count -gt 0) {
    Out-Line "[CRIT]" "CRITICAL: $($crit -join '; ')"
    exit 2
} elseif ($warn.Count -gt 0) {
    Out-Line "[WARN]" "WARNINGS: $($warn -join '; ')"
    exit 1
} else {
    Out-Line "[ OK ]" "All green - $($hot.Count) hot, $($okDays.Count) pruned, $($ackedDays.Count) acked, $historicalGaps historical gap day(s)"
    exit 0
}