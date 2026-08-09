# set_ops_env.ps1 - persist the ops alert-egress credentials for the CURRENT
# user (setx, user-level). Run once after filling in your credentials; the
# scheduled tasks (MoneyPrinterDailyPipeline, MoneyPrinterDataBackup) and any
# console mp-ops run will then see them. No secrets are written to the repo.
#
# Usage:
#   .\ops\scripts\set_ops_env.ps1 -BotToken "123456:ABC..." -ChatId "-1001234567890" [-P1Webhook "https://hooks.example.com/p1"]
#
# Afterwards verify with:   powershell -NoProfile -Command "echo $env:TELEGRAM_BOT_TOKEN"

param(
    [string]$BotToken,      # Telegram bot token from @BotFather
    [string]$ChatId,        # Telegram chat id to deliver to
    [string]$P1Webhook,     # HTTPS P1 webhook URL (optional)
    [switch]$Clear          # remove all money-printer ops env vars
)

$vars = @("TELEGRAM_BOT_TOKEN", "TELEGRAM_CHAT_ID", "MP_OPS_P1_WEBHOOK")

if ($Clear) {
    foreach ($v in $vars) {
        [Environment]::SetEnvironmentVariable($v, $null, "User")
        Write-Host "[OK] cleared $v"
    }
    exit 0
}

if (-not $BotToken -or -not $ChatId) {
    Write-Host "[!!] -BotToken and -ChatId are required (or use -Clear)." -ForegroundColor Red
    exit 1
}

[Environment]::SetEnvironmentVariable("TELEGRAM_BOT_TOKEN", $BotToken, "User")
[Environment]::SetEnvironmentVariable("TELEGRAM_CHAT_ID", $ChatId, "User")
Write-Host "[OK] TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID set (user-level)." -ForegroundColor Green

if ($P1Webhook) {
    [Environment]::SetEnvironmentVariable("MP_OPS_P1_WEBHOOK", $P1Webhook, "User")
    Write-Host "[OK] MP_OPS_P1_WEBHOOK set (user-level)." -ForegroundColor Green
} else {
    Write-Host "[..] MP_OPS_P1_WEBHOOK not set - mp-ops p1-webhook stays 'dead until credentials'." -ForegroundColor Gray
}

Write-Host "     Note: already-running processes keep old env; new shells / scheduled tasks pick these up." -ForegroundColor Gray
