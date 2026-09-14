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
#   - Transient staging/verify dirs (.vps-drain-staging, .offhost-staging,
#     .offhost-verify) are excluded from the mirror: their contents are either
#     VPS-sourced files whose authoritative copy is still on the VPS until the
#     drain lands+releases them, or encrypted off-host copies re-creatable from
#     the corpus — never unique landed data. Integrity counts exclude them on
#     BOTH sides so the comparison stays apples-to-apples; -PruneStale treats
#     any destination-side staging files as junk (the mirror never writes them).
#   - Backups are self-describing: every run appends a JSONL manifest BESIDE the
#     mirrored tree ('<dest>\backup_manifest.jsonl') so the backup knows what it
#     holds even if the source dies; each entry records src/dst counts AND how
#     much transient staging was deliberately skipped (src_staging_excluded_*).
#     It deliberately lives OUTSIDE the robocopy
#     tree: robocopy /XF suppresses the detail line for excluded files but STILL
#     counts them in the summary 'Extras' column (quirk verified 09-06), which
#     would keep exit bit 2 (extras present) permanently lit for the manifest.
#   - Mirror-side stale extras (files deleted from the source) accumulate by
#     design; -PruneStale removes them and never runs in the scheduled task
#     (W-6: only the human deletes). Review the listing via -PruneStale -WhatIf
#     first, then run -PruneStale.
#
# Usage:
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup"
#   .\ops\scripts\backup_data.ps1 -Destination "\\NAS\mp-backup" -SkipIntegrity
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup" -Register   # daily 00:07 UTC task
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup" -WhatIf     # dry run (copy plan / prune listing)
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup" -PruneStale -WhatIf   # list stale extras only
#   .\ops\scripts\backup_data.ps1 -Destination "D:\mp-backup" -PruneStale            # delete them (W-6: human-invoked)
#
# Exit codes: 0 = copied + integrity green (or prune clean); 1 = copy failed /
# prune partially failed; 2 = integrity mismatch after copy; 3 = config/bad
# destination; 10 = partial (copy fell short of source; files marked for retry).

param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,           # backup root; the mirror lands in <Dest>\data
    [switch]$Register,              # register the MoneyPrinterDataBackup daily task
    [switch]$SkipIntegrity,         # skip the file-count/size comparison pass
    [switch]$PruneStale,            # delete mirror-side stale extras (absent from source); -WhatIf lists only
    [switch]$WhatIf,                # dry run: print copy plan / prune listing, do not change anything
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
# Manifest lives beside the mirrored tree, NOT inside it: robocopy /XF does not
# remove a file from the 'Extras' summary count (quirk, verified 09-06), so an
# in-tree manifest would keep exit bit 2 (extras present) permanently lit. The
# name-based guards below (prune filter, stats filter, /XF entry) remain as
# defense against legacy in-tree copies from before the 09-06 relocation.
$manifestF = Join-Path $Destination "backup_manifest.jsonl"
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

# Same-physical-volume guard (audit 08-10): a mirror on the SAME drive as the
# source protects against accidental deletion but NOT against a dead disk -
# the whole corpus dies with the one drive. Compare the volume root of source
# vs destination and WARN loudly when they match, so a same-drive destination
# is never mistaken for disaster recovery. The warning is advisory: the task
# still runs (a same-drive mirror beats no mirror at all).
function Get-VolumeRoot {
    param([string]$Path)
    try {
        $leaf = Get-Item $Path -ErrorAction Stop
        return [System.IO.Path]::GetPathRoot($leaf.FullName)
    } catch {
        # Destination may not exist yet; resolve from the closest existing ancestor.
        $p = $Path
        while (-not (Test-Path $p) -and $p -ne [System.IO.Path]::GetPathRoot($p)) { $p = Split-Path $p -Parent }
        try { return [System.IO.Path]::GetPathRoot((Get-Item $p).FullName) } catch { return "?" }
    }
}
$srcVol = Get-VolumeRoot $srcRoot
$dstVol = Get-VolumeRoot $Destination
if ($srcVol -ne "?" -and $srcVol -eq $dstVol) {
    $warnBody = "destination {0} is on the SAME volume ({1}) as the source - likely the same physical disk, so a dead drive takes both. " +
        "Move -Destination to a separate physical drive or network share for real disaster recovery (W-6)."
    Log ("WARNING: " + ($warnBody -f $Destination, $dstVol)) "WARN"
}

if (-not (Test-Path $srcRoot)) { Log "source data dir missing: $srcRoot" "ERROR"; Exit 3 }

