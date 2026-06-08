// tmux-web — bootstrap entry (Phase 1 ES Modules refactor)
//
// 1:1 копия bootstrap из IIFE `tmux-web/static/app.js` (6552-6692) +
// global keydown listeners (Cmd/Ctrl+B, Esc) и mqlMobile change handler
// (app.js:745-851, 797-825) — всё wiring.

import { state } from './state.js';
import {
    $btnNew, $btnNewPath, $windowNewBtn,
    $tabTerminal, $tabTasks, $tabGit, $tabDocker, $tabTelescope, $tabEcho,
    $tasksReload, $tasksNew,
    $projectSettings, $screensaverToggle,
    $btnSidebarToggle, $sidebarOverlay, $layout,
} from './dom.js';
import {
    _mqlMobile, isMobileViewport, applyTerminalFontSize,
} from './viewport.js';
import { loadHealthz, isRemoteMode } from '../remote/healthz.js';
import { fetchRemoteServers, stopRemoteHealthPoll } from '../remote/servers.js';
import { loadActiveOriginFromStorage } from '../sidebar/origin-tabs.js';
import {
    applySidebarCollapsed, setMobileSidebarOpen, toggleSidebar, restoreSidebarState,
} from '../sidebar/mobile.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { loadActiveThemeOrNull } from '../themes/api.js';
import { initTerminal, showPlaceholder, setStatus } from '../terminal/xterm.js';
import { switchTab } from '../tabs/tabs.js';
import { initTuiTabs, closeGitWs } from '../tabs/tui-tabs.js';
import {
    fetchSessions, startPolling, stopPolling, createSessionPrompt, createSessionInPath,
} from '../sessions/sessions.js';
import { createWindow } from '../sessions/windows.js';
import { disconnectWs } from '../ws/attach.js';
import {
    fetchTasks, connectTasksWs, disconnectTasksWs, stopTasksPolling,
} from '../ws/tasks-ws.js';
import {
    fetchTodos, connectTodosWs, disconnectTodosWs, stopTodosPolling,
} from '../ws/todos-ws.js';
import { openSettingsModal } from '../settings/modal.js';
import { showScreensaver } from '../screensaver/screensaver.js';
import { initTooltips } from '../ui/tooltip.js';
import { initNextStepPopup } from '../sessions/next-step-popup.js';
import { fetchUserSettings } from '../settings/user-settings-api.js';
import { openCreateModal } from '../tasks/modals.js';
import {
    initEcho, connectEchoWs, disconnectEchoWs, teardownEcho, _debugState as echoDebug,
} from '../echo/main.js';

// ----- side-effect global listeners (app.js:745-851) -----
// Sidebar overlay click / Esc / Cmd+B hotkey + mqlMobile change.
if ($btnSidebarToggle) {
    $btnSidebarToggle.addEventListener('click', toggleSidebar);
}
if ($sidebarOverlay) {
    $sidebarOverlay.addEventListener('click', () => {
        setMobileSidebarOpen(false);
    });
}
window.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape') return;
    if (!isMobileViewport()) return;
    if (!document.body.classList.contains('sidebar-open')) return;
    setMobileSidebarOpen(false);
});
if (_mqlMobile) {
    const _onMqlChange = (e) => {
        if (e.matches) {
            if ($layout && $layout.classList.contains('sidebar-collapsed')) {
                $layout.classList.remove('sidebar-collapsed');
            }
            setMobileSidebarOpen(false);
        } else {
            document.body.classList.remove('sidebar-open');
            let collapsed = false;
            try {
                collapsed = localStorage.getItem('forge.sidebarCollapsed') === '1';
            } catch (_) {}
            applySidebarCollapsed(collapsed);
        }
        applyTerminalFontSize();
    };
    if (typeof _mqlMobile.addEventListener === 'function') {
        _mqlMobile.addEventListener('change', _onMqlChange);
    } else if (typeof _mqlMobile.addListener === 'function') {
        _mqlMobile.addListener(_onMqlChange);
    }
}
window.addEventListener('keydown', (ev) => {
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    const mod = isMac ? ev.metaKey && !ev.ctrlKey : ev.ctrlKey && !ev.metaKey;
    if (mod && !ev.altKey && !ev.shiftKey && ev.key.toLowerCase() === 'b') {
        if (!isMac) {
            const tgt = ev.target;
            if (tgt && tgt.classList && (
                tgt.classList.contains('xterm-helper-textarea') ||
                (tgt.closest && tgt.closest('.xterm'))
            )) {
                return;
            }
        }
        ev.preventDefault();
        ev.stopPropagation();
        toggleSidebar();
    }
}, true);

