// tmux-web — Origin tabs + collapsed-state + active-origin persistence
// (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - loadActiveOriginFromStorage  (app.js:861)
//   - saveActiveOriginToStorage    (app.js:876)
//   - _collapsedOrigins/getCollapsedOrigins/persistCollapsedOrigins
//     /isOriginCollapsed/toggleOriginCollapsed (app.js:887-921)
//   - renderOriginTabs             (app.js:931)

import { state } from '../core/state.js';
import { $originTabs } from '../core/dom.js';
import { isRemoteMode } from '../remote/healthz.js';
import { loadRemoteSessions } from '../remote/servers.js';
import { renderSidebar } from './sidebar.js';
import { openSettingsModal } from '../settings/modal.js';
import { disconnectTasksWs, connectTasksWs } from '../ws/tasks-ws.js';
import { disconnectTodosWs, connectTodosWs } from '../ws/todos-ws.js';

export function loadActiveOriginFromStorage() {
    try {
        const saved = localStorage.getItem('forge.activeOrigin');
        if (saved === 'all' || saved === 'local') {
            state.activeOrigin = saved;
        } else if (saved && state.remoteServers.some((s) => s.id === saved)) {
            state.activeOrigin = saved;
        } else {
            state.activeOrigin = 'all';
        }
    } catch (_) {
        state.activeOrigin = 'all';
    }
}

export function saveActiveOriginToStorage() {
    try {
        localStorage.setItem('forge.activeOrigin', state.activeOrigin);
    } catch (_) { /* privacy mode — игнор */ }
}

let _collapsedOrigins = null;
export function getCollapsedOrigins() {
    if (_collapsedOrigins) return _collapsedOrigins;
    _collapsedOrigins = new Set();
    try {
        const raw = localStorage.getItem('forge.collapsedOrigins');
        if (raw) {
            const arr = JSON.parse(raw);
            if (Array.isArray(arr)) {
                arr.forEach((k) => _collapsedOrigins.add(k));
            }
        }
    } catch (_) { /* ignore */ }
    return _collapsedOrigins;
}
export function persistCollapsedOrigins() {
    try {
        localStorage.setItem(
            'forge.collapsedOrigins',
            JSON.stringify(Array.from(getCollapsedOrigins())),
        );
    } catch (_) { /* ignore */ }
}
export function isOriginCollapsed(key) {
    return getCollapsedOrigins().has(key);
}
export function toggleOriginCollapsed(key) {
    const set = getCollapsedOrigins();
    if (set.has(key)) {
        set.delete(key);
    } else {
        set.add(key);
    }
    persistCollapsedOrigins();
}

export function renderOriginTabs() {
    if (!$originTabs) return;
    if (!isRemoteMode()) {
        $originTabs.hidden = true;
        $originTabs.innerHTML = '';
        return;
    }
    $originTabs.hidden = false;
    $originTabs.innerHTML = '';

    const mkTab = (originKey, label, dotKind) => {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'origin-tab';
        if (state.activeOrigin === originKey) btn.classList.add('active');
        if (dotKind) {
            const dot = document.createElement('span');
            dot.className = 'origin-dot ' + dotKind;
            btn.appendChild(dot);
        }
        const span = document.createElement('span');
        span.textContent = label;
        btn.appendChild(span);
        btn.addEventListener('click', () => {
            state.activeOrigin = originKey;
            saveActiveOriginToStorage();
            // При выборе конкретного remote — lazy-load его сессий.
            if (originKey !== 'all' && originKey !== 'local') {
                if (!state.remoteSessions.has(originKey)) {
                    loadRemoteSessions(originKey).then(() => renderSidebar());
                }
            }
            // Реcоединяем tasks/todos WS чтобы они переподписались
            // на нужный origin (см. connectTasksWs/connectTodosWs — они
            // читают state.activeOrigin при формировании URL).
            disconnectTasksWs();
            disconnectTodosWs();
            state.tasksData = null;
            state.todosData = [];
            state.todosCurrentPath = null;
            setTimeout(() => { connectTasksWs(); connectTodosWs(); }, 0);
            renderSidebar();
        });
        return btn;
    };

    $originTabs.appendChild(mkTab('all', 'All', null));
    $originTabs.appendChild(mkTab('local', 'Local', 'local'));
    for (const srv of state.remoteServers) {
        const status = state.remoteOnline.get(srv.id) || 'unknown';
        $originTabs.appendChild(mkTab(srv.id, srv.label || srv.id, status));
    }
    // [+] таб — открыть Settings → Remote servers.
    const plus = document.createElement('button');
    plus.type = 'button';
    plus.className = 'origin-tab origin-tab-add';
    plus.title = 'Add remote server';
    plus.textContent = '+';
    plus.addEventListener('click', () => {
        openSettingsModal('remotes');
    });
    $originTabs.appendChild(plus);
}
