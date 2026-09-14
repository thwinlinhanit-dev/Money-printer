# data_backup_check.ps1 - Post-run watchdog for the nightly MoneyPrinterDataBackup task.
#
# Watches the 00:07 UTC data-backup run through the Task Scheduler OPERATIONAL
# log (Microsoft-Windows-TaskScheduler/Operational) and verifies the two things
# that matter for the 09-05 launch-race incident (LastTaskResult 0xFFFD0000 =
# "action failed to launch" - the script never ran, so backup.log stayed silent):
#
#   1. LAUNCH: a run instance actually started (event 100/110/129/200), and
#   2. EXIT CODE: the action completed (event 201) with return code 0.
#   3. MIRROR EXCLUSION (2026-09-06): the transient staging/verify dirs
#      (.vps-drain-staging, .offhost-staging, .offhost-verify) must NEVER appear
#      under the mirror destination - backup_data.ps1 excludes them from
#      robocopy (/XD) and the integrity counts. Any reappearance = CRIT: it
#      proves the exclusion is not holding over time.
#
# The operational log was enabled on 2026-09-05 (64 MB cap) precisely so these
# trails exist; this check is the consumer. It reports the NEWEST run instance
# in the trail, so it works both as a scheduled post-run check (00:45 UTC) and
# as a manual "did the last run land?" probe any time.
#
# Usage:
#   .\ops\scripts\data_backup_check.ps1            # check the newest run instance
#   .\ops\scripts\data_backup_check.ps1 -Destination "C:\mp-backup"
#   .\ops\scripts\data_backup_check.ps1 -Register  # daily 00:45 UTC check task
#
# Exit codes (mirrors mp_health.ps1):
#   0 = launch verified + completed with exit 0 + mirror exclusion holds
#   1 = warning   (run still in progress at check time)
#   2 = critical  (no trail / never launched / non-zero exit / no run in 26h /
#                  transient staging/verify dir reappeared in the mirror)
#
# Safe to run any time; read-only. Requires the operational log to be enabled
# (wevtutil sl Microsoft-Windows-TaskScheduler/Operational /e:true).

param(
    [switch]$Register,             # register (or refresh) the daily MoneyPrinterDataBackupCheck task
    [string]$Destination = "C:\mp-backup"  # backup root whose "data" subdir is the mirror
)

$ErrorActionPreference = "Continue"
$TaskName = "MoneyPrinterDataBackup"
$LogName  = "Microsoft-Windows-TaskScheduler/Operational"
$root     = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$logFile  = Join-Path $PSScriptRoot "data_backup_check.log"

$warn = @(); $crit = @()

function Out-Line([string]$mark, [string]$msg) {
    $color = switch ($mark) { '[ OK ]' {'Green'} '[WARN]' {'Yellow'} '[CRIT]' {'Red'} default {'Gray'} }
    Write-Host ("{0,-7} {1}" -f $mark, $msg) -ForegroundColor $color
}

