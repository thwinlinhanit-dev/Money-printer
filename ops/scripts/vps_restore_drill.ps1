# vps_restore_drill.ps1 - Restore drill for the VPS backup (vps-phase0-bringup.md
# sec 4 / deploy.md sec 7: "an untested backup is a hope, not a backup").
#
# Restores the latest day's corpus from the VPS backup (<dest>\vps-data) into a
# scratch dir and proves it usable with the REAL tooling:
#   1. Byte identity vs the live VPS source of truth (prefix-hash for the
#      append-only raw logs - they keep growing - full hash for the static
#      scorecard).
#   2. mp-ops audit on the restored raw logs (must parse and audit cleanly).
#   3. mp-ops promote against the restored scorecards (when one exists yet).
#
# Usage:
#   .\ops\scripts\vps_restore_drill.ps1 -Destination "C:\mp-backup"
#   .\ops\scripts\vps_restore_drill.ps1 -Destination "C:\mp-backup" -KeepScratch
#
# Exit codes: 0 = PASS (restore verified); 1 = FAIL; 3 = config error.

param(
    [Parameter(Mandatory = $true)]
    [string]$Destination,           # backup root that holds <Dest>\vps-data
    [string]$VpsHost = $env:MP_VPS_HOST,
    [string]$SshUser = "mp-egress",
    [string]$SshKey  = "",
    [switch]$KeepScratch            # keep the scratch dir on FAIL/PASS for inspection
)

$ErrorActionPreference = "Stop"
### VPS: fail-closed on missing host (PD-2; audit M-1) - never committed, never blank.
if ([string]::IsNullOrWhiteSpace($VpsHost)) {
    Write-Host "[!!] No VPS host set. Pass -VpsHost or set MP_VPS_HOST (never commit the IP - PD-2)." -ForegroundColor Red
    Exit 2
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

$vpsData  = Join-Path $Destination "vps-data"
$mpOps    = Join-Path $root "target\release\mp-ops.exe"
$ssh      = Join-Path $env:WINDIR "System32\OpenSSH\ssh.exe"
if (-not $SshKey) { $SshKey = Join-Path $HOME ".ssh\mp-egress_ed25519" }

$fail = $false
function Fail {
    param([string]$Msg)
    Write-Host "[FAIL] $Msg" -ForegroundColor Red
    $script:fail = $true
}

# ---- config sanity ------------------------------------------------------------
if (-not (Test-Path $mpOps))    { Write-Host "[!!] mp-ops.exe not found: $mpOps" -ForegroundColor Red; Exit 3 }
if (-not (Test-Path $ssh))      { Write-Host "[!!] ssh.exe not found: $ssh" -ForegroundColor Red; Exit 3 }
if (-not (Test-Path $SshKey))   { Write-Host "[!!] ssh key not found: $SshKey" -ForegroundColor Red; Exit 3 }
if (-not (Test-Path $vpsData))  { Write-Host "[!!] no VPS backup at $vpsData - run vps_backup.ps1 first" -ForegroundColor Red; Exit 3 }

# ---- pick the latest day present in the backup --------------------------------
$rawDir = Join-Path $vpsData "data\raw"
$scDir  = Join-Path $vpsData "data\scorecards"
$btc = Get-ChildItem -Path $rawDir -Filter "*_hyperliquid_BTC.log" -File -ErrorAction SilentlyContinue |
    Sort-Object BaseName -Descending | Select-Object -First 1
$eth = Get-ChildItem -Path $rawDir -Filter "*_hyperliquid_ETH.log" -File -ErrorAction SilentlyContinue |
    Sort-Object BaseName -Descending | Select-Object -First 1
if (-not $btc -or -not $eth) { Write-Host "[!!] backup holds no hyperliquid BTC+ETH raw logs yet" -ForegroundColor Red; Exit 1 }
$date = $btc.BaseName.Substring(0, 8)
Write-Host "== restore drill: latest backed-up day = $date (BTC: $($btc.Name), ETH: $($eth.Name))" -ForegroundColor Cyan

$scratch = Join-Path $env:TEMP ("mp-vps-drill-" + (Get-Date).ToString("yyyyMMddHHmmss"))
New-Item -ItemType Directory -Path (Join-Path $scratch "data\raw")  -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $scratch "data\scorecards") -Force | Out-Null

