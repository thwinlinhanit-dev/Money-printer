# vps_backup.ps1 - Nightly pull of the VPS recorder corpus (vps-phase0-bringup.md sec 4).
# The Phase-0 recorder now lives on the VPS (/opt/money-printer/data). This script
# pulls that corpus to the Windows box so the 7-day streak's raw evidence is never a
# single-VM single-point-of-failure (W-6).
#
# Design constraints (mirrors backup_data.ps1):
#   - READ-ONLY on the source. The VPS side only reads + tars; nothing under
#     /opt/money-printer/data is ever deleted/renamed/mutated.
#   - Incremental by mtime: a marker (<dest>\vps-data\.last_backup_ts, epoch
#     seconds) filters the remote tar via GNU tar --newer-mtime. Raw logs are
#     append-only and the collector rotates at UTC midnight, so between runs only
#     the current day's log + that day's scorecard/features change. First run
#     (no marker) or -Force does a full copy.
#   - Transfer: `ssh 'bash ~/vps_pull.sh "<mtime>"' | tar -xf -` executed via
#     cmd /c so the tar stream stays binary-safe (PowerShell's text pipeline
#     would corrupt it).
#   - Integrity: remote delta-set count/bytes vs the local delta-set count/bytes.
#     The active day's log grows while we copy, so a small size delta is a NOTE;
#     source files missing from the destination are a failure (exit 2).
#   - Self-describing: appends JSONL to <dest>\vps-data\backup_manifest.jsonl.
#   - Fail-safe ordering (2026-09-04): the marker is written ONLY after the
#     integrity pass verifies every delta file landed, and a transfer that lands
#     nothing is retried then failed. The old order (marker before integrity)
#     advanced the marker past files that never landed - 173 files were skipped
#     forever on 2026-09-03 before the -Force resync.
#   - Live transient coordination files are excluded on the VPS side
#     (vps_stats.sh / vps_pull.sh: .lock_*, *.pid, *.heartbeat - held open by
#     the running collectors; transient state, recreated on restore).
#   - Retention-archive semantics (2026-09-06): the VPS hot tier prunes old raw
#     logs by design (14-day retention), so files absent from the VPS are NOT
#     stale - this mirror is the indefinite archive (W-6). Only the transient
#     pattern set is ever prunable here, via -PruneStale.
#
# Usage:
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup"
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -Register   # daily 00:30 UTC task
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -Force      # full copy
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -WhatIf     # dry run
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -PruneStale # delete stale transient files (dry-run with -WhatIf)
#
# Exit codes: 0 = copied + integrity green; 1 = transfer failed; 2 = integrity
# mismatch; 3 = config error; 10 = partial.

param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,           # backup root; the VPS mirror lands in <Dest>\vps-data
    # The VPS host comes from -VpsHost or $env:MP_VPS_HOST, never a committed
    # default (PD-2; audit M-1 - a hardcoded IP previously leaked the live
    # money-printer host into a tracked file). Fail-closed to exit 3 below.
    [string]$VpsHost = $env:MP_VPS_HOST,
    [string]$SshUser = "mp-egress",
    [string]$SshKey  = "",
    [switch]$Register,              # register the MoneyPrinterVpsBackup daily task
    [switch]$SkipIntegrity,         # skip the count/size comparison pass
    [switch]$PruneStale,            # delete mirror-side transient leftovers (.lock_*/*.pid/*.heartbeat); dry-run with -WhatIf
    [switch]$Force,                 # full copy even if a marker exists
    [switch]$WhatIf,                # dry run: print plan, do not transfer
    [int]$MaxTransferAttempts = 3,  # retry failed/empty transfers before failing (2026-09-04)
    [int]$RetryDelaySec = 30        # pause between transfer attempts
)

$ErrorActionPreference = "Stop"

