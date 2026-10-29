// tmux-web — switchTab dispatcher (Phase 1 ES Modules refactor)
//
// 1:1 копия switchTab из IIFE `tmux-web/static/app.js` (app.js:1913).

import { state } from '../core/state.js';
import {
    $terminalEl, $placeholder, $windowBar,
    $tasksEl, $tabTerminal, $tabTasks,
    $gitEl, $dockerEl, $telescopeEl,
    $tabGit, $tabDocker, $tabTelescope,
    $tabEcho, $echoEl,
    $telegramToggle, $telegram,
    $vkToggle, $vk,
    $bitrixToggle, $b24,
    $trackerToggle, $tracker,
} from '../core/dom.js';
import { scheduleResizeFromTerm } from '../terminal/xterm.js';
import { connectTasksWs, fetchTasks, stopTasksPolling } from '../ws/tasks-ws.js';
import { renderTasks } from '../tasks/render.js';
import { fetchGitCommits, initGanttControls } from '../tasks/gantt.js';
import { closeGitWs, openLazygitForActiveProject } from './tui-tabs.js';
import {
    initEcho, connectEchoWs, disconnectEchoWs, _debugState as echoDebug,
} from '../echo/main.js';
import { hideScreensaver } from '../screensaver/screensaver.js';
import { initTelegram } from '../telegram/main.js';
import { applyTgTheme } from '../telegram/theme.js';
import { initVk } from '../vk/main.js';
import { applyVkTheme } from '../vk/theme.js';
import { initB24 } from '../bitrix/main.js';
import { applyB24Theme } from '../bitrix/theme.js';
// Тема трекера — статический импорт, в отличие от самой панели: модуль
// автономен (ноль связей с js/tracker/main.js) и обязан отработать до того,
// как панель покажется.
import { applyTrackerTheme } from '../tracker/theme.js';

