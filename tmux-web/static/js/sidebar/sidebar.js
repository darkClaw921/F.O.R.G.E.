// tmux-web — Sidebar render
//
// Группировка только по folder_id/folder_label (без projects).
// В remote-mode добавляются origin-секции (local + remote-серверы).

import { state } from '../core/state.js';
import { $sidebar } from '../core/dom.js';
import { isRemoteMode } from '../remote/healthz.js';
import { renderOriginTabs, isOriginCollapsed, toggleOriginCollapsed } from './origin-tabs.js';
import { loadRemoteSessions } from '../remote/servers.js';
import { buildSessionItem, groupSessionsByFolder } from '../sessions/sessions.js';

const ORPHAN_KEY = '__orphan__';

export function renderSidebar() {
    renderOriginTabs();

    if (isRemoteMode()) {
        renderSidebarWithOrigin();
        return;
    }

    $sidebar.innerHTML = '';
    if (state.sessions.length === 0) {
        const li = document.createElement('li');
        li.className = 'empty';
        li.textContent = 'Нет активных сессий';
        $sidebar.appendChild(li);
        return;
    }

    renderFolderGroups(state.sessions);
}

export function renderSidebarWithOrigin() {
    $sidebar.innerHTML = '';

    const showLocal = state.activeOrigin === 'all' || state.activeOrigin === 'local';
    const remoteIds = state.remoteServers.map((s) => s.id);
    const showRemotes = state.activeOrigin === 'all'
        ? remoteIds
        : (state.activeOrigin === 'local' ? [] : remoteIds.filter((id) => id === state.activeOrigin));

    const isAllView = state.activeOrigin === 'all';

    if (showLocal) {
        renderOriginSection('local', 'Local', 'local', state.sessions, {
            isRemote: false,
            isOffline: false,
        });
    }
    for (const sid of showRemotes) {
        const srv = state.remoteServers.find((s) => s.id === sid);
        if (!srv) continue;
        const status = state.remoteOnline.get(sid) || 'unknown';
        const isOffline = status === 'offline';
        const sessions = state.remoteSessions.get(sid);

        const shouldLazyLoad = !isOffline && (
            isAllView || !isOriginCollapsed(sid)
        );
        if (shouldLazyLoad && sessions === undefined) {
            loadRemoteSessions(sid).then(() => renderSidebar());
        }
        renderOriginSection(
            sid,
            srv.label || sid,
            status,
            sessions || [],
            {
                isRemote: true,
                isOffline,
                remoteLoading: !isOffline && sessions === undefined,
            },
        );
    }

    if ($sidebar.children.length === 0) {
        const li = document.createElement('li');
        li.className = 'empty';
        li.textContent = 'Нет активных сессий';
        $sidebar.appendChild(li);
    }
}

export function renderOriginSection(originKey, label, dotKind, sessions, opts) {
    opts = opts || {};
    const collapsed = isOriginCollapsed(originKey);
    const isOffline = !!opts.isOffline;

    const header = document.createElement('li');
    header.className = 'origin-group-header';
    if (isOffline) header.classList.add('origin-offline');
    header.dataset.origin = originKey;

    const caret = document.createElement('span');
    caret.className = 'origin-caret';
    caret.textContent = collapsed ? '▸' : '▾';
    header.appendChild(caret);

    const dot = document.createElement('span');
    dot.className = 'origin-dot ' + (dotKind || 'unknown');
    header.appendChild(dot);

    const lbl = document.createElement('span');
    lbl.className = 'origin-label';
    lbl.textContent = label;
    header.appendChild(lbl);

    if (isOffline) {
        const badge = document.createElement('span');
        badge.className = 'origin-badge origin-badge-offline';
        badge.textContent = 'offline';
        header.appendChild(badge);
    } else {
        const meta = document.createElement('span');
        meta.className = 'origin-meta';
        meta.textContent = `${sessions.length} sess`;
        header.appendChild(meta);
    }

    header.addEventListener('click', () => {
        toggleOriginCollapsed(originKey);
        renderSidebar();
    });
    $sidebar.appendChild(header);

    if (collapsed) return;

    if (isOffline) {
        const li = document.createElement('li');
        li.className = 'empty empty-offline';
        li.textContent = 'Сервер недоступен';
        $sidebar.appendChild(li);
        return;
    }

    if (opts.remoteLoading && sessions.length === 0) {
        const li = document.createElement('li');
        li.className = 'empty';
        li.textContent = 'Loading…';
        $sidebar.appendChild(li);
        return;
    }
    if (sessions.length === 0) {
        const li = document.createElement('li');
        li.className = 'empty';
        li.textContent = 'Нет активных сессий';
        $sidebar.appendChild(li);
        return;
    }

    renderFolderGroups(sessions, { inOrigin: true });
}

function renderFolderGroups(sessions, opts) {
    const inOrigin = !!(opts && opts.inOrigin);
    const byFolder = groupSessionsByFolder(sessions, ORPHAN_KEY);

    const nonOrphanKeys = [];
    for (const key of byFolder.keys()) {
        if (key !== ORPHAN_KEY) nonOrphanKeys.push(key);
    }
    nonOrphanKeys.sort((a, b) => {
        const la = (byFolder.get(a)[0].folder_label || a).toLowerCase();
        const lb = (byFolder.get(b)[0].folder_label || b).toLowerCase();
        return la.localeCompare(lb);
    });

    const orphans = byFolder.get(ORPHAN_KEY);
    const hasFolders = nonOrphanKeys.length > 0;

    for (const key of nonOrphanKeys) {
        const arr = byFolder.get(key);
        if (!arr || arr.length === 0) continue;
        const keyDisplay = key.startsWith('__folder:') ? key.slice('__folder:'.length) : key;
        const ph = document.createElement('li');
        ph.className = inOrigin ? 'project-sub-header' : 'session-group-header';
        ph.textContent = arr[0].folder_label || keyDisplay;
        $sidebar.appendChild(ph);
        for (const sess of arr) {
            const li = buildSessionItem(sess);
            if (inOrigin) li.classList.add('in-origin');
            $sidebar.appendChild(li);
        }
    }
    if (orphans && orphans.length > 0) {
        // Если есть folder-группы — рендерим заголовок "All sessions" / "Orphan";
        // если folder'ов нет — выводим сессии плоско, без заголовка.
        if (hasFolders) {
            const ph = document.createElement('li');
            ph.className = inOrigin ? 'project-sub-header' : 'session-group-header';
            ph.textContent = 'All sessions';
            $sidebar.appendChild(ph);
        }
        for (const sess of orphans) {
            const li = buildSessionItem(sess);
            if (inOrigin) li.classList.add('in-origin');
            $sidebar.appendChild(li);
        }
    }
}
