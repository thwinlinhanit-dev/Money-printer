# recordings.ps1 - Single source of truth for resolving the Phase-0 recording
# set (SPEC-019).  Dot-sourced by start_collectors.ps1,
# ops/watchdog_collectors.ps1, and ops/scripts/daily_pipeline.ps1 so the
# venue->symbol parse, the binance default, and the fallback set can never
# drift between the three (audit 08-10: start_collectors.ps1 hardcoded binance
# heartbeats after the live recordings moved to hyperliquid:BTC/ETH on 08-08).
#
# Grammar (ops/core_symbols.txt, one entry per line):
#   venue:SYMBOL   -> exact venue/symbol pair (lowercase venue, uppercase symbol)
#   SYMBOL         -> binance venue (legacy default)
#   # / blank      -> ignored
# Anything else THROWS (fail loud): a malformed entry must never silently
# narrow the recorded or required set.  An empty/missing file falls back to
# the current Phase-0 pair hyperliquid:BTC + hyperliquid:ETH.
#
# Usage (from a caller that has dot-sourced this file):
#   $pairs = @(Resolve-Recordings -CoreFile $coreFile)      # from the file
#   $pairs = @(Resolve-Recordings -Raw @('hyperliquid:BTC')) # explicit override
# Each $pair is a pscustomobject with .venue and .symbol.

function Resolve-Recordings {
    param(
        [string]$CoreFile,
        [string[]]$Raw = @()
    )
    $lines = @()
    if ($Raw.Count -gt 0) {
        $lines = @($Raw)
    } elseif (Test-Path $CoreFile) {
        # Trim BEFORE filtering so a line with leading whitespace is not
        # silently dropped (audit 08-08).
        $lines = @(Get-Content $CoreFile | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '^[A-Za-z0-9]' -and $_ -notmatch '^\s*#' } | Where-Object { $_ -ne '' })
    }
    if ($lines.Count -eq 0) { $lines = @('hyperliquid:BTC', 'hyperliquid:ETH') }
    foreach ($line in $lines) {
        $entry = $line.Trim()
        if ($entry -match '^([a-z][a-z0-9]*):([A-Z0-9]{2,20})$') {
            [pscustomobject]@{ venue = $Matches[1]; symbol = $Matches[2] }
        } elseif ($entry -match '^([A-Z0-9]{2,20})$') {
            [pscustomobject]@{ venue = 'binance'; symbol = $Matches[1] }
        } else {
            throw "Invalid recording '$entry' (expected venue:SYMBOL or SYMBOL)"
        }
    }
}
