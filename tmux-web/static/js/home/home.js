// tmux-web — Home (главная страница: история недавних сессий)
//
// Рендерит экран «Недавние сессии» в #main, когда нет активных tmux-сессий.
// История приходит с бэкенда (Phase 2): GET /api/sessions/history возвращает
// массив HistorySession { name, path, folder_label, windows:[{index,name}],
// first_seen, last_seen }. Карточки позволяют восстановить (запустить) сессию
// в tmux либо удалить запись из истории.
//
// Endpoint'ы (Phase 2 backend):
//   GET    /api/sessions/history             — список HistorySession
//   POST   /api/sessions/history/restore     — body {name, path}
//   POST   /api/sessions/history/restore-all
//   DELETE /api/sessions/history             — body {name, path}
//
// Видимость #home переключает showHome(); интеграция с sidebar/sessions —
// см. js/sessions/sessions.js::fetchSessions и js/sidebar/sidebar.js.
//
// Зависимости:
//   - apiFetch (core/api.js) — origin-aware REST (история только local, origin
//     не передаётся → обычный fetch на текущий хост).
//   - fetchSessions/openSession (sessions/sessions.js) — обновить список и
//     открыть восстановленную сессию.
//   - showPlaceholder (terminal/xterm.js) — скрыть placeholder при показе home.
//   - DOM-ссылки $home, $homeCards, $homeRestoreAll, $homeEmpty (core/dom.js).
//
// Импорты из sessions.js статические: цикл (sessions.js → home.js и обратно)
// безопасен, т.к. все cross-обращения происходят внутри тел функций (вызовы
// в рантайме), а не при инициализации модуля.

import { apiFetch } from '../core/api.js';
import { fetchSessions, openSession } from '../sessions/sessions.js';
import { showPlaceholder } from '../terminal/xterm.js';
import { $home, $homeCards, $homeRestoreAll, $homeEmpty } from '../core/dom.js';

// Защита от повторной навески listener на кнопку «Открыть все».
let restoreAllBound = false;

/**
 * Строит DOM-карточку для одной записи истории. Использует document.createElement
 * (без innerHTML с пользовательскими данными — защита от XSS).
 */
function buildHomeCard(rec) {
    const card = document.createElement('div');
    card.className = 'home-card';

    const name = document.createElement('div');
    name.className = 'home-card-name';
    name.textContent = rec.name;
    card.appendChild(name);

    if (rec.folder_label) {
        const folder = document.createElement('div');
        folder.className = 'home-card-folder';
        folder.textContent = rec.folder_label;
        card.appendChild(folder);
    }

    if (rec.path) {
        const path = document.createElement('div');
        path.className = 'home-card-path';
        path.textContent = rec.path;
        card.appendChild(path);
    }

    const windows = Array.isArray(rec.windows) ? rec.windows : [];
    const winInfo = document.createElement('div');
    winInfo.className = 'home-card-windows';
    const count = windows.length;
    winInfo.textContent = `${count} ${count === 1 ? 'window' : 'windows'}`;
    card.appendChild(winInfo);

    if (count > 0) {
        const list = document.createElement('div');
        list.className = 'home-card-windows-list';
        for (const w of windows) {
            const tag = document.createElement('span');
            tag.className = 'home-card-window-tag';
            tag.textContent = w && w.name ? w.name : String(w && w.index != null ? w.index : '?');
            list.appendChild(tag);
        }
        card.appendChild(list);
    }

    const actions = document.createElement('div');
    actions.className = 'home-card-actions';

    const btnRestore = document.createElement('button');
    btnRestore.type = 'button';
    btnRestore.className = 'home-card-restore';
    btnRestore.textContent = '▶ Запустить';
    btnRestore.title = `Восстановить сессию ${rec.name}`;
    btnRestore.addEventListener('click', (ev) => {
        ev.stopPropagation();
        restoreSession(rec.name, rec.path);
    });
    actions.appendChild(btnRestore);

    const btnDelete = document.createElement('button');
    btnDelete.type = 'button';
    btnDelete.className = 'home-card-delete';
    btnDelete.textContent = '✕';
    btnDelete.title = `Удалить «${rec.name}» из истории`;
    btnDelete.addEventListener('click', (ev) => {
        ev.stopPropagation();
        deleteHistory(rec.name, rec.path);
    });
    actions.appendChild(btnDelete);

    card.appendChild(actions);
    return card;
}

/**
 * Загружает историю и отрисовывает карточки в $homeCards. Пустую историю
 * показывает через #home-empty и скрывает кнопку «Открыть все».
 */
export async function renderHome() {
    if (!$homeCards) return;

    // Навешиваем restoreAll один раз при первом рендере.
    if (!restoreAllBound && $homeRestoreAll) {
        $homeRestoreAll.addEventListener('click', () => restoreAll());
        restoreAllBound = true;
    }

    let records = [];
    try {
        const resp = await apiFetch('/api/sessions/history', {
            headers: { 'Accept': 'application/json' },
        });
        if (!resp.ok) throw new Error('HTTP ' + resp.status);
        const data = await resp.json();
        records = Array.isArray(data) ? data : [];
    } catch (e) {
        console.warn('renderHome: fetch history failed', e);
        records = [];
    }

    $homeCards.innerHTML = '';

    const isEmpty = records.length === 0;
    if ($homeEmpty) $homeEmpty.hidden = !isEmpty;
    if ($homeRestoreAll) $homeRestoreAll.hidden = isEmpty;

    if (isEmpty) return;

    for (const rec of records) {
        $homeCards.appendChild(buildHomeCard(rec));
    }
}

/**
 * Восстанавливает одну сессию: POST /api/sessions/history/restore {name, path}.
 * При успехе обновляет список сессий и открывает её. 409 (имя занято)
 * обрабатывается понятным сообщением.
 */
export async function restoreSession(name, path) {
    try {
        const resp = await apiFetch('/api/sessions/history/restore', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name, path }),
        });
        if (resp.status === 409) {
            window.alert(`Сессия «${name}» уже запущена или имя занято.`);
            return;
        }
        if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось восстановить сессию: ' + (text || resp.status));
            return;
        }
        await fetchSessions();
        openSession(name);
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

/**
 * Восстанавливает все сессии из истории: POST /api/sessions/history/restore-all.
 * После — обновляет список активных сессий.
 */
export async function restoreAll() {
    try {
        const resp = await apiFetch('/api/sessions/history/restore-all', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
        });
        if (!resp.ok) {
            const text = await resp.text();
            window.alert('Не удалось восстановить сессии: ' + (text || resp.status));
            return;
        }
        await fetchSessions();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

/**
 * Удаляет запись из истории: DELETE /api/sessions/history {name, path}.
 * После — перерисовывает главную.
 */
export async function deleteHistory(name, path) {
    try {
        const resp = await apiFetch('/api/sessions/history', {
            method: 'DELETE',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name, path }),
        });
        if (!resp.ok && resp.status !== 204) {
            const text = await resp.text();
            window.alert('Не удалось удалить запись: ' + (text || resp.status));
            return;
        }
        await renderHome();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
    }
}

/**
 * Переключает видимость #home. При show=true скрывает placeholder, чтобы
 * не было одновременного показа home + placeholder.
 */
export function showHome(show) {
    if (!$home) return;
    $home.style.display = show ? 'flex' : 'none';
    if (show) showPlaceholder(false);
}
