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
    for (const [k, v] of Object.entries({ bars, foot, cvd, funding, oi, whale })) {
      if (v.note) state.notes[k] = v.note;
    }
    state.scrubIdx = null;
    render();
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

function fmtPx(x) {
  return x >= 1000 ? x.toFixed(1) : x >= 1 ? x.toFixed(4) : x.toFixed(6);
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
  renderMain();
  renderCvd();
  renderStrip();
  renderDom();
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
  const priceMin = Math.min(...bars.map((b) => b.low));
  const priceMax = Math.max(...bars.map((b) => b.high));
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
    const m = Math.max(...vals.map(Math.abs), 1e-9);
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
  const lo = Math.min(...vs), hi = Math.max(...vs);
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
    const lo = Math.min(...fvals), hi = Math.max(...fvals);
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
    const m = Math.max(...ov.map(Math.abs), 1e-9);
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
async function renderDom() {
  const body = $("domBody");
  const pts = state.dom;
  if (!pts.length) {
    body.innerHTML = `<div class="empty">${state.notes.dom || "scrub the price pane to inspect the book"}</div>`;
    return;
  }
  const s = pts[Math.min(Math.floor(pts.length / 2), pts.length - 1)];
  if (s.stale) {
    body.innerHTML = `<div class="empty">no trusted book at this time (seq gap)</div>`;
    return;
  }
  let html = `<div style="color:var(--dim);margin:2px 0 6px;">${fmtTime(s.ts_ns)} · ${state.venue}/${state.symbol}</div><table>`;
  for (const [px, qty] of s.asks) html += `<tr class="ask"><td class="px">${fmtPx(px)}</td><td class="qty">${qty.toFixed(4)}</td></tr>`;
  html += `<tr><td colspan="2" style="border-top:1px solid var(--border)"></td></tr>`;
  for (const [px, qty] of s.bids) html += `<tr class="bid"><td class="px">${fmtPx(px)}</td><td class="qty">${qty.toFixed(4)}</td></tr>`;
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

window.addEventListener("resize", () => render());
document.addEventListener("DOMContentLoaded", () => {
  $("refresh").onclick = load;
  $("tf").onchange = load;
  $("bucket").onchange = load;
  $("kind").onchange = load;
  attachScrub();
  loadSymbols().catch((e) => { status.textContent = `error: ${e.message}`; status.classList.add("error"); });
});