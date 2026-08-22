#!/bin/bash
# Deploy 2026-08-13: bybit collector fix — the v5 subscribe used the dead
# `liquidation.` topic; bybit rejects the WHOLE subscribe frame ("handler not
# found"), so no stream flowed. Rebuilt with `allLiquidation.` (verified live:
# subscribe ack + orderbook/tickers flow). Install + restart, self-elevates.
set -euo pipefail

install -m 0755 /home/mp-egress/mp-build/target/release/mp-collector /opt/money-printer/bin/mp-collector
systemctl restart mp-collector@BTCUSDT
sleep 5
systemctl status mp-collector@BTCUSDT --no-pager | head -8
echo "--- bybit data ---"
ls -la /opt/money-printer/data/raw/ 2>/dev/null | grep -i bybit || echo "(none yet)"
