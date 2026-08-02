// tmux-web — Home (главная страница: история недавних сессий)
//
// Рендерит экран «Недавние сессии» в #main, когда нет активных tmux-сессий.
// История приходит с бэкенда (Phase 2): GET /api/sessions/history возвращает
// массив HistorySession { name, path, folder_label, windows:[{index,name}],
// first_seen, last_seen }. Карточки позволяют восстановить (запустить) сессию
// в tmux либо удалить запись из истории.
//
// Карточки сгруппированы по ДНЮ СОЗДАНИЯ сессии (first_seen): каждая группа —
// сворачиваемая секция с заголовком «Сегодня»/«Вчера»/дата и кнопкой «Открыть
// все за день». По умолчанию раскрыт только сегодняшний день; ручное
// сворачивание/разворачивание переживает периодический poll renderHome.
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
import { $home, $homeCards, $homeRestoreAll, $homeRestoreSelected, $homeEmpty } from '../core/dom.js';

// Защита от повторной навески listener на кнопки заголовка.
let headerBound = false;

// Выбранные записи истории (режим мультивыбора). Ключ — name\npath,
// значение — {name, path}. Сбрасывается при каждом renderHome().
const selected = new Map();

// Состояние сворачивания групп-дней. Ключ — dayKey (YYYY-MM-DD), значение —
// true (свёрнут). Переживает периодические перерендеры renderHome (poll ~3с),
// поэтому ручные переключения пользователя не сбрасываются. Дни, которых больше
// нет в актуальной истории, вычищаются в renderHome (как и selected).
const dayCollapsed = new Map();

/** Стабильный ключ записи истории для Map выбранных. */
function selKey(name, path) {
    return `${name}\n${path || ''}`;
}

/**
 * Ключ дня (YYYY-MM-DD, локальная зона) из Unix-времени в СЕКУНДАХ.
 * first_seen/last_seen в истории хранятся в секундах, поэтому домножаем на 1000.
 */
function dayKeyFromUnix(sec) {
    const d = new Date((Number(sec) || 0) * 1000);
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    return `${y}-${m}-${day}`;
}

/** Сегодняшний dayKey (локальная зона). */
function todayKey() {
    return dayKeyFromUnix(Date.now() / 1000);
}

/** dayKey «вчера» относительно сегодняшнего дня (локальная зона). */
function yesterdayKey() {
    const d = new Date();
    d.setDate(d.getDate() - 1);
    return dayKeyFromUnix(d.getTime() / 1000);
}

/**
 * Человекочитаемый заголовок дня: «Сегодня» / «Вчера» / иначе
 * «21 июля 2026, суббота» (локаль ru-RU, день недели переставлен в конец —
 * тот же приём, что в daily-summary.js::humanDate).
 */
function humanDayLabel(dayKey) {
    if (dayKey === todayKey()) return 'Сегодня';
    if (dayKey === yesterdayKey()) return 'Вчера';
    const [y, m, d] = dayKey.split('-').map(Number);
    const dt = new Date(y, m - 1, d);
    try {
        const s = dt.toLocaleDateString('ru-RU', {
            day: 'numeric', month: 'long', year: 'numeric', weekday: 'long',
        });
        const m2 = s.match(/^([^,]+),\s*(.+)$/);
        return m2 ? `${m2[2]}, ${m2[1]}` : s;
    } catch {
        return dayKey;
    }
}

/**
 * Группирует записи истории по дню СОЗДАНИЯ (first_seen). Возвращает массив
 * `{ dayKey, recs }`, отсортированный по dayKey убыв. (свежие дни — сверху);
 * внутри группы записи отсортированы по first_seen убыв.
 */
function groupByDay(records) {
    const byDay = new Map();
    for (const rec of records) {
        const key = dayKeyFromUnix(rec.first_seen);
        let bucket = byDay.get(key);
        if (!bucket) {
            bucket = [];
            byDay.set(key, bucket);
        }
        bucket.push(rec);
    }
    const groups = Array.from(byDay.entries()).map(([dayKey, recs]) => {
        recs.sort((a, b) => (Number(b.first_seen) || 0) - (Number(a.first_seen) || 0));
        return { dayKey, recs };
    });
    groups.sort((a, b) => (a.dayKey < b.dayKey ? 1 : a.dayKey > b.dayKey ? -1 : 0));
    return groups;
}

