# fetch_tools.ps1 - Vendors portable rclone + age into ops/tools/ (off-host
# backup tier, deploy.md section 7). The backup must never depend on a global
# install, so these live inside the repo (gitignored, PD-2 hygiene).
#
# Usage:  .\ops\scripts\fetch_tools.ps1
# Idempotent: skips a tool whose .exe already exists.

$ErrorActionPreference = "Stop"
$Tools = Join-Path $PSScriptRoot "..\tools"
New-Item -ItemType Directory -Force -Path $Tools | Out-Null
$curl = "curl.exe"

if (-not (Test-Path (Join-Path $Tools "rclone.exe"))) {
    Write-Host "fetch_tools: downloading rclone ..."
    $zip = Join-Path $Tools "rclone.zip"
    & $curl -fsSL --retry 3 -o $zip "https://downloads.rclone.org/rclone-current-windows-amd64.zip"
    Expand-Archive -Path $zip -DestinationPath (Join-Path $Tools "rclone-x") -Force
    $exe = Get-ChildItem (Join-Path $Tools "rclone-x") -Recurse -Filter rclone.exe | Select-Object -First 1
    Move-Item $exe.FullName (Join-Path $Tools "rclone.exe") -Force
    Remove-Item $zip, (Join-Path $Tools "rclone-x") -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "fetch_tools: rclone ready"
}

if (-not (Test-Path (Join-Path $Tools "age.exe"))) {
    Write-Host "fetch_tools: downloading age ..."
    $zip = Join-Path $Tools "age.zip"
    & $curl -fsSL --retry 3 -o $zip "https://github.com/FiloSottile/age/releases/download/v1.2.0/age-v1.2.0-windows-amd64.zip"
    Expand-Archive -Path $zip -DestinationPath (Join-Path $Tools "age-x") -Force
    $exe = Get-ChildItem (Join-Path $Tools "age-x") -Recurse -Filter age.exe | Select-Object -First 1
    Move-Item $exe.FullName (Join-Path $Tools "age.exe") -Force
    $kg = Get-ChildItem (Join-Path $Tools "age-x") -Recurse -Filter age-keygen.exe | Select-Object -First 1
    if ($kg) { Move-Item $kg.FullName (Join-Path $Tools "age-keygen.exe") -Force }
    Remove-Item $zip, (Join-Path $Tools "age-x") -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "fetch_tools: age ready"
}

& (Join-Path $Tools "rclone.exe") version | Select-Object -First 1
& (Join-Path $Tools "age.exe") --version
Write-Host "fetch_tools: done"
