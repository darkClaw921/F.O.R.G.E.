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
import { syncGitToCurrentSession, syncTelescopeToCurrentSession, syncDockerToCurrentSession } from '../tabs/tui-tabs.js';
import { syncTasksToCurrentSession } from '../ws/tasks-ws.js';
import { syncTodosToCurrentSession } from '../ws/todos-ws.js';
import { renderHome, showHome } from '../home/home.js';

export async function fetchSessions() {
    try {
        // Догрузка предложений «следующего шага» идёт параллельно с основным
        // списком сессий, но НЕ влияет на него: ошибка next-steps не должна
        // ломать рендер сайдбара. Поэтому fetchNextSteps глотает свои ошибки.
        const [resp] = await Promise.all([
            fetch('/api/sessions', { headers: { 'Accept': 'application/json' } }),
            fetchNextSteps(),
        ]);
        if (!resp.ok) {
            throw new Error('HTTP ' + resp.status);
        }
        const data = await resp.json();
        state.sessions = Array.isArray(data) ? data : [];
        renderSidebar();
        syncHomeVisibility();
    } catch (e) {
        console.warn('fetchSessions failed', e);
    }
}

// Догружает текущие эфемерные предложения «следующего шага» из плагина Echo и
// складывает их в state.nextSteps (map session → { content }). Ошибка запроса
// НЕ пробрасывается наружу (graceful): если эндпоинт недоступен — предыдущее
// состояние остаётся, рендер сессий не ломается. Вызывается из fetchSessions()
// (poll каждые 3с) и напрямую из echo/ws.js по событию NextStepEvent для
// мгновенной реакции.
export async function fetchNextSteps() {
    try {
        const resp = await fetch('/api/echo/next-steps', { headers: { 'Accept': 'application/json' } });
        if (!resp.ok) {
            throw new Error('HTTP ' + resp.status);
        }
        const data = await resp.json();
        const items = (data && Array.isArray(data.items)) ? data.items : [];
        const next = {};
        for (const it of items) {
            if (it && typeof it.session === 'string') {
                next[it.session] = { content: it.content || '' };
            }
        }
        state.nextSteps = next;
    } catch (e) {
        console.warn('fetchNextSteps failed', e);
    }
}

// Показывает главную (#home) при пустом списке сессий и отсутствии активной
// сессии; иначе скрывает её. Гарантирует, что home и placeholder не
// показываются одновременно (showHome(true) скрывает placeholder).
function syncHomeVisibility() {
    if (state.sessions.length === 0 && !state.currentSession) {
        renderHome();
        showHome(true);
    } else {
        showHome(false);
    }
}

// Человекочитаемая длительность из секунд: «5 с», «1 мин 20 с», «2 ч 3 мин».
function formatDuration(totalSecs) {
    const secs = Math.max(0, Math.floor(totalSecs));
    if (secs < 60) return `${secs} с`;
    const mins = Math.floor(secs / 60);
    const rem = secs % 60;
    if (mins < 60) return rem ? `${mins} мин ${rem} с` : `${mins} мин`;
    const hours = Math.floor(mins / 60);
    const remMin = mins % 60;
    return remMin ? `${hours} ч ${remMin} мин` : `${hours} ч`;
}

// Текст кастомного tooltip синего индикатора работы (✶), кладётся в
// data-tooltip спарка и показывается через js/ui/tooltip.js. Объясняет,
// ПОЧЕМУ индикатор горит: в этой сессии содержимое терминала (последние 50
// строк pane) изменилось за последний тик watcher'а (1.5 с) — Claude печатает,
// идёт вывод процесса и т.п. generating_since_secs (если есть) показывает,
// как давно началась текущая непрерывная серия генерации.
function generatingTooltip(s) {
    const base = `Индикатор работы: в сессии «${s.name}» содержимое терминала меняется`;
    if (typeof s.generating_since_secs === 'number') {
        return `${base} — генерация идёт уже ${formatDuration(s.generating_since_secs)}`;
    }
    return `${base} прямо сейчас`;
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
    // Голубое свечение, если для сессии есть предложение «следующего шага».
    // Визуально отдельно от синего спарка ✶ (is_generating ниже) — это два
    // независимых индикатора: ✶ = Claude печатает прямо сейчас, has-next-step =
    // эпизод завершён и есть готовое предложение что делать дальше. Класс
    // снимается автоматически при перерендере, когда предложение исчезает из
    // state.nextSteps (poll или WS NextStepEvent{has_suggestion:false}).
    if (state.nextSteps && state.nextSteps[s.name]) {
        li.classList.add('has-next-step');
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
        spark.dataset.tooltip = generatingTooltip(s);
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
    // Создаём в рабочем каталоге АКТИВНОЙ сессии (cwd той, что выбрана в момент
    // нажатия). Если активной сессии нет — path не шлём, и бэкенд использует
    // свой дефолтный active_path (cwd процесса devforge).
    const activeSess = (state.sessions || []).find((s) => s && s.name === state.currentSession);
    const activePath = (activeSess && activeSess.path) ? activeSess.path : null;
    const payload = { name: trimmed };
    if (activePath) payload.path = activePath;
    try {
        const resp = await fetch('/api/sessions', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
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

// Создание сессии в указанной папке (кнопка «+ в папке» рядом с «+ new»).
// Открывает НАТИВНЫЙ системный диалог выбора папки через бэкенд
// (GET /api/fs/pick-folder → osascript/zenity на машине сервера), затем
// спрашивает имя и шлёт POST /api/sessions { name, path }.
export async function createSessionInPath() {
    let path = '';
    try {
        const resp = await fetch('/api/fs/pick-folder', { headers: { Accept: 'application/json' } });
        if (resp.status === 204) return; // пользователь нажал «Отмена»
        if (resp.status === 501) {
            // Нативный диалог недоступен — fallback на ручной ввод пути.
            const manual = window.prompt('Диалог недоступен на этой системе. Введите путь к папке вручную:', '');
            if (manual === null) return;
            path = manual.trim();
        } else if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось открыть диалог выбора папки: ' + (text || resp.status));
            return;
        } else {
            const data = await resp.json();
            path = (data && data.path) ? String(data.path).trim() : '';
        }
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        return;
    }
    if (!path) return;

    const name = window.prompt('Имя новой сессии в папке\n' + path + ':', '');
    if (!name) return;
    const trimmedName = name.trim();
    if (!trimmedName) return;
    try {
        const resp = await fetch('/api/sessions', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmedName, path }),
        });
        if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось создать сессию: ' + (text || resp.status));
            return;
        }
        await fetchSessions();
        openSession(trimmedName);
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

    if (state.ws && state.ws.readyState === WebSocket.OPEN) {
        switchSession(name);
        return;
    }
    connectWs(name, sessOrigin);
    syncGitToCurrentSession();
    syncTasksToCurrentSession();
    syncTodosToCurrentSession();
    syncTelescopeToCurrentSession();
    syncDockerToCurrentSession();
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
        syncTodosToCurrentSession();
        syncTelescopeToCurrentSession();
        syncDockerToCurrentSession();
    } catch (e) {
        console.warn('switch failed', e);
        disconnectWs();
        connectWs(name);
    }
}
