// tmux-web — /ws/tasks WebSocket + fetchTasks + polling
// (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - TASKS_WS_BACKOFFS_MS / TASKS_POLL_INTERVAL_MS (app.js:1890, 1884)
//   - startTasksPolling/stopTasksPolling (app.js:2724/2729)
//   - connectTasksWs/disconnectTasksWs/scheduleTasksWsReconnect
//     (app.js:2749/2823/2839)
//   - handleTasksWsMessage (app.js:2858)
//   - fetchTasks (app.js:2926)
//   - setTasksStatus (app.js:2947)

import { state } from '../core/state.js';
import { withWsToken } from '../core/auth.js';
import { isRemoteMode } from '../remote/healthz.js';
import { $tasksStatus } from '../core/dom.js';
import { renderTasks } from '../tasks/render.js';
// Циклический импорт с tasks/gantt.js (gantt.js импортирует sessionCwdOrNull
// отсюда). Безопасен: оба биндинга вызываются только в рантайме (не при
// инициализации модуля), ES-модули корректно разрешают такой цикл.
import { fetchGitCommits } from '../tasks/gantt.js';

const TASKS_POLL_INTERVAL_MS = 30000;
const TASKS_WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

// Tasks следуют за cwd текущей tmux-сессии (по аналогии с git-вкладкой).
// Возвращает абсолютный путь сессии или null, если сессия не выбрана/без path.
// Экспортируется для tasks/gantt.js (since/path для GET /api/git/commits).
export function sessionCwdOrNull() {
    const name = state.currentSession;
    if (!name) return null;
    const list = Array.isArray(state.sessions) ? state.sessions : [];
    const sess = list.find((s) => s && s.name === name);
    return sess && sess.path ? sess.path : null;
}

export function startTasksPolling() {
    if (state.tasksPollTimer) clearInterval(state.tasksPollTimer);
    state.tasksPollTimer = setInterval(fetchTasks, TASKS_POLL_INTERVAL_MS);
}

export function stopTasksPolling() {
    if (state.tasksPollTimer) {
        clearInterval(state.tasksPollTimer);
        state.tasksPollTimer = null;
    }
}

export function connectTasksWs() {
    if (state.tasksWs && (
        state.tasksWs.readyState === WebSocket.OPEN ||
        state.tasksWs.readyState === WebSocket.CONNECTING
    )) {
        return;
    }
    state.tasksWsClosedByUs = false;

    if (state.tasksWsReconnectTimer) {
        clearTimeout(state.tasksWsReconnectTimer);
        state.tasksWsReconnectTimer = null;
    }

    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const server = (isRemoteMode()
        && state.activeOrigin
        && state.activeOrigin !== 'local'
        && state.activeOrigin !== 'all')
        ? state.activeOrigin
        : null;
    // ws_tasks.rs принимает `?path=<abs_cwd>` (Phase 4: убрали project_id).
    // Если cwd пустой — backend fallback на active_path_tx.
    const cwd = !server ? sessionCwdOrNull() : null;
    state.tasksCurrentCwd = cwd;
    let qs = '';
    if (cwd && !server) {
        qs = `?path=${encodeURIComponent(cwd)}`;
    } else if (server) {
        qs = `?server=${encodeURIComponent(server)}`;
    }
    const url = `${proto}://${location.host}/ws/tasks${qs}`;
    let ws;
    try {
        ws = new WebSocket(withWsToken(url));
    } catch (e) {
        console.warn('tasks ws constructor failed', e);
        scheduleTasksWsReconnect();
        return;
    }
    state.tasksWs = ws;
    setTasksStatus('reconnecting', 'tasks: connecting…');

    ws.onopen = () => {
        state.tasksWsBackoffStep = 0;
        stopTasksPolling();
        setTasksStatus('ok', 'tasks: live');
    };
    ws.onmessage = (ev) => {
        handleTasksWsMessage(ev.data);
    };
    ws.onerror = (ev) => {
        console.debug('tasks ws error', ev);
        setTasksStatus('error', 'tasks: ws error');
    };
    ws.onclose = () => {
        state.tasksWs = null;
        if (state.tasksWsClosedByUs) {
            setTasksStatus('ok', '');
            return;
        }
        setTasksStatus('reconnecting', 'tasks: reconnecting…');
        startTasksPolling();
        scheduleTasksWsReconnect();
    };
}

