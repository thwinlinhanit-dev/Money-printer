# _launch_offhost_backup.ps1 - one-shot launcher: runs offhost_backup.ps1 in a
# detached process with stdout/stderr captured, so a long full push can run
# without blocking the terminal. Not part of the pipeline - scratch helper.
$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$script = Join-Path $PSScriptRoot "offhost_backup.ps1"
$out = Join-Path $repo "data\offhost_backup.log"
$err = Join-Path $repo "data\offhost_backup.err.log"
Remove-Item $out, $err -Force -ErrorAction SilentlyContinue
Start-Process -FilePath "powershell.exe" `
    -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$script`"", "-Remote", "gdrive:money-printer", "-Register") `
    -RedirectStandardOutput $out -RedirectStandardError $err -WindowStyle Hidden
Write-Host "launched offhost_backup.ps1 detached; log = $out"
