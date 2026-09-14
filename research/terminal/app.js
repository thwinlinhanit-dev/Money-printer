/* termd viewer — terminal Slice 1 (spec 011). Read-only canvas renderer over
 * the local termd API. No framework, no build step. */
"use strict";

const COL = {
  up: "#3fb950", down: "#f85149", gold: "#e3b341", grid: "#21262d",
  dim: "#8b949e", line: "#e6edf3", purple: "#a371f7", orange: "#d29922",
  bg: "#161b22",
};

const NS = { S: 1e9, MIN: 60e9, H: 3600e9 };

const state = {
  venue: null, symbol: null, date: null, tf: 60, bucket: "mid", kind: "delta",
  symbols: [], dates: new Set(), rawDates: new Set(),
  bars: [], footprint: [], cvd: [], funding: [], oi: [], whale: [],
  notes: {}, scrubIdx: null, dom: [], domAt: 0,
  // live push (spec 041 TER-4): one WS per symbol-day; the server polls the
  // feature parquet and coalesces updates into batch frames (≥100ms cadence).
  ws: null, wsTries: 0, wsSubKey: null,
  // app-level keepalive: an unanswered ping means the connection is dead —
  // close it so the badge flips to err and backoff reconnect takes over.
  wsPingTimer: null, wsAwaitingPong: false,
  lastDataTs: null,       // newest ts_ns seen from REST or push
  liveStatus: "off",      // off | on | err
  barsRefreshTimer: null, // coalesces trade pushes into one bars refetch
};

const $ = (id) => document.getElementById(id);
const status = $("status");

async function getJSON(path) {
  const r = await fetch(path);
  if (!r.ok) {
    let msg = r.statusText;
    try { msg = (await r.json()).error || msg; } catch (_) {}
    throw new Error(msg);
  }
  return r.json();
}

async function loadSymbols() {
  const { symbols } = await getJSON("/v1/symbols");
  const venues = [...new Set(symbols.map((s) => s.venue))].sort();
  const venueSel = $("venue");
  venueSel.innerHTML = venues.map((v) => `<option>${v}</option>`).join("");
  state.symbols = symbols;
  venueSel.onchange = populateSymbols;
  populateSymbols();
}

function populateSymbols() {
  const venue = $("venue").value;
  const syms = state.symbols.filter((s) => s.venue === venue).sort((a, b) =>
    a.symbol.localeCompare(b.symbol));
  const symSel = $("symbol");
  symSel.innerHTML = syms.map((s) => `<option>${s.symbol}</option>`).join("");
  symSel.onchange = populateDates;
  populateDates();
}

async function populateDates() {
  const venue = $("venue").value;
  const symbol = $("symbol").value;
  status.textContent = "loading dates…";
  try {
    const [feat, raw] = await Promise.all([
      getJSON(`/v1/dates?venue=${venue}&symbol=${symbol}`),
      getJSON(`/v1/rawdates?venue=${venue}&symbol=${symbol}`),
    ]);
    state.dates = new Set(feat.dates);
    state.rawDates = new Set(raw.dates);
    const all = [...new Set([...state.dates, ...state.rawDates])].sort();
    const dateSel = $("date");
    dateSel.innerHTML = all.map((d) => `<option>${d}</option>`).join("");
    dateSel.onchange = load;
    if (all.length) { dateSel.value = all[all.length - 1]; }
    load();
  } catch (e) {
    status.textContent = `error: ${e.message}`;
    status.classList.add("error");
  }
}

