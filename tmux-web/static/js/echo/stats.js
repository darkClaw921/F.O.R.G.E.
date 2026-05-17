// tmux-web — Echo stats sparkline + summary (Phase 5c)
//
// Простой canvas-renderer для tokens/min sparkline + summary text.
// Без сторонних charting-библиотек (vanilla bundle).
//
// API:
//   initStats(pollMs=30000)        — стартует polling getStats и привязывает к canvas
//   updateFromWs({tokens_in_per_min, tokens_out_per_min})  — incremental из WS
//   stopStats()                     — остановить polling

import { getStats } from './api.js';
import { $echoStatsCanvas, $echoStatsText } from '../core/dom.js';

const MAX_BUCKETS = 60; // последний час по minute-buckets

const state = {
    buckets_in: new Array(MAX_BUCKETS).fill(0),
    buckets_out: new Array(MAX_BUCKETS).fill(0),
    pollTimer: null,
};

export function initStats(pollMs) {
    pollMs = pollMs || 30000;
    pollOnce();
    if (state.pollTimer) clearInterval(state.pollTimer);
    state.pollTimer = setInterval(pollOnce, pollMs);
}

export function stopStats() {
    if (state.pollTimer) {
        clearInterval(state.pollTimer);
        state.pollTimer = null;
    }
}

async function pollOnce() {
    try {
        const data = await getStats('minute');
        if (data && Array.isArray(data.items)) {
            // Перезаполняем buckets по последним N точкам.
            const last = data.items.slice(-MAX_BUCKETS);
            const inN = last.map((p) => p.tokens_in || 0);
            const outN = last.map((p) => p.tokens_out || 0);
            // Pad до MAX_BUCKETS слева нулями
            state.buckets_in = padLeft(inN, MAX_BUCKETS, 0);
            state.buckets_out = padLeft(outN, MAX_BUCKETS, 0);
            redraw();
            const sumIn = inN.reduce((a, b) => a + b, 0);
            const sumOut = outN.reduce((a, b) => a + b, 0);
            updateSummary(sumIn, sumOut);
        }
    } catch (e) {
        // Не показываем toast — stats не критичны.
        console.debug('[echo-stats] poll failed', e);
    }
}

/**
 * Принимает один update от WS — добавляет в текущий bucket. Pусть просто
 * двигает последний bucket — фронт не пытается синхронизироваться с
 * server-tz, поскольку точное время минуты не критично для sparkline.
 */
export function updateFromWs(payload) {
    const last = state.buckets_in.length - 1;
    state.buckets_in[last] += (payload.tokens_in_per_min || 0);
    state.buckets_out[last] += (payload.tokens_out_per_min || 0);
    redraw();
    const sumIn = state.buckets_in.reduce((a, b) => a + b, 0);
    const sumOut = state.buckets_out.reduce((a, b) => a + b, 0);
    updateSummary(sumIn, sumOut);
}

function padLeft(arr, n, fill) {
    if (arr.length >= n) return arr.slice(-n);
    const pad = new Array(n - arr.length).fill(fill);
    return pad.concat(arr);
}

function updateSummary(sumIn, sumOut) {
    if (!$echoStatsText) return;
    $echoStatsText.textContent = `↓${formatK(sumIn)} ↑${formatK(sumOut)}`;
}

function formatK(n) {
    if (n < 1000) return String(n);
    if (n < 1_000_000) return (n / 1000).toFixed(1) + 'k';
    return (n / 1_000_000).toFixed(1) + 'M';
}

function redraw() {
    if (!$echoStatsCanvas) return;
    const ctx = $echoStatsCanvas.getContext('2d');
    if (!ctx) return;
    const W = $echoStatsCanvas.width;
    const H = $echoStatsCanvas.height;
    ctx.clearRect(0, 0, W, H);

    // Берём цвета из CSS-переменных (тематика).
    const cs = getComputedStyle(document.documentElement);
    const accent = cs.getPropertyValue('--accent').trim() || '#2a7fff';
    const warn = cs.getPropertyValue('--warn').trim() || '#d29922';
    const dim = cs.getPropertyValue('--fg-dim').trim() || '#8b949e';

    const maxV = Math.max(
        ...state.buckets_in,
        ...state.buckets_out,
        1,
    );

    const dx = W / MAX_BUCKETS;

    // baseline
    ctx.strokeStyle = dim + '44';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, H - 1);
    ctx.lineTo(W, H - 1);
    ctx.stroke();

    // tokens_out (warn)
    drawSeries(ctx, state.buckets_out, W, H, dx, maxV, warn);
    // tokens_in (accent)
    drawSeries(ctx, state.buckets_in, W, H, dx, maxV, accent);
}

function drawSeries(ctx, arr, W, H, dx, maxV, color) {
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    for (let i = 0; i < arr.length; i++) {
        const x = i * dx;
        const y = H - (arr[i] / maxV) * (H - 4) - 2;
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
    }
    ctx.stroke();
}
