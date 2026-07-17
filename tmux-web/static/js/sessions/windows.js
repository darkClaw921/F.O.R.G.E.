// tmux-web — Windows in active session (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - fetchWindows       (app.js:1349)
//   - renderWindowBar    (app.js:1378)
//   - selectWindow       (app.js:1431)
//   - createWindow       (app.js:1452)
//   - killWindow         (app.js:1482)
//   - renameWindow       (app.js:1504)
//   - startWindowsPolling/stopWindowsPolling (app.js:1533-1547)

import { state } from '../core/state.js';
import { apiFetch } from '../core/api.js';
import { $windowBar, $windowTabs } from '../core/dom.js';

export async function fetchWindows() {
    const session = state.currentSession;
    if (!session) {
        state.currentWindows = [];
        renderWindowBar();
        return;
    }
    try {
        const resp = await apiFetch(
            '/api/sessions/' + encodeURIComponent(session) + '/windows',
            { headers: { 'Accept': 'application/json' } },
            state.attachWsOrigin,
        );
        if (!resp.ok) {
            if (resp.status === 400 || resp.status === 404) {
                state.currentWindows = [];
                renderWindowBar();
            }
            return;
        }
        const data = await resp.json();
        state.currentWindows = Array.isArray(data) ? data : [];
        renderWindowBar();
    } catch (e) {
        console.warn('fetchWindows failed', e);
    }
}

export function renderWindowBar() {
    if (!$windowBar || !$windowTabs) return;
    const onTerminal = state.activeTab === 'terminal' || !state.activeTab;
    const hasSession = !!state.currentSession;
    const visible = onTerminal && hasSession;
    $windowBar.hidden = !visible;
    if (!visible) {
        $windowTabs.innerHTML = '';
        return;
    }

    $windowTabs.innerHTML = '';
    for (const w of state.currentWindows) {
        const tab = document.createElement('button');
        tab.type = 'button';
        tab.className = 'window-tab' + (w.active ? ' active' : '');
        tab.dataset.index = String(w.index);
        tab.title = `Окно ${w.index}: ${w.name} (${w.panes} pane${w.panes === 1 ? '' : 's'})`;

        const idx = document.createElement('span');
        idx.className = 'window-tab-idx';
        idx.textContent = String(w.index);
        tab.appendChild(idx);

        const label = document.createElement('span');
        label.className = 'window-tab-name';
        label.textContent = w.name;
        tab.appendChild(label);

        tab.addEventListener('click', (ev) => {
            ev.stopPropagation();
            selectWindow(w.index);
        });
        tab.addEventListener('dblclick', (ev) => {
            ev.stopPropagation();
            renameWindow(w.index, w.name);
        });

        const close = document.createElement('button');
        close.type = 'button';
        close.className = 'window-tab-close';
        close.textContent = '×';
        close.title = `Убить окно ${w.index}`;
        close.addEventListener('click', (ev) => {
            ev.stopPropagation();
            killWindow(w.index, w.name);
        });
        tab.appendChild(close);

        $windowTabs.appendChild(tab);
    }
}

export async function selectWindow(index) {
    const session = state.currentSession;
    if (!session) return;
    try {
        const resp = await apiFetch(
            '/api/sessions/' + encodeURIComponent(session)
                + '/windows/' + encodeURIComponent(index) + '/select',
            { method: 'POST' },
            state.attachWsOrigin,
        );
        if (!resp.ok && resp.status !== 204) {
            const text = await resp.text();
            window.alert('Не удалось переключить окно: ' + (text || resp.status));
            return;
        }
        await fetchWindows();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function createWindow() {
    const session = state.currentSession;
    if (!session) return;
    const input = window.prompt('Имя нового окна (пусто = по умолчанию):', '');
    if (input === null) return;
    const trimmed = input.trim();
    const body = trimmed ? JSON.stringify({ name: trimmed }) : '';
    try {
        const init = {
            method: 'POST',
            headers: trimmed ? { 'Content-Type': 'application/json' } : {},
        };
        if (body) init.body = body;
        const resp = await apiFetch(
            '/api/sessions/' + encodeURIComponent(session) + '/windows',
            init,
            state.attachWsOrigin,
        );
        if (!resp.ok && resp.status !== 201) {
            const text = await resp.text();
            window.alert('Не удалось создать окно: ' + (text || resp.status));
            return;
        }
        await fetchWindows();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function createWorktreeWindow() {
    const session = state.currentSession;
    if (!session) return;
    try {
        const resp = await apiFetch(
            '/api/sessions/' + encodeURIComponent(session) + '/windows/worktree',
            { method: 'POST' },
            state.attachWsOrigin,
        );
        if (!resp.ok && resp.status !== 201) {
            const text = await resp.text();
            window.alert('Не удалось создать worktree-окно: ' + (text || resp.status));
            return;
        }
        await fetchWindows();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function killWindow(index, name) {
    const session = state.currentSession;
    if (!session) return;
    const isWorktree = typeof name === 'string' && name.startsWith('wt:');
    const msg = isWorktree
        ? `Убить окно ${index} "${name}" и удалить его git worktree?\n`
          + `Несохранённые изменения в worktree будут потеряны. `
          + `Коммиты в ветке forge/... сохранятся.`
        : `Убить окно ${index} "${name}"?`;
    if (!window.confirm(msg)) return;
    const url = '/api/sessions/' + encodeURIComponent(session)
        + '/windows/' + encodeURIComponent(index)
        + (isWorktree ? '/worktree' : '');
    try {
        const resp = await apiFetch(url, { method: 'DELETE' }, state.attachWsOrigin);
        if (!resp.ok && resp.status !== 204) {
            const text = await resp.text();
            window.alert('Не удалось убить окно: ' + (text || resp.status));
            return;
        }
        await fetchWindows();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export async function renameWindow(index, oldName) {
    const session = state.currentSession;
    if (!session) return;
    const input = window.prompt(`Новое имя окна ${index}:`, oldName);
    if (input === null) return;
    const trimmed = input.trim();
    if (!trimmed || trimmed === oldName) return;
    try {
        const resp = await apiFetch(
            '/api/sessions/' + encodeURIComponent(session)
                + '/windows/' + encodeURIComponent(index),
            {
                method: 'PATCH',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ name: trimmed }),
            },
            state.attachWsOrigin,
        );
        if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось переименовать окно: ' + (text || resp.status));
            return;
        }
        await fetchWindows();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

export function startWindowsPolling() {
    stopWindowsPolling();
    fetchWindows();
    state.windowsPollTimer = setInterval(fetchWindows, 2000);
}

export function stopWindowsPolling() {
    if (state.windowsPollTimer) {
        clearInterval(state.windowsPollTimer);
        state.windowsPollTimer = null;
    }
    state.currentWindows = [];
    renderWindowBar();
}
