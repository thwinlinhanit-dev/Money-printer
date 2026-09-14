# vps_drain.ps1 - Nightly DRAIN of the VPS relay corpus into the Windows
# MASTER corpus (data/raw), releasing the VPS copy only after byte-verification.
# This is the "VPS never accumulates" mechanism behind the storage-budget
# alert's relay cap (OPS-15): closed day-files move off the relay to the box
# where the corpus lives, so the 28 GB VPS disk stays flat.
#
# Contrast with vps_backup.ps1: that is a READ-ONLY mirror to a separate
# backup root (W-6 safety copy). This drain is a MOVE into the master corpus -
# it deletes from the VPS ONLY files whose sha256 was verified end-to-end into
# data/raw. The two are complementary: backup first (00:30 UTC), drain after
# (01:00 UTC), both after the VPS gate (00:05 UTC) has audited the day.
#
# Safety properties (the "guard against partial transfers"):
#   1. CLOSED days only: {YYYYMMDD}_*.log with date < today (UTC). The current
#      partial day is never touched; the collector rotates at UTC midnight.
#   2. Stage-then-verify: files land in data\.vps-drain-staging first; each is
#      sha256+size checked against the VPS-side list BEFORE it moves into
#      data/raw. A mismatch fails the run (exit 2) and NOTHING is released.
#   3. Collision policy: if data/raw already has a file of the same name,
#      identical hash = already landed (release the VPS copy); DIFFERENT hash
#      = the Windows host's own A-B recording (bringup doc sec 6) -> the VPS
#      copy is NEVER overwritten and NEVER released; WARN + manifest. After
#      the handoff (Windows recorder stopped) those files drain normally.
#   4. Release re-verifies: the VPS-side vps_drain_release.sh re-hashes each
#      file right before deletion; a changed file is skipped, not deleted.
#      Delete-only-when-verified is the W-6 exception the owner approved for
#      the relay drain (2026-08-14).
#   5. Slow-link economy (2026-08-16): the master corpus + manifest are
#      consulted BEFORE the pull - a file byte-identical to master (a release
#      re-attempt) and a KNOWN A-B collision (latest manifest entry
#      action=collision with the same VPS sha256) are never re-transferred;
#      the slow link moves only genuinely new closed days. The master-side
#      hash check stays the authority, so a deleted master file re-pulls
#      correctly (no stale skip).
#   6. Per-file transfer + resume (2026-08-18): each new day-file moves in its
#      OWN ssh|tar stream (the pull script's include-list form streams exactly
#      one rel), so a slow-link drop fails ONLY that file - there is no single
#      multi-GB archive to lose. ssh and tar stderr are captured into
#      vps_drain.log on any failed transfer (a dropped pipe exits non-zero, or
#      yields an empty/absent stream that the post-transfer check catches and
#      logs). Already-verified staging copies from a failed run are REUSED
#      (size+sha256 against the current list), never re-transferred; partial
#      copies are re-pulled. A transfer drop is exit 1 (partial): the verified
#      files still land and release, and the failed ones re-attempt next run.
#      The exit-2 "nothing released" contract is reserved for genuine
#      integrity mismatches, which still stop the run with staged evidence kept.
#
# Usage:
#   .\ops\scripts\vps_drain.ps1                          # drain closed days
#   .\ops\scripts\vps_drain.ps1 -NoRelease               # land+verify, never delete VPS
#   .\ops\scripts\vps_drain.ps1 -Register                # daily 01:00 UTC task
#   .\ops\scripts\vps_drain.ps1 -WhatIf                  # dry run (list only)
#   .\ops\scripts\vps_drain.ps1 -VpsBase /tmp/drain-test # scratch base (tests)
#
# Exit codes: 0 = drained + verified; 1 = partial (some transfers failed or
# some releases skipped - re-attempt next run); 2 = integrity mismatch
# (nothing released); 3 = config error.
#
# Manifest (data/vps_drain_manifest.jsonl): one entry per candidate with
# `action` (landed | collision | missing) and `release` (the VPS copy's
# disposition: released | skipped: <reason> | ssh_failed | no_release |
# kept). `kept` = never in the releasable set (A-B collision etc.) - the VPS
# copy stays by design. A `landed` entry whose `release` is NOT `released`
# means the VPS is still holding the file. A transfer that never landed also
# records `transfer_error: transfer_failed` (the VPS copy stays; re-attempt
# next run).

