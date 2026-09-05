# offhost_backup.ps1 - Off-host backup tier (deploy.md section 7, OPS-5).
# Encrypts the corpus (raw + cold + features) and durable state (journal,
# runs, configs) with age and pushes to an rclone remote. Protects against
# the W-6 single-volume risk: the local backup_data.ps1 mirror is the SAME
# physical drive - a dead disk takes both, so the off-host copy is the
# true recovery tier.
#
# Design:
#   - Tooling is vendored at ops/tools/ (rclone.exe, age.exe, age-keygen.exe)
#     so the backup never depends on a global install.
#   - The age PUBLIC key is committed-referenced via the private key at
#     ops/keys/offhost.agekey (gitignored; PD-2). The private key must ALSO
#     be stored off-host by the owner - an encrypted backup whose key lives
#     on the same dead disk is not a backup (see deploy.md section 7).
#   - Push is incremental: rclone copy with --transfers and --checkers; only
#     new/changed files move. Encryption is gzip -> age per file (streaming,
#     no plaintext on disk; the raw corpus is text-heavy and gzips ~4x, so a
#     free-tier remote fits). The manifest stores the PLAINTEXT size AND
#     sha256 (computed before encryption - the off-host hash-of-record); the
#     restore drill gunzips and hash-verifies, so semantics are unchanged.
#   - Verify is hash-based (audit 2026-08-17): the push must use --size-only
#     (age re-encrypts every run, so ciphertext bytes differ for unchanged
#     files), which cannot see a same-size bit-flip in the corpus. The
#     post-push check therefore decrypts each REMOTE artifact and compares
#     the plaintext sha256 against the manifest - size-only alone is no
#     longer trusted. -SkipVerify opts out (huge remotes).
#   - Fail-closed: missing tool, missing key, missing remote, or any rclone
#     error => exit non-zero. An unconfigured backup must never silently
#     succeed.
#
# Usage:
#   .\ops\scripts\offhost_backup.ps1 -Remote "b2:money-printer"            # push
#   .\ops\scripts\offhost_backup.ps1 -Remote "b2:money-printer" -Register  # + daily task
#   .\ops\scripts\offhost_backup.ps1 -Remote "local:ops/offhost-drill"     # local drill (no cloud)
#
# Exit codes: 0 = pushed + verified; 1 = push failed; 2 = integrity/verify
# failed; 3 = config (missing tool/key/remote); 4 = preflight remote missing.

param(
    [Parameter(Mandatory = $true)][string]$Remote,   # rclone remote:path (e.g. b2:money-printer)
    [switch]$Register,                               # register MoneyPrinterOffhostBackup daily task
    [switch]$SkipVerify,                             # skip post-push rclone check (slow on big remotes)
    [string]$RcloneConfig,                           # optional path to rclone.conf (default ~/.config/rclone/rclone.conf)
    [string]$DataRoot = "data",                      # corpus root relative to repo root
    [string]$StateRoot = "journal"                   # durable state relative to repo root (comma list ok)
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression
$Repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$Tools = Join-Path $Repo "ops\tools"
$Rclone = Join-Path $Tools "rclone.exe"
$Age = Join-Path $Tools "age.exe"
$AgeKey = Join-Path $Repo "ops\keys\offhost.agekey"
$PublicKey = Join-Path $Repo "ops\keys\offhost.age.pub"

# gzip helper (built-in .NET GZipStream - no external tool, works on any Windows host).
function Compress-Gzip {
    param([string]$InFile, [string]$OutFile)
    $inStream = [System.IO.File]::OpenRead($InFile)
    try {
        $outStream = [System.IO.File]::Create($OutFile)
        try {
            $gz = New-Object System.IO.Compression.GZipStream($outStream, [System.IO.Compression.CompressionMode]::Compress)
            $inStream.CopyTo($gz)
            $gz.Dispose()
        } finally { $outStream.Dispose() }
    } finally { $inStream.Dispose() }
}

# gunzip helper (mirror of offhost_restore_drill.ps1) - used by the
# post-push hash verify to check the decrypted plaintext.
function Expand-Gzip {
    param([string]$InFile, [string]$OutFile)
    $inStream = [System.IO.File]::OpenRead($InFile)
    try {
        $gz = New-Object System.IO.Compression.GZipStream($inStream, [System.IO.Compression.CompressionMode]::Decompress)
        try {
            $outStream = [System.IO.File]::Create($OutFile)
            try { $gz.CopyTo($outStream) } finally { $outStream.Dispose() }
        } finally { $gz.Dispose() }
    } finally { $inStream.Dispose() }
}

# True if the file starts with the gzip magic bytes 1f 8b (legacy age-only
# artifacts are handled as-is by the verify).
function Test-GzipMagic {
    param([string]$Path)
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $buf = New-Object byte[] 2
        $n = $fs.Read($buf, 0, 2)
        return ($n -eq 2 -and $buf[0] -eq 0x1f -and $buf[1] -eq 0x8b)
    } finally { $fs.Dispose() }
}

