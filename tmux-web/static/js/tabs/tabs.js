// tmux-web — switchTab dispatcher (Phase 1 ES Modules refactor)
//
// 1:1 копия switchTab из IIFE `tmux-web/static/app.js` (app.js:1913).

import { state } from '../core/state.js';
import {
    $terminalEl, $placeholder, $windowBar,
    $tasksEl, $tabTerminal, $tabTasks,
    $gitEl, $dockerEl, $telescopeEl,
    $tabGit, $tabDocker, $tabTelescope,
} from '../core/dom.js';
import { scheduleResizeFromTerm } from '../terminal/xterm.js';
import { connectTasksWs, fetchTasks, stopTasksPolling } from '../ws/tasks-ws.js';
import { renderTasks } from '../tasks/render.js';
import { closeGitWs, openLazygitForActiveProject } from './tui-tabs.js';

export function switchTab(name) {
    if (name !== 'terminal' && name !== 'tasks' && name !== 'git'
        && name !== 'docker' && name !== 'telescope') return;
    if (state.activeTab === name) return;
    const prev = state.activeTab;
    state.activeTab = name;

    const onTerminal = name === 'terminal';
    const onTasks = name === 'tasks';
    const onGit = name === 'git';
    const onDocker = name === 'docker';
    const onTelescope = name === 'telescope';
    $terminalEl.hidden = !onTerminal;
    if ($placeholder) $placeholder.hidden = !onTerminal;
    if ($windowBar) $windowBar.hidden = !onTerminal || !state.currentSession;
    $tasksEl.hidden = !onTasks;
    if ($gitEl) $gitEl.hidden = !onGit;
    if ($dockerEl) $dockerEl.hidden = !onDocker;
    if ($telescopeEl) $telescopeEl.hidden = !onTelescope;

    $tabTerminal.classList.toggle('active', onTerminal);
    $tabTasks.classList.toggle('active', onTasks);
    if ($tabGit) $tabGit.classList.toggle('active', onGit);
    if ($tabDocker) $tabDocker.classList.toggle('active', onDocker);
    if ($tabTelescope) $tabTelescope.classList.toggle('active', onTelescope);

    if (prev === 'git' && !onGit) {
        closeGitWs('tab switched away');
    }
    if (prev === 'docker' && !onDocker && state.dockerTerm) {
        state.dockerTerm.close('tab switched away');
    }
    if (prev === 'telescope' && !onTelescope && state.telescopeTerm) {
        state.telescopeTerm.close('tab switched away');
    }
    if (prev === 'tasks' && !onTasks) {
        stopTasksPolling();
    }

    if (onTerminal) {
        requestAnimationFrame(() => {
            try { state.fitAddon && state.fitAddon.fit(); } catch (_) {}
            if (state.term) {
                scheduleResizeFromTerm();
                state.term.focus();
            }
        });
    } else if (onTasks) {
        if (state.tasksData == null) {
            fetchTasks();
        } else {
            renderTasks();
        }
        connectTasksWs();
    } else if (onGit) {
        openLazygitForActiveProject();
    } else if (onDocker) {
        if (state.dockerTerm) state.dockerTerm.openForActiveProject();
    } else if (onTelescope) {
        if (state.telescopeTerm) state.telescopeTerm.openForActiveProject();
    }
}