# ---- transient staging/verify dirs (used by the prune block AND stats) -------
# Defined here, ABOVE the -PruneStale block: script functions/variables are
# visible only after their definition line executes top-to-bottom, so a helper
# defined in the stats section below would not exist yet when prune runs
# (CommandNotFound, caught live 09-06). Same dirs are excluded from the mirror
# via robocopy /XD.
$ExcludedDirNames = ".vps-drain-staging", ".offhost-staging", ".offhost-verify"
function Test-ExcludedDir {
    param([string]$FullPath)
    foreach ($seg in ($FullPath -split '[\\/]')) {
        if ($ExcludedDirNames -contains $seg) { return $true }
    }
    return $false
}

# ---- scheduled-task registration (mirrors daily_pipeline.ps1) ----------------
if ($Register) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddMinutes(7), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): -Daily anchors StartBoundary to the
    # DATE passed in -At, so registering AT/AFTER the target time puts the
    # boundary in the past and Task Scheduler can fire the task immediately into
    # its own registration (MoneyPrinterDataBackup 00:07Z launch failure,
    # 0xFFFD0000). Pin the first trigger to tomorrow in that case.
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
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
        # Log the registration run explicitly: this branch sits AFTER the volume WARN
        # and exits BEFORE 'Backup start', so without this line a -Register run is
        # indistinguishable from a mid-flight death in backup.log (misfiled as a
        # silent crash on 2026-09-05 03:35:25Z; the engine log's HostApplication
        # field proved it was a clean, deliberate registration).
        Log "Registration run: $TaskName re-registered daily $($utcTarget.ToString('HH:mm')) UTC - backup skipped by design"
        Write-Host "[OK] Registered $TaskName daily at $($localAt.ToString('HH:mm')) local (=$($utcTarget.ToString('HH:mm')) UTC)." -ForegroundColor Green
        Write-Host "     Re-run -Register after a DST change to re-pin the UTC window." -ForegroundColor Gray
        Exit 0
    } catch {
        Log "Register-ScheduledTask failed: $($_.Exception.Message)" "ERROR"
        Exit 1
    }
}