async function load() {
  const venue = $("venue").value;
  const symbol = $("symbol").value;
  const date = $("date").value;
  const tf = parseInt($("tf").value, 10);
  state.venue = venue; state.symbol = symbol; state.date = date; state.tf = tf;
  state.notes = {};
  status.textContent = "loading…";
  status.classList.remove("error");
  if (!date) { render(); return; }
  const q = `venue=${venue}&symbol=${symbol}&date=${date}`;
  try {
    const [bars, foot, cvd, funding, oi, whale] = await Promise.all([
      getJSON(`/v1/bars?${q}&tf=${tf}`),
      getJSON(`/v1/footprint?${q}&bucket=${$("bucket").value}&kind=${$("kind").value}`),
      getJSON(`/v1/cvd?${q}`),
      getJSON(`/v1/funding?${q}`),
      getJSON(`/v1/oi?${q}`),
      getJSON(`/v1/whale?${q}`),
    ]);
    state.bars = bars.points;
    state.footprint = foot.points;
    state.cvd = cvd.points;
    state.funding = funding.points;
    state.oi = oi.points;
    state.whale = whale.points;
    if (cvd.note) state.notes.cvd = cvd.note; else delete state.notes.cvd;
    for (const [k, v] of Object.entries({ bars, foot, cvd, funding, oi, whale })) {
      if (v.note) state.notes[k] = v.note;
    }
    state.scrubIdx = null;
    state.lastDataTs = maxTs(
      bars.points, cvd.points, foot.points, funding.points, oi.points, whale.points
    );
    render();
    liveRefresh();
    const missing = Object.values(state.notes);
    status.textContent =
      `${bars.points.length} bars · ${foot.points.length} fp · ${cvd.points.length} cvd` +
      (missing.length ? ` · ${missing.join("; ")}` : "");
  } catch (e) {
    status.textContent = `error: ${e.message}`;
    status.classList.add("error");
  }
}

function fmtTime(ts) {
  const d = new Date(ts / 1e6);
  return d.toISOString().slice(11, 19) + "Z";
}

function maxTs(...series) {
  let m = null;
  for (const pts of series) {
    for (let i = pts.length - 1; i >= 0; i--) {
      const t = pts[i] && pts[i].ts_ns;
      if (typeof t === "number" && (m === null || t > m)) { m = t; break; }
    }
  }
  return m;
}

function upsertPoints(pts, updates) {
  let changed = false;
  for (const u of updates) {
    if (!u || typeof u.ts_ns !== "number") continue;
    const last = pts[pts.length - 1];
    if (last && u.ts_ns <= last.ts_ns) continue; // only appends; series are sorted
    pts.push({ ts_ns: u.ts_ns, value: u.value });
    changed = true;
  }
  return changed;
}

function fmtPx(x) {
  return x >= 1000 ? x.toFixed(1) : x >= 1 ? x.toFixed(4) : x.toFixed(6);
}

/* min+max in one pass without spread args: a feature-backed day carries tens
 * of thousands of cvd rows, and `Math.min(...xs)` fanned out over that blows
 * the engine's argument limit (RangeError: Maximum call stack size exceeded). */
