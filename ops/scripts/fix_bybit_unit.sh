#!/bin/bash
# One-off fix (2026-08-13): the bybit unit was missing
# WorkingDirectory=/opt/money-printer, so the collector's relative "data" dir
# resolved to /data — read-only under ProtectSystem=strict — and the service
# crash-looped with EROFS (os error 30). Install the corrected unit and
# restart. Self-elevates: mp-egress is in google-sudoers (passwordless).
set -euo pipefail

sudo install -m 0644 /home/mp-egress/mp-collector@.service /etc/systemd/system/mp-collector@.service
sudo systemctl daemon-reload
sudo systemctl restart mp-collector@BTCUSDT
sleep 4
sudo systemctl status mp-collector@BTCUSDT --no-pager | head -10
echo "--- bybit data ---"
ls -la /opt/money-printer/data/raw/ 2>/dev/null | grep -i bybit || echo "(no bybit files yet)"
