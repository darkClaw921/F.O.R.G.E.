// tmux-web — Sessions (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - fetchSessions          (app.js:476)
//   - buildSessionItem       (app.js:496)
//   - groupSessionsByFolder  (app.js:1249)
//   - startPolling/stopPolling (app.js:1305)
//   - createSessionPrompt    (app.js:1321)
//   - renameSession          (app.js:1549)
//   - killSession            (app.js:1586)
//   - openSession            (app.js:1609)
//   - switchSession          (app.js:1643)

import { state } from '../core/state.js';
import { apiFetch, dtoOrigin } from '../core/api.js';
import { showPlaceholder, scheduleResizeFromTerm } from '../terminal/xterm.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { connectWs, disconnectWs } from '../ws/attach.js';
import { switchActiveProject } from '../projects/projects.js';
import { syncGitToCurrentSession } from '../tabs/tui-tabs.js';
import { syncTasksToCurrentSession } from '../ws/tasks-ws.js';

export async function fetchSessions() {
    try {
        const resp = await fetch('/api/sessions', { headers: { 'Accept': 'application/json' } });
        if (!resp.ok) {
            throw new Error('HTTP ' + resp.status);
        }
        const data = await resp.json();
        state.sessions = Array.isArray(data) ? data : [];
        renderSidebar();
    } catch (e) {
        console.warn('fetchSessions failed', e);
    }
}

export function buildSessionItem(s) {
    const li = document.createElement('li');
    li.className = 'session-item';
    if (s.name === state.currentSession) {
        li.classList.add('active');
    }
    if (s.needs_attention) {
        li.classList.add('needs-attention');
    }
    li.dataset.session = s.name;

    const meta = document.createElement('div');
    meta.className = 'session-meta';

    const name = document.createElement('div');
    name.className = 'session-name';
    name.textContent = s.name;
    meta.appendChild(name);

    const sub = document.createElement('div');
    sub.className = 'session-sub';
    const winsTxt = `${s.windows} ${s.windows === 1 ? 'window' : 'windows'}`;
    if (s.attached > 0) {
        sub.innerHTML = `${winsTxt} · <span class="attached-flag">attached(${s.attached})</span>`;
    } else {
        sub.textContent = winsTxt;
    }
    meta.appendChild(sub);

    li.appendChild(meta);

    const sessOrigin = dtoOrigin(s);

    const actions = document.createElement('div');
    actions.className = 'session-actions';

    const btnRename = document.createElement('button');
    btnRename.type = 'button';
    btnRename.className = 'btn-rename';
    btnRename.textContent = 'rename';
    btnRename.title = `Переименовать сессию ${s.name}`;
    btnRename.addEventListener('click', (ev) => {
        ev.stopPropagation();
        renameSession(s.name, sessOrigin);
    });
    actions.appendChild(btnRename);

    const btnKill = document.createElement('button');
    btnKill.type = 'button';
    btnKill.className = 'btn-kill';
    btnKill.textContent = 'kill';
    btnKill.title = `Убить сессию ${s.name}`;
    btnKill.addEventListener('click', (ev) => {
        ev.stopPropagation();
        killSession(s.name, sessOrigin);
    });
    actions.appendChild(btnKill);

    li.appendChild(actions);

    if (s.is_generating) {
        const spark = document.createElement('span');
        spark.className = 'claude-spark';
        spark.title = 'Claude генерирует';
        spark.textContent = '✶';
        li.appendChild(spark);
    }

    li.addEventListener('click', () => openSession(s.name, sessOrigin));

    return li;
}

export function groupSessionsByFolder(sessions, orphanKey) {
    const ORPHAN_KEY = orphanKey || '__orphan__';
    const byFolder = new Map();
    for (const sess of sessions) {
        const key = sess.folder_id == null ? ORPHAN_KEY : sess.folder_id;
        if (!byFolder.has(key)) byFolder.set(key, []);
        byFolder.get(key).push(sess);
    }
    for (const arr of byFolder.values()) {
        arr.sort((a, b) => a.name.localeCompare(b.name));
    }
    return byFolder;
}

export function startPolling() {
    if (state.pollTimer) clearInterval(state.pollTimer);
    state.pollTimer = setInterval(fetchSessions, 3000);
}

export function stopPolling() {
    if (state.pollTimer) {
        clearInterval(state.pollTimer);
        state.pollTimer = null;
    }
}

export async function createSessionPrompt() {
    const name = window.prompt('Имя новой tmux-сессии:', '');
    if (!name) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    try {
        const resp = await fetch('/api/sessions', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed }),
        });
        if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось создать сессию: ' + (text || resp.status));
            return;
        }
        await fetchSessions();
        openSession(trimmed);
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function renameSession(oldName, origin) {
    const input = window.prompt(`Новое имя сессии "${oldName}":`, oldName);
    if (input === null) return;
    const trimmed = input.trim();
    if (!trimmed || trimmed === oldName) return;
    try {
        const resp = await apiFetch('/api/sessions/' + encodeURIComponent(oldName), {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed }),
        }, origin);
        if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось переименовать сессию: ' + (text || resp.status));
            return;
        }
        let newName = trimmed;
        try {
            const data = await resp.json();
            if (data && typeof data.name === 'string') newName = data.name;
        } catch (_) {}

        if (state.currentSession === oldName) {
            disconnectWs();
            state.currentSession = null;
            showPlaceholder(true);
            await fetchSessions();
            openSession(newName, origin);
        } else {
            await fetchSessions();
        }
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function killSession(name, origin) {
    if (!window.confirm(`Убить сессию "${name}"?`)) return;
    try {
        const resp = await apiFetch('/api/sessions/' + encodeURIComponent(name), {
            method: 'DELETE',
        }, origin);
        if (!resp.ok && resp.status !== 204) {
            const text = await resp.text();
            window.alert('Не удалось убить сессию: ' + (text || resp.status));
            return;
        }
        if (state.currentSession === name) {
            disconnectWs();
            state.currentSession = null;
            showPlaceholder(true);
        }
        await fetchSessions();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function openSession(name, origin) {
    if (!name) return;
    const sessionKey = name;
    if (state.currentSession === sessionKey && state.ws && state.ws.readyState === WebSocket.OPEN) {
        return;
    }

    const sess = state.sessions.find((s) => s.name === name);
    const sessOrigin = origin || dtoOrigin(sess);
    if (sessOrigin === 'local') {
        const targetProjectId = sess && sess.project_id ? sess.project_id : null;
        if (targetProjectId && targetProjectId !== state.activeProjectId) {
            await switchActiveProject(targetProjectId);
            connectWs(name, 'local');
            syncGitToCurrentSession();
            syncTasksToCurrentSession();
            return;
        }
    }

    if (state.ws && state.ws.readyState === WebSocket.OPEN) {
        switchSession(name);
        return;
    }
    connectWs(name, sessOrigin);
    syncGitToCurrentSession();
    syncTasksToCurrentSession();
}

export function switchSession(name) {
    try {
        state.ws.send(JSON.stringify({ type: 'switch', session: name }));
        state.currentSession = name;
        if (state.term) state.term.reset();
        renderSidebar();
        scheduleResizeFromTerm();
        syncGitToCurrentSession();
        syncTasksToCurrentSession();
    } catch (e) {
        console.warn('switch failed', e);
        disconnectWs();
        connectWs(name);
    }
}