# ---- VPS host must be provided at runtime (PD-2; audit M-1) ------------------
if ([string]::IsNullOrWhiteSpace($VpsHost)) {
    Write-Host "[!!] No VPS host set. Pass -VpsHost or set MP_VPS_HOST (never commit the IP - PD-2)." -ForegroundColor Red
    Exit 3
}

# ---- workspace root resolution (walk up to the [workspace] Cargo.toml) -------
$root = $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
while ($true) {
    $manifest = Join-Path $root "Cargo.toml"
    if ((Test-Path $manifest) -and (Select-String -Path $manifest -Pattern '^\[workspace\]' -Quiet)) { break }
    $parent = Split-Path $root -Parent
    if ($parent -eq $root) { Write-Host "[!!] workspace root not found from $PSScriptRoot" -ForegroundColor Red; Exit 1 }
    $root = $parent
}

$destRoot  = Join-Path $Destination "vps-data"
$markerF   = Join-Path $destRoot ".last_backup_ts"
$manifestF = Join-Path $destRoot "backup_manifest.jsonl"
$logFile   = Join-Path $root "ops\scripts\vps_backup.log"
$TaskName  = "MoneyPrinterVpsBackup"

$ssh = Join-Path $env:WINDIR "System32\OpenSSH\ssh.exe"
$tar = Join-Path $env:WINDIR "System32\tar.exe"
if (-not $SshKey) { $SshKey = Join-Path $HOME ".ssh\mp-egress_ed25519" }

function Log {
    param([string]$Msg, [string]$Level = "INFO")
    $ts = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    Add-Content -Path $logFile -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") { "Yellow" } elseif ($Level -eq "ERROR") { "Red" } else { "White" }
    Write-Host $line -ForegroundColor $col
}

# ---- config sanity ------------------------------------------------------------
foreach ($bin in @($ssh, $tar)) {
    if (-not (Test-Path $bin)) { Log "required binary missing: $bin" "ERROR"; Exit 3 }
}
if (-not (Test-Path $SshKey)) { Log "ssh key not found: $SshKey (pass -SshKey)" "ERROR"; Exit 3 }
try { New-Item -ItemType Directory -Path $Destination -Force | Out-Null } catch {
    Log "destination not writable: $Destination" "ERROR"; Exit 3
}
New-Item -ItemType Directory -Path $destRoot -Force | Out-Null

