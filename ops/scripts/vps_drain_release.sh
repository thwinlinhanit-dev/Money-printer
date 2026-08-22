#!/bin/bash
# vps_drain_release.sh - Release (delete) VPS day-files that the nightly drain
# has byte-verified into the Windows master corpus. Reads lines of
#   <relpath> <sha256>
# on stdin; for each, RE-HASHES the file right now and deletes ONLY if the
# hash still matches. The re-hash is the partial-transfer guard's last line:
# if the file changed between the Windows-side pull and this call (it
# shouldn't for a closed day, but belt-and-braces), it is SKIPPED, not
# deleted, and the run reports it. Delete-only-when-verified = the W-6
# exception the owner approved for the relay drain (2026-08-14).
#
# Output: one line per decision:  released <relpath> | skipped <relpath> (reason)
# Exit: 0 = all released; 1 = any skipped (the caller logs and re-attempts next run).
set -euo pipefail

# Self-elevation guard (established VPS pattern, cf. deploy_bybit_multi.sh):
# the raw day-files are printer-owned and /opt/money-printer/data/raw is 755,
# so mp-egress has NO delete rights (the nightly release aborted with
# "rm: Permission denied" on every run since the drain shipped, 2026-08-16
# diagnosis). mp-egress is in google-sudoers (GCP NOPASSWD), so re-exec as
# root to make the verified deletes. Stdin (the file/hash pairs) is inherited
# through exec. Fail fast (-n) rather than hang if a password is required.
if [ "$(id -u)" -ne 0 ]; then
    exec sudo -n bash "$0" "$@"
fi

BASE="${1:-/opt/money-printer/data}"
cd "$BASE"

fail=0
# Strip CR: the Windows caller (PowerShell) pipes CRLF lines into ssh stdin.
while IFS=' ' read -r rel hash; do
    rel="${rel%$'\r'}"
    hash="${hash%$'\r'}"
    [ -z "$rel" ] && continue
    if [ ! -f "$rel" ]; then
        echo "skipped $rel (missing)"
        continue
    fi
    cur="$(sha256sum "$rel" | awk '{print $1}')"
    if [ "$cur" = "$hash" ]; then
        rm -f -- "$rel"
        echo "released $rel"
    else
        echo "skipped $rel (hash changed: expected ${hash:0:12} got ${cur:0:12})"
        fail=1
    fi
done
exit $fail