# ---- 1. restore the files ------------------------------------------------------
Copy-Item -Path $btc.FullName -Destination (Join-Path $scratch "data\raw\")
Copy-Item -Path $eth.FullName -Destination (Join-Path $scratch "data\raw\")
$scFile = Join-Path $scDir "$date.json"
$scCopied = $false
if (Test-Path $scFile) {
    Copy-Item -Path $scFile -Destination (Join-Path $scratch "data\scorecards\")
    $scCopied = $true
    Write-Host "restored scorecard: $date.json"
} else {
    Write-Host "no scorecard for $date in backup yet (first one lands after the first 00:05 UTC daily run) - promote check deferred" -ForegroundColor Yellow
}

# ---- 2. byte identity vs the live VPS -----------------------------------------
function Get-RemoteHashPrefix {
    param([string]$RemotePath, [int64]$Bytes)
    # PS 5.1 EAP hazard (audit 2026-08-17): native stderr is a TERMINATING
    # error under EAP=Stop even with 2>$null - override around the ssh call
    # and check the exit code so a transient ssh stderr line cannot abort the
    # drill mid-way.
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $out = & $ssh -i $SshKey -o BatchMode=yes -o StrictHostKeyChecking=accept-new `
        "$SshUser@$VpsHost" "head -c $Bytes $RemotePath | sha256sum" 2>$null
    $code = $LASTEXITCODE
    $ErrorActionPreference = $oldEap
    if ($code -ne 0) { Fail "remote hash failed for $RemotePath (ssh exit $code)"; return "" }
    return ($out -join "").Trim().Split(" ")[0]
}
function Get-RemoteHash {
    param([string]$RemotePath)
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $out = & $ssh -i $SshKey -o BatchMode=yes -o StrictHostKeyChecking=accept-new `
        "$SshUser@$VpsHost" "sha256sum $RemotePath" 2>$null
    $code = $LASTEXITCODE
    $ErrorActionPreference = $oldEap
    if ($code -ne 0) { Fail "remote hash failed for $RemotePath (ssh exit $code)"; return "" }
    return ($out -join "").Trim().Split(" ")[0]
}

foreach ($f in @($btc, $eth)) {
    $local = $f.FullName
    $size  = (Get-Item $local).Length
    $localHash = (Get-FileHash -Path $local -Algorithm SHA256).Hash.ToLowerInvariant()
    $remotePath = "/opt/money-printer/data/raw/" + $f.Name
    $remoteHash = Get-RemoteHashPrefix $remotePath $size
    if ($remoteHash -eq $localHash) {
        Write-Host "byte-identity OK (prefix): $($f.Name) ($size bytes)" -ForegroundColor Green
    } else {
        Fail "byte-identity MISMATCH on $($f.Name): local=$localHash remote_prefix=$remoteHash"
    }
}
if ($scCopied) {
    $local = (Join-Path $scratch "data\scorecards\$date.json")
    $localHash = (Get-FileHash -Path $local -Algorithm SHA256).Hash.ToLowerInvariant()
    $remoteHash = Get-RemoteHash "/opt/money-printer/data/scorecards/$date.json"
    if ($remoteHash -eq $localHash) {
        Write-Host "byte-identity OK: $date.json" -ForegroundColor Green
    } else {
        Fail "byte-identity MISMATCH on $date.json: local=$localHash remote=$remoteHash"
    }
}

# ---- 3. usability: real audit + promote against the restored state ------------
Push-Location $scratch
try {
    foreach ($sym in @("BTC", "ETH")) {
        $out = & $mpOps audit --date $date --venue hyperliquid --symbol $sym 2>&1
        $code = $LASTEXITCODE
        if ($code -eq 0 -and ($out -join " ") -match '"event_count"') {
            Write-Host "audit OK on restored $sym log (exit 0, event_count present)" -ForegroundColor Green
        } else {
            Fail "audit FAILED on restored $sym log (exit $code): $($out | Select-Object -First 3)"
        }
    }
    if ($scCopied) {
        $out = & $mpOps promote --scorecards-dir (Join-Path $scratch "data\scorecards") `
            --required hyperliquid:BTC --required hyperliquid:ETH 2>&1
        $code = $LASTEXITCODE
        if ($code -eq 0) {
            Write-Host "promote OK on restored scorecard (verdict parses; exit 0)" -ForegroundColor Green
        } else {
            Fail "promote FAILED on restored scorecard (exit $code): $($out | Select-Object -First 3)"
        }
    }
} finally {
    Pop-Location
}

# ---- 4. verdict ----------------------------------------------------------------
if ($fail) {
    Write-Host "== RESTORE DRILL: FAIL ==" -ForegroundColor Red
    if (-not $KeepScratch) { Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue }
    Exit 1
}
Write-Host "== RESTORE DRILL: PASS - backup is byte-identical and usable ==" -ForegroundColor Green
if (-not $KeepScratch) { Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue }
Exit 0
