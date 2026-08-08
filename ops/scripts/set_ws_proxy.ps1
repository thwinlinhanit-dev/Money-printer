# set_ws_proxy.ps1 - one-command activation/rollback of the collector WS egress
# proxy (spec 024 incident, ops/runbooks/ws-egress-filter.md).
#
# Activate (after provisioning a proxy/VPN with Binance-allowed egress):
#   .\ops\scripts\set_ws_proxy.ps1 -ProxyUrl http://127.0.0.1:7890
#   .\ops\scripts\set_ws_proxy.ps1 -ProxyUrl socks5://127.0.0.1:1080
# Rollback (30s, zero data loss - W-6 append-only):
#   .\ops\scripts\set_ws_proxy.ps1 -Clear
# Skip the collector restart (e.g. testing):
#   .\ops\scripts\set_ws_proxy.ps1 -ProxyUrl http://127.0.0.1:7890 -NoRestart
#
# What it does, in order:
#   1. Sets (or clears) the User-scope MP_WS_PROXY env var - the collector
#      reads it on both the --config and flag paths (mp-collector.rs) and it
#      overrides any config `proxy =` line.
#   2. PRE-FLIGHT (activate only, fail-closed / CONV-8): verifies Binance REST
#      is reachable THROUGH the proxy with `curl.exe -x` BEFORE any collector
#      is touched. A dead tunnel aborts and rolls the env var back - a broken
#      proxy must never silently send the 24/7 collectors down a black hole.
#      TLS still terminates against Binance; the proxy only carries bytes.
#   3. Restarts the MoneyPrinterCollectorsWatchdog task so newly spawned
#      collectors inherit the variable.
#
# Why User scope: the watchdog task runs as the logged-on user (interactive,
# verified 2026-08-06), so a User-scope variable reaches spawned collectors.
# Do NOT use Machine scope unless the task is re-registered with -AsSystem.
#
# PowerShell 5.1-compatible (the operational host runs Windows PowerShell).

param(
    [string]$ProxyUrl,
    [switch]$Clear,
    [switch]$NoRestart,
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"

function Say {
    param([string]$Msg, [string]$Color = "White")
    if (-not $Quiet) { Write-Host $Msg -ForegroundColor $Color }
}

if ($Clear) {
    [Environment]::SetEnvironmentVariable('MP_WS_PROXY', $null, 'User')
    Say "[ok] MP_WS_PROXY cleared (User scope) - collectors fall back to direct egress + REST mitigations" "Green"
} else {
    if ([string]::IsNullOrWhiteSpace($ProxyUrl)) {
        Write-Host "usage: set_ws_proxy.ps1 -ProxyUrl http://host:port | socks5://host:port   (or -Clear)" -ForegroundColor Red
        Exit 2
    }
    if ($ProxyUrl -notmatch '^(http|socks5)://') {
        Write-Host "[FAIL] ProxyUrl must be an http:// or socks5:// URL - got '$ProxyUrl'" -ForegroundColor Red
        Exit 2
    }
    # curl.exe MUST exist BEFORE we set anything - otherwise a missing curl
    # would abort after SetEnvironmentVariable and leave MP_WS_PROXY pointing
    # at a dead proxy (fail-closed rollback would never run).
    if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) {
        Write-Host "[FAIL] curl.exe not found on PATH - cannot pre-flight the tunnel; nothing was changed." -ForegroundColor Red
        Exit 1
    }

    # 1. Set the env var (new processes see it; collectors restart next).
    [Environment]::SetEnvironmentVariable('MP_WS_PROXY', $ProxyUrl, 'User')
    Say "[..] MP_WS_PROXY=$ProxyUrl (User scope)" "Yellow"

    # 2. Pre-flight: can Binance REST be reached THROUGH the proxy? Fail-closed.
    $code = & curl.exe -s -o NUL -w "%{http_code}" -x $ProxyUrl -m 15 https://fapi.binance.com/fapi/v1/ping 2>$null
    if ($LASTEXITCODE -ne 0 -or $code -ne "200") {
        [Environment]::SetEnvironmentVariable('MP_WS_PROXY', $null, 'User')
        Write-Host "[FAIL] pre-flight: curl.exe -x $ProxyUrl -> HTTP $code (exit $LASTEXITCODE). MP_WS_PROXY rolled back - nothing was restarted." -ForegroundColor Red
        Write-Host "       Check the proxy client is running and its egress region is Binance-allowed (Frankfurt/Tokyo are safe; US/Ontario/UK/NL are not)." -ForegroundColor Yellow
        Exit 1
    }
    Say "[ok] pre-flight: Binance REST reachable through proxy (HTTP $code)" "Green"
}

# 3. Restart the watchdog so new collectors pick up the variable.
$task = Get-ScheduledTask -TaskName MoneyPrinterCollectorsWatchdog -ErrorAction SilentlyContinue
if ($task -and -not $NoRestart) {
    Say "[..] restarting MoneyPrinterCollectorsWatchdog ..." "Yellow"
    Stop-ScheduledTask -TaskName MoneyPrinterCollectorsWatchdog -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    Start-ScheduledTask -TaskName MoneyPrinterCollectorsWatchdog
    Say "[ok] watchdog restarted - collectors now route WS through the proxy. Verify BEFORE trusting recordings:" "Green"
} elseif (-not $task) {
    Say "[!!] MoneyPrinterCollectorsWatchdog task is not registered - start the watchdog first: .\ops\watchdog_collectors.ps1" "Yellow"
} elseif ($NoRestart) {
    if ($Clear) {
        Say "[ok] -NoRestart: MP_WS_PROXY cleared; it applies to the NEXT watchdog spawn (restart the task when ready)." "Green"
    } else {
        Say "[ok] -NoRestart: env var set; it applies to the NEXT watchdog spawn (restart the task when ready)." "Green"
    }
}

Say ""
Say "Next - Tier-1 verification (~10-15 min after spawn):" "Cyan"
Say "  target/release/mp-audit.exe --data-dir data --date <YYYYMMDD> --json" "Cyan"
Say "  - forceOrder must appear in the streams map (no REST fallback - its presence proves WS flows again)" "Cyan"
Say "  - mark_price should trend toward ~86400/day (1s WS) vs ~4488/day (15s REST poll)" "Cyan"
Say "  - trace log: grep 'routing WS through egress proxy' in data/raw/trace_*.log" "Cyan"
Say "Rollback anytime: .\ops\scripts\set_ws_proxy.ps1 -Clear" "Cyan"