# --- Preflight: fail closed on anything missing -------------------------
foreach ($t in @($Rclone, $Age)) {
    if (-not (Test-Path $t)) {
        Write-Error "offhost_backup: missing tool $t - run ops/tools/fetch_tools.ps1"; exit 3
    }
}
if (-not (Test-Path $AgeKey)) {
    Write-Error "offhost_backup: missing age private key $AgeKey (ops/keys/offhost.agekey)"; exit 3
}
if (-not (Test-Path $PublicKey)) {
    Write-Error "offhost_backup: missing age public key $PublicKey - regenerate or commit the .pub"; exit 3
}
$remoteName = ($Remote -split ":")[0]
$remotes = & $Rclone listremotes 2>$null
if ($LASTEXITCODE -ne 0 -or ($remotes -notcontains "${remoteName}:")) {
    Write-Error "offhost_backup: rclone remote '$remoteName' not configured (rclone config). Run a local drill with -Remote 'local:...' to test the pipeline."; exit 4
}

Write-Host "offhost_backup: remote=$Remote key=$PublicKey"

# --- Encrypt + push: corpus (raw/cold/features) -------------------------
# Per-file age encryption, then rclone copy (incremental). One file = one
# .age artifact; the manifest lists the mapping so restore knows names.
$staging = Join-Path $Repo "data\.offhost-staging"
New-Item -ItemType Directory -Force -Path $staging | Out-Null

