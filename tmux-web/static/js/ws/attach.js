// tmux-web — /ws/attach WebSocket + reconnect (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - connectWs                    (app.js:1691)
//   - ATTACH_WS_BACKOFFS_MS/JITTER (app.js:1795)
//   - scheduleAttachWsReconnect    (app.js:1798)
//   - handleControlFromServer      (app.js:1820)
//   - disconnectWs                 (app.js:1828)

import { state } from '../core/state.js';
import { withWsToken } from '../core/auth.js';
import { isRemoteMode } from '../remote/healthz.js';
import { setStatus, showPlaceholder } from '../terminal/xterm.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { startWindowsPolling, stopWindowsPolling } from '../sessions/windows.js';

export function connectWs(sessionName, origin) {
    // На всякий случай закрываем старый.
    disconnectWs();

    if (!state.term) {
        console.error('terminal not initialized');
        return;
    }

    // Перед подключением fit — чтобы прислать корректные cols/rows в query.
    try { state.fitAddon.fit(); } catch (_) {}
    const cols = state.term.cols || 80;
    const rows = state.term.rows || 24;

    const proto = (location.protocol === 'https:') ? 'wss:' : 'ws:';
    const serverParam = (isRemoteMode() && origin && origin !== 'local')
        ? `&server=${encodeURIComponent(origin)}`
        : '';
    const url = `${proto}//${location.host}/ws/attach`
        + `?session=${encodeURIComponent(sessionName)}`
        + `&cols=${cols}&rows=${rows}`
        + serverParam;

    setStatus('connecting', `connecting → ${sessionName}…`);
    state.currentSession = sessionName;
    state.attachWsOrigin = origin || null;
    renderSidebar();

    let ws;
    try {
        ws = new WebSocket(withWsToken(url));
    } catch (e) {
        console.error('WebSocket ctor failed', e);
        setStatus('error', 'ws ctor error');
        scheduleAttachWsReconnect();
        return;
    }
    ws.binaryType = 'arraybuffer';
    state.ws = ws;
    state.lastResizeKey = cols + 'x' + rows;

    ws.onopen = () => {
        setStatus('connected', `attached → ${sessionName}`);
        showPlaceholder(false);
        state.attachWsBackoffStep = 0;
        if (state.term) {
            state.term.reset();
            state.term.focus();
        }
        startWindowsPolling();
    };

    ws.onmessage = (ev) => {
        const data = ev.data;
        if (data instanceof ArrayBuffer) {
            if (state.term) {
                state.term.write(new Uint8Array(data));
            }
        } else if (typeof data === 'string') {
            try {
                const msg = JSON.parse(data);
                handleControlFromServer(msg);
            } catch (_) {
                if (state.term) state.term.write(data);
            }
        }
    };

    ws.onerror = (ev) => {
        console.warn('ws error', ev);
        setStatus('error', 'ws error');
    };

    ws.onclose = (ev) => {
        console.info('ws closed', ev.code, ev.reason);
        state.ws = null;
        if (state.attachWsClosedByUs) {
            state.attachWsClosedByUs = false;
            setStatus('disconnected', 'disconnected');
            return;
        }
        setStatus('reconnecting', 'reconnecting…');
        scheduleAttachWsReconnect();
    };
}

const ATTACH_WS_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000];
const ATTACH_WS_JITTER_MAX_MS = 1000;

export function scheduleAttachWsReconnect() {
    if (state.attachWsClosedByUs) return;
    if (state.attachWsReconnectTimer) return;
    const session = state.currentSession;
    if (!session) return;
    const origin = state.attachWsOrigin || null;
    const idx = Math.min(state.attachWsBackoffStep || 0, ATTACH_WS_BACKOFFS_MS.length - 1);
    const base = ATTACH_WS_BACKOFFS_MS[idx];
    const jitter = Math.floor(Math.random() * ATTACH_WS_JITTER_MAX_MS);
    const delay = base + jitter;
    state.attachWsBackoffStep = Math.min(
        (state.attachWsBackoffStep || 0) + 1,
        ATTACH_WS_BACKOFFS_MS.length - 1,
    );
    state.attachWsReconnectTimer = setTimeout(() => {
        state.attachWsReconnectTimer = null;
        if (!state.currentSession) return;
        connectWs(state.currentSession, origin);
    }, delay);
}

export function handleControlFromServer(msg) {
    if (!msg || typeof msg !== 'object') return;
    if (msg.type === 'error') {
        console.warn('server reported error:', msg.message);
    }
}

export function disconnectWs() {
    state.attachWsClosedByUs = true;
    if (state.attachWsReconnectTimer) {
        clearTimeout(state.attachWsReconnectTimer);
        state.attachWsReconnectTimer = null;
    }
    stopWindowsPolling();
    if (state.ws) {
        try {
            state.ws.onmessage = null;
            state.ws.onerror = null;
            state.ws.onclose = null;
            state.ws.close();
        } catch (_) {}
        state.ws = null;
    }
}