# ---- scheduled-task registration (mirrors backup_data.ps1) --------------------
if ($Register) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddMinutes(30), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): -Daily anchors StartBoundary to the
    # DATE passed in -At, so registering AT/AFTER the target time puts the
    # boundary in the past and Task Scheduler can fire the task immediately into
    # its own registration (MoneyPrinterDataBackup 00:07Z launch failure,
    # 0xFFFD0000). Pin the first trigger to tomorrow in that case.
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $action  = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`" -Destination `"$Destination`" -VpsHost `"$VpsHost`""
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Hours 8) `
        -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 5)
    try {
        Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
            -Settings $settings -User $env:USERNAME -Force -ErrorAction Stop | Out-Null
        Write-Host "[OK] Registered $TaskName daily at $($localAt.ToString('HH:mm')) local (=$($utcTarget.ToString('HH:mm')) UTC)." -ForegroundColor Green
        Write-Host "     Re-run -Register after a DST change to re-pin the UTC window." -ForegroundColor Gray
        Exit 0
    } catch {
        Log "Register-ScheduledTask failed: $($_.Exception.Message)" "ERROR"
        Exit 1
    }
}

# ---- prune stale transient files (2026-09-06) ---------------------------------
# The pull never deletes, so files pulled BEFORE the transient exclusion
# (pre-2026-08-13 full-copy era) are stranded forever. -PruneStale removes
# mirror-side files matching the transient pattern set (.lock_*, *.pid,
# *.heartbeat) - junk BY POLICY: the VPS-side filter excludes them, so any copy
# in the mirror is historical coordination state, recreated on restore. Corpus
# files absent from the VPS are NEVER pruned here: the VPS hot tier deletes old
# raw logs by design (14-day retention) and this mirror is the indefinite
# archive (W-6) - remote-absence means the archive is working, not stale.
if ($PruneStale) {
    $mirrorRoot = Join-Path $destRoot "data"
    $patterns = @(".lock_*", "*.pid", "*.heartbeat")
    $stale = @(Get-ChildItem -Path $mirrorRoot -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object {
            $n = $_.Name
            $hit = $false
            foreach ($p in $patterns) { if ($n -like $p) { $hit = $true; break } }
            $hit
        })
    if ($stale.Count -eq 0) {
        Log "Prune: no stale transient files under $mirrorRoot (.lock_*/*.pid/*.heartbeat)"
        Exit 0
    }
    $staleBytes = ($stale | Measure-Object -Property Length -Sum).Sum
    if ($WhatIf) {
        Log ("DRY RUN: {0} stale transient file(s) / {1:N2} KiB would be deleted:" -f $stale.Count, ($staleBytes / 1KB))
        foreach ($f in $stale) {
            Log ("  PRUNE {0} ({1:N0} B)" -f $f.FullName.Substring($destRoot.Length).TrimStart('\\'), $f.Length)
        }
        Exit 0
    }
    $failures = 0
    foreach ($f in $stale) {
        try {
            Remove-Item -LiteralPath $f.FullName -Force -ErrorAction Stop
            Log ("PRUNE deleted {0} ({1:N0} B)" -f $f.FullName.Substring($destRoot.Length).TrimStart('\\'), $f.Length)
        } catch {
            Log "PRUNE FAILED $($f.FullName): $($_.Exception.Message)" "ERROR"
            $failures++
        }
    }
    # sweep any dirs left empty by the deletion, deepest-first. NOT Remove-Item:
    # PS 5.1 Remove-Item -Force on a NON-EMPTY dir deadlocks under EAP=Stop when
    # the error is suppressed (2026-09-06 live incident - the first prune run
    # hung mid-sweep and died before the manifest append). .NET Delete throws
    # immediately on non-empty; try/catch skips those.
    Get-ChildItem -Path $mirrorRoot -Recurse -Directory -ErrorAction SilentlyContinue |
        Sort-Object { $_.FullName.Length } -Descending |
        ForEach-Object {
            try { [System.IO.Directory]::Delete($_.FullName) } catch { }
        }
    $entry = [ordered]@{
        ts_utc          = (Get-Date).ToUniversalTime().ToString("o")
        action          = "prune"
        scope           = "transient-by-policy"
        files_deleted   = $stale.Count - $failures
        bytes_reclaimed = $staleBytes
        failures        = $failures
    }
    Add-Content -Path $manifestF -Value ($entry | ConvertTo-Json -Compress) -Encoding UTF8
    Log "Manifest appended: $manifestF"
    Log ("Prune complete: {0} file(s) / {1:N2} KiB reclaimed, {2} failure(s)" -f ($stale.Count - $failures), ($staleBytes / 1KB), $failures)
    if ($failures -gt 0) { Exit 1 }
    Exit 0
}

# ---- marker / mtime ------------------------------------------------------------
$runStartEpoch = [int64](([DateTime]::UtcNow - [DateTime]::new(1970, 1, 1, 0, 0, 0, [DateTimeKind]::Utc)).TotalSeconds)
$full = $Force -or -not (Test-Path $markerF)
if ($full) {
    $mtime = "1970-01-01 00:00:00"
    $mtimeEpoch = 0
} else {
    $mtimeEpoch = [int64](Get-Content $markerF -Raw).Trim()
    $mtime = [DateTimeOffset]::FromUnixTimeSeconds($mtimeEpoch).UtcDateTime.ToString("yyyy-MM-dd HH:mm:ss")
}

# ---- remote delta stats (count, bytes, and the exact file list) --------------
# PS 5.1 EAP hazard (audit 2026-08-17): native stderr becomes a TERMINATING
# error under EAP=Stop even with 2>$null - override around the ssh call and
# check $LASTEXITCODE. (Without the exit-code check a dead VPS would parse as
# an empty delta and "succeed" - fail-closed: ssh failure is exit 3.)
$oldEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$statsOut = & $ssh -i $SshKey -o BatchMode=yes -o StrictHostKeyChecking=accept-new `
    "$SshUser@$VpsHost" "bash ~/vps_stats.sh '$mtime'" 2>$null
