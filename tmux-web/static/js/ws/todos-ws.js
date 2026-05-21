// tmux-web — /ws/todos WebSocket + fetchTodos + polling
//
// Todos фильтруются по cwd-path активной сессии (?path=<abs>).

import { state } from '../core/state.js';
import { withWsToken } from '../core/auth.js';
import { isRemoteMode } from '../remote/healthz.js';
import { renderTasks, currentTodosPath } from '../tasks/render.js';

const TODOS_WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];
const TODOS_POLL_INTERVAL_MS = 30000;

// Todos следуют за path текущей tmux-сессии. Возвращает sess.path или null,
// если сессия не выбрана / не нашлась в state.sessions / без cwd.
function sessionPathOrNull() {
    const name = state.currentSession;
    if (!name) return null;
    const list = Array.isArray(state.sessions) ? state.sessions : [];
    const sess = list.find((s) => s && s.name === name);
    return sess && sess.path ? sess.path : null;
}

export async function fetchTodos(path) {
    const p = path || currentTodosPath();
    try {
        const url = p ? '/api/todos?path=' + encodeURIComponent(p) : '/api/todos';
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

    const p = currentTodosPath();
    const proto = (location.protocol === 'https:') ? 'wss' : 'ws';
    const server = (isRemoteMode()
        && state.activeOrigin
        && state.activeOrigin !== 'local'
        && state.activeOrigin !== 'all')
        ? state.activeOrigin
        : null;
    let qs = '';
    if (p && !server) {
        qs = '?path=' + encodeURIComponent(p);
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

// Синхронизирует todos с path текущей сессии. Если path совпадает с уже
// загруженным — no-op. Иначе: чистит снапшот, переподключает ws и
// рефетчит todos для нового path. Вызывается из openSession / switchSession
// после смены currentSession.
export function syncTodosToCurrentSession() {
    const p = sessionPathOrNull();
    if (state.todosCurrentPath === p) return;
    state.todosCurrentPath = p;
    state.todosData = [];
    disconnectTodosWs();
    fetchTodos(p);
    setTimeout(connectTodosWs, 0);
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