# ---- stale-extra pruning (W-6: human-invoked; dry-run with -WhatIf) -----------
# robocopy /E mirrors without deletion, so a file deleted from the source stays
# in the mirror forever (observed 09-06: 33 extras / 5.32 GiB, mostly staged
# VPS logs duplicated after the drain moved them into raw/). -PruneStale is the
# inverse pass: it deletes destination files with no counterpart anywhere under
# the source, listing everything first. W-6 ("only the human deletes") means
# this never runs in the scheduled task - the operator reviews the -WhatIf
# listing, then deletes deliberately. backup_manifest.jsonl is destination-only
# metadata BY DESIGN and is never pruned. Files under the staging/verify dirs
# are pruned REGARDLESS of source state: the mirror no longer writes them, so
# any destination-side copy is historical junk (observed 09-06: dst held a full
# mirrored generation of .offhost-staging .age files). Standalone mode: exits
# after pruning, no backup is run.
if ($PruneStale) {
    if (-not (Test-Path $destRoot)) {
        Log "PRUNE: destination $destRoot does not exist - nothing to prune"
        Exit 0
    }
    $srcSet = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
    Get-ChildItem -Path $srcRoot -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
        [void]$srcSet.Add($_.FullName.Substring($srcRoot.Length).TrimStart('\', '/'))
    }
    # Staging/verify files are junk BY POLICY (the mirror never writes them, so
    # any destination-side copy is historical), thus pruned regardless of source
    # state; other extras still require absence from the source - a file that
    # exists in the live corpus is never an extra.
    $extras = @(Get-ChildItem -Path $destRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object {
        $rel = $_.FullName.Substring($destRoot.Length).TrimStart('\', '/')
        ($rel -ne "backup_manifest.jsonl") -and ((Test-ExcludedDir $_.FullName) -or (-not $srcSet.Contains($rel)))
    })
    if ($extras.Count -eq 0) {
        Log "PRUNE: no stale extras in $destRoot"
        Exit 0
    }
    $totalBytes = [long]($extras | Measure-Object -Property Length -Sum).Sum
    $mode = if ($WhatIf) { "dry run" } else { "deleting" }
    Log ("PRUNE ({0}): {1} stale extras = {2:N2} GiB" -f $mode, $extras.Count, ($totalBytes / 1GB))
    foreach ($x in ($extras | Sort-Object FullName)) {
        $sz = if ($x.Length -ge 1MB) { "{0:N1} MiB" -f ($x.Length / 1MB) } else { "{0:N0} B" -f $x.Length }
        Write-Host ("  - {0}  ({1})" -f $x.FullName.Substring($destRoot.Length).TrimStart('\', '/'), $sz)
    }
    if ($WhatIf) {
        Log "PRUNE dry run: nothing deleted (re-run without -WhatIf to delete)"
        Exit 0
    }
    $failed = 0
    foreach ($x in $extras) {
        try {
            Remove-Item -LiteralPath $x.FullName -Force -ErrorAction Stop
        } catch {
            $failed++
            Log "PRUNE failed on $($x.FullName): $($_.Exception.Message)" "ERROR"
        }
    }
    # sweep now-empty directories (deepest first) so the prune leaves no husks.
    # .NET Delete (not Remove-Item): PS 5.1 Remove-Item -Force on a non-empty
    # dir deadlocks under EAP=Stop when the error is suppressed (2026-09-06
    # vps_backup.ps1 incident) - the emptiness check above had a TOCTOU race.
    Get-ChildItem -Path $destRoot -Recurse -Directory -ErrorAction SilentlyContinue |
        Sort-Object { $_.FullName.Length } -Descending | ForEach-Object {
            try { [System.IO.Directory]::Delete($_.FullName) } catch { }
        }
    $deleted = $extras.Count - $failed
    Log ("PRUNE: deleted {0} files, {1:N2} GiB reclaimed" -f $deleted, ($totalBytes / 1GB))
    Add-Content -Path $manifestF -Value ([ordered]@{
        ts_utc           = (Get-Date).ToUniversalTime().ToString("o")
        action           = "prune"
        files_deleted    = $deleted
        bytes_reclaimed  = $totalBytes
        failures         = $failed
    } | ConvertTo-Json -Compress) -Encoding UTF8
    if ($failed -gt 0) { Exit 1 }
    Log "PRUNE complete"
    Exit 0
}

# ---- corpus stats helper ------------------------------------------------------
# Transient staging/verify dirs are excluded from the mirror (robocopy /XD) and
# from the integrity counts on BOTH sides, so src/dst remain comparable and a
# mid-drain staging file is not misreported as a missing backup.
# ($ExcludedDirNames / Test-ExcludedDir are defined above the prune block.)
function Get-CorpusStats {
    param([string]$Path)
    # Exclude live lock files AND transient staging/verify dirs from BOTH source
    # and dest so the counts stay comparable (see the header notes).
    # The manifest is destination-only metadata BY DESIGN - excluded from counts
    # on both sides so integrity reads literal missing=0 (dst otherwise runs +1
    # file ahead) and robocopy stops flagging it as a permanent 'extra'.
    $files = Get-ChildItem -Path $Path -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -notlike ".lock_*" -and $_.Name -ne "backup_manifest.jsonl" -and -not (Test-ExcludedDir $_.FullName) }
    $bytes = ($files | Measure-Object -Property Length -Sum).Sum
    return [PSCustomObject]@{ Files = $files.Count; Bytes = [long]$bytes }
}

$srcBefore = Get-CorpusStats $srcRoot
$excluded = @(Get-ChildItem -Path $srcRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -like ".lock_*" }).Count
# Files under the transient staging/verify dirs are deliberately NOT mirrored
# (robocopy /XD + Get-CorpusStats exclusion, header notes) - record how much
# was skipped so the run self-describes (manifest fields src_staging_excluded_*).
$stagingExcluded = @(Get-ChildItem -Path $srcRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object { Test-ExcludedDir $_.FullName })
$stagingExcludedBytes = [long]($stagingExcluded | Measure-Object -Property Length -Sum).Sum
if ($WhatIf) {
    Log ("DRY RUN: {0:N0} files / {1:N2} GiB would be mirrored to {2}" -f $srcBefore.Files, ($srcBefore.Bytes / 1GB), $Destination)
    Exit 0
}

Log ("Backup start: source={0:N0} files, {1:N2} GiB -> {2}" -f $srcBefore.Files, ($srcBefore.Bytes / 1GB), $Destination)
$sw = [System.Diagnostics.Stopwatch]::StartNew()

# ---- mirror pass (robocopy /E copies new+changed files, never deletes) -------
New-Item -ItemType Directory -Path $destRoot -Force | Out-Null
$roArgs = @($srcRoot, $destRoot, "/E", "/R:$Retries", "/W:1", "/XJ", "/NP", "/NFL", "/NDL", "/XD", ".vps-drain-staging", ".offhost-staging", ".offhost-verify", "/XF", ".lock_*", "backup_manifest.jsonl")
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
    src_staging_excluded_files = $stagingExcluded.Count
    src_staging_excluded_bytes = $stagingExcludedBytes
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