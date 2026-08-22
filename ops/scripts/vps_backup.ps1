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
#   - Live .lock_* files are excluded (held open by the running collectors;
#     transient coordination state, recreated on restore).
#
# Usage:
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup"
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -Register   # daily 00:30 UTC task
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -Force      # full copy
#   .\ops\scripts\vps_backup.ps1 -Destination "C:\mp-backup" -WhatIf     # dry run
#
# Exit codes: 0 = copied + integrity green; 1 = transfer failed; 2 = integrity
# mismatch; 3 = config error; 10 = partial.

param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,           # backup root; the VPS mirror lands in <Dest>\vps-data
    [string]$VpsHost = "34.135.127.147",
    [string]$SshUser = "mp-egress",
    [string]$SshKey  = "",
    [switch]$Register,              # register the MoneyPrinterVpsBackup daily task
    [switch]$SkipIntegrity,         # skip the count/size comparison pass
    [switch]$Force,                 # full copy even if a marker exists
    [switch]$WhatIf                 # dry run: print plan, do not transfer
)

$ErrorActionPreference = "Stop"

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
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $action  = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`" -Destination `"$Destination`""
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

# ---- transfer (binary-safe pipe via cmd /c) -----------------------------------
# Outer quotes wrap the whole /c command; ssh's remote command keeps ONE pair of
# double quotes with only single quotes inside (the mtime has a space). No path
# in the command contains spaces, so no further quoting is needed.
$remote = "bash ~/vps_pull.sh '$mtime'"
$cmdLine = '""{0}" -i {1} -o BatchMode=yes -o StrictHostKeyChecking=accept-new {2}@{3} "{4}" | "{5}" -xf - -C {6}"' -f `
    $ssh, $SshKey, $SshUser, $VpsHost, $remote, $tar, $destRoot
$proc = Start-Process -FilePath "cmd.exe" -ArgumentList "/c", $cmdLine -NoNewWindow -Wait -PassThru
$sw.Stop()
if ($proc.ExitCode -ne 0) {
    Log "transfer failed (cmd exit $($proc.ExitCode))" "ERROR"
    Exit 1
}
Log "transfer done in $($sw.Elapsed.TotalMinutes.ToString('F1'))m"

# ---- marker + manifest (write marker = run START so nothing between start and
#      end is ever missed: files changed mid-run are re-copied next run) --------
Set-Content -Path $markerF -Value "$runStartEpoch" -Encoding ASCII -NoNewline
Log "marker written: $runStartEpoch ($([DateTimeOffset]::FromUnixTimeSeconds($runStartEpoch).UtcDateTime.ToString('u')) UTC)"

# ---- integrity pass (exact: every source delta file must exist in the backup)
$dest = $null
if ($SkipIntegrity) {
    Log "Integrity check skipped (-SkipIntegrity)"
} else {
    $missing = @()
    $dstBytes = [int64]0
    foreach ($rel in $srcFiles) {
        $dstPath = Join-Path $destRoot ($rel.Replace("/", "\"))
        if (Test-Path $dstPath) {
            $dstBytes += (Get-Item $dstPath).Length
        } else {
            $missing += $rel
        }
    }
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
