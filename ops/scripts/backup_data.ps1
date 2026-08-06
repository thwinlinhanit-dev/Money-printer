# backup_data.ps1 - Incremental mirror backup of the recorded market-data corpus.
# The raw event logs under data/raw are the project's only irreplaceable asset
# (~14 GiB as of 08-05, gitignored by design). This script copies that corpus to
# a destination outside the workspace so a dead drive / deleted folder cannot
# destroy it.
#
# Design constraints (spec 024 / W-6):
#   - READ-ONLY on the source. Never deletes, renames, or mutates anything under
#     data/ (W-6: the data dirs are append-only; only the human deletes).
#   - robocopy /E (mirror without deletion) plus a two-pass integrity check:
#     file + byte counts on source vs destination after the copy, so a failed
#     copy is *noticed*.
#   - Live files grow while we copy (collectors write 24/7); a small size delta
#     on the active day's logs is expected and only logged as a NOTE.
#   - Live collector lock files (.lock_*) are excluded: the running collector
#     holds them with an exclusive handle, so a live mirror can never copy them
#     (robocopy ERROR 32, observed 08-06). They are transient coordination
#     state, not corpus data — on restore the collectors simply recreate them.
#   - Backups are self-describing: every run appends a JSONL manifest inside the
#     destination ('<dest>\data\backup_manifest.jsonl') so the backup knows what
#     it holds even if the source dies.
#
# Usage:
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup"
#   .\ops\scripts\backup_data.ps1 -Destination "\\NAS\mp-backup" -SkipIntegrity
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup" -Register   # daily 00:07 UTC task
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup" -WhatIf     # dry run
#
# Exit codes: 0 = copied + integrity green; 1 = copy failed; 2 = integrity
# mismatch after copy; 3 = config/bad destination; 10 = partial (copy fell
# short of source; files marked for retry).

param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,           # backup root; the mirror lands in <Dest>\data
    [switch]$Register,              # register the MoneyPrinterDataBackup daily task
    [switch]$SkipIntegrity,         # skip the file-count/size comparison pass
    [switch]$WhatIf,                # dry run: print copy plan, do not copy
    [int]$Retries = 2               # robocopy /R file retries (default 2)
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
$srcRoot   = Join-Path $root "data"
$destRoot  = Join-Path $Destination "data"
$manifestF = Join-Path $destRoot "backup_manifest.jsonl"
$logFile   = Join-Path $root "ops\scripts\backup.log"
$TaskName  = "MoneyPrinterDataBackup"

function Log {
    param([string]$Msg, [string]$Level = "INFO")
    $ts = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    Add-Content -Path $logFile -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") { "Yellow" } elseif ($Level -eq "ERROR") { "Red" } else { "White" }
    Write-Host $line -ForegroundColor $col
}

if (-not (Test-Path $srcRoot)) { Log "source data dir missing: $srcRoot" "ERROR"; Exit 3 }

# ---- scheduled-task registration (mirrors daily_pipeline.ps1) ----------------
if ($Register) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddMinutes(7), [DateTimeKind]::Utc)
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

# ---- corpus stats helper ------------------------------------------------------
function Get-CorpusStats {
    param([string]$Path)
    # Exclude live lock files from BOTH source and dest so the counts stay
    # comparable (see the .lock_* exclusion note in the header).
    $files = Get-ChildItem -Path $Path -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike ".lock_*" }
    $bytes = ($files | Measure-Object -Property Length -Sum).Sum
    return [PSCustomObject]@{ Files = $files.Count; Bytes = [long]$bytes }
}

$srcBefore = Get-CorpusStats $srcRoot
$excluded = @(Get-ChildItem -Path $srcRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -like ".lock_*" }).Count
if ($WhatIf) {
    Log ("DRY RUN: {0:N0} files / {1:N2} GiB would be mirrored to {2}" -f $srcBefore.Files, ($srcBefore.Bytes / 1GB), $Destination)
    Exit 0
}

Log ("Backup start: source={0:N0} files, {1:N2} GiB -> {2}" -f $srcBefore.Files, ($srcBefore.Bytes / 1GB), $Destination)
$sw = [System.Diagnostics.Stopwatch]::StartNew()

# ---- mirror pass (robocopy /E copies new+changed files, never deletes) -------
New-Item -ItemType Directory -Path $destRoot -Force | Out-Null
$roArgs = @($srcRoot, $destRoot, "/E", "/R:$Retries", "/W:1", "/XJ", "/NP", "/NFL", "/NDL", "/XF", ".lock_*")
if ($WhatIf) { $roArgs += "/L" }
$roOut = & robocopy @roArgs 2>&1
$roCode = $LASTEXITCODE
if ($roCode -ge 8) {
    Log "robocopy failed (exit $roCode): $($roOut | Out-String)" "ERROR"
    Exit 1
}
$sw.Stop()
$roDesc = switch ($roCode) {
    0 { "no files copied" }; 1 { "files copied" }; 2 { "extras present" }
    3 { "copied + extras" }; 4 { "mismatched files processed" }; default { "copy ok" }
}
Log "robocopy done in $($sw.Elapsed.TotalMinutes.ToString('F1'))m (exit $roCode=$roDesc)"

# ---- integrity pass (unless skipped) ----------------------------------------
$dest = $null
if ($SkipIntegrity) {
    Log "Integrity check skipped (-SkipIntegrity)"
} else {
    $dest = Get-CorpusStats $destRoot
    $missing = $srcBefore.Files - $dest.Files
    $sizeRatio = if ($srcBefore.Bytes -gt 0) {
        [math]::Round(($dest.Bytes - $srcBefore.Bytes) / $srcBefore.Bytes, 4)
    } else { 0 }
    Log ("Integrity: src={0:N0} files/{1:N2} GiB  dst={2:N0} files/{3:N2} GiB  missing={4:N0}  size_delta={5:P1}" -f
        $srcBefore.Files, ($srcBefore.Bytes / 1GB), $dest.Files, ($dest.Bytes / 1GB), $missing, $sizeRatio)
    if ($missing -gt 0) {
        # Source has files the dest does NOT - a real failure signal.
        Log "MISMATCH: $missing source files are missing from the backup." "ERROR"
        Exit 2
    }
    # size delta beyond 2% is expected only if a collector rotated mid-copy
    if ([math]::Abs($sizeRatio) -gt 0.02) {
        Log "Large size delta ($sizeRatio) - expected only if a collector rotated mid-copy; inspect." "WARN"
    }
}

# ---- manifest (appended on the destination so the backup self-describes) -----
$entry = [ordered]@{
    ts_utc         = (Get-Date).ToUniversalTime().ToString("o")
    destination    = $Destination
    src_files      = $srcBefore.Files
    src_bytes      = $srcBefore.Bytes
    src_lock_files_excluded = $excluded
    dst_files      = if ($dest) { $dest.Files } else { $null }
    dst_bytes      = if ($dest) { $dest.Bytes } else { $null }
    robocopy_exit  = $roCode
    elapsed_sec    = [int]$sw.Elapsed.TotalSeconds
    skip_integrity = [bool]$SkipIntegrity
}
Add-Content -Path $manifestF -Value ($entry | ConvertTo-Json -Compress) -Encoding UTF8
Log "Manifest appended: $manifestF"
Log "Backup complete -> $Destination"
Exit 0