// tmux-web — bootstrap entry (Phase 1 ES Modules refactor)
//
// 1:1 копия bootstrap из IIFE `tmux-web/static/app.js` (6552-6692) +
// global keydown listeners (Cmd/Ctrl+B, Esc) и mqlMobile change handler
// (app.js:745-851, 797-825) — всё wiring.

import { state } from './state.js';
import {
    $btnNew, $btnNewPath, $windowNewBtn, $windowNewWorktreeBtn,
    $tabTerminal, $tabTasks, $tabGit, $tabDocker, $tabTelescope, $tabEcho,
    $tasksReload, $tasksNew,
    $projectSettings, $screensaverToggle, $telegramToggle, $vkToggle,
    $bitrixToggle,
    $btnSidebarToggle, $sidebarOverlay, $layout,
    $claudeMemoryBtn,
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
import { initTerminalPasteFiles, initTerminalDropFiles } from '../terminal/paste-files.js';
import { switchTab } from '../tabs/tabs.js';
import { initTuiTabs, closeGitWs } from '../tabs/tui-tabs.js';
import {
    fetchSessions, startPolling, stopPolling, createSessionPrompt, createSessionInPath,
} from '../sessions/sessions.js';
import { createWindow, createWorktreeWindow } from '../sessions/windows.js';
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
import { openClaudeMemoryModal } from '../sessions/claude-memory.js';
import {
    initEcho, connectEchoWs, disconnectEchoWs, teardownEcho, _debugState as echoDebug,
} from '../echo/main.js';
import {
    initTelegramGate, suspendTelegram, resumeTelegram, teardownTelegram,
} from '../telegram/main.js';
import { initTelegramTheme, teardownTelegramTheme } from '../telegram/theme.js';
import {
    initVkGate, suspendVk, resumeVk, teardownVk,
} from '../vk/main.js';
import { initVkTheme, teardownVkTheme } from '../vk/theme.js';
import {
    initB24Gate, suspendB24, resumeB24, teardownB24,
} from '../bitrix/main.js';
import { initB24Theme, teardownB24Theme } from '../bitrix/theme.js';
import * as meetApi from '../meet/api.js';
import { initMeetGate } from '../meet/api.js';
import {
    connectMeetWs, suspendMeet, resumeMeet, teardownMeet,
} from '../meet/ws.js';
import { installMeetPopups, teardownMeetPopups } from '../messenger/meet-popup.js';

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
    // Передача файлов в сессию — только после того, как xterm создал свои
    // элементы внутри #terminal: слушатель вставки висит на document в capture
    // и ориентируется на target внутри #terminal, drag&drop — на самом
    // #terminal, тоже в capture.
    initTerminalPasteFiles();
    initTerminalDropFiles();
    applyTerminalFontSize();
    showPlaceholder(true);
    setStatus('disconnected', 'disconnected');

    $btnNew.addEventListener('click', createSessionPrompt);
    if ($btnNewPath) $btnNewPath.addEventListener('click', createSessionInPath);
    if ($windowNewBtn) $windowNewBtn.addEventListener('click', createWindow);
    if ($windowNewWorktreeBtn) $windowNewWorktreeBtn.addEventListener('click', createWorktreeWindow);

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

    if ($claudeMemoryBtn) {
        $claudeMemoryBtn.addEventListener('click', openClaudeMemoryModal);
    }

    if ($screensaverToggle) {
        $screensaverToggle.addEventListener('click', showScreensaver);
    }

    if ($telegramToggle) {
        $telegramToggle.addEventListener('click', () => switchTab('telegram'));
    }

    if ($vkToggle) {
        $vkToggle.addEventListener('click', () => switchTab('vk'));
    }

    if ($bitrixToggle) {
        $bitrixToggle.addEventListener('click', () => switchTab('bitrix'));
    }

    // Тема панели Telegram: восстанавливается ДО гейта, чтобы вкладка, если
    // её откроют сразу, не мигнула чужой палитрой. Работает и при выключенном
    // плагине (разметка #telegram в index.html есть всегда) — накладных
    // расходов это не создаёт: один MutationObserver на <html>.
    try {
        initTelegramTheme();
    } catch (e) {
        console.warn('[bootstrap] initTelegramTheme failed', e);
    }

    // Гейт кнопки ✈: спрашиваем GET /api/telegram/status. Плагин выключен
    // (404) — кнопка просто остаётся скрытой, ошибок в консоли нет.
    try {
        initTelegramGate();
    } catch (e) {
        console.warn('[bootstrap] initTelegramGate failed', e);
    }

    // Тема панели VK — по тем же причинам ДО гейта: иначе вкладка, открытая
    // сразу после загрузки, мигнула бы чужой палитрой. Второй MutationObserver
    // на <html> приемлем: связать его с телеграмным означало бы править
    // js/telegram/theme.js, а стоимость одного наблюдателя нулевая.
    try {
        initVkTheme();
    } catch (e) {
        console.warn('[bootstrap] initVkTheme failed', e);
    }

    // Гейт кнопки Ⓥ: GET /api/vk/status. 404 = плагин выключен, кнопка
    // остаётся скрытой и в консоль НИЧЕГО не пишется (штатный путь для всех,
    // кто запускает devforge без --vk).
    try {
        initVkGate();
    } catch (e) {
        console.warn('[bootstrap] initVkGate failed', e);
    }

    // Тема панели Битрикс24 — по тем же причинам ДО гейта: иначе вкладка,
    // открытая сразу после загрузки, мигнула бы чужой палитрой. Третий
    // MutationObserver на <html> приемлем: связать его с соседними означало
    // бы править js/telegram/theme.js и js/vk/theme.js, а стоимость одного
    // наблюдателя нулевая.
    try {
        initB24Theme();
    } catch (e) {
        console.warn('[bootstrap] initB24Theme failed', e);
    }

    // Гейт кнопки Ⓑ: GET /api/bitrix/status. 404 = плагин выключен, кнопка
    // остаётся скрытой и в консоль НИЧЕГО не пишется (штатный путь для всех,
    // кто запускает devforge без --bitrix).
    try {
        initB24Gate();
    } catch (e) {
        console.warn('[bootstrap] initB24Gate failed', e);
    }

    // Гейт плагина встреч: РОВНО ОДИН GET /api/meet/status за загрузку
    // страницы. 404 = запущено без `--meet`, и тогда не происходит вообще
    // ничего: кнопок нет, батчей нет, соединение не открывается, в консоль не
    // пишется ни строчки. Своей кнопки в шапке у встреч нет — они живут внутри
    // лент трёх мессенджеров, поэтому здесь только транспорт.
    //
    // Соединение поднимается ЗДЕСЬ, а не при открытии панели: индикатор записи
    // обязан работать, когда человек нажал кнопку в Telegram и ушёл в терминал.
    try {
        initMeetGate()
            .then((status) => {
                if (!status) return;
                connectMeetWs();
                // Карточка встречи включается ЗДЕСЬ же и только при 200:
                // `installMeetPopups` подставляет обработчик клика кнопке в
                // лентах, а сами узлы создаются лениво, при первом клике в
                // конкретной панели. Без `--meet` не создаётся ничего.
                installMeetPopups({ api: meetApi });
            })
            .catch(() => { /* гейт не бросает; ловим на случай будущих правок */ });
    } catch (e) {
        console.warn('[bootstrap] initMeetGate failed', e);
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
        teardownTelegram();
        teardownTelegramTheme();
        teardownVk();
        teardownVkTheme();
        teardownB24();
        teardownB24Theme();
        teardownMeet();
        // Слушатель Esc карточки живёт на документе и переживает уход страницы
        // в bfcache — снимается явно, иначе выстрелит по мёртвому DOM.
        teardownMeetPopups();
    });
    document.addEventListener('visibilitychange', () => {
        if (document.hidden) {
            stopPolling();
            stopTasksPolling();
            stopTodosPolling();
            if (state.activeTab === 'echo') {
                disconnectEchoWs();
            }
            // WS Telegram постоянный (бейдж с любой вкладки), поэтому его
            // закрываем не по смене вкладки, а по уходу страницы в фон.
            suspendTelegram();
            suspendVk();
            suspendB24();
            // WS встреч тоже постоянный и по той же причине: индикатор записи
            // не привязан к активной вкладке.
            suspendMeet();
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
            resumeTelegram();
            resumeVk();
            resumeB24();
            // Плагин выключен — resumeMeet() выходит первой строкой.
            resumeMeet();
        }
    });

    // Сигнал готовности UI для PWA-bootstrap (Фаза 5): обработка
    // shortcuts/share-target из manifest (?tab=/?view=/?share-target=) ждёт
    // этого флага/события, чтобы switchTab/showDailySummary не вызвались до
    // полной инициализации. Без --pwa слушателей нет — no-op побочный эффект.
    window.__FORGE_APP_READY = true;
    try {
        window.dispatchEvent(new Event('forge:app-ready'));
    } catch (_) { /* старые браузеры без Event-конструктора — флага достаточно */ }
}