param(
    [string]$VpsHost = $env:MP_VPS_HOST,
    [string]$SshUser = "mp-egress",
    [string]$SshKey  = "",
    [string]$VpsBase = "/opt/money-printer/data",   # scratch base for tests
    [string]$MasterRaw = "",                        # override master dir (tests); default <root>\data\raw
    [switch]$Register,                              # register the MoneyPrinterVpsDrain daily task
    [switch]$NoRelease,                             # land + verify only; never delete from the VPS
    [switch]$WhatIf                                 # dry run: list candidates, no transfer
)

$ErrorActionPreference = "Stop"
### VPS host must be provided at runtime (PD-2; audit M-1) - never committed.
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
    if ($parent -eq $root) { Write-Host "[!!] workspace root not found from $PSScriptRoot" -ForegroundColor Red; Exit 3 }
    $root = $parent
}

$rawDir     = if ($MasterRaw) { $MasterRaw } else { Join-Path $root "data\raw" }
$stagingDir = Join-Path (Split-Path $rawDir -Parent) ".vps-drain-staging"
$manifestF  = Join-Path (Split-Path $rawDir -Parent) "vps_drain_manifest.jsonl"
$logFile    = Join-Path $root "ops\scripts\vps_drain.log"
$TaskName   = "MoneyPrinterVpsDrain"

$ssh = Join-Path $env:WINDIR "System32\OpenSSH\ssh.exe"
if (-not $SshKey) { $SshKey = Join-Path $HOME ".ssh\mp-egress_ed25519" }

function Log {
    param([string]$Msg, [string]$Level = "INFO")
    $ts = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $line = "[$ts][$Level] $Msg"
    Add-Content -Path $logFile -Value $line -Encoding UTF8
    $col = if ($Level -eq "WARN") { "Yellow" } elseif ($Level -eq "ERROR") { "Red" } else { "White" }
    Write-Host $line -ForegroundColor $col
}

function Get-Sha256Hex {
    param([string]$Path)
    return (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# ---- remote ssh with bounded retry + captured stderr (2026-08-25) ------------
# The nightly legs are SINGLE-SHOT ssh calls: one transient failure (exit 255,
# no output - seen 2026-08-24 19:48 on the release leg) held every verified
# file on the VPS until the NEXT nightly run, piling closed days onto a disk
# with no headroom (14 manifest entries went ssh_failed before this fix).
# Windows OpenSSH does not support ControlMaster multiplexing, so connection
# reuse is unavailable; resilience comes from bounded retries with linear
# backoff instead. stderr is captured to $SshErrFile (never discarded - the
# old `2>$null` made failures undiagnosable) and folded into the drain log.
#
# Returns the output lines (possibly empty), or $null when every attempt
# failed (caller decides whether that is fatal). $script:LastRemoteExit and
# $script:LastRemoteErr carry the final attempt's exit code / stderr summary.
# PS 5.1 turns native stderr into a TERMINATING error under EAP=Stop even with
# a redirect (the same bug fixed in the release/list phases below), so EAP is
# overridden around the call and $LASTEXITCODE is authoritative.
function Invoke-Remote {
    param(
        [string]$RemoteCmd,
        [string]$SshErrFile,
        [int]$MaxAttempts = 3,
        $InputLines = $null   # optional stdin (the release leg pipes pairs)
    )
    $script:LastRemoteExit = -1
    $script:LastRemoteErr = ""
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
            Remove-Item $SshErrFile -Force -ErrorAction SilentlyContinue
            $out = if ($null -ne $InputLines) {
                $InputLines | & $ssh -i $SshKey -o BatchMode=yes -o StrictHostKeyChecking=accept-new `
                    "$SshUser@$VpsHost" $RemoteCmd 2> $SshErrFile
            } else {
                & $ssh -i $SshKey -o BatchMode=yes -o StrictHostKeyChecking=accept-new `
                    "$SshUser@$VpsHost" $RemoteCmd 2> $SshErrFile
            }
            $script:LastRemoteExit = $LASTEXITCODE
            if ($LASTEXITCODE -eq 0) { return ,@($out) }
            $errSummary = "(no stderr captured)"
            if (Test-Path $SshErrFile) {
                $lines = @(Get-Content $SshErrFile -Encoding UTF8 -ErrorAction SilentlyContinue | Where-Object { $_.Trim() -ne "" })
                if ($lines.Count -gt 0) {
                    $errSummary = ($lines | Select-Object -First 3) -join "; "
                    $script:LastRemoteErr = ($lines | Select-Object -First 10) -join "; "
                }
            }
            Log ("ssh attempt {0}/{1} failed (exit {2}): {3}" -f $attempt, $MaxAttempts, $LASTEXITCODE, $errSummary) "WARN"
            if ($attempt -lt $MaxAttempts) { Start-Sleep -Seconds (10 * $attempt) }
        }
        return $null
    } finally {
        $ErrorActionPreference = $oldEap
    }
}