export function switchTab(name) {
    if (name !== 'terminal' && name !== 'tasks' && name !== 'git'
        && name !== 'docker' && name !== 'telescope' && name !== 'echo'
        && name !== 'telegram' && name !== 'vk' && name !== 'bitrix'
        && name !== 'tracker') return;
    if (state.activeTab === name) return;
    // Заставка лежит поверх области терминала — при переключении вкладок гасим её.
    hideScreensaver();
    const prev = state.activeTab;
    state.activeTab = name;

    const onTerminal = name === 'terminal';
    const onTasks = name === 'tasks';
    const onGit = name === 'git';
    const onDocker = name === 'docker';
    const onTelescope = name === 'telescope';
    const onEcho = name === 'echo';
    const onTelegram = name === 'telegram';
    const onVk = name === 'vk';
    const onBitrix = name === 'bitrix';
    const onTracker = name === 'tracker';
    $terminalEl.hidden = !onTerminal;
    if ($placeholder) $placeholder.hidden = !onTerminal;
    if ($windowBar) $windowBar.hidden = !onTerminal || !state.currentSession;
    $tasksEl.hidden = !onTasks;
    if ($gitEl) $gitEl.hidden = !onGit;
    if ($dockerEl) $dockerEl.hidden = !onDocker;
    if ($telescopeEl) $telescopeEl.hidden = !onTelescope;
    if ($echoEl) $echoEl.hidden = !onEcho;
    if ($telegram) $telegram.hidden = !onTelegram;
    if ($vk) $vk.hidden = !onVk;
    if ($b24) $b24.hidden = !onBitrix;
    if ($tracker) $tracker.hidden = !onTracker;

    $tabTerminal.classList.toggle('active', onTerminal);
    $tabTasks.classList.toggle('active', onTasks);
    if ($tabGit) $tabGit.classList.toggle('active', onGit);
    if ($tabDocker) $tabDocker.classList.toggle('active', onDocker);
    if ($tabTelescope) $tabTelescope.classList.toggle('active', onTelescope);
    if ($tabEcho) $tabEcho.classList.toggle('active', onEcho);
    // Кнопка Telegram живёт в #settings-bar, а не в #tab-bar, но подсветку
    // активной вкладки получает по тому же правилу.
    if ($telegramToggle) $telegramToggle.classList.toggle('active', onTelegram);
    if ($vkToggle) $vkToggle.classList.toggle('active', onVk);
    if ($bitrixToggle) $bitrixToggle.classList.toggle('active', onBitrix);
    if ($trackerToggle) $trackerToggle.classList.toggle('active', onTracker);

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
    if (prev === 'echo' && !onEcho) {
        disconnectEchoWs();
    }
    // Уход с вкладки трекера ЗАКРЫВАЕТ его сокет — в отличие от трёх
    // мессенджеров, чьи соединения постоянны ради бейджа непрочитанного.
    // Трекеру, пока панель закрыта, сообщать не о чем: задачи заводит сам
    // пользователь, извне они не приходят.
    if (prev === 'tracker' && !onTracker) {
        import('../tracker/main.js')
            .then((m) => m.suspendTracker())
            .catch(() => { /* модуль не грузился — закрывать нечего */ });
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
        // Гант: навесить переключатель диапазона (идемпотентно) и подгрузить
        // git-коммиты корня текущей сессии (fetchGitCommits сам перерисует гант).
        initGanttControls();
        fetchGitCommits();
    } else if (onGit) {
        openLazygitForActiveProject();
    } else if (onDocker) {
        if (state.dockerTerm) state.dockerTerm.openForActiveProject();
    } else if (onTelescope) {
        if (state.telescopeTerm) state.telescopeTerm.openForActiveProject();
    } else if (onEcho) {
        initEcho();
        const dbg = echoDebug();
        if (dbg && dbg.activeConversationId) {
            connectEchoWs(dbg.activeConversationId);
        }
    } else if (onTelegram) {
        // Тема пересчитывается на КАЖДОМ входе: в режиме auto пользователь мог
        // сменить тему devforge, пока вкладка была закрыта.
        applyTgTheme();
        // Teardown при уходе с вкладки НЕ делаем: WS у Telegram постоянный —
        // он нужен для unread-бейджа и тостов с любой вкладки.
        initTelegram();
    } else if (onVk) {
        // Ровно те же два правила, что у телеграмной ветки: тема
        // пересчитывается на КАЖДОМ входе (в режиме auto пользователь мог
        // сменить тему devforge, пока вкладка была закрыта), а teardown при
        // уходе НЕ делается — WS постоянный, иначе бейдж и тосты появлялись бы
        // только после первого захода на вкладку.
        applyVkTheme();
        initVk();
    } else if (onTracker) {
        // Тема пересчитывается на КАЖДОМ входе — то же правило, что у трёх
        // мессенджеров: в режиме auto пользователь мог сменить тему devforge,
        // пока вкладка была закрыта. Вызов синхронный и стоит перед ленивым
        // import'ом панели: атрибут обязан оказаться на #tracker до того, как
        // панель станет видимой, иначе она мелькнёт чужой палитрой.
        applyTrackerTheme();
        // ЛЕНИВАЯ ЗАГРУЗКА: модуль панели (рендер, сокет, разбор строки,
        // карточка) приезжает при ПЕРВОМ открытии вкладки, а не на старте
        // приложения. Гейт кнопки живёт отдельно (js/tracker/gate.js) и
        // тянет за собой только один fetch. Повторные переключения вкладки
        // модуль не пересоздают — import кэширован, а initTracker
        // идемпотентна.
        import('../tracker/main.js')
            .then((m) => m.initTracker())
            .catch((e) => console.warn('[tabs] initTracker failed', e));
    } else if (onBitrix) {
        // Ровно те же два правила, что у телеграмной и VK-веток: тема
        // пересчитывается на КАЖДОМ входе (в режиме auto пользователь мог
        // сменить тему devforge, пока вкладка была закрыта), а teardown при
        // уходе НЕ делается — WS постоянный, иначе бейдж и тосты появлялись
        // бы только после первого захода на вкладку.
        applyB24Theme();
        initB24();
    }
}