export function disconnectTasksWs() {
    state.tasksWsClosedByUs = true;
    if (state.tasksWsReconnectTimer) {
        clearTimeout(state.tasksWsReconnectTimer);
        state.tasksWsReconnectTimer = null;
    }
    if (state.tasksWs) {
        try { state.tasksWs.close(); } catch (_) {}
        state.tasksWs = null;
    }
}

export function scheduleTasksWsReconnect() {
    if (state.tasksWsClosedByUs) return;
    if (state.tasksWsReconnectTimer) return;
    const idx = Math.min(state.tasksWsBackoffStep, TASKS_WS_BACKOFFS_MS.length - 1);
    const delay = TASKS_WS_BACKOFFS_MS[idx];
    state.tasksWsBackoffStep = Math.min(state.tasksWsBackoffStep + 1, TASKS_WS_BACKOFFS_MS.length - 1);
    state.tasksWsReconnectTimer = setTimeout(() => {
        state.tasksWsReconnectTimer = null;
        connectTasksWs();
    }, delay);
}

export function handleTasksWsMessage(raw) {
    let msg;
    try {
        msg = JSON.parse(raw);
    } catch (e) {
        console.warn('tasks ws: non-JSON message', raw);
        return;
    }
    if (!msg || typeof msg !== 'object') return;

    switch (msg.kind) {
        case 'snapshot':
            state.tasksData = msg.data || { issues: [], total: 0 };
            renderTasks();
            break;

        case 'upsert': {
            const issue = msg.issue;
            if (!issue || typeof issue !== 'object' || !issue.id) {
                console.warn('upsert without issue.id', msg);
                return;
            }
            if (!state.tasksData || !Array.isArray(state.tasksData.issues)) {
                state.tasksData = { issues: [issue], total: 1 };
            } else {
                const arr = state.tasksData.issues;
                const i = arr.findIndex((it) => it && it.id === issue.id);
                if (i >= 0) {
                    arr[i] = issue;
                } else {
                    arr.unshift(issue);
                    if (typeof state.tasksData.total === 'number') {
                        state.tasksData.total += 1;
                    }
                }
            }
            renderTasks();
            break;
        }

        case 'removed': {
            const id = msg.id;
            if (!id || !state.tasksData || !Array.isArray(state.tasksData.issues)) return;
            const arr = state.tasksData.issues;
            const i = arr.findIndex((it) => it && it.id === id);
            if (i >= 0) {
                arr.splice(i, 1);
                if (typeof state.tasksData.total === 'number') {
                    state.tasksData.total = Math.max(0, state.tasksData.total - 1);
                }
                renderTasks();
            }
            break;
        }

        case 'reload':
            fetchTasks();
            break;

        default:
            console.debug('tasks ws: unknown kind', msg.kind);
    }
}

export async function fetchTasks() {
    try {
        const cwd = sessionCwdOrNull();
        const url = cwd
            ? `/api/tasks?path=${encodeURIComponent(cwd)}`
            : '/api/tasks';
        const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/tasks failed:', r.status);
            state.tasksData = { issues: [], total: 0 };
            setTasksStatus('error', 'tasks: HTTP ' + r.status);
            renderTasks();
            return;
        }
        state.tasksData = await r.json();
        setTasksStatus('ok', '');
        renderTasks();
    } catch (e) {
        console.warn('fetchTasks failed', e);
        state.tasksData = state.tasksData || { issues: [], total: 0 };
        setTasksStatus('error', 'tasks: network');
        renderTasks();
    }
}

// Синхронизирует tasks с cwd текущей сессии (по образцу syncGitToCurrentSession).
// Если cwd не изменился — no-op. Иначе закрываем старый WS, чистим snapshot и,
// если вкладка tasks активна, делаем fetchTasks + connectTasksWs. Если вкладка
// неактивна — ws/fetch произойдут при следующем switchTab('tasks').
export function syncTasksToCurrentSession() {
    const cwd = sessionCwdOrNull();
    if (state.tasksCurrentCwd === cwd) return;
    state.tasksCurrentCwd = cwd;
    state.tasksData = null;
    // Гант следует за cwd: сбрасываем коммиты прошлого корня и, если вкладка
    // активна, подгружаем коммиты нового (fetchGitCommits перерисует гант).
    state.gitCommits = [];
    disconnectTasksWs();
    if (state.activeTab === 'tasks') {
        fetchTasks();
        setTimeout(connectTasksWs, 0);
        fetchGitCommits();
    }
}

export function setTasksStatus(_kind, text) {
    if ($tasksStatus) $tasksStatus.textContent = text || '';
}