# ---- config sanity ------------------------------------------------------------
foreach ($bin in @($ssh)) {
    if (-not (Test-Path $bin)) { Log "required binary missing: $bin" "ERROR"; Exit 3 }
}
if (-not (Test-Path $SshKey)) { Log "ssh key not found: $SshKey (pass -SshKey)" "ERROR"; Exit 3 }
if (-not (Test-Path $rawDir)) { Log "master corpus dir missing: $rawDir" "ERROR"; Exit 3 }

# ---- scheduled-task registration ----------------------------------------------
if ($Register) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(1), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): -Daily anchors StartBoundary to the
    # DATE passed in -At, so registering AT/AFTER the target time puts the
    # boundary in the past and Task Scheduler can fire the task immediately into
    # its own registration (MoneyPrinterDataBackup 00:07Z launch failure,
    # 0xFFFD0000). Pin the first trigger to tomorrow in that case.
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $action  = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
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

# ---- list phase: VPS-side closed-day files with size + sha256 -----------------
# Retry-wrapped (Invoke-Remote, 2026-08-25): a transient ssh failure here used
# to exit 3 and skip the ENTIRE night's drain.
$listOut = Invoke-Remote `
    -RemoteCmd "bash ~/vps_drain_list.sh '$VpsBase'" `
    -SshErrFile (Join-Path $env:TEMP "vps-drain-list-ssh.err")
