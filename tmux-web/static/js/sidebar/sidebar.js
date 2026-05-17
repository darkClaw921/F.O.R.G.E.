// tmux-web — Sidebar render (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - renderSidebar           (app.js:563)
//   - renderSidebarWithOrigin (app.js:1017)
//   - renderOriginSection     (app.js:1101)

import { state } from '../core/state.js';
import { $sidebar } from '../core/dom.js';
import { isRemoteMode } from '../remote/healthz.js';
import { renderOriginTabs, isOriginCollapsed, toggleOriginCollapsed } from './origin-tabs.js';
import { loadRemoteProjects, loadRemoteSessions } from '../remote/servers.js';
import { buildSessionItem, groupSessionsByFolder } from '../sessions/sessions.js';

export function renderSidebar() {
    // Phase 5: в remote-mode рендерим origin-табы (или прячем UI, если не).
    renderOriginTabs();

    // Phase 5: в remote-mode используем двухуровневую группировку
    // Origin → Project → Sessions. В legacy режиме — поведение Phase 6.B
    // (project-grouping) сохраняется побитово.
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

    const ORPHAN_KEY = '__orphan__';
    const projectFilter = state.projectFilter;
    const visible = projectFilter === '__all__'
        ? state.sessions
        : state.sessions.filter((s) => s.project_id === projectFilter);

    if (visible.length === 0) {
        const li = document.createElement('li');
        li.className = 'empty';
        li.textContent = projectFilter === '__all__'
            ? 'Нет активных сессий'
            : 'Нет сессий в этом проекте';
        $sidebar.appendChild(li);
        return;
    }

    const groups = new Map();
    for (const sess of visible) {
        const key = sess.folder_id == null ? ORPHAN_KEY : sess.folder_id;
        if (!groups.has(key)) groups.set(key, []);
        groups.get(key).push(sess);
    }
    for (const arr of groups.values()) {
        arr.sort((a, b) => a.name.localeCompare(b.name));
    }

    const nonOrphanKeys = [];
    for (const key of groups.keys()) {
        if (key !== ORPHAN_KEY) nonOrphanKeys.push(key);
    }
    nonOrphanKeys.sort((a, b) => {
        const la = (groups.get(a)[0].folder_label || a).toLowerCase();
        const lb = (groups.get(b)[0].folder_label || b).toLowerCase();
        return la.localeCompare(lb);
    });

    for (const key of nonOrphanKeys) {
        const arr = groups.get(key);
        if (!arr || arr.length === 0) continue;
        const keyDisplay = key.startsWith('__folder:') ? key.slice('__folder:'.length) : key;
        const header = document.createElement('li');
        header.className = 'session-group-header';
        header.textContent = arr[0].folder_label || keyDisplay;
        $sidebar.appendChild(header);
        for (const sess of arr) {
            $sidebar.appendChild(buildSessionItem(sess));
        }
    }
    const orphans = groups.get(ORPHAN_KEY);
    if (orphans && orphans.length > 0) {
        const header = document.createElement('li');
        header.className = 'session-group-header';
        header.textContent = 'Orphan';
        $sidebar.appendChild(header);
        for (const sess of orphans) {
            $sidebar.appendChild(buildSessionItem(sess));
        }
    }
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
        renderOriginSection('local', 'Local', 'local', state.projects, state.sessions, {
            isRemote: false,
            isOffline: false,
        });
    }
    for (const sid of showRemotes) {
        const srv = state.remoteServers.find((s) => s.id === sid);
        if (!srv) continue;
        const status = state.remoteOnline.get(sid) || 'unknown';
        const isOffline = status === 'offline';
        const projects = state.remoteProjects.get(sid);
        const sessions = state.remoteSessions.get(sid);

        const shouldLazyLoad = !isOffline && (
            isAllView || !isOriginCollapsed(sid)
        );
        if (shouldLazyLoad) {
            if (projects === undefined) {
                loadRemoteProjects(sid);
            }
            if (sessions === undefined) {
                loadRemoteSessions(sid).then(() => renderSidebar());
            }
        }
        renderOriginSection(
            sid,
            srv.label || sid,
            status,
            projects || [],
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

export function renderOriginSection(originKey, label, dotKind, projects, sessions, opts) {
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

    const ORPHAN_KEY = '__orphan__';
    const pf = state.projectFilter;
    const visible = (pf && pf !== '__all__')
        ? sessions.filter((s) => s.project_id === pf)
        : sessions;

    if (visible.length === 0) {
        const li = document.createElement('li');
        li.className = 'empty';
        li.textContent = (pf && pf !== '__all__')
            ? 'Нет сессий в этом проекте'
            : 'Нет активных сессий';
        $sidebar.appendChild(li);
        return;
    }

    const byFolder = groupSessionsByFolder(visible, ORPHAN_KEY);

    const nonOrphanKeys = [];
    for (const key of byFolder.keys()) {
        if (key !== ORPHAN_KEY) nonOrphanKeys.push(key);
    }
    nonOrphanKeys.sort((a, b) => {
        const la = (byFolder.get(a)[0].folder_label || a).toLowerCase();
        const lb = (byFolder.get(b)[0].folder_label || b).toLowerCase();
        return la.localeCompare(lb);
    });

    for (const key of nonOrphanKeys) {
        const arr = byFolder.get(key);
        if (!arr || arr.length === 0) continue;
        const keyDisplay = key.startsWith('__folder:') ? key.slice('__folder:'.length) : key;
        const ph = document.createElement('li');
        ph.className = 'project-sub-header';
        ph.textContent = arr[0].folder_label || keyDisplay;
        $sidebar.appendChild(ph);
        for (const sess of arr) {
            const li = buildSessionItem(sess);
            li.classList.add('in-origin');
            $sidebar.appendChild(li);
        }
    }
    const orphans = byFolder.get(ORPHAN_KEY);
    if (orphans && orphans.length > 0) {
        const ph = document.createElement('li');
        ph.className = 'project-sub-header';
        ph.textContent = 'Orphan';
        $sidebar.appendChild(ph);
        for (const sess of orphans) {
            const li = buildSessionItem(sess);
            li.classList.add('in-origin');
            $sidebar.appendChild(li);
        }
    }
}
