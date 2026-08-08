# ws-egress-filter (spec 024 incident — Binance futures WS geo-filter)

`fstream.binance.com` silently drops the non-book WS streams (`aggTrade`,
`markPrice@1s`, `forceOrder`, `ticker`) for this network's egress IP while the
book streams (`depth@100ms`, `bookTicker`) keep flowing. NOT a collector defect
— subscription URLs are correct; verified by a raw probe (see Diagnosis). The
interim REST mitigations (COL-25..28) keep `trade`/`mark_price`/`funding`
populated, but `forceOrder` (liquidations) has NO REST fallback, and a day fed
by the 15s REST mark poll cannot be promotable. Full fidelity requires the
proxy fix below.

## Symptoms
- `mp-audit --json` streams map shows `book` (WS) plus REST-injected
  `trade`/`mark_price`/`funding` — and no `forceOrder`.
- Daily scorecard not promotable: `sequence_gap` ×N + `stale_stream` + possibly
  `missing_stream` (the REST fallbacks' watermark gaps / 15s cadence).
- Raw probe: `STREAM aggTrade=0`, `markPrice@1s=0`, `forceOrder=0` while
  `depth@100ms`/`bookTicker` count > 0.

## Diagnosis
1. **Raw probe (direct egress)** — 20s window, counts frames per stream:
   ```bash
   node ops/scripts/ws_probe.mjs
   ```
   Book streams > 0 with trade/mark/forceOrder at 0 ⇒ the filter is active
   right now. (Node ≥ 22; Python is not installed on this host. A
   PowerShell/.NET ClientWebSocket probe undercounts to zero on the same
   connection — do not use it.)
2. Confirm REST is unaffected (it always has been): `curl -s
   https://fapi.binance.com/fapi/v1/ping` → 200.
3. Confirm the recording side: `mp-audit --data-dir data --venue binance
   --symbol BTCUSDT --date <YYYYMMDD> --json` — inspect the `streams` map.
4. `fstream.binance.vision` is **globally NXDOMAIN** — not a fallback host.
   Spot `stream.binance.com` delivers `aggTrade` but has no futures streams.
5. **2026-08-07 evidence — the drop is triggered by BOTH restricted-market geo
   AND datacenter/cloud reputation, not by region alone.** Four live tests
   (all with `curl` and `node` egress verified identical via `ipinfo.io`, no
   app-level split tunnel, Binance REST always 200):
   - **52.220.157.15, AWS Singapore (ap-southeast-1)** — the pre-existing
     egress; later identified as the Avira Phantom VPN exit (auto-connected).
     Dropped.
   - **18.153.213.167, AWS Frankfurt (eu-central-1)** — allowed region, Avira
     DE exit. Dropped (`aggTrade=0 / markPrice@1s=0 / forceOrder=0` while
     `depth@100ms`/`bookTicker` flowed).
   - **176.34.0.190, AWS Tokyo (ap-northeast-1)** — allowed region, Avira JP
     exit. Dropped, identical signature.
   - **202.165.92.154, Wundwin, Myanmar (Telecom International Myanmar)** —
     the host's own residential egress (true home ISP, no VPN). Myanmar is a
     Binance-restricted market. Dropped.
   Conclusion: the non-book WS streams require an egress that is BOTH in an
   allowed region AND not flagged — an allowed-region VPS/consumer-VPN
   (datacenter class) and restricted-market residential both fail. The repair
   needs a **residential/ISP-class static IP in an allowed region** (e.g.
   IPRoyal static ISP proxy, Frankfurt, ~$2.70/mo, crypto payment) or any
   egress that passes the step-1 probe BEFORE collectors are switched (spec
   024 rule).

## Remediation (proxy/VPN with allowed-region egress)

The collector WS transport already supports an egress proxy: `proxy =
"http://host:port"` (HTTP CONNECT) or `"socks5://host:port"` in the collector
config, or the `MP_WS_PROXY` env var which **overrides** config on both the
`--config` and flag paths. TLS is terminated against Binance with webpki roots
— the proxy only carries bytes, it never sees plaintext.

1. Provision a proxy/VPN whose egress IP is in an allowed region AND passes
   the step-1 probe — an allowed region alone is NOT sufficient (2026-08-07:
   AWS-Frankfurt consumer-VPN exit still filtered, see Diagnosis item 5). A
   local proxy client (Clash/v2ray-style) listening on `127.0.0.1` is fine —
   its upstream does the region hop. (As of 2026-08-06 no proxy is provisioned
   on this host:
   no client process, no listening proxy port, `MP_WS_PROXY` unset — this step
   is the owner action; steps 2–4 are one command once it exists.)
2. **Activate with one command** — `ops/scripts/set_ws_proxy.ps1` does steps
   2–4: sets the User-scope `MP_WS_PROXY`, fail-closed pre-flights the tunnel
   (`curl.exe -x` → Binance REST must return 200, else it rolls the var back
   and exits 1), then restarts the watchdog:
   ```powershell
   .\ops\scripts\set_ws_proxy.ps1 -ProxyUrl http://127.0.0.1:7890
   .\ops\scripts\set_ws_proxy.ps1 -Clear            # rollback
   ```
   Manual equivalent (if you prefer): the watchdog spawns collectors with no
   `--config`, so set the env var user-wide (spawned processes inherit it):
   ```powershell
   [Environment]::SetEnvironmentVariable('MP_WS_PROXY','http://127.0.0.1:7890','User')
   Stop-ScheduledTask -TaskName MoneyPrinterCollectorsWatchdog
   Start-ScheduledTask -TaskName MoneyPrinterCollectorsWatchdog
   ```
   Scope notes: the task currently runs as the logged-on user (interactive,
   verified 2026-08-06) so User scope is correct. ONLY if the task is
   re-registered with `-RegisterTask -AsSystem` do SYSTEM processes need
   `'Machine'` scope instead. Alternatively add `proxy = "..."` to a collector
   config and pass `--config` via the watchdog's `Spawn-Collector`.
   Confirm a clean spawn in the watchdog log / fresh heartbeat before trusting
   the recording, and grep the trace log for `routing WS through egress proxy`.
4. **Verify BEFORE trusting new recordings** (spec 024 rule). Check the audit
   `streams` map on a fresh window:
   ```bash
   mp-audit --data-dir data --venue binance --symbol BTCUSDT --date <new day> --json
   ```
   - `forceOrder` must appear — it has NO REST fallback, so its presence is the
     honest proof that WS non-book streams flow again.
   - `mark_price` must jump to ~1s cadence (≈86400/day) vs ~4488/day from the
     15s REST poll.
5. Restore full WS fidelity: flip `trade_source = "ws"` and `mark_source = "ws"`
   (or drop `--trade-source rest --mark-source rest` from the watchdog spawn)
   so recordings carry native trade/mark streams with no REST watermark gaps.
6. When a closed day audits clean (`all_clean: true`, no blocking findings),
   the Phase-0 7-day promotion streak starts counting again.

## Escalation
- `forceOrder` still absent after the proxy is configured ⇒ the proxy's egress
  region is also filtered — try another region/provider.
- Confirm the proxy supports full TCP (the WS upgrade) and that its upstream
  node's region is on Binance's allowed list.
