// tmux-web — /ws/todos WebSocket + fetchTodos + polling
// (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - TODOS_WS_BACKOFFS_MS / TODOS_POLL_INTERVAL_MS (app.js:1896, 1902)
//   - fetchTodos             (app.js:2977)
//   - startTodosPolling/stopTodosPolling (app.js:2998, 3003)
//   - connectTodosWs         (app.js:3020)
//   - disconnectTodosWs      (app.js:3080)
//   - scheduleTodosWsReconnect (app.js:3092)
//   - handleTodosWsMessage   (app.js:3114)

import { state } from '../core/state.js';
import { withWsToken } from '../core/auth.js';
import { isRemoteMode } from '../remote/healthz.js';
import { renderTasks, currentTodosProjectId } from '../tasks/render.js';

const TODOS_WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];
const TODOS_POLL_INTERVAL_MS = 30000;

export async function fetchTodos(projectId) {
    const pid = projectId || currentTodosProjectId();
    try {
        const url = pid ? '/api/todos?project_id=' + encodeURIComponent(pid) : '/api/todos';
        const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/todos failed:', r.status);
            state.todosData = [];
            renderTasks();
            return;
        }
        const data = await r.json();
        state.todosData = Array.isArray(data) ? data : [];
        renderTasks();
    } catch (e) {
        console.warn('fetchTodos failed', e);
        state.todosData = state.todosData || [];
        renderTasks();
    }
}

export function startTodosPolling() {
    if (state.todosPollTimer) clearInterval(state.todosPollTimer);
    state.todosPollTimer = setInterval(() => fetchTodos(), TODOS_POLL_INTERVAL_MS);
}

export function stopTodosPolling() {
    if (state.todosPollTimer) {
        clearInterval(state.todosPollTimer);
        state.todosPollTimer = null;
    }
}

export function connectTodosWs() {
    if (state.todosWs && (
        state.todosWs.readyState === WebSocket.OPEN ||
        state.todosWs.readyState === WebSocket.CONNECTING
    )) {
        return;
    }
    state.todosWsClosedByUs = false;

    if (state.todosWsReconnectTimer) {
        clearTimeout(state.todosWsReconnectTimer);
        state.todosWsReconnectTimer = null;
    }

    const pid = currentTodosProjectId();
    const proto = (location.protocol === 'https:') ? 'wss' : 'ws';
    const server = (isRemoteMode()
        && state.activeOrigin
        && state.activeOrigin !== 'local'
        && state.activeOrigin !== 'all')
        ? state.activeOrigin
        : null;
    let qs = '';
    if (pid && !server) {
        qs = '?project_id=' + encodeURIComponent(pid);
    } else if (server) {
        qs = '?server=' + encodeURIComponent(server);
    }
    const url = `${proto}://${location.host}/ws/todos${qs}`;

    let ws;
    try {
        ws = new WebSocket(withWsToken(url));
    } catch (e) {
        console.warn('todos ws constructor failed', e);
        scheduleTodosWsReconnect();
        return;
    }
    state.todosWs = ws;

    ws.onopen = () => {
        state.todosWsBackoffStep = 0;
        stopTodosPolling();
    };
    ws.onmessage = (ev) => {
        handleTodosWsMessage(ev.data);
    };
    ws.onerror = (ev) => {
        console.debug('todos ws error', ev);
    };
    ws.onclose = () => {
        state.todosWs = null;
        if (state.todosWsClosedByUs) return;
        startTodosPolling();
        scheduleTodosWsReconnect();
    };
}

export function disconnectTodosWs() {
    state.todosWsClosedByUs = true;
    if (state.todosWsReconnectTimer) {
        clearTimeout(state.todosWsReconnectTimer);
        state.todosWsReconnectTimer = null;
    }
    if (state.todosWs) {
        try { state.todosWs.close(); } catch (_) {}
        state.todosWs = null;
    }
}

export function scheduleTodosWsReconnect() {
    if (state.todosWsClosedByUs) return;
    if (state.todosWsReconnectTimer) return;
    const idx = Math.min(state.todosWsBackoffStep, TODOS_WS_BACKOFFS_MS.length - 1);
    const delay = TODOS_WS_BACKOFFS_MS[idx];
    state.todosWsBackoffStep = Math.min(
        state.todosWsBackoffStep + 1,
        TODOS_WS_BACKOFFS_MS.length - 1,
    );
    state.todosWsReconnectTimer = setTimeout(() => {
        state.todosWsReconnectTimer = null;
        connectTodosWs();
    }, delay);
}

export function handleTodosWsMessage(raw) {
    let msg;
    try {
        msg = JSON.parse(raw);
    } catch (e) {
        console.warn('todos ws: non-JSON message', raw);
        return;
    }
    if (!msg || typeof msg !== 'object') return;

    switch (msg.kind) {
        case 'snapshot':
            state.todosData = Array.isArray(msg.todos) ? msg.todos : [];
            renderTasks();
            break;

        case 'upsert': {
            const todo = msg.todo;
            if (!todo || typeof todo !== 'object' || !todo.id) {
                console.warn('todos upsert without todo.id', msg);
                return;
            }
            if (!Array.isArray(state.todosData)) state.todosData = [];
            const i = state.todosData.findIndex((t) => t && t.id === todo.id);
            if (i >= 0) {
                state.todosData[i] = todo;
            } else {
                state.todosData.unshift(todo);
            }
            renderTasks();
            break;
        }

        case 'removed': {
            const id = msg.id;
            if (!id || !Array.isArray(state.todosData)) return;
            const i = state.todosData.findIndex((t) => t && t.id === id);
            if (i >= 0) {
                state.todosData.splice(i, 1);
                renderTasks();
            }
            break;
        }

        case 'reload':
            fetchTodos();
            break;

        default:
            console.debug('todos ws: unknown kind', msg.kind);
    }
}