// ---- Phase 6 — экспорт хелперов в window.__forge для регресс-тестов ----
import { groupSessionsByFolder } from '../sessions/sessions.js';
import { aggregateAllOrigins } from '../remote/servers.js';
if (typeof window !== 'undefined') {
    window.__forge = window.__forge || {};
    window.__forge.groupSessionsByFolder = groupSessionsByFolder;
    window.__forge.aggregateAllOrigins = aggregateAllOrigins;
}

export async function bootstrap() {
    await loadHealthz();
    restoreSidebarState();
    const termTheme = await loadActiveThemeOrNull();
    initTerminal(termTheme);
    applyTerminalFontSize();
    showPlaceholder(true);
    setStatus('disconnected', 'disconnected');

    $btnNew.addEventListener('click', createSessionPrompt);
    if ($btnNewPath) $btnNewPath.addEventListener('click', createSessionInPath);
    if ($windowNewBtn) $windowNewBtn.addEventListener('click', createWindow);

    if ($tabTerminal) $tabTerminal.addEventListener('click', () => switchTab('terminal'));
    if ($tabTasks) $tabTasks.addEventListener('click', () => switchTab('tasks'));
    if ($tasksReload) $tasksReload.addEventListener('click', () => fetchTasks());
    if ($tasksNew) $tasksNew.addEventListener('click', () => openCreateModal());

    initTuiTabs();
    applyTerminalFontSize();
    initTooltips();
    initNextStepPopup();

    if ($tabGit) $tabGit.addEventListener('click', () => switchTab('git'));
    if ($tabDocker) $tabDocker.addEventListener('click', () => switchTab('docker'));
    if ($tabTelescope) $tabTelescope.addEventListener('click', () => switchTab('telescope'));
    if ($tabEcho) $tabEcho.addEventListener('click', () => switchTab('echo'));

    // Eager-init Echo (заполняет model picker, sidebar, fetch conversations).
    // Сама вкладка остаётся hidden до switchTab('echo').
    try { initEcho(); } catch (e) { console.warn('[bootstrap] initEcho failed', e); }

    if ($projectSettings) {
        $projectSettings.addEventListener('click', openSettingsModal);
    }

    if ($screensaverToggle) {
        $screensaverToggle.addEventListener('click', showScreensaver);
    }

    if (isRemoteMode()) {
        fetchRemoteServers().then(() => {
            loadActiveOriginFromStorage();
            renderSidebar();
        });
    }

    fetchSessions();
    startPolling();
    connectTasksWs();
    fetchTodos();
    connectTodosWs();

    // Best-effort preload пользовательских настроек (TODO behavior).
    // fetchUserSettings уже глотает ошибки и возвращает null — не блокируем UI.
    try {
        fetchUserSettings().catch(() => {});
    } catch (_) { /* never reached, defensive */ }

    window.addEventListener('beforeunload', () => {
        stopPolling();
        stopTasksPolling();
        stopTodosPolling();
        disconnectTasksWs();
        disconnectTodosWs();
        disconnectWs();
        closeGitWs('beforeunload');
        if (state.dockerTerm) state.dockerTerm.close('beforeunload');
        if (state.telescopeTerm) state.telescopeTerm.close('beforeunload');
        stopRemoteHealthPoll();
        teardownEcho();
    });
    document.addEventListener('visibilitychange', () => {
        if (document.hidden) {
            stopPolling();
            stopTasksPolling();
            stopTodosPolling();
            if (state.activeTab === 'echo') {
                disconnectEchoWs();
            }
        } else {
            fetchSessions();
            startPolling();
            if (state.activeTab === 'tasks') {
                connectTasksWs();
                if (!state.tasksWs || state.tasksWs.readyState !== WebSocket.OPEN) {
                    fetchTasks();
                }
            }
            connectTodosWs();
            if (!state.todosWs || state.todosWs.readyState !== WebSocket.OPEN) {
                fetchTodos();
            }
            if (state.activeTab === 'echo') {
                const dbg = echoDebug();
                if (dbg && dbg.activeConversationId) {
                    connectEchoWs(dbg.activeConversationId);
                }
            }
        }
    });
}