if ($null -eq $listOut) {
    Log "remote list failed after retries (last exit $script:LastRemoteExit): $script:LastRemoteErr - VPS unreachable or vps_drain_list.sh missing" "ERROR"
    Exit 3
}
$candidates = [System.Collections.Generic.List[object]]::new()
foreach ($line in $listOut) {
    if ($line -match '^file (\S+) (\d+) ([0-9a-f]{64})$') {
        $candidates.Add([PSCustomObject]@{
            Rel  = $Matches[1]
            Name = Split-Path $Matches[1] -Leaf
            Size = [int64]$Matches[2]
            Hash = $Matches[3]
        })
    }
}
if ($candidates.Count -eq 0) {
    Log "no closed day-files on the VPS - nothing to drain"
    Exit 0
}
Log ("Candidates: {0} closed day-file(s) from {1} ({2:N2} MiB)" -f `
    $candidates.Count, $VpsBase, (($candidates | Measure-Object -Property Size -Sum).Sum / 1MB))

# Core-first ordering (2026-08-25): the REQUIRED Phase-0 recordings transfer
# before every secondary leg (bybit swing days, macro, netflow). The slow link
# moves one multi-hundred-MB file at a time, and a backlog night otherwise
# starves the 07:30 gate of exactly the files it audits - on 2026-08-25 the
# drain was still landing hyperliquid masters WHILE the gate ran, which both
# dirtied that morning's determinism check and raced the scorecard. Ordering
# only: classification/verification below is untouched, and a resolver
# failure keeps the original order (an optimization, never a gate input).
$corePairs = @()
try {
    . (Join-Path $PSScriptRoot "recordings.ps1")
    $corePairs = @(Resolve-Recordings -CoreFile (Join-Path $root "ops\core_symbols.txt"))
} catch {
    Log "core resolver unavailable - drain order stays as listed" "WARN"
}
if ($corePairs.Count -gt 0) {
    $coreRes = @()
    # Resolve-Recordings returns PSCustomObjects (.venue/.symbol), NOT strings:
    # `$p -split ':'` coerces to "@{venue=...; symbol=...}" (no colon -> count 1)
    # and silently skipped EVERY pair, making core-first ordering a no-op
    # (audit 2026-08-26). Read the properties directly.
    foreach ($p in $corePairs) {
        if ($p.venue -and $p.symbol) {
            $coreRes += ("^\d{{8}}_{0}_{1}\.log$" -f [regex]::Escape($p.venue), [regex]::Escape($p.symbol))
        }
    }
    if ($coreRes.Count -gt 0) {
        $isCoreFile = { param($name) foreach ($re in $coreRes) { if ($name -match $re) { return $true } } $false }
        $coreList  = @($candidates | Where-Object { & $isCoreFile $_.Name })
        $otherList = @($candidates | Where-Object { -not (& $isCoreFile $_.Name) })
        if ($coreList.Count -gt 0 -and $otherList.Count -gt 0) {
            Log ("Core-first: transferring {0} required-recording file(s) before {1} other leg(s)" -f $coreList.Count, $otherList.Count)
            $ordered = [System.Collections.Generic.List[object]]::new()
            foreach ($c in $coreList)  { $ordered.Add($c) }
            foreach ($c in $otherList) { $ordered.Add($c) }
            $candidates = $ordered
        }
    }
}

if ($WhatIf) {
    foreach ($c in $candidates) {
        $land = if (Test-Path (Join-Path $rawDir $c.Name)) { "EXISTS in master (verify)" } else { "-> data/raw" }
        Log ("DRY RUN: {0}  {1:N0} B  sha256 {2}  {3}" -f $c.Rel, $c.Size, $c.Hash.Substring(0, 12), $land)
    }
    Exit 0
}

# ---- stage phase: classify candidates, then pull ONLY the genuinely new ----
# ---- day-files into staging, PER-FILE (resumable) --------------------------
# Each new day-file moves in its own ssh|tar stream (the pull script's include
# list form streams exactly one rel), so a slow-link drop fails ONLY that file
# and the rest of the run proceeds - a single multi-GB archive is no longer
# lost to one blip (2026-08-18; previously one stream over one connection,
# where a mid-stream drop discarded every file's progress). The guard against
# partial transfers is unchanged: extraction lands in STAGING, every file is
# sha256+size-verified against the list, and nothing moves into data/raw or is
# released unless it verifies. The list hashes are stable: closed days are
# frozen (the collector rotated at UTC midnight).
#
# Pre-pull classification (2026-08-16) consults the master corpus + the
# manifest so the slow link moves only genuinely new closed days:
#   - byte-identical to master (same sha256) = previously drained, release
#     never confirmed (a held file) -> NO re-transfer, release re-attempted;
#   - different from master AND the manifest's LATEST entry for it is
#     action=collision with the same VPS sha256 = a KNOWN A-B overlap, kept
#     for the handoff -> NO transfer, NO release;
#   - anything else (new, or a first-time collision with no record yet) ->
#     transferred and classified by the land phase.
New-Item -ItemType Directory -Path $stagingDir -Force | Out-Null
# Resume (2026-08-18): leftovers from a previous failed run are re-VERIFIED,
# not discarded - a staged file whose size+sha256 match the CURRENT list is
# byte-identical to the frozen closed-day VPS file and is reused without
# re-transfer. A partial/dropped copy fails the check and is re-pulled;
# stale leftovers (no longer candidates) are cleaned at the end of the run.

$tar = Join-Path $env:WINDIR "System32\tar.exe"
if (-not (Test-Path $tar)) { Log "required binary missing: $tar" "ERROR"; Exit 3 }

# Manifest memory: per-file LATEST entry (the manifest is append-only; a
# later run's entry for a file wins). Used ONLY to prove a file is a known
# kept collision (action=collision, same VPS sha256) - the master-side hash
# check below remains the authority, so a deleted master file re-pulls
# correctly. A corrupt line never aborts the run (best-effort memory).
$manifestLatest = @{}
$invC = [System.Globalization.CultureInfo]::InvariantCulture
if (Test-Path $manifestF) {
    Get-Content -Path $manifestF -Encoding UTF8 | ForEach-Object {
        try {
            $e = $_ | ConvertFrom-Json
            if (-not $e.file -or -not $e.ts_utc) { return }
            # Audit 2026-08-17: culture-sensitive [datetime]::Parse on the
            # "o"-format timestamps breaks on non-invariant hosts - parse with
            # InvariantCulture + RoundtripKind (keeps UTC as UTC).
            $newTs = [datetime]::Parse($e.ts_utc, $invC, [System.Globalization.DateTimeStyles]::RoundtripKind)
            if (-not $manifestLatest.ContainsKey($e.file) -or
                $newTs -gt [datetime]::Parse($manifestLatest[$e.file].ts_utc, $invC, [System.Globalization.DateTimeStyles]::RoundtripKind)) {
                $manifestLatest[$e.file] = $e
            }
        } catch { }
    }
}

$toPull = [System.Collections.Generic.List[object]]::new()       # transferred + verified
$preIdentical = [System.Collections.Generic.List[object]]::new() # release re-attempts, no transfer
$knownCollisions = [System.Collections.Generic.List[object]]::new()
foreach ($c in $candidates) {
    $destPath = Join-Path $rawDir $c.Name
    if (Test-Path $destPath) {
        $destHash = Get-Sha256Hex $destPath
        if ($destHash -eq $c.Hash) {
            Log "already landed (identical): $($c.Name) - release re-attempt without re-transfer"
            $preIdentical.Add($c)
            continue
        }
        $mem = $manifestLatest[$c.Rel]
        if ($mem -and $mem.action -eq "collision" -and $mem.sha256 -eq $c.Hash) {
            Log "KNOWN COLLISION (transfer skipped, kept for handoff): $($c.Name)" "WARN"
            $knownCollisions.Add($c)
            continue
        }
    }
    $toPull.Add($c)
}

# ---- resume pass: reuse verified staging copies from a previous failed run ---
$resume = [System.Collections.Generic.List[object]]::new()
$needPull = [System.Collections.Generic.List[object]]::new()
foreach ($c in $toPull) {
    $stagePath = Join-Path $stagingDir $c.Name
    if (Test-Path $stagePath) {
        $sz = (Get-Item $stagePath).Length
        $h  = Get-Sha256Hex $stagePath
        if ($sz -eq $c.Size -and $h -eq $c.Hash) {
            Log "resumed from staging (size+sha256 match list): $($c.Name)"
            $resume.Add($c)
            continue
        }
        Log "staged copy of $($c.Name) does not verify ($sz B/$($h.Substring(0,12)) vs list $($c.Size) B/$($c.Hash.Substring(0,12))) - re-pulling" "WARN"
        Remove-Item $stagePath -Force
    }
    $needPull.Add($c)
}
$toPull = $needPull
# Gate-priority ordering (2026-08-18 §6 handoff): the 07:30 UTC Windows gate
# requires the hyperliquid recordings (core_symbols.txt), which the drain
# lands from the VPS - pull those FIRST so they land before the gate even on
# a slow night; the bybit legs (VPS-gate-only) follow whenever the link
# allows. Same list, same verify/land/release path - order only.
$sorted = @($toPull | Sort-Object { $_.Name -notmatch '_hyperliquid_' }, Name)
$toPull = [System.Collections.Generic.List[object]]::new($sorted)
Log ("pull plan: {0} new to pull, {1} resumed from staging (verified), {2} identical (release re-attempt), {3} known A-B collisions (transfer skipped)" -f `
    $toPull.Count, $resume.Count, $preIdentical.Count, $knownCollisions.Count)