# ---- scheduled-task registration (mirrors backup_data.ps1 / mp_health.ps1) --
if ($Register) {
    $utcTarget = [DateTime]::SpecifyKind((Get-Date).ToUniversalTime().Date.AddMinutes(45), [DateTimeKind]::Utc)
    $localAt = $utcTarget.ToLocalTime()
    # Registration-race guard (2026-09-05): pin the first trigger to tomorrow
    # when registering at/after the target time (MoneyPrinterDataBackup 00:07Z
    # launch failure, 0xFFFD0000 - see daily_pipeline.ps1 -RegisterTask).
    if ($localAt -le (Get-Date)) { $localAt = $localAt.AddDays(1) }
    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument (`
        "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`" *> `"$logFile`"")
    $trigger = New-ScheduledTaskTrigger -Daily -At $localAt
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Hours 1)
    Register-ScheduledTask -TaskName "MoneyPrinterDataBackupCheck" -Action $action -Trigger $trigger `
        -Settings $settings -User $env:USERNAME -Force | Out-Null
    Write-Host ("[OK] Registered MoneyPrinterDataBackupCheck daily at {0} local (= {1} UTC, 38 min after the 00:07 backup), log {2}" -f `
        $localAt.ToString("HH:mm"), $utcTarget.ToString("HH:mm"), $logFile) -ForegroundColor Green
    Write-Host "     Re-run -Register after a DST change to re-pin the UTC window." -ForegroundColor Gray
    exit 0
}

Write-Host ("=== MoneyPrinterDataBackup post-run check  {0} ===" -f (Get-Date).ToString("yyyy-MM-dd HH:mm"))
Write-Host ""

# ---- collect the task's trail from the operational log (last 2 days) --------
# Word-boundary match: the check's own task (MoneyPrinterDataBackupCheck) logs
# events whose message CONTAINS "MoneyPrinterDataBackup" - a plain substring
# match would let the watcher certify its own run (found 2026-09-06).
$events = @()
try {
    $events = @(Get-WinEvent -FilterHashtable @{ LogName = $LogName; StartTime = (Get-Date).AddDays(-2) } -ErrorAction SilentlyContinue |
        Where-Object { $_.Message -match "\b$([regex]::Escape($TaskName))\b" })
} catch { }

if ($events.Count -eq 0) {
    Out-Line "[CRIT]" "no operational-log trail for $TaskName - log disabled (wevtutil sl $LogName /e:true) or task never ran"
    $crit += "no task trail in operational log"
} else {
    # Launch evidence: 100 started / 110+129 launched (with PID) / 200 action launched.
    # Completion: 201 carries the action's return code. 103/203/204 = failure events.
    $launch = $events | Where-Object { $_.Id -in 100,110,129,200 } | Sort-Object TimeCreated -Descending | Select-Object -First 1
    $done   = $events | Where-Object { $_.Id -eq 201 }                     | Sort-Object TimeCreated -Descending | Select-Object -First 1
    $failed = $events | Where-Object { $_.Id -in 103,203,204,111 }         | Sort-Object TimeCreated -Descending | Select-Object -First 1

    if ($null -eq $launch) {
        # Queued but never started is exactly the 0xFFFD0000 launch-race signature.
        $when = if ($failed) { $failed.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss") } elseif ($events[0]) { $events[0].TimeCreated.ToString("yyyy-MM-dd HH:mm:ss") } else { "?" }
        Out-Line "[CRIT]" "no launch event for $TaskName (latest trail $when) - action FAILED TO LAUNCH (0xFFFD0000 class)"
        $crit += "data backup never launched"
    } else {
        $procId = ""
        if ($launch.Message -match "process ID (\d+)") { $procId = " (PID $($Matches[1]))" }
        Out-Line "  ..." ("newest run instance started {0}{1}" -f $launch.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss"), $procId)

        if ($null -ne $done -and $done.TimeCreated -ge $launch.TimeCreated) {
            if ($done.Message -match "with return code (-?\d+)") {
                $code = [int]$Matches[1]
                if ($code -eq 0) {
                    Out-Line "[ OK ]" ("launch verified + completed with return code 0 at {0}" -f $done.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss"))
                } else {
                    Out-Line "[CRIT]" ("launch verified but completed with return code {0} at {1} - backup FAILED (integrity/copy error)" -f $code, $done.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss"))
                    $crit += "data backup exit code $code"
                }
            } else {
                Out-Line "[CRIT]" "completion event found but return code unparseable: $($done.Message)"
                $crit += "data backup exit code unparseable"
            }
        } elseif ((Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue).State -eq "Running") {
            Out-Line "[WARN]" ("run in progress since {0} - still running at check time (slow night?), re-check later" -f $launch.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss"))
            $warn += "data backup still running"
        } else {
            Out-Line "[CRIT]" ("launched at {0} but no completion event and task not running - died mid-flight (no exit code landed)" -f $launch.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss"))
            $crit += "data backup died mid-flight"
        }
    }

    # Freshness gate: the newest completed run must be a recent night's.
    if ($null -ne $done) {
        $ageH = ((Get-Date) - $done.TimeCreated).TotalHours
        if ($ageH -gt 26) {
            Out-Line "[CRIT]" ("newest completed run is {0:N1}h old ({1}) - missed a night or the log stopped capturing" -f $ageH, $done.TimeCreated.ToString("yyyy-MM-dd HH:mm:ss"))
            $crit += "data backup stale (>26h)"
        }
    }
}

# ---- mirror exclusion guard (2026-09-06) --------------------------------------
# The data mirror must NEVER contain the transient staging/verify dirs:
# backup_data.ps1 excludes them from robocopy (/XD $ExcludedDirNames), from the
# integrity counts, and -PruneStale treats any copy as junk by policy. Presence
# here at any depth proves the exclusion is NOT holding (a copy slipped past
# /XD, the flag was dropped, or something wrote them manually). Keep in sync
# with backup_data.ps1: $ExcludedDirNames.
$bkDataRoot = Join-Path $Destination "data"
$ExcludedDirNames = ".vps-drain-staging", ".offhost-staging", ".offhost-verify"
if (Test-Path $bkDataRoot) {
    $found = @(Get-ChildItem -Path $bkDataRoot -Recurse -Directory -Force -ErrorAction SilentlyContinue |
        Where-Object { $ExcludedDirNames -contains $_.Name })
    if ($found.Count -gt 0) {
        $detail = foreach ($d in $found) {
            $files = @(Get-ChildItem -LiteralPath $d.FullName -Recurse -File -Force -ErrorAction SilentlyContinue)
            $b = ($files | Measure-Object Length -Sum).Sum
            "{0} ({1} file(s) / {2:N2} MiB)" -f $d.FullName, $files.Count, ($b / 1MB)
        }
        Out-Line "[CRIT]" ("mirror exclusion VIOLATED - transient dir(s) present: {0}" -f ($detail -join "; "))
        $crit += "transient staging/verify dir in mirror"
    } else {
        Out-Line "[ OK ]" "mirror exclusion holds - no $($ExcludedDirNames -join '/') under $bkDataRoot"
    }
}

# ---- persist a timestamped verdict line (gitignored *.log) -------------------
$verdict = if ($crit.Count -gt 0) { "CRIT" } elseif ($warn.Count -gt 0) { "WARN" } else { "OK" }
$detail  = if ($crit.Count -gt 0) { $crit -join "; " } elseif ($warn.Count -gt 0) { $warn -join "; " } else { "launch + exit 0 verified" }
Add-Content -Path $logFile -Value ("[{0}][{1}] {2}" -f (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ"), $verdict, $detail) -Encoding UTF8

Write-Host ""
if ($crit.Count -gt 0) {
    Out-Line "[CRIT]" "CRITICAL: $($crit -join '; ')"
    exit 2
} elseif ($warn.Count -gt 0) {
    Out-Line "[WARN]" "WARNINGS: $($warn -join '; ')"
    exit 1
} else {
    Out-Line "[ OK ]" "All green"
    exit 0
}