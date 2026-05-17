// tmux-web — Multi-project (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - fetchProjects        (app.js:3688)
//   - renderProjectSelect  (app.js:3733)
//   - switchActiveProject  (app.js:3762)

import { state } from '../core/state.js';
import {
    $projectSelect,
    $gitPlaceholder, $gitTermEl,
    $dockerPlaceholder, $dockerTermEl,
    $telescopePlaceholder, $telescopeTermEl,
} from '../core/dom.js';
import { disconnectWs } from '../ws/attach.js';
import { showPlaceholder, setStatus } from '../terminal/xterm.js';
import { fetchSessions } from '../sessions/sessions.js';
import { disconnectTasksWs, connectTasksWs, fetchTasks } from '../ws/tasks-ws.js';
import { disconnectTodosWs, connectTodosWs, fetchTodos } from '../ws/todos-ws.js';
import { openLazygitForActiveProject, getActiveProject } from '../tabs/tui-tabs.js';

export async function fetchProjects() {
    try {
        const r = await fetch('/api/projects', { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/projects failed:', r.status);
            return;
        }
        const data = await r.json();
        state.projects = Array.isArray(data) ? data : [];
        const active = state.projects.find((p) => p.active);
        state.activeProjectId = active ? active.id : (state.projects[0] ? state.projects[0].id : null);
        try {
            const saved = localStorage.getItem('forge.projectFilter');
            if (saved === '__all__') {
                state.projectFilter = '__all__';
            } else if (saved && state.projects.some((p) => p.id === saved)) {
                state.projectFilter = saved;
            } else {
                state.projectFilter = '__all__';
            }
        } catch (_) {
            state.projectFilter = '__all__';
        }
        renderProjectSelect();
        if (state.activeTab === 'git' && state.gitTerm && !state.gitTerm.ws) {
            openLazygitForActiveProject();
        }
        if (state.activeTab === 'docker' && state.dockerTerm && !state.dockerTerm.ws) {
            state.dockerTerm.openForActiveProject();
        }
        if (state.activeTab === 'telescope' && state.telescopeTerm && !state.telescopeTerm.ws) {
            state.telescopeTerm.openForActiveProject();
        }
    } catch (e) {
        console.warn('fetchProjects failed', e);
    }
}

export function renderProjectSelect() {
    if (!$projectSelect) return;
    $projectSelect.innerHTML = '';
    const allOpt = document.createElement('option');
    allOpt.value = '__all__';
    allOpt.textContent = 'All projects';
    if (state.projectFilter === '__all__') {
        allOpt.selected = true;
    }
    $projectSelect.appendChild(allOpt);
    for (const p of state.projects) {
        const opt = document.createElement('option');
        opt.value = p.id;
        opt.textContent = p.name + (p.tmux_prefix ? ` [${p.tmux_prefix}]` : '');
        if (p.id === state.projectFilter) {
            opt.selected = true;
        }
        $projectSelect.appendChild(opt);
    }
}

export async function switchActiveProject(id) {
    if (!id || id === state.activeProjectId) return;
    try {
        const r = await fetch('/api/projects/active', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ id }),
        });
        if (!r.ok) {
            const text = await r.text();
            window.alert('Не удалось переключить проект: ' + (text || r.status));
            renderProjectSelect();
            return;
        }
        disconnectWs();
        state.currentSession = null;
        showPlaceholder(true);
        setStatus('disconnected', 'disconnected');
        state.tasksData = null;
        await fetchProjects();
        state.activeProjectId = id;
        await fetchSessions();
        disconnectTasksWs();
        setTimeout(connectTasksWs, 0);
        if (state.activeTab === 'tasks') {
            fetchTasks();
        }
        state.todosData = [];
        disconnectTodosWs();
        setTimeout(connectTodosWs, 0);
        fetchTodos();
        const newActive = getActiveProject();
        const newPath = newActive && newActive.path ? newActive.path : null;
        const tuiTabs = [
            {
                tab: state.gitTerm,
                activeTabName: 'git',
                placeholderEl: $gitPlaceholder,
                termEl: $gitTermEl,
            },
            {
                tab: state.dockerTerm,
                activeTabName: 'docker',
                placeholderEl: $dockerPlaceholder,
                termEl: $dockerTermEl,
            },
            {
                tab: state.telescopeTerm,
                activeTabName: 'telescope',
                placeholderEl: $telescopePlaceholder,
                termEl: $telescopeTermEl,
            },
        ];
        for (const entry of tuiTabs) {
            if (!entry.tab) continue;
            const isActive = state.activeTab === entry.activeTabName;
            if (newPath) {
                if (isActive) {
                    if (entry.placeholderEl) entry.placeholderEl.hidden = true;
                    if (entry.termEl) entry.termEl.hidden = false;
                }
                if (entry.tab.ws) {
                    entry.tab.switchCwd(newPath);
                } else if (isActive) {
                    entry.tab.openForActiveProject();
                }
            } else {
                if (isActive) {
                    if (entry.placeholderEl) entry.placeholderEl.hidden = false;
                    if (entry.termEl) entry.termEl.hidden = true;
                }
                entry.tab.close('no active project after switch');
            }
        }
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        renderProjectSelect();
    }
}
