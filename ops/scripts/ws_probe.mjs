#!/usr/bin/env node
// ws_probe.mjs — raw WebSocket probe for the spec 024 egress filter.
//
// Connects DIRECTLY to the Binance futures WS (fstream.binance.com), subscribes
// to the streams that were silently dropped from this network since 2026-08-04
// (aggTrade, markPrice@1s, forceOrder) plus the book streams that DO flow
// (depth@100ms, bookTicker) as a positive control, and counts frames per
// stream over a fixed window.
//
// Why node: the PowerShell/.NET ClientWebSocket probe undercounted to zero on
// the same connection where this probe counted 4398 frames (PS 5.1
// async-over-sync receive quirk); node's native WebSocket is reliable here.
// Python is not installed on this host.
//
// Usage:
//   node ops/scripts/ws_probe.mjs                 # futures defaults, 20s
//   node ops/scripts/ws_probe.mjs <url> <secs>    # custom endpoint/window
//
// The streams are FIXED to BTCUSDT (edit $streams below to probe another
// symbol). Output: one "STREAM <label>=<count>" line per subscription. A
// subscription that should flow but counts 0 is the filter.
// Exit: 0 = probe completed AND the SUBSCRIBE ack arrived; 2 = connect or
// subscribe failed (fail-closed — a dead connection must not masquerade as
// "everything filtered", CONV-8).
//
// Proxy limitation: node's built-in WebSocket cannot route through a proxy, so
// this probe only verifies DIRECT egress. To verify that a proxy restores the
// dropped streams, point the collector at it (MP_WS_PROXY or config `proxy`)
// and check the recorded streams map (mp-audit --json) after a restart — the
// collector's transport is the same HTTP-CONNECT/SOCKS5 path the proxy fix
// depends on (collectors/src/ws.rs, mock-proxy unit tests, spec 024).
//
// Uses only node built-ins (WebSocket global, node >= 22).

const DEFAULTS = {
  url: 'wss://fstream.binance.com/ws',
  seconds: 20,
  streams: [
    'btcusdt@depth@100ms',
    'btcusdt@bookTicker',
    'btcusdt@aggTrade',
    'btcusdt@markPrice@1s',
    'btcusdt@forceOrder',
  ],
};

const url = process.argv[2] || DEFAULTS.url;
const seconds = Number(process.argv[3] || DEFAULTS.seconds);
const streams = DEFAULTS.streams;

// subscription suffix -> the Binance event type ("e" field) it produces.
const eventBySub = {
  'depth@100ms': 'depthUpdate',
  bookTicker: 'bookTicker',
  aggTrade: 'trade',
  'markPrice@1s': 'markPriceUpdate',
  forceOrder: 'forceOrder',
};

const tally = {};
const started = Date.now();
let acked = false;
let connectFailed = false;
let subscribeRejected = false;
const ws = new WebSocket(url);

ws.onopen = () => {
  console.log('PROBE direct -> ' + url);
  ws.send(JSON.stringify({ method: 'SUBSCRIBE', params: streams, id: 1 }));
};

ws.onmessage = (m) => {
  try {
    const j = JSON.parse(m.data);
    // A response to our SUBSCRIBE (id === 1): Binance answers success with
    // { "result": null, "id": 1 } and rejection with
    // { "error": { code, msg }, "id": 1 }. Audit 2026-08-08 (P2): the old gate
    // (`j.id === 1 && !j.e`) treated BOTH as the ack, so a subscribe REJECTION
    // exited 0 — defeating the documented fail-closed exit-2 contract. Require
    // the success shape explicitly and flag a rejection as a failure.
    if (j.id === 1) {
      if (j.error) {
        subscribeRejected = true;
        console.log('SUBSCRIBE_REJECTED=' + String(j.error.code || 'unknown'));
      } else if ('result' in j) {
        acked = true;
        console.log('SUBSCRIBE_ACKS=1');
      }
      return;
    }
    if (j.e) tally[j.e] = (tally[j.e] || 0) + 1;
  } catch (e) {
    /* non-JSON frame (e.g. pong) — ignore */
  }
};

ws.onerror = () => {
  connectFailed = true;
  console.log('WS_ERROR');
};

setTimeout(() => {
  const total = Object.values(tally).reduce((a, b) => a + b, 0);
  console.log('TOTAL_FRAMES=' + total);
  for (const sub of streams) {
    const event = eventBySub[sub.replace('btcusdt@', '')];
    console.log('STREAM ' + sub.replace('btcusdt@', '') + '=' + (tally[event] || 0));
  }
  console.log('ELAPSED_MS=' + (Date.now() - started));
  try {
    ws.close();
  } catch (e) {
    /* already closed */
  }
  // Fail-closed (CONV-8): a connection that errored, never got its SUBSCRIBE
  // ack, OR had its SUBSCRIBE rejected must exit 2, not 0 — otherwise a dead
  // connection or a refused subscription looks like "the filter dropped
  // everything".
  process.exit(connectFailed || subscribeRejected || !acked ? 2 : 0);
}, seconds * 1000);