$transferFailed = [System.Collections.Generic.List[string]]::new()
if ($toPull.Count -gt 0) {
    # One ssh|tar stream PER FILE: the pull script's include-list form streams
    # exactly the one rel, so a dropped stream fails only that file and the
    # next run re-pulls just the missing ones (resume). ssh stderr (connection
    # /pipe errors) and tar stderr (extraction errors, truncated archive) are
    # redirected to temp files and folded into the drain log on failure - never
    # mixed into the binary stream (a 2>&1 merge would corrupt the archive).
    # --strip-components=1: the stream carries paths relative to VpsBase
    # ("raw/<file>"), so extraction lands the bare filenames in staging.
    $sshErrFile = Join-Path $env:TEMP "vps-drain-ssh.err"
    $tarErrFile = Join-Path $env:TEMP "vps-drain-tar.err"
    foreach ($c in $toPull) {
        $remote = "bash ~/vps_drain_pull.sh '$VpsBase' '$($c.Rel)'"
        # cmd /c doubled-quote form (PS 5.1 Start-Process wraps the argument
        # and cmd strips the outer pair) - the PROVEN production construction.
        $cmdLine = '""{0}" -i {1} -o BatchMode=yes -o StrictHostKeyChecking=accept-new {2}@{3} "{4}" 2> "{5}" | "{6}" -xf - --strip-components=1 -C {7} 2> "{8}"' -f `
            $ssh, $SshKey, $SshUser, $VpsHost, $remote, $sshErrFile, $tar, $stagingDir, $tarErrFile
        # Stale stderr from a previous attempt must not be read as this one's.
        Remove-Item $sshErrFile, $tarErrFile -Force -ErrorAction SilentlyContinue
        Log ("transferring: {0} ({1:N0} B)" -f $c.Name, $c.Size)
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process -FilePath "cmd.exe" -ArgumentList "/c", $cmdLine -NoNewWindow -Wait -PassThru
        $sw.Stop()
        $failed = $proc.ExitCode -ne 0
        if (-not $failed -and -not (Test-Path (Join-Path $stagingDir $c.Name))) {
            # Exit 0 but nothing landed: the pipe carried no bytes - e.g. ssh
            # died before streaming (cmd's exit code reflects only the last
            # pipe command, tar). Read the captured stderr and treat it as a
            # transfer-level failure, not an integrity mismatch.
            $failed = $true
        }
        if ($failed) {
            $errLines = [System.Collections.Generic.List[string]]::new()
            foreach ($ef in @($sshErrFile, $tarErrFile)) {
                if (Test-Path $ef) {
                    foreach ($l in (Get-Content $ef -Encoding UTF8 -ErrorAction SilentlyContinue)) {
                        if ($l.Trim() -ne "") { $errLines.Add($l.Trim()) }
                    }
                }
            }
            if ($errLines.Count -eq 0) { $errLines.Add("(no stderr captured; ssh/tar exited $($proc.ExitCode))") }
            Log "transfer FAILED after $([math]::Round($sw.Elapsed.TotalMinutes,1))m: $($c.Name) (ssh/tar exit $($proc.ExitCode)) - re-attempt next run" "ERROR"
            foreach ($l in ($errLines | Select-Object -First 25)) { Log "    | $l" }
            $transferFailed.Add($c.Name)
            continue
        }
        Log "transferred in $([math]::Round($sw.Elapsed.TotalMinutes,1))m: $($c.Name)"
    }
} elseif ($resume.Count -gt 0) {
    Log "no new day-files to pull - $($resume.Count) already staged + verified from a previous run"
} else {
    Log "no new day-files to pull - all candidates pre-classified"
}

$verified = [System.Collections.Generic.List[object]]::new()
$mismatch = [System.Collections.Generic.List[string]]::new()
foreach ($c in $resume) {
    # Reused staging copies were size+sha256-verified against the list in the
    # resume pass - land them without re-verification.
    $verified.Add($c)
}
foreach ($c in $toPull) {
    if ($transferFailed.Contains($c.Name)) { continue }
    $stagePath = Join-Path $stagingDir $c.Name
    # A failed attempt never reaches here (absent files are caught in the
    # transfer loop); present files must match size AND sha256.
    $localSize = (Get-Item $stagePath).Length
    $localHash = Get-Sha256Hex $stagePath
    if ($localSize -ne $c.Size -or $localHash -ne $c.Hash) {
        $mismatch.Add("$($c.Rel) (size $localSize/$($c.Size) or hash mismatch)")
        Log "VERIFY FAIL for $($c.Rel): local $localSize B/$($localHash.Substring(0,12)) vs remote $($c.Size) B/$($c.Hash.Substring(0,12))" "ERROR"
        continue
    }
    $verified.Add($c)
    Log "verified: $($c.Rel) ($localSize B, sha256 $($localHash.Substring(0,12)))"
}

if ($mismatch.Count -gt 0) {
    Log "INTEGRITY MISMATCH: $($mismatch.Count) file(s) failed verification - NOTHING released. Re-attempt next run." "ERROR"
    Get-ChildItem -Path $env:TEMP -Filter "vps-drain-*-ssh.err" -File -ErrorAction SilentlyContinue | Remove-Item -Force
Remove-Item (Join-Path $env:TEMP "vps-drain-tar.err") -Force -ErrorAction SilentlyContinue
    Exit 2
}

# ---- land phase: move verified files into the master corpus (collision-aware) --
# $preIdentical (held re-attempts, classified before the pull) release first;
# the land loop adds the newly-landed files after the transfer.
$releasable = $preIdentical
foreach ($c in $verified) {
    $stagePath = Join-Path $stagingDir $c.Name
    $destPath  = Join-Path $rawDir $c.Name
    if (Test-Path $destPath) {
        $destHash = Get-Sha256Hex $destPath
        if ($destHash -eq $c.Hash) {
            Log "already landed (identical): $($c.Name) - releasing VPS copy"
            Remove-Item $stagePath -Force
            $releasable.Add($c)
        } else {
            Log "COLLISION: $($c.Name) already in master with different content (A-B overlap?) - VPS copy kept, NOT released, NOT overwritten" "WARN"
            Remove-Item $stagePath -Force
        }
        continue
    }
    Move-Item -Path $stagePath -Destination $destPath
    Log "landed: $($c.Name) -> data/raw"
    $releasable.Add($c)
}

# ---- release phase: only byte-verified + landed files are released ------------
# Per-file outcome feeds the manifest `release` field, so a silent release
# failure (ssh down, remote skip) is visible in the audit trail, not just the
# log. Values: released | skipped: <reason> | ssh_failed | no_release.
$released = 0; $skipped = 0
$releaseState = @{}   # Rel -> outcome; unset = not releasable (kept by design)
if ($NoRelease) {
    Log "-NoRelease: landed $($releasable.Count) file(s); VPS copies untouched (run without -NoRelease to release)"
    foreach ($r in $releasable) { $releaseState[$r.Rel] = "no_release" }
} elseif ($releasable.Count -gt 0) {
    $pairs = ($releasable | ForEach-Object { "$($_.Rel) $($_.Hash)" }) -join "`n"
    # Retry-wrapped (Invoke-Remote, 2026-08-25): this leg used to be ONE ssh
    # call per night - a transient exit 255 held every verified file on the
    # VPS until the next nightly run while the disk had no headroom. Retries
    # are safe here: the release script re-hashes each file before deleting,
    # so a re-run of an already-released pair comes back `skipped (missing)`.
    $relOut = Invoke-Remote `
        -RemoteCmd "bash ~/vps_drain_release.sh '$VpsBase'" `
        -SshErrFile (Join-Path $env:TEMP "vps-drain-release-ssh.err") `
        -InputLines $pairs
    if ($null -eq $relOut) {
        # Every attempt failed - stderr is captured in the temp file and the
        # log; count every releasable as not-released so the run exits 1 and
        # the task result is visible. Per-file states stay ssh_failed below.
        Log "release ssh failed after retries (last exit $script:LastRemoteExit): $script:LastRemoteErr" "ERROR"
        $skipped = $releasable.Count
        foreach ($r in $releasable) { $releaseState[$r.Rel] = "ssh_failed" }
    }
    if ($null -ne $relOut -and $script:LastRemoteExit -ne 0) {
        # The remote script ran but exited non-zero on the final attempt
        # (one or more skips, e.g. a hash changed before deletion) -
        # per-line states below are authoritative; do NOT blanket-count
        # releasable as skipped.
        Log "release script exited $($script:LastRemoteExit) - per-file states below" "WARN"
    }
    foreach ($line in $relOut) {
        if ($line -match '^released (\S+)') {
            $released++; $releaseState[$Matches[1]] = "released"; Log "released: $($Matches[1])"
        }
        elseif ($line -match '^skipped (\S+) (.+)$') {
            $skipped++; $releaseState[$Matches[1]] = "skipped: $($Matches[2])"
            Log "release skipped: $($Matches[1]) ($($Matches[2]))" "WARN"
        }
    }
    # Any releasable file without a confirmed outcome (e.g. the ssh stream died
    # mid-way) was never processed - mark it not-released so the manifest lies
    # in no direction.
    foreach ($r in $releasable) {
        if (-not $releaseState.ContainsKey($r.Rel)) {
            $releaseState[$r.Rel] = "ssh_failed"; $skipped++
        }
    }
    Log "release: $released released, $skipped skipped (skipped re-attempts next run)"
}

# ---- manifest (one entry per candidate, action- and release-labeled) ----------
foreach ($c in $candidates) {
    # Action from the MASTER state (authoritative for every candidate - the
    # pre-classified identical/known-collision files never entered the stream).
    $destPath = Join-Path $rawDir $c.Name
    if (-not (Test-Path $destPath)) {
        $action = "missing"
    } else {
        $action = if ((Get-Sha256Hex $destPath) -eq $c.Hash) { "landed" } else { "collision" }
    }
    # release: the VPS copy's disposition. "kept" = never in the releasable set
    # (A-B collision, missing) - the VPS copy stays by design.
    $release = if ($releaseState.ContainsKey($c.Rel)) { $releaseState[$c.Rel] } else { "kept" }
    $entry = [ordered]@{
        ts_utc     = (Get-Date).ToUniversalTime().ToString("o")
        vps_host   = $VpsHost
        vps_base   = $VpsBase
        file       = $c.Rel
        size       = $c.Size
        sha256     = $c.Hash
        action     = $action
        release    = $release
        no_release = [bool]$NoRelease
    }
    # A transfer that never landed is visible in the audit trail, not just the
    # log. The Rust manifest reader ignores unknown fields (tolerant parse).
    if ($transferFailed.Contains($c.Name)) { $entry["transfer_error"] = "transfer_failed" }
    Add-Content -Path $manifestF -Value ($entry | ConvertTo-Json -Compress) -Encoding UTF8
}

# ---- cleanup: staging holds only leftovers once every verified file has ---
# ---- been moved into data/raw (stale non-candidates, or unverified partials ---
# ---- from a dropped stream) - delete them and the transient stderr files so ---
# ---- the next run starts clean (resume re-verifies whatever a crash left ---
# ---- behind, never trusts it). Mismatch paths exit 2 BEFORE this point, so ---
# ---- staged evidence stays for inspection. ---------------------------------
Get-ChildItem -Path $stagingDir -File -ErrorAction SilentlyContinue | Remove-Item -Force
Get-ChildItem -Path $env:TEMP -Filter "vps-drain-*-ssh.err" -File -ErrorAction SilentlyContinue | Remove-Item -Force
Remove-Item (Join-Path $env:TEMP "vps-drain-tar.err") -Force -ErrorAction SilentlyContinue

Log "Drain complete -> $rawDir (manifest: $manifestF)"
# Audit 2026-08-17: the `$mismatch -gt 0 -> Exit 2` here was dead code - an
# integrity mismatch already exits 2 above (nothing is ever released on one),
# so only the partial-release contract (exit 1) applies at this point.
if ($transferFailed.Count -gt 0) {
    Log "transfer failures this run: $($transferFailed.Count) - $($transferFailed -join ', ') (re-attempt next run)" "WARN"
    Exit 1
}
if ($skipped -gt 0) { Exit 1 }
Exit 0