# True if any decoding of a staging artifact stem maps to an EXISTING source
# file. The stem encodes rel paths with every separator -> '__', so a stem
# may also contain literal '__' (legal in a file name): every subset of the
# '__' occurrences can be a separator. Prune only when NO decoding exists -
# never delete an artifact whose source might still be live.
function Test-StagingSourceExists {
    param([string]$Stem)
    $hits = [regex]::Matches($Stem, "__")
    for ($mask = 0; $mask -lt [math]::Pow(2, $hits.Count); $mask++) {
        # Reconstruct left-to-right from the ORIGINAL match indices (fix
        # 2026-09-04): the old LastIndexOf-substitution loop could never split
        # a 3+ underscore run (`...ver=0___params.age` from a `_params` source
        # filename) correctly - it always took the rightmost pair, so every
        # `_params` artifact false-negatived, got pruned locally + remotely
        # every run, and churned ~76 re-uploads daily.
        $sb = New-Object System.Text.StringBuilder
        $ptr = 0
        for ($i = 0; $i -lt $hits.Count; $i++) {
            if ($mask -band [math]::Pow(2, $i)) {
                [void]$sb.Append($Stem.Substring($ptr, $hits[$i].Index - $ptr))
                [void]$sb.Append('\')
                $ptr = $hits[$i].Index + 2
            }
        }
        [void]$sb.Append($Stem.Substring($ptr))
        if (Test-Path (Join-Path $Repo $sb.ToString())) { return $true }
    }
    return $false
}

# --- Staging prune (audit 2026-08-17): artifacts whose corpus source no
# longer exists push forever and keep the remote's orphaned ciphertext alive
# (the corpus is W-6 append-only, but deleted-file artifacts must still stop
# being pushed). Before re-encrypting: drop orphaned staging artifacts and
# their remote counterparts. Fail-closed: a failed remote delete fails the run.
$verifyDir = Join-Path (Split-Path $staging -Parent) ".offhost-verify"
if (Test-Path $verifyDir) { Remove-Item $verifyDir -Recurse -Force }
foreach ($old in Get-ChildItem -Path $staging -File -Filter "*.age") {
    $stem = $old.Name.Substring(0, $old.Name.Length - 4)
    if (Test-StagingSourceExists -Stem $stem) { continue }
    Write-Host "offhost_backup: prune staging orphan $($old.Name) (corpus source no longer exists)"
    Remove-Item $old.FullName -Force
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"  # native stderr is not an error record
    & $Rclone deletefile "$Remote/$($old.Name)" 2>$null
    $delCode = $LASTEXITCODE
    $ErrorActionPreference = $prevEAP
    if ($delCode -ne 0) {
        Write-Error "offhost_backup: rclone deletefile failed for remote orphan $($old.Name) (exit $delCode)"; exit 1
    }
}

# Remote-only orphan sweep (fix 2026-09-04): the loop above only reaches
# remote orphans whose LOCAL artifact still exists. If a prior run died between
# the local Remove-Item and the remote deletefile, the remote ciphertext is
# never revisited - and since the verify phase's `rclone check` requires an
# exact mirror (it exits 1 on ANY difference, remote-only included), the run
# fails exit 2 every day forever. Sweep the other direction too: anything on
# the remote absent from the (post-prune) staging dir is an orphan and gets
# deleted, restoring the mirror invariant the check phase requires.
# Fail-closed: a failed remote list/delete fails the run.
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = "Continue"  # native stderr is not an error record
$remoteFiles = & $Rclone lsf "$Remote" 2>$null
$lsfCode = $LASTEXITCODE
$ErrorActionPreference = $prevEAP
if ($lsfCode -ne 0) {
    Write-Error "offhost_backup: rclone lsf failed during remote orphan sweep (exit $lsfCode)"; exit 1
}
$localNames = @(Get-ChildItem -Path $staging -File | ForEach-Object { $_.Name })
foreach ($rName in ($remoteFiles | ForEach-Object { $_.Trim() })) {
    if ($localNames -contains $rName) { continue }
    Write-Host "offhost_backup: prune remote-only orphan $rName (no local staging artifact)"
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $Rclone deletefile "$Remote/$rName" 2>$null
    $delCode = $LASTEXITCODE
    $ErrorActionPreference = $prevEAP
    if ($delCode -ne 0) {
        Write-Error "offhost_backup: rclone deletefile failed for remote-only orphan $rName (exit $delCode)"; exit 1
    }
}

$manifest = New-Object System.Collections.ArrayList
$corpusRoot = Join-Path $Repo $DataRoot
$sources = @(
    @{ name = "raw";      path = Join-Path $corpusRoot "raw" }
    @{ name = "cold";     path = Join-Path $corpusRoot "cold" }
    @{ name = "features"; path = Join-Path $corpusRoot "features" }
)
$nFiles = 0
$skipped = @()
$encrypt = {
    param($f)
    $rel = $f.FullName.Substring($Repo.Length).TrimStart('\', '/')
    $out = Join-Path $staging (($rel -replace '[\\/]', '__') + ".age")
    $parent = Split-Path $out -Parent
    if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    $gzTmp = "$out.gz"
    try {
        # Plaintext sha256 FIRST (audit 2026-08-17): the manifest is the
        # off-host tier's hash-of-record and MUST be computed before the
        # encryption step - the post-push verify decrypts the REMOTE artifact
        # and compares against this hash (a same-size bit-flip in the corpus
        # re-encrypts and replaces the remote copy, and --size-only can never
        # see it).
        $sha = (Get-FileHash -Path $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        Compress-Gzip $f.FullName $gzTmp
        # EAP Continue around the native call (same class as the push fix
        # 2026-09-04): age stderr must not become a NativeCommandError under
        # the global EAP=Stop; $LASTEXITCODE is the real signal.
        $prevEAP = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        & $Age -e -R $PublicKey -o $out $gzTmp
        $ageCode = $LASTEXITCODE
        $ErrorActionPreference = $prevEAP
        if ($ageCode -ne 0) {
            Remove-Item $gzTmp -Force -ErrorAction SilentlyContinue
            $today = (Get-Date).ToUniversalTime().ToString("yyyyMMdd")
            if ($f.Name -like "*${today}*") {
                Write-Warning "offhost_backup: SKIP locked live file (backed up tomorrow): $rel"
                $script:skipped += $rel
                return
            }
            Write-Error "age encrypt failed: $($f.FullName)"; exit 1
        }
        Remove-Item $gzTmp -Force
    } catch {
        Remove-Item $gzTmp -Force -ErrorAction SilentlyContinue
        # A live collector file is exclusive-locked -> cannot be read. The
        # current recording day is backed up tomorrow (backup_data.ps1
        # live-file semantics); anything else is a real failure. The lock
        # check matches today's date anywhere in the name (covers both the
        # `YYYYMMDD_*.log` recordings and the collector's `trace_YYYYMMDD_*.log`
        # debug output, which is also held open while it records).
        $today = (Get-Date).ToUniversalTime().ToString("yyyyMMdd")
        if ($f.Name -like "*${today}*") {
            Write-Warning "offhost_backup: SKIP locked live file (backed up tomorrow): $rel"
            $script:skipped += $rel
            return
        }
        Write-Error "encrypt failed (gzip/age): $($f.FullName) - $($_.Exception.Message)"; exit 1
    }
    $script:manifest.Add(@{ rel = $rel; size = $f.Length; sha256 = $sha }) | Out-Null
    $script:nFiles++
}
foreach ($s in $sources) {
    if (-not (Test-Path $s.path)) { continue }
    # .lock_* files are held open exclusively by running collectors (transient
    # coordination state, recreated on restore) - same exclusion as
    # backup_data.ps1 / vps_backup.ps1.
    Get-ChildItem -Path $s.path -Recurse -File | Where-Object { $_.Name -notlike '.lock_*' } | ForEach-Object { & $encrypt $_ }
}
Write-Host "offhost_backup: encrypted $nFiles corpus files -> $staging (skipped $($skipped.Count))"

# --- Encrypt + push: durable state (journal, runs, configs) -------------
foreach ($s in ($StateRoot -split ",")) {
    $p = Join-Path $Repo $s.Trim()
    if (-not (Test-Path $p)) { continue }
    Get-ChildItem -Path $p -Recurse -File | Where-Object { $_.Name -notlike '.lock_*' } | ForEach-Object { & $encrypt $_ }
}

# --- Manifest (unencrypted index; names/sizes only - no content) --------
$manifestPath = Join-Path $staging "_manifest.json"
@{
    artifacts = @($manifest)
    skipped   = @($skipped)
} | ConvertTo-Json -Depth 4 | Set-Content -Path $manifestPath -Encoding utf8

# --- Push ------------------------------------------------------------------
# --size-only: age re-encrypts with a fresh random key every run, so .age bytes
# differ even for unchanged files; gzip output is byte-identical for unchanged
# files and the corpus is append-only, so size equality == unchanged. Without
# this flag every daily run would re-upload the whole remote (defeats the
# incremental design). The manifest (plaintext sha256 per file) is pushed
# alongside and is what the check phase verifies against - size-only alone is
# never trusted for integrity (audit 2026-08-17).
# EAP must be Continue here (fix 2026-09-04): every other rclone call is
# wrapped, but the push was not - so rclone's stderr NOTICE (shared gdrive
# client_id retirement warning) became a NativeCommandError under the global
# EAP=Stop and terminally killed the run at push start, uploading nothing
# (exit 1, silent). Native stderr is NOT a failure signal; $LASTEXITCODE is.
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = "Continue"  # native stderr is not an error record
# Explicit IO/connect timeouts (2026-09-05): rclone's defaults let a stalled
# link hang the push forever (observed: 8h execution-limit kill mid-verify).
# A timeout turns a dead link into a fast failure the retry can recover from.
& $Rclone copy $staging "$Remote" --transfers 4 --checkers 8 --size-only --timeout 10m --contimeout 30s
$copyCode = $LASTEXITCODE
$ErrorActionPreference = $prevEAP
if ($copyCode -ne 0) { Write-Error "offhost_backup: rclone copy failed (exit $copyCode)"; exit 1 }
Write-Host "offhost_backup: pushed $nFiles artifacts to $Remote"

# --- Verify (post-push check, opt-out for huge remotes) --------------------
# --size-only cannot tell a same-size bit-flip from an unchanged file, and the
# remote copy of an unchanged file is STALE ciphertext of the old plaintext
# (age re-encrypts every run; the --size-only copy skips re-uploading it). So
# the real check decrypts each REMOTE artifact and compares the plaintext
# sha256 against the manifest (audit 2026-08-17) - a full hash verify of the
# off-host tier every run.
if (-not $SkipVerify) {
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = "Continue"  # native stderr is not an error record
    $verifyDir = Join-Path (Split-Path $staging -Parent) ".offhost-verify"
    try {
        New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null
        # Retry the check gate (2026-09-04): a just-uploaded gdrive object can
        # list with a stale size for a few seconds (eventual consistency), and
        # the link is flaky - a single transient failure must not fail the tier.
        $checkOk = $false
        for ($attempt = 1; $attempt -le 3 -and -not $checkOk; $attempt++) {
            & $Rclone check $staging "$Remote" --size-only --fast-list --timeout 10m --contimeout 30s 2>$null
            if ($LASTEXITCODE -eq 0) { $checkOk = $true }
            elseif ($attempt -lt 3) {
                Write-Warning "offhost_backup: rclone check attempt $attempt failed (exit $LASTEXITCODE) - retrying"
                Start-Sleep -Seconds 20
            }
        }
        if (-not $checkOk) {
            Write-Error "offhost_backup: rclone check FAILED after 3 attempts - push not verified (exit $LASTEXITCODE)"; exit 2
        }
        $vFail = 0
        foreach ($entry in $manifest) {
            $name = ($entry.rel -replace '[\\/]', '__') + ".age"
            $tmpAge = Join-Path $verifyDir "v.age"
            $tmpGz  = Join-Path $verifyDir "v.gz"
            $tmpPlain = Join-Path $verifyDir "v.plain"
            Remove-Item $tmpAge, $tmpGz, $tmpPlain -Force -ErrorAction SilentlyContinue
            # Retry the per-file download (2026-09-04): one flaky-link blip on
            # any of ~1,100 artifacts must not fail the whole tier's verify.
            $dlOk = $false
            for ($attempt = 1; $attempt -le 3 -and -not $dlOk; $attempt++) {
                # --timeout/--contimeout (2026-09-05): a stalled download hung
                # the verify past the task's execution limit (8h kill at
                # 04:24). A timeout fails the attempt fast; the retry loop
                # below recovers. 10m IO idle is generous for big parquet.
                & $Rclone copyto "$Remote/$name" $tmpAge --timeout 10m --contimeout 30s 2>$null
                if ($LASTEXITCODE -eq 0) { $dlOk = $true }
                elseif ($attempt -lt 3) { Start-Sleep -Seconds 10 }
            }
            if (-not $dlOk) {
                Write-Error "offhost_backup: verify download failed for $name (3 attempts)"; $vFail = 1; continue
            }
            & $Age -d -i $AgeKey -o $tmpGz $tmpAge
            if ($LASTEXITCODE -ne 0) {
                Write-Error "offhost_backup: verify decrypt failed for $($entry.rel)"; $vFail = 1; continue
            }
            if (Test-GzipMagic $tmpGz) {
                Expand-Gzip $tmpGz $tmpPlain
            } else {
                Move-Item $tmpGz $tmpPlain -Force
            }
            $sha = (Get-FileHash -Path $tmpPlain -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($sha -ne $entry.sha256) {
                Write-Error "offhost_backup: HASH MISMATCH $($entry.rel) remote=$sha manifest=$($entry.sha256)"; $vFail = 1
            }
        }
        if ($vFail -ne 0) { exit 2 }
    } finally {
        Remove-Item $verifyDir -Recurse -Force -ErrorAction SilentlyContinue
        $ErrorActionPreference = $prevEAP
    }
    Write-Host "offhost_backup: rclone check + plaintext hash verify green (local == remote)"
}

# --- Register daily task ----------------------------------------------------
if ($Register) {
    # 02:30 UTC (after the 01:00 drain lands closed days) - convert to local so
    # the trigger stays pinned to UTC across DST, same as vps_backup.ps1.
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddHours(2).AddMinutes(30), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Output redirected to a gitignored log (2026-09-04): the old action
    # captured nothing, so a scheduled exit-2 had no visible cause.
    $logPath = Join-Path $PSScriptRoot "offhost_backup.log"
    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument ("-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden " +
        "-Command `"& '{0}' -Remote '{1}' *> '{2}'`"" -f $PSCommandPath, $Remote, $logPath)
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        # ExecutionTimeLimit 36h (2026-09-05): the 8h limit killed a healthy
        # run mid-verify at exactly start+8h (observed 04:24, result 267014).
        # The pipeline legitimately needs >24h at observed link speeds: ~2h
        # encrypt + push + a full 53 GiB per-file hash verify at ~0.7 MiB/s
        # single-stream gdrive (~21h, measured 2026-09-05) - the 24h limit
        # was a near-miss kill risk. Tradeoff: IgnoreNew drops the next-day
        # trigger if a run spills past 09:00, so a very slow verify can skip
        # one day - acceptable vs losing the whole verify to a kill.
        -ExecutionTimeLimit (New-TimeSpan -Hours 36) `
        -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 5)
    Register-ScheduledTask -TaskName "MoneyPrinterOffhostBackup" -Action $action -Trigger $trigger -Settings $settings -User $env:USERNAME -Force | Out-Null
    Write-Host "offhost_backup: registered MoneyPrinterOffhostBackup (daily $($utcTarget.ToString('HH:mm')) UTC = $($localAt.ToString('HH:mm')) local)"
}

Write-Host "offhost_backup: done (files=$nFiles skipped=$($skipped.Count))"
if ($skipped.Count -gt 0) {
    Write-Warning "offhost_backup: PARTIAL - $($skipped.Count) live files skipped (exit 10)"
    exit 10
}
exit 0