function extent(xs) {
  let lo = Infinity, hi = -Infinity;
  for (let i = 0; i < xs.length; i++) {
    const v = xs[i];
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  return [lo, hi];
}

function setupCanvas(id) {
  const c = $(id);
  const dpr = window.devicePixelRatio || 1;
  const rect = c.getBoundingClientRect();
  c.width = Math.max(1, Math.floor(rect.width * dpr));
  c.height = Math.max(1, Math.floor(rect.height * dpr));
  const ctx = c.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { ctx, w: rect.width, h: rect.height };
}

function clearPane(paneId, label) {
  const pane = $(paneId);
  const { ctx, w, h } = setupCanvas(paneId);
  ctx.fillStyle = COL.bg;
  ctx.fillRect(0, 0, w, h);
  if (label) {
    ctx.fillStyle = COL.dim;
    ctx.font = "13px sans-serif";
    ctx.textAlign = "center";
    ctx.fillText(label, w / 2, h / 2);
  }
  return { ctx, w, h };
}

const PAD = { l: 64, r: 14, t: 12, b: 22 };

function render() {
  // Each pane renders in its own guard: a crash in one (e.g. a degenerate
  // series) must not skip the rest — that is how a cvd error used to leave the
  // funding/OI strip and the DOM pane blank.
  for (const [id, fn] of [["main", renderMain], ["cvd", renderCvd], ["strip", renderStrip]]) {
    try {
      fn();
    } catch (e) {
      console.error(`render ${id}:`, e);
      try { clearPane(id, `render error: ${e.message}`); } catch (_) {}
    }
  }
  try { renderDom(); } catch (e) { console.error("render dom:", e); }
}

function barX(i, n) {
  const w = $( "main").getBoundingClientRect().width;
  const inner = w - PAD.l - PAD.r;
  return PAD.l + (n <= 1 ? inner / 2 : (i + 0.5) * inner / n);
}

function renderMain() {
  const bars = state.bars;
  const pane = $("mainPane");
  if (!bars.length) {
    const note = state.notes.bars || "no data for this symbol-day";
    clearPane("main", "no price data — " + note);
    return;
  }
  const { ctx, w, h } = clearPane("main");
  const innerW = w - PAD.l - PAD.r, innerH = h - PAD.t - PAD.b;
  const n = bars.length;
  const t0 = bars[0].ts_ns, t1 = bars[n - 1].ts_ns;
  const [priceMin] = extent(bars.map((b) => b.low));
  const [, priceMax] = extent(bars.map((b) => b.high));
  const pad = (priceMax - priceMin) * 0.05 || 1;
  const y = (p) => PAD.t + innerH - ((p - (priceMin - pad)) / (priceMax + pad - (priceMin - pad))) * innerH;
  const x = (i) => PAD.l + (n <= 1 ? innerW / 2 : (i + 0.5) * innerW / n);
  const bw = Math.max(1, innerW / n * 0.6);
  const fp = new Map(state.footprint.map((p) => [p.ts_ns, p.value]));
  const whaleAt = new Set(state.whale.map((p) => p.ts_ns));

  // grid + axis
  ctx.strokeStyle = COL.grid;
  ctx.fillStyle = COL.dim;
  ctx.font = "10px monospace";
  ctx.textAlign = "right";
  const ticks = 6;
  for (let i = 0; i <= ticks; i++) {
    const yy = PAD.t + innerH * i / ticks;
    ctx.beginPath(); ctx.moveTo(PAD.l, yy); ctx.lineTo(w - PAD.r, yy); ctx.stroke();
    ctx.fillText(fmtPx(priceMax + pad - (priceMax + pad - (priceMin - pad)) * i / ticks), PAD.l - 6, yy + 3);
  }
  ctx.textAlign = "center";
  for (let i = 0; i < n; i += Math.max(1, Math.floor(n / 6))) {
    ctx.fillText(fmtTime(bars[i].ts_ns), x(i), h - 6);
  }

  // candles + footprint coloring
  for (let i = 0; i < n; i++) {
    const b = bars[i];
    const v = fp.get(b.ts_ns);
    const up = b.close >= b.open;
    const color = v === undefined ? (up ? COL.up : COL.down) : (v >= 0 ? COL.up : COL.down);
    ctx.strokeStyle = color;
    ctx.fillStyle = color;
    const cx = x(i);
    // wick
    ctx.beginPath(); ctx.moveTo(cx, y(b.high)); ctx.lineTo(cx, y(b.low)); ctx.stroke();
    // body
    const yO = y(b.open), yC = y(b.close);
    const top = Math.min(yO, yC), hh = Math.max(1, Math.abs(yO - yC));
    ctx.fillRect(cx - bw / 2, top, bw, hh);
  }

  // footprint delta strip at the bottom of the pane
  if (fp.size) {
    const vals = [...fp.values()];
    const [, fAbsMax] = extent(vals.map(Math.abs));
    const m = Math.max(fAbsMax, 1e-9);
    for (let i = 0; i < n; i++) {
      const v = fp.get(bars[i].ts_ns);
      if (v === undefined) continue;
      const bh = (Math.abs(v) / m) * (innerH * 0.25);
      const yBase = h - PAD.b;
      ctx.fillStyle = v >= 0 ? COL.up : COL.down;
      ctx.globalAlpha = 0.35 + 0.65 * (Math.abs(v) / m);
      ctx.fillRect(x(i) - bw / 2, yBase - bh, bw, bh);
      ctx.globalAlpha = 1;
    }
  }

  // whale markers
  if (whaleAt.size) {
    for (let i = 0; i < n; i++) {
      if (!whaleAt.has(bars[i].ts_ns)) continue;
      const cx = x(i), cy = y(bars[i].high);
      ctx.fillStyle = COL.gold;
      ctx.beginPath();
      ctx.moveTo(cx, cy - 6); ctx.lineTo(cx + 5, cy - 1); ctx.lineTo(cx, cy + 5); ctx.lineTo(cx - 5, cy - 1);
      ctx.closePath(); ctx.fill();
    }
  }

  drawScrub(ctx, h, t0, t1);
}

function drawScrub(ctx, h, t0, t1) {
  if (state.scrubIdx === null || !state.bars.length) return;
  const i = Math.max(0, Math.min(state.scrubIdx, state.bars.length - 1));
  const cx = barX(i, state.bars.length);
  ctx.strokeStyle = COL.dim;
  ctx.setLineDash([4, 4]);
  ctx.beginPath(); ctx.moveTo(cx, 0); ctx.lineTo(cx, h); ctx.stroke();
  ctx.setLineDash([]);
  const b = state.bars[i];
  ctx.fillStyle = COL.dim;
  ctx.font = "11px monospace";
  ctx.textAlign = "left";
  ctx.fillText(`${fmtTime(b.ts_ns)}  O ${fmtPx(b.open)} H ${fmtPx(b.high)} L ${fmtPx(b.low)} C ${fmtPx(b.close)}`, 8, 10);
}

function renderCvd() {
  const pts = state.cvd;
  if (!pts.length) {
    clearPane("cvd", state.notes.cvd || "no cvd for this symbol-day");
    return;
  }
  const { ctx, w, h } = clearPane("cvd");
  const innerW = w - PAD.l - PAD.r, innerH = h - PAD.t - PAD.b;
  let cum = 0;
  const series = pts.map((p) => { cum += p.value; return { ts_ns: p.ts_ns, v: cum }; });
  const vs = series.map((p) => p.v);
  const [lo, hi] = extent(vs);
  const rng = (hi - lo) || 1;
  const y = (v) => PAD.t + innerH - ((v - lo) / rng) * innerH;
  const x = (ts) => {
    const n = state.bars.length;
    if (!n) return PAD.l;
    const t0 = state.bars[0].ts_ns, t1 = state.bars[n - 1].ts_ns;
    const f = (ts - t0) / (t1 - t0 || 1);
    return PAD.l + f * innerW;
  };
  ctx.strokeStyle = COL.grid;
  for (let i = 0; i <= 4; i++) {
    const yy = PAD.t + innerH * i / 4;
    ctx.beginPath(); ctx.moveTo(PAD.l, yy); ctx.lineTo(w - PAD.r, yy); ctx.stroke();
  }
  ctx.strokeStyle = COL.line;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  series.forEach((p, i) => {
    const xx = x(p.ts_ns), yy = y(p.v);
    i ? ctx.lineTo(xx, yy) : ctx.moveTo(xx, yy);
  });
  ctx.stroke();
  ctx.fillStyle = COL.line;
  ctx.font = "10px monospace";
  ctx.textAlign = "right";
  for (let i = 0; i <= 4; i++) {
    ctx.fillText((lo + rng * i / 4).toFixed(2), PAD.l - 6, PAD.t + innerH - innerH * i / 4 + 3);
  }
  drawScrub(ctx, h, state.bars[0]?.ts_ns ?? 0, state.bars[state.bars.length - 1]?.ts_ns ?? 0);
}

function renderStrip() {
  const { ctx, w, h } = clearPane("strip");
  const innerW = w - PAD.l - PAD.r;
  const half = (h - PAD.t - PAD.b) / 2;
  // funding line (top half)
  const fund = state.funding;
  ctx.fillStyle = COL.dim;
  ctx.font = "10px monospace";
  ctx.textAlign = "left";
  ctx.fillText("funding", 8, PAD.t + 10);
  if (fund.length) {
    const fvals = fund.map((p) => p.value);
    const [lo, hi] = extent(fvals);
    const rng = (hi - lo) || 1;
    const y = (v) => PAD.t + half - ((v - lo) / rng) * (half - 14);
    ctx.strokeStyle = COL.purple;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    fund.forEach((p, i) => {
      const xx = PAD.l + (fund.length <= 1 ? innerW / 2 : i * innerW / (fund.length - 1));
      const yy = y(p.value);
      i ? ctx.lineTo(xx, yy) : ctx.moveTo(xx, yy);
    });
    ctx.stroke();
  } else {
    ctx.fillStyle = COL.dim; ctx.textAlign = "center";
    ctx.fillText(state.notes.funding || "no funding", w / 2, PAD.t + half / 2);
  }
  // oi delta bars (bottom half)
  ctx.fillStyle = COL.dim;
  ctx.font = "10px monospace";
  ctx.textAlign = "left";
  ctx.fillText("oi delta", 8, PAD.t + half + 12);
  const oi = state.oi;
  if (oi.length) {
    const ov = oi.map((p) => p.value);
    const [, oAbsMax] = extent(ov.map(Math.abs));
    const m = Math.max(oAbsMax, 1e-9);
    const mid = PAD.t + half + (half) / 2;
    const maxH = half * 0.8;
    oi.forEach((p, i) => {
      const bw = Math.max(1, innerW / oi.length * 0.6);
      const xx = PAD.l + (oi.length <= 1 ? innerW / 2 : (i + 0.5) * innerW / oi.length);
      const bh = (Math.abs(p.value) / m) * maxH;
      ctx.fillStyle = p.value >= 0 ? COL.orange : COL.down;
      ctx.fillRect(xx - bw / 2, p.value >= 0 ? mid - bh : mid, bw, Math.max(1, bh));
    });
  } else {
    ctx.fillStyle = COL.dim; ctx.textAlign = "center";
    ctx.fillText(state.notes.oi || "no oi", w / 2, PAD.t + half + half / 2 + 6);
  }
}

let domFetchTimer = null;
function renderDom() {
  const body = $("domBody");
  const pts = state.dom;
  if (!pts.length) {
    body.innerHTML = `<div class="empty">${state.notes.dom || "scrub the price pane to inspect the book"}</div>`;
    return;
  }
  const s = pts[Math.min(Math.floor(pts.length / 2), pts.length - 1)];
  // Malformed point guard: /v1/dom points are mp-query output; a defensive
  // shape check keeps one bad row from blanking the pane (render()'s try/catch
  // would catch it, but an explicit message beats a silent blank).
  if (!s || !Array.isArray(s.asks) || !Array.isArray(s.bids)) {
    body.innerHTML = `<div class="empty">malformed book sample — skipping</div>`;
    return;
  }
  if (s.stale) {
    body.innerHTML = `<div class="empty">no trusted book at this time (seq gap)</div>`;
    return;
  }
  let html = `<div style="color:var(--dim);margin:2px 0 6px;">${fmtTime(s.ts_ns)} · ${state.venue}/${state.symbol}</div><table>`;
  for (const [px, qty] of s.asks) html += `<tr class="ask"><td class="px">${fmtPx(px)}</td><td class="qty">${Number(qty).toFixed(4)}</td></tr>`;
  html += `<tr><td colspan="2" style="border-top:1px solid var(--border)"></td></tr>`;
  for (const [px, qty] of s.bids) html += `<tr class="bid"><td class="px">${fmtPx(px)}</td><td class="qty">${Number(qty).toFixed(4)}</td></tr>`;
  html += "</table>";
  body.innerHTML = html;
}

async function scrub(tsNs) {
  const venue = state.venue, symbol = state.symbol, date = state.date;
  if (!venue || !symbol || !date) return;
  clearTimeout(domFetchTimer);
  domFetchTimer = setTimeout(async () => {
    try {
      const from = tsNs - 120 * NS.S, to = tsNs + 120 * NS.S;
      const r = await getJSON(`/v1/dom?venue=${venue}&symbol=${symbol}&date=${date}&from=${from}&to=${to}&every_ms=2000&levels=6`);
      state.dom = r.points;
      if (r.note) state.notes.dom = r.note; else delete state.notes.dom;
      renderDom();
    } catch (e) {
      state.dom = [];
      renderDom();
    }
  }, 150);
}

function attachScrub() {
  $("main").addEventListener("mousemove", (e) => {
    if (!state.bars.length) return;
    const rect = $("main").getBoundingClientRect();
    const x = e.clientX - rect.left - PAD.l;
    const innerW = rect.width - PAD.l - PAD.r;
    const n = state.bars.length;
    const idx = Math.max(0, Math.min(Math.floor(x / (innerW / n)), n - 1));
    if (idx !== state.scrubIdx) {
      state.scrubIdx = idx;
      renderMain();
      renderCvd();
      scrub(state.bars[idx].ts_ns);
    }
  });
  $("main").addEventListener("mouseleave", () => {
    state.scrubIdx = null;
    state.dom = [];
    renderMain();
    renderCvd();
    renderDom();
  });
}

// ─── Live push (spec 041 TER-4): /v1/ws on this termd ───
// Frame contract (termd.py): server → {type:"hello"}, {type:"subscribed"},
// {type:"batch", updates:[{feature_id,ts_ns,value},...], stale}, {type:"pong"},
// {type:"error"}; client → {type:"subscribe",features,venue,symbol,date}.
// The server polls the feature parquet per subscription and coalesces pushes
// into one batch ≥100ms apart, so the viewer just merges appended points.
function wsUrl() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}/v1/ws`;
}

function setLiveBadge() {
  const b = $("liveBadge");
  if (!b) return;
  b.className = state.liveStatus === "on" ? "on" : state.liveStatus === "err" ? "off" : "";
  b.textContent = "live · " + state.liveStatus;
}

// ─── WS keepalive ───
// termd answers {type:"ping"} with {type:"pong"} synchronously in its read
// loop, so an unanswered ping is a reliable silently-dead-socket signal (TCP
// half-open: sends succeed, nothing ever comes back). Cadence 3s → death is
// detected within 3–6s, well inside "seconds".
const WS_PING_MS = 3000;

function startPingLoop(sock) {
  stopPingLoop();
  state.wsAwaitingPong = false;
  state.wsPingTimer = setInterval(() => {
    // Superseded or already-closed socket: stand down without touching
    // shared state (the replacement owns the live loop now).
    if (state.ws !== sock || sock.readyState !== 1) return;
    if (state.wsAwaitingPong) {
      console.warn(`ws keepalive: no pong in ~${WS_PING_MS}ms — closing dead socket`);
      try { sock.close(); } catch (_) {} // onclose → badge err + backoff reconnect
      return;
    }
    state.wsAwaitingPong = true;
    try { sock.send(JSON.stringify({ type: "ping" })); }
    catch (e) {
      console.error("ws keepalive send failed:", e);
      try { sock.close(); } catch (_) {}
    }
  }, WS_PING_MS);
}

function stopPingLoop() {
  if (state.wsPingTimer) { clearInterval(state.wsPingTimer); state.wsPingTimer = null; }
  state.wsAwaitingPong = false;
}

function liveSubscribe() {
  const ws = state.ws;
  if (!ws || ws.readyState !== 1) return;
  state.wsSubKey = `cvd|${state.venue}|${state.symbol}|${state.date}`;
  ws.send(JSON.stringify({
    type: "subscribe",
    features: ["cvd.hyperliquid"],
    venue: state.venue, symbol: state.symbol, date: state.date,
  }));
}

function liveRefresh() {
  const key = state.venue && state.symbol && state.date
    ? `cvd|${state.venue}|${state.symbol}|${state.date}` : null;
  if (state.ws && key && state.wsSubKey === key) return; // already on this day
  wsConnect();
}

function wsConnect() {
  stopPingLoop(); // the new socket owns the keepalive loop
  if (state.ws) { try { state.ws.onclose = null; state.ws.close(); } catch (_) {} state.ws = null; }
  state.wsSubKey = null;
  if (!(state.venue && state.symbol && state.date)) return;
  let sock;
  try { sock = new WebSocket(wsUrl()); }
  catch (e) { console.error("ws connect failed:", e); state.liveStatus = "err"; setLiveBadge(); return; }
  state.ws = sock;
  sock.onopen = () => {
    state.wsTries = 0;
    state.liveStatus = "on";
    setLiveBadge();
    liveSubscribe();
    startPingLoop(sock);
  };
  sock.onmessage = (e) => {
    let msg = null;
    try { msg = JSON.parse(e.data); }
    catch (err) { console.error("ws: bad json frame", err); return; }
    try { handleLiveMsg(msg, sock); }
    catch (err) { console.error("ws: handler failed", err); }
  };
  sock.onclose = () => {
    if (state.ws !== sock) return;
    stopPingLoop();
    state.ws = null;
    state.wsSubKey = null;
    state.liveStatus = "err";
    setLiveBadge();
    const delay = Math.min(30000, 1000 * Math.pow(2, state.wsTries++))
      + Math.floor(Math.random() * 500);
    setTimeout(wsConnect, delay); // same symbol-day; load() replaces us on day switch
  };
  sock.onerror = () => {};
}

function handleLiveMsg(msg, sock) {
  switch (msg.type) {
    case "hello":
      break; // protocol handshake; version asserted server-side
    case "subscribed":
      break;
    case "batch": {
      // Drop frames from a superseded socket (e.g. a batch in flight across a
      // day switch) — they belong to another symbol-day.
      if (sock !== state.ws || state.wsSubKey === null) break;
      const upd = (msg.updates || []).filter((u) =>
        u && u.feature_id === "cvd.hyperliquid" && typeof u.ts_ns === "number");
      if (!upd.length) break;
      if (upsertPoints(state.cvd, upd)) {
        state.lastDataTs = maxTs(state.cvd);
        renderCvd();
      }
      if (upd.length) scheduleBarsRefresh(); // new trades landed → candles are stale
      break;
    }
    case "pong":
      if (sock === state.ws) state.wsAwaitingPong = false; // keepalive answered
      break;
    case "error":
      console.error("ws server error:", msg.error, msg.status || "");
      break;
    default:
      break;
  }
}

function scheduleBarsRefresh() {
  if (state.barsRefreshTimer) return; // coalesce bursts into one refetch
  state.barsRefreshTimer = setTimeout(() => {
    state.barsRefreshTimer = null;
    if (!(state.venue && state.symbol && state.date)) return;
    const q = `venue=${state.venue}&symbol=${state.symbol}&date=${state.date}`;
    getJSON(`/v1/bars?${q}&tf=${state.tf}`).then((bars) => {
      state.bars = bars.points;
      if (bars.note) state.notes.bars = bars.note;
      renderMain();
    }).catch((err) => console.error("bars refresh failed:", err));
  }, 2000); // ≥ tf-minimum; far below the mp-query cost of per-push refetch
}

window.addEventListener("resize", () => render());
document.addEventListener("DOMContentLoaded", () => {
  $("refresh").onclick = load;
  $("tf").onchange = load;
  $("bucket").onchange = load;
  $("kind").onchange = load;
  attachScrub();
  setLiveBadge();
  loadSymbols().catch((e) => { status.textContent = `error: ${e.message}`; status.classList.add("error"); });
});