# Design note — frame-header slimming (kind|len|crc, 9 B/event)

**Status:** design note, 2026-08-14 — not binding, no requirement IDs (same
stance as the spec 001 appendix it supports). Anything adopted here becomes a
future amendment to spec 001 with a `FORMAT_VER` bump; BDC-8 freezes the
current format, so nothing below changes today's writer.

Motivates the appendix's "bundling" revisit trigger: *if a format change is
needed anyway, fold the varint flip in for a single `FORMAT_VER` bump* — the
note prices the "e.g. slimming the 9 B/event frame header" example.

## The 9 bytes

`core/src/log.rs`: every frame is `kind:u8 || len:u32 || crc32:u32 ||
payload[len]` (`FRAME_HEADER_LEN = 9`, `MAX_FRAME_LEN = 64 MiB` sanity cap).
`kind` discriminates event vs symbol frames; `len` bounds the payload; `crc32`
is IEEE CRC-32 over the payload (EVT-4 torn-write detection). `schema_ver`
(u16) lives *inside* the payload and is untouched by any of this.

## What the header actually costs (measured 2026-08-14, real corpus)

Header share of stored bytes, per corpus type (`data/raw` + `data/collected.eventlog`, 38.8 M frames, 22.3 GB):

| corpus | payload B/ev | header share |
|---|---|---|
| trade-only day (2026-07-16, schema-1, 1 088 ev) | 68 | **11.7%** |
| hyperliquid market day (2026-08-14, BTC/ETH/positions) | ~150 | 5.4–5.8% |
| binance book day (2026-07-20, BTCUSDT, 2.73 M ev) | 325 | 2.7% |
| whole warehouse (38.8 M frames) | 566 avg | **1.57%** |

The header is already cheap: book-heavy days bury it under 1%, and even the
smallest-payload day (where the 9 B is most visible) it is 11.7%. Warehouse-wide
it is 1.6% — an order of magnitude below the varint payload win (appendix:
−29% to −38%).

## Slimming options

| option | header | total-file win (trade day / mixed day) | verdict |
|---|---|---|---|
| **len u32 → u24** (3-byte LE) | 9→8 B | −1.3% / −0.6% | viable — caps len at 16 MiB, 240× headroom over the largest real frame |
| **len u32 → u16** | 9→7 B | −2.6% / −1.3% | **off the table** — corpus has a 68 888 B frame (binance 2026-07-20) > 64 KiB; would need a frame-splitting policy |
| **crc32 → crc16** | 9→7 B | −2.6% / −1.3% | not worth it — EVT-4 names CRC32; halves the checksum (1/2^16 false accept vs 1/2^32) for the same rounding error |
| drop `kind` | 9→8 B | — | impossible — distinguishes event vs symbol frames |

The one realistic cut is **len u32 → u24** (9→8 B): the reader/writer change is
a 3-byte little-endian read/write plus a `MAX_FRAME_LEN` policy adjustment
(64 MiB → 16 MiB, still 240× the largest real frame). Everything else either
hits a real frame in the warehouse (u16) or weakens the crash-safety contract
for nothing (crc16).

## Bundling arithmetic

Per-event totals on the appendix's mixed day (payload 149.2 B/ev):

| wire | total B/ev | Δ vs today |
|---|---|---|
| today (legacy, 9 B hdr) | 158.2 | — |
| varint only | 112.8 | **−28.7%** |
| varint + u24 header | 111.8 | **−29.3%** |

Trade day (94.0 B/ev): −38.3% varint-only vs −39.3% bundled. The header slim
adds ~1 point on top of the varint win — free to bundle (same `FORMAT_VER`
bump, same reader dual-path), but it is a companion win, not a motivation.

## Recommendation

Keep the appendix's bundling trigger, with corrected expectations: the header
is 1.6% of warehouse bytes today; slimming it to 8 B (u24 len) is worth
≤1.3% and only makes sense folded into the varint flip. Reject u16 (real
64 KiB+ frames) and crc16 (contract downgrade) without further debate. Bincode
"fingerprints" (the appendix's other bundling example) are orthogonal — a
self-describing-format feature, not a size or CPU change; not priced here.

## Open questions

- None — if a flip is ever adopted, capture u24-frame golden vectors alongside
  the `fver == 2` varint vectors (same procedure as BDC-1/2).