/**
 * Обновляет видимость и подпись кнопки «Открыть выбранные» по текущему
 * содержимому selected.
 */
function updateSelectionUI() {
    if (!$homeRestoreSelected) return;
    const n = selected.size;
    $homeRestoreSelected.hidden = n === 0;
    $homeRestoreSelected.textContent = `Открыть выбранные (${n})`;
}

/**
 * Строит DOM-карточку для одной записи истории. Использует document.createElement
 * (без innerHTML с пользовательскими данными — защита от XSS).
 */
function buildHomeCard(rec) {
    const card = document.createElement('div');
    card.className = 'home-card';

    const key = selKey(rec.name, rec.path);
    if (selected.has(key)) card.classList.add('selected');

    // Индикатор выбора (галочка) в углу карточки.
    const check = document.createElement('div');
    check.className = 'home-card-check';
    check.setAttribute('aria-hidden', 'true');
    check.textContent = '✓';
    card.appendChild(check);

    // Клик по карточке (не по кнопкам действий) переключает выбор.
    card.addEventListener('click', () => {
        if (selected.has(key)) {
            selected.delete(key);
            card.classList.remove('selected');
        } else {
            selected.set(key, { name: rec.name, path: rec.path });
            card.classList.add('selected');
        }
        updateSelectionUI();
    });

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
 * Сворачивает/разворачивает группу-день по клику на заголовок. Обновляет
 * dayCollapsed и DOM напрямую (класс + стрелка), БЕЗ вызова renderHome —
 * чтобы не дёргать fetch истории и не мигать списком.
 */
function toggleDay(dayKey, sectionEl, toggleEl) {
    const nowCollapsed = !dayCollapsed.get(dayKey);
    dayCollapsed.set(dayKey, nowCollapsed);
    sectionEl.classList.toggle('collapsed', nowCollapsed);
    if (toggleEl) toggleEl.textContent = nowCollapsed ? '▸' : '▾';
}

/**
 * Строит секцию одного дня: заголовок (стрелка сворачивания, дата, счётчик,
 * кнопка «Открыть все за день») + грид карточек. Состояние сворачивания берётся
 * из dayCollapsed (инициализируется вызывающим renderHome).
 */
function buildDayGroup(dayKey, recs) {
    const section = document.createElement('section');
    section.className = 'home-day-group';
    const collapsed = dayCollapsed.get(dayKey) === true;
    if (collapsed) section.classList.add('collapsed');

    const header = document.createElement('div');
    header.className = 'home-day-header';

    const toggle = document.createElement('span');
    toggle.className = 'home-day-toggle';
    toggle.setAttribute('aria-hidden', 'true');
    toggle.textContent = collapsed ? '▸' : '▾';
    header.appendChild(toggle);

    const title = document.createElement('span');
    title.className = 'home-day-title';
    title.textContent = humanDayLabel(dayKey);
    header.appendChild(title);

    const count = document.createElement('span');
    count.className = 'home-day-count';
    count.textContent = `· ${recs.length}`;
    header.appendChild(count);

    const btnRestore = document.createElement('button');
    btnRestore.type = 'button';
    btnRestore.className = 'home-day-restore';
    btnRestore.textContent = `Открыть все (${recs.length})`;
    btnRestore.title = `Восстановить все сессии за ${humanDayLabel(dayKey)}`;
    btnRestore.addEventListener('click', (ev) => {
        ev.stopPropagation();
        restoreDay(recs);
    });
    header.appendChild(btnRestore);

    header.addEventListener('click', () => toggleDay(dayKey, section, toggle));
    section.appendChild(header);

    const cards = document.createElement('div');
    cards.className = 'home-day-cards';
    for (const rec of recs) {
        cards.appendChild(buildHomeCard(rec));
    }
    section.appendChild(cards);

    return section;
}

/**
 * Загружает историю и отрисовывает карточки в $homeCards, сгруппированные по
 * дню создания сессии (first_seen). Пустую историю
 * показывает через #home-empty и скрывает кнопку «Открыть все».
 */
export async function renderHome() {
    if (!$homeCards) return;

    // Навешиваем listener'ы кнопок заголовка один раз при первом рендере.
    if (!headerBound) {
        if ($homeRestoreAll) $homeRestoreAll.addEventListener('click', () => restoreAll());
        if ($homeRestoreSelected) $homeRestoreSelected.addEventListener('click', () => restoreSelected());
        headerBound = true;
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

    // Выбор сохраняется между перерендерами (renderHome зовётся периодическим
    // poll'ом fetchSessions каждые 3 с — иначе выделение пропадало бы). Чистим
    // только записи, которых больше нет в актуальной истории.
    const validKeys = new Set(records.map((r) => selKey(r.name, r.path)));
    for (const key of selected.keys()) {
        if (!validKeys.has(key)) selected.delete(key);
    }

    $homeCards.innerHTML = '';

    const isEmpty = records.length === 0;
    if ($homeEmpty) $homeEmpty.hidden = !isEmpty;
    if ($homeRestoreAll) $homeRestoreAll.hidden = isEmpty;

    if (isEmpty) {
        dayCollapsed.clear();
        updateSelectionUI();
        return;
    }

    const groups = groupByDay(records);
    const today = todayKey();

    // Инициализируем состояние сворачивания для новых дней: по умолчанию раскрыт
    // только сегодняшний день, остальные свёрнуты. Дни, уже присутствующие в
    // dayCollapsed, сохраняют ручной выбор пользователя (переживают poll).
    const validDays = new Set(groups.map((g) => g.dayKey));
    for (const { dayKey } of groups) {
        if (!dayCollapsed.has(dayKey)) {
            dayCollapsed.set(dayKey, dayKey !== today);
        }
    }
    // Чистим состояние дней, которых больше нет в истории.
    for (const dayKey of dayCollapsed.keys()) {
        if (!validDays.has(dayKey)) dayCollapsed.delete(dayKey);
    }

    for (const { dayKey, recs } of groups) {
        $homeCards.appendChild(buildDayGroup(dayKey, recs));
    }
    updateSelectionUI();
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
 * Восстанавливает набор записей истории пачкой. Для каждой делает
 * POST /api/sessions/history/restore {name, path}; 409 (имя занято) считается
 * мягкой ошибкой и не прерывает цикл. Возвращает `{ firstOk, failed }`:
 * `firstOk` — имя первой успешно восстановленной сессии (или null),
 * `failed` — имена сессий, восстановить которые не удалось (не считая 409).
 * НЕ трогает UI (fetchSessions/openSession/alert) — это делает вызывающий.
 */
async function restoreRecords(recs) {
    let firstOk = null;
    const failed = [];

    for (const rec of recs) {
        try {
            const resp = await apiFetch('/api/sessions/history/restore', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ name: rec.name, path: rec.path }),
            });
            if (resp.ok) {
                if (!firstOk) firstOk = rec.name;
            } else if (resp.status !== 409) {
                failed.push(rec.name);
            }
        } catch (e) {
            failed.push(rec.name);
        }
    }

    return { firstOk, failed };
}

/**
 * Восстанавливает только выбранные кликом сессии. 409 (имя занято) — мягкая
 * ошибка, не прерывает цикл. После — обновляет список активных сессий и
 * открывает первую успешно восстановленную.
 */
export async function restoreSelected() {
    if (selected.size === 0) return;

    const { firstOk, failed } = await restoreRecords(Array.from(selected.values()));

    selected.clear();
    updateSelectionUI();
    await fetchSessions();
    if (firstOk) openSession(firstOk);
    if (failed.length) {
        window.alert('Не удалось восстановить: ' + failed.join(', '));
    }
}

/**
 * Восстанавливает все сессии одного дня (группы). Принимает записи группы
 * напрямую (см. buildDayGroup). 409 (имя занято) — мягкая ошибка. После —
 * обновляет список активных сессий и открывает первую успешно восстановленную.
 */
export async function restoreDay(recs) {
    if (!Array.isArray(recs) || recs.length === 0) return;

    const { firstOk, failed } = await restoreRecords(recs);

    await fetchSessions();
    if (firstOk) openSession(firstOk);
    if (failed.length) {
        window.alert('Не удалось восстановить: ' + failed.join(', '));
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