$ErrorActionPreference = $oldEap
if ($LASTEXITCODE -ne 0) {
    Log "remote stats failed (ssh exit $LASTEXITCODE) - VPS unreachable or vps_stats.sh missing" "ERROR"
    Exit 3
}
$srcCount = 0; $srcBytes = [int64]0; $srcFiles = [System.Collections.Generic.List[string]]::new()
foreach ($line in $statsOut) {
    if ($line -match '^count (\d+)$') { $srcCount = [int]$Matches[1] }
    elseif ($line -match '^bytes (\d+)$') { $srcBytes = [int64]$Matches[1] }
    elseif ($line -match '^file (.+)$') { $srcFiles.Add($Matches[1]) }
}
if ($srcCount -eq 0) {
    Log "delta set is empty - nothing changed since $mtime UTC; nothing to do"
    Exit 0
}

if ($WhatIf) {
    Log ("DRY RUN: {0} file(s) / {1:N2} MiB (mtime > {2} UTC) would be pulled to {3}" -f `
        $srcCount, ($srcBytes / 1MB), $mtime, $destRoot)
    Exit 0
}

Log ("Backup start: delta={0} files, {1:N2} MiB (full={2}) -> {3}" -f $srcCount, ($srcBytes / 1MB), $full, $destRoot)
$sw = [System.Diagnostics.Stopwatch]::StartNew()

# Outer quotes wrap the whole /c command; ssh's remote command keeps ONE pair of
# double quotes with only single quotes inside (the mtime has a space). No path
# in the command contains spaces, so no further quoting is needed.
$remote = "bash ~/vps_pull.sh '$mtime'"
$cmdLine = '""{0}" -i {1} -o BatchMode=yes -o StrictHostKeyChecking=accept-new {2}@{3} "{4}" | "{5}" -xf - -C {6}"' -f `
    $ssh, $SshKey, $SshUser, $VpsHost, $remote, $tar, $destRoot
# ---- transfer (binary-safe pipe via cmd /c), with retry -----------------------
# The ssh link is flaky (drain ssh_failed backlog; 2026-08-25 retry fix). Retry
# BOTH non-zero cmd exits AND zero-landed "successes": Windows bsdtar exits 0 on
# an empty stream, so a link drop at stream start otherwise parses as a clean
# copy (the 2026-09-03 "transfer done in 0.4m" no-op that skipped 173 files).
# Partials too (2026-09-05): a drop mid-stream can land only some of the delta
# with exit 0; the old `$ok = $true` on ANY partial landing burned the retry
# budget on the first partial (00:30 UTC 09-05: 17 of 26 landed, 9 missing,
# integrity exit 2, no same-night retry). The loop now retries until every
# delta file is present or the attempt budget is exhausted.
$missing = [System.Collections.Generic.List[string]]::new()
$dstBytes = [int64]0
$attempt = 0
$ok = $false
while (-not $ok) {
    $attempt++
    $proc = Start-Process -FilePath "cmd.exe" -ArgumentList "/c", $cmdLine -NoNewWindow -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        Log "transfer attempt $attempt/$MaxTransferAttempts failed (cmd exit $($proc.ExitCode))" "ERROR"
    } else {
        # Count what actually landed; the integrity pass reuses this exact list.
        $missing.Clear()
        $dstBytes = [int64]0
        foreach ($rel in $srcFiles) {
            $dstPath = Join-Path $destRoot ($rel.Replace("/", "\"))
            if (Test-Path $dstPath) {
                $dstBytes += (Get-Item $dstPath).Length
            } else {
                $missing.Add($rel)
            }
        }
        if ($missing.Count -eq $srcCount) {
            Log ("transfer attempt {0}/{1} landed 0 of {2} delta file(s) - empty/no-op stream (bsdtar exit-0 on empty input)" -f `
                $attempt, $MaxTransferAttempts, $srcCount) "ERROR"
        } elseif ($missing.Count -gt 0) {
            Log ("transfer attempt {0}/{1} landed {2} of {3} delta file(s) - {4} still missing, retrying" -f `
                $attempt, $MaxTransferAttempts, ($srcCount - $missing.Count), $srcCount, $missing.Count) "WARN"
        } else {
            $ok = $true
        }
    }
    if (-not $ok) {
        if ($attempt -ge $MaxTransferAttempts) {
            Log "transfer failed after $MaxTransferAttempts attempt(s) - marker NOT advanced" "ERROR"
            Exit 1
        }
        Start-Sleep -Seconds $RetryDelaySec
    }
}
$sw.Stop()
Log "transfer done in $($sw.Elapsed.TotalMinutes.ToString('F1'))m (attempt $attempt)"

# ---- landed/integrity BEFORE the marker (2026-09-04 fix) ----------------------
# The old order wrote .last_backup_ts before the integrity pass, so any failed
# run (exit 1/2) left the marker past files that never landed and no incremental
# could ever re-pull them (2026-09-03 incident). The marker now advances ONLY
# after the delta is verified complete; it still carries the run-START epoch, so
# files modified mid-run are re-copied next run.
$dest = $null
if ($SkipIntegrity) {
    Log "Integrity check skipped (-SkipIntegrity)"
} else {
    $sizeRatio = if ($srcBytes -gt 0) { [math]::Round(($dstBytes - $srcBytes) / $srcBytes, 4) } else { 0 }
    Log ("Integrity: src_delta={0:N0} files/{1:N2} MiB  present_in_backup={2:N0} files/{3:N2} MiB  missing={4:N0}  size_delta={5:P1}" -f
        $srcCount, ($srcBytes / 1MB), ($srcCount - $missing.Count), ($dstBytes / 1MB), $missing.Count, $sizeRatio)
    if ($missing.Count -gt 0) {
        Log ("MISMATCH: {0} delta files missing from the backup: {1}" -f $missing.Count, ($missing -join ", ")) "ERROR"
        Exit 2
    }
    if ([math]::Abs($sizeRatio) -gt 0.02) {
        Log "Large size delta ($sizeRatio) - expected only if the active day's log grew mid-copy; inspect." "WARN"
    }
    $dest = [PSCustomObject]@{ Files = $srcCount - $missing.Count; Bytes = $dstBytes }
}

# ---- marker: ONLY after transfer + integrity are green ------------------------
Set-Content -Path $markerF -Value "$runStartEpoch" -Encoding ASCII -NoNewline
Log "marker written: $runStartEpoch ($([DateTimeOffset]::FromUnixTimeSeconds($runStartEpoch).UtcDateTime.ToString('u')) UTC)"

$entry = [ordered]@{
    ts_utc          = (Get-Date).ToUniversalTime().ToString("o")
    destination     = $Destination
    vps_host        = $VpsHost
    full_copy       = $full
    mtime_utc       = $mtime
    src_delta_files = $srcCount
    src_delta_bytes = $srcBytes
    dst_delta_files = if ($dest) { $dest.Files } else { $null }
    dst_delta_bytes = if ($dest) { $dest.Bytes } else { $null }
    elapsed_sec     = [int]$sw.Elapsed.TotalSeconds
    skip_integrity  = [bool]$SkipIntegrity
}
Add-Content -Path $manifestF -Value ($entry | ConvertTo-Json -Compress) -Encoding UTF8
Log "Manifest appended: $manifestF"
Log "Backup complete -> $destRoot"
Exit 0
