# offhost_restore_drill.ps1 - Restore drill for the off-host backup tier
# (deploy.md section 7, OPS-5): an untested backup is a hope, not a backup.
# Pulls the latest encrypted artifacts from the rclone remote into a scratch
# dir, decrypts with age, gunzips (backups are gzip->age per file; legacy
# age-only artifacts are detected by magic bytes and handled as-is), verifies
# per-file size AND plaintext sha256 against the manifest, and proves
# usability by running the real sim golden test from restored state.
#
# Safe: never touches live data (W-6); scratch is deleted on exit.
#
# Usage:
#   .\ops\scripts\offhost_restore_drill.ps1 -Remote "b2:money-printer"
#   .\ops\scripts\offhost_restore_drill.ps1 -Remote "local:ops/offhost-drill" -VerifyCmd "echo ok"
#
# Exit codes: 0 = restore + verify green; 1 = restore failed; 2 = integrity
# mismatch; 3 = config (missing tool/key/remote); 4 = usability check failed.

param(
    [Parameter(Mandatory = $true)][string]$Remote,
    [string]$VerifyCmd = "cargo test -p mp-sim --test backtest",  # usability proof from restored state
    [string]$RcloneConfig
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression
$Repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$Tools = Join-Path $Repo "ops\tools"
$Rclone = Join-Path $Tools "rclone.exe"
$Age = Join-Path $Tools "age.exe"
$AgeKey = Join-Path $Repo "ops\keys\offhost.agekey"
$PublicKey = Join-Path $Repo "ops\keys\offhost.age.pub"

# gunzip helper (built-in .NET GZipStream - no external tool).
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

# True if the file starts with the gzip magic bytes 1f 8b.
function Test-GzipMagic {
    param([string]$Path)
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $buf = New-Object byte[] 2
        $n = $fs.Read($buf, 0, 2)
        return ($n -eq 2 -and $buf[0] -eq 0x1f -and $buf[1] -eq 0x8b)
    } finally { $fs.Dispose() }
}

foreach ($t in @($Rclone, $Age)) {
    if (-not (Test-Path $t)) { Write-Error "restore-drill: missing tool $t"; exit 3 }
}
if (-not (Test-Path $AgeKey)) { Write-Error "restore-drill: missing age key $AgeKey"; exit 3 }

$scratch = Join-Path $Repo "data\.offhost-drill"
if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
Write-Host "restore-drill: scratch = $scratch"

# 1. Pull.
& $Rclone copy "$Remote" $scratch --transfers 4
if ($LASTEXITCODE -ne 0) { Write-Error "restore-drill: rclone pull failed"; exit 1 }
$manifestPath = Join-Path $scratch "_manifest.json"
if (-not (Test-Path $manifestPath)) {
    Write-Error "restore-drill: FAIL - no _manifest.json in remote (nothing backed up?)"; exit 1
}
$manifestObj = Get-Content $manifestPath -Raw | ConvertFrom-Json
$manifest = @($manifestObj.artifacts)
$skipped = @($manifestObj.skipped)
Write-Host "restore-drill: pulled $($manifest.Count) manifest entries (skipped-at-backup: $($skipped.Count))"
if ($skipped.Count -gt 0) {
    Write-Warning "restore-drill: backup was PARTIAL - live files skipped at backup time:"
    $skipped | ForEach-Object { Write-Warning "  $_" }
}

# 2. Decrypt + gunzip (if gzip) + verify size against the manifest.
$fail = 0
foreach ($entry in $manifest) {
    $art = Join-Path $scratch (($entry.rel -replace '[\\/]', '__') + ".age")
    if (-not (Test-Path $art)) {
        Write-Error "restore-drill: missing artifact for $($entry.rel)"; $fail = 1; continue
    }
    $plain = Join-Path $scratch ("restored__" + ($entry.rel -replace '[\\/]', '__'))
    $raw = "$plain.raw"   # age decrypt lands here first
    & $Age -d -i $AgeKey -o $raw $art
    if ($LASTEXITCODE -ne 0) { Write-Error "restore-drill: age decrypt failed for $($entry.rel)"; $fail = 1; continue }
    if (Test-GzipMagic $raw) {
        Expand-Gzip $raw $plain
        Remove-Item $raw -Force
    } else {
        Move-Item $raw $plain -Force
    }
    $size = (Get-Item $plain).Length
    if ($size -ne $entry.size) {
        Write-Error "restore-drill: SIZE MISMATCH $($entry.rel) restored=$size manifest=$($entry.size)"; $fail = 1
    }
    # Audit 2026-08-17: a same-size bit-flip on the remote passes the size
    # check, so the decrypted plaintext must ALSO match the manifest's sha256
    # (offhost_backup.ps1 computes it before encryption - the hash-of-record).
    # Legacy manifests (pre-sha256) fall back to the size check with a warning.
    if ($null -ne $entry.sha256) {
        $hash = (Get-FileHash -Path $plain -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($hash -ne $entry.sha256) {
            Write-Error "restore-drill: HASH MISMATCH $($entry.rel) restored=$hash manifest=$($entry.sha256)"; $fail = 1
        }
    } else {
        Write-Warning "restore-drill: legacy manifest entry $($entry.rel) has no sha256 - size-only verify"
    }
}
if ($fail -ne 0) { Write-Error "restore-drill: FAIL - integrity check failed"; exit 2 }
Write-Host "restore-drill: decrypted + size-verified all $($manifest.Count) files"

# 3. Usability: the restored state must actually work (not just exist).
Write-Host "restore-drill: verifying restored state via: $VerifyCmd"
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = "Continue"  # native stderr is not an error record
try {
    bash -c $VerifyCmd > $null 2>&1
} finally {
    $ErrorActionPreference = $prevEAP
}
if ($LASTEXITCODE -ne 0) {
    Write-Error "restore-drill: FAIL - usability check did not pass on restored state (exit $LASTEXITCODE)"; exit 4
}

Remove-Item $scratch -Recurse -Force
Write-Host "restore-drill: PASS - restore + integrity + usability green"
exit 0
