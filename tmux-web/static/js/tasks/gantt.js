// tmux-web — Gantt timeline (gantt-диаграмма задач + git-коммиты)
//
// Рендерит под канбан-доской вкладки Tasks горизонтальную диаграмму:
//   - по одной строке (.gantt-row) на каждую задачу в статусе
//     in_progress | closed, попавшую во временно́е окно;
//   - вертикальные черты git-коммитов (.gantt-commit) поверх дорожек;
//   - верхнюю ось дней (.gantt-axis с засечками .gantt-tick).
//
// Чистый DOM (document.createElement) + CSS из css/tasks.css, без чарт-
// библиотек. Состояние берётся из core/state.js: state.tasksData.issues
// (задачи), state.gitCommits (коммиты), state.ganttRange (домен:
//   number N (дней) | 'all' | 'today' | 'yesterday').
//
// ОГРАНИЧЕНИЕ: момент «задача начата» = created_at. beads не хранит время
// перехода open → in_progress, поэтому левая граница полосы всегда привязана
// к дате создания задачи. Это фиксированное допущение, не источник истины
// о реальном старте работы.
//
// ЕДИНИЦЫ ВРЕМЕНИ: домен [t0,t1] и границы полос — в миллисекундах
// (Date.now() / Date.parse()). Поле commit.ts из GET /api/git/commits —
// committer date в СЕКУНДАХ Unix, поэтому для сравнения с доменом
// умножается на 1000.

import { state } from '../core/state.js';
import { $ganttCanvas, $ganttRange } from '../core/dom.js';
import { sessionCwdOrNull } from '../ws/tasks-ws.js';

const DAY_MS = 86400e3;
// Целевое число засечек оси при широком окне ('all' / 30д), чтобы не плодить
// сотни дневных меток. При узком окне (7д) рисуем по дню.
const MAX_TICKS = 12;
const MIN_TICKS = 6;
// Максимальная длина укороченного title в подписи строки.
const LABEL_TITLE_MAX = 40;

// Парсит ISO-дату в мс; null/невалидное → NaN (caller проверяет).
function parseTs(iso) {
    if (!iso) return NaN;
    return Date.parse(iso);
}

// Кламп значения в [lo, hi].
function clamp(v, lo, hi) {
    return v < lo ? lo : (v > hi ? hi : v);
}

// Человекочитаемая дата для tooltip'ов/оси.
function fmtDate(ms) {
    try {
        return new Date(ms).toLocaleString();
    } catch (_) {
        return String(ms);
    }
}

function fmtAxisDate(ms) {
    try {
        const d = new Date(ms);
        return d.toLocaleDateString(undefined, { day: '2-digit', month: '2-digit' });
    } catch (_) {
        return '';
    }
}

// Время HH:MM для засечек оси при узких (дневных) диапазонах.
function fmtAxisTime(ms) {
    try {
        return new Date(ms).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
    } catch (_) {
        return '';
    }
}

// Укорачивает title до LABEL_TITLE_MAX с эллипсисом.
function shortTitle(title) {
    const t = String(title || '');
    return t.length > LABEL_TITLE_MAX ? t.slice(0, LABEL_TITLE_MAX - 1) + '…' : t;
}

// Человекочитаемая длительность из миллисекунд: «3д 4ч», «2ч 15м», «45м»,
// «30с». Берутся максимум две старшие ненулевые единицы. <1с → «0с».
function fmtDuration(ms) {
    let s = Math.max(0, Math.round(Number(ms) / 1000));
    if (!Number.isFinite(s) || s < 1) return '0с';
    const d = Math.floor(s / 86400); s -= d * 86400;
    const h = Math.floor(s / 3600); s -= h * 3600;
    const m = Math.floor(s / 60); s -= m * 60;
    const parts = [];
    if (d) parts.push(d + 'д');
    if (h) parts.push(h + 'ч');
    if (m) parts.push(m + 'м');
    if (s && parts.length === 0) parts.push(s + 'с');
    return parts.slice(0, 2).join(' ') || '0с';
}

// Ключи развёрнутых групп (hash коммита либо OPEN_GROUP_KEY). Пустой Set →
// все группы свёрнуты (поведение по умолчанию). Сохраняется между renderGantt().
const expandedGroups = new Set();
// Ключ хвостовой группы задач без последующего коммита.
const OPEN_GROUP_KEY = '__open__';

// Строит верхнюю ось с равномерными засечками дат.
// При узком окне (<= MAX_TICKS дней) — по дню; иначе ~MAX_TICKS равномерных меток.
function buildAxis(t0, t1) {
    const axis = document.createElement('div');
    axis.className = 'gantt-axis';

    const span = t1 - t0;
    if (span <= 0) return axis;

    const days = span / DAY_MS;
    let ticks;
    if (days <= MAX_TICKS) {
        ticks = Math.max(MIN_TICKS, Math.ceil(days));
    } else {
        ticks = MAX_TICKS;
    }
    // Узкое окно (дневные диапазоны 'today'/'yesterday', span <= 2 суток) —
    // подписи времени HH:MM; широкое (7д/30д/'all') — даты как прежде.
    const fmtTick = (span <= 2 * DAY_MS) ? fmtAxisTime : fmtAxisDate;
    // Равномерные метки: ticks интервалов → ticks+1 границ, но крайнюю правую
    // (100%) опускаем, чтобы подпись не вылезала за край канваса.
    for (let i = 0; i < ticks; i++) {
        const frac = i / ticks;
        const ms = t0 + frac * span;
        const tick = document.createElement('div');
        tick.className = 'gantt-tick';
        tick.style.left = (frac * 100) + '%';
        tick.textContent = fmtTick(ms);
        axis.appendChild(tick);
    }
    return axis;
}

// Вычисляет временно́е окно диаграммы по state.ganttRange.
// Возвращает { t0, t1, since, until }, где:
//   - t0, t1 — границы домена в МИЛЛИСЕКУНДАХ (для рендера полос/оси);
//   - since, until — те же границы в СЕКУНДАХ Unix для запроса коммитов
//     (GET /api/git/commits?since=&until=); null означает «не ограничивать».
// rows — отфильтрованные строки задач ({ start } в мс), нужны только для 'all'
// (t0 = минимальный start). Для 'all' при пустом rows → t0 = null (сигнал
// caller'у показать пустое состояние).
// Семантика диапазонов:
//   'today'     — от начала сегодняшних локальных суток до now; until=null.
//   'yesterday' — от начала вчерашних суток до начала сегодняшних; until задан.
//   number N    — последние N дней до now; until=null.
//   'all'       — от самой ранней задачи до now; since=null, until=null.
export function ganttWindow(rows) {
    const range = state.ganttRange;
    const now = Date.now();

    if (range === 'today') {
        const start = new Date();
        start.setHours(0, 0, 0, 0);
        const t0 = start.getTime();
        return { t0, t1: now, since: Math.floor(t0 / 1000), until: null };
    }

    if (range === 'yesterday') {
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        const t1 = today.getTime();
        const t0 = t1 - DAY_MS;
        return {
            t0,
            t1,
            since: Math.floor(t0 / 1000),
            until: Math.floor(t1 / 1000),
        };
    }

    if (range === 'all') {
        const list = Array.isArray(rows) ? rows : [];
        if (list.length === 0) {
            return { t0: null, t1: now, since: null, until: null };
        }
        const t0 = list.reduce((min, r) => (r.start < min ? r.start : min), list[0].start);
        return { t0, t1: now, since: null, until: null };
    }

    // number N (дней); невалидное → 7 дней по умолчанию.
    const days = Number.isFinite(Number(range)) ? Number(range) : 7;
    const t0 = now - days * DAY_MS;
    return { t0, t1: now, since: Math.floor(t0 / 1000), until: null };
}

// Основной рендер. Чистит $ganttCanvas и перерисовывает ось/строки/коммиты.
// Guard: если канваса нет в DOM — тихо выходит.
export function renderGantt() {
    const canvas = $ganttCanvas;
    if (!canvas) return;

    canvas.innerHTML = '';

    const data = state.tasksData || { issues: [] };
    const issues = Array.isArray(data.issues) ? data.issues : [];

    // Фильтр по статусу (lowercase) + наличие валидной created_at.
    const rows = [];
    for (const issue of issues) {
        if (!issue) continue;
        const status = String(issue.status || '').toLowerCase();
        if (status !== 'in_progress' && status !== 'closed') continue;
        const start = parseTs(issue.created_at);
        if (Number.isNaN(start)) continue;
        rows.push({ issue, status, start });
    }

    // Домен [t0, t1] в мс из единого хелпера. Для 'all' t0 — минимальный
    // start задачи; t0===null означает «нет данных» (пустой rows в 'all').
    const win = ganttWindow(rows);
    const t0 = win.t0;
    const t1 = win.t1;

    if (t0 === null) {
        renderEmpty(canvas);
        return;
    }
    if (!(t1 > t0)) {
        // Вырожденный домен — нечего рисовать.
        renderEmpty(canvas);
        return;
    }
    const span = t1 - t0;

    // Ось — прямой потомок #gantt-canvas (sticky к верху при прокрутке).
    canvas.appendChild(buildAxis(t0, t1));

    // Контейнер строк: скроллится вместе с #gantt-canvas, служит relative-
    // контекстом для overlay коммитов (overlay покрывает полную высоту строк).
    const rowsEl = document.createElement('div');
    rowsEl.className = 'gantt-rows';
    canvas.appendChild(rowsEl);

    // Вычисляем end каждой полосы, отбрасываем целиком выпавшие из окна,
    // клампим границы и сортируем по created_at (возр.).
    const visible = [];
    for (const r of rows) {
        const end = (r.status === 'closed')
            ? parseTs(r.issue.closed_at)
            : t1;
        const endMs = Number.isNaN(end) ? t1 : end;
        // Целиком вне окна.
        if (endMs < t0 || r.start > t1) continue;
        const left = clamp(r.start, t0, t1);
        const right = clamp(endMs, t0, t1);
        visible.push({ ...r, end: endMs, left, right });
    }
    visible.sort((a, b) => a.start - b.start);

    if (visible.length === 0) {
        // Нет задач в окне — всё равно покажем ось + заглушку под ней,
        // чтобы пользователь видел диапазон. Заглушка и overlay коммитов
        // живут в .gantt-rows (overlay относительно него).
        const empty = document.createElement('div');
        empty.className = 'gantt-empty';
        empty.textContent = 'Нет задач in_progress/closed в выбранном диапазоне';
        rowsEl.appendChild(empty);
        renderCommits(rowsEl, t0, span);
        return;
    }

    // Группируем задачи по «закрывающему» коммиту и рисуем группы (свёрнутые
    // по умолчанию). Развёрнутые группы раскрываются в отдельные строки задач.
    const groups = groupByCommit(visible, t0, t1);
    for (const g of groups) {
        renderGroupHeader(rowsEl, g, t0, span);
        if (expandedGroups.has(g.key)) {
            for (const v of g.tasks) renderTaskRow(rowsEl, v, t0, span, true);
        }
    }

    // Коммиты — вертикальные черты поверх дорожек (overlay-слой внутри
    // .gantt-rows, чтобы черты шли до самого низа всех строк).
    renderCommits(rowsEl, t0, span);
}

// Рисует одну строку задачи. grouped=true добавляет отступ (.is-grouped) для
// визуального вложения под заголовок группы.
function renderTaskRow(rowsEl, v, t0, span, grouped) {
    const row = document.createElement('div');
    row.className = grouped ? 'gantt-row is-grouped' : 'gantt-row';

    const label = document.createElement('div');
    label.className = 'gantt-row-label';
    const id = v.issue.id || '';
    label.textContent = id + ' ' + shortTitle(v.issue.title);
    label.title = id + ' ' + String(v.issue.title || '');
    row.appendChild(label);

    const leftPct = ((v.left - t0) / span) * 100;
    const widthPct = ((v.right - v.left) / span) * 100;

    const bar = document.createElement('div');
    bar.className = 'gantt-bar status-' + v.status;
    bar.style.left = leftPct + '%';
    bar.style.width = widthPct + '%';

    const endText = (v.status === 'closed') ? fmtDate(v.end) : 'в работе';
    bar.title = id + ' · ' + fmtDate(v.start) + ' → ' + endText;
    row.appendChild(bar);

    rowsEl.appendChild(row);
}

// Группирует видимые задачи по «закрывающему» коммиту: группа задачи = первый
// коммит с ts >= anchor (anchor = closed_at для closed, иначе t1). Задачи без
// последующего коммита уходят в хвостовую группу OPEN_GROUP_KEY. Возвращает
// массив групп в хронологическом порядке (по ts коммита), хвостовая — последней.
//   commit.ts в СЕКУНДАХ → *1000 для сравнения с anchor в мс.
function groupByCommit(visible, t0, t1) {
    const commits = (Array.isArray(state.gitCommits) ? state.gitCommits : [])
        .filter((c) => c && Number.isFinite(Number(c.ts)))
        .map((c) => ({ ...c, tsMs: Number(c.ts) * 1000 }))
        .sort((a, b) => a.tsMs - b.tsMs);

    const byKey = new Map();
    const order = [];
    const groupFor = (key, commit) => {
        let g = byKey.get(key);
        if (!g) {
            g = { key, commit: commit || null, tasks: [] };
            byKey.set(key, g);
            order.push(g);
        }
        return g;
    };

    for (const v of visible) {
        const anchor = (v.status === 'closed') ? v.end : t1;
        const commit = commits.find((c) => c.tsMs >= anchor) || null;
        const key = commit ? String(commit.hash || OPEN_GROUP_KEY) : OPEN_GROUP_KEY;
        groupFor(key, commit).tasks.push(v);
    }

    // Сортировка групп: по ts коммита по возрастанию, хвостовая (нет коммита) —
    // в конец.
    order.sort((a, b) => {
        const at = a.commit ? a.commit.tsMs : Infinity;
        const bt = b.commit ? b.commit.tsMs : Infinity;
        return at - bt;
    });

    // Сводные метрики каждой группы.
    for (const g of order) {
        let gStart = Infinity;
        let gEnd = -Infinity;
        let hasOngoing = false;
        for (const v of g.tasks) {
            if (v.start < gStart) gStart = v.start;
            if (v.status !== 'closed') { hasOngoing = true; }
            const e = (v.status === 'closed') ? v.end : t1;
            if (e > gEnd) gEnd = e;
        }
        g.gStart = gStart;
        g.gEnd = hasOngoing ? Math.max(gEnd, t1) : gEnd;
        g.hasOngoing = hasOngoing;
        g.totalMs = Math.max(0, g.gEnd - g.gStart);
    }

    return order;
}

// Рисует строку-заголовок группы: каретка ▶/▼, метка (subject коммита или
// «Без коммита»), summary-бар [gStart,gEnd] и бейдж длительности. Клик по
// заголовку переключает свёрнутость; наведение на бейдж — попап разбивки.
function renderGroupHeader(rowsEl, group, t0, span) {
    const expanded = expandedGroups.has(group.key);
    const row = document.createElement('div');
    row.className = 'gantt-group' + (expanded ? ' is-expanded' : '');

    const label = document.createElement('div');
    label.className = 'gantt-group-label';

    const caret = document.createElement('span');
    caret.className = 'gantt-group-caret';
    caret.textContent = expanded ? '▼' : '▶';
    label.appendChild(caret);

    const subject = group.commit
        ? String(group.commit.subject || group.commit.hash || '')
        : 'Без коммита / в работе';
    const text = document.createElement('span');
    text.className = 'gantt-group-title';
    text.textContent = shortTitle(subject) + ' · ' + group.tasks.length;
    text.title = subject;
    label.appendChild(text);
    row.appendChild(label);

    // Summary-бар группы.
    const left = clamp(group.gStart, t0, t0 + span);
    const right = clamp(group.gEnd, t0, t0 + span);
    const bar = document.createElement('div');
    bar.className = 'gantt-group-bar' + (group.hasOngoing ? ' is-ongoing' : '');
    bar.style.left = (((left - t0) / span) * 100) + '%';
    bar.style.width = (((right - left) / span) * 100) + '%';
    row.appendChild(bar);

    // Бейдж длительности (hover → попап разбивки).
    const dur = document.createElement('div');
    dur.className = 'gantt-group-duration';
    dur.textContent = fmtDuration(group.totalMs) + (group.hasOngoing ? '+' : '');
    dur.title = fmtDate(group.gStart) + ' → '
        + (group.hasOngoing ? 'в работе' : fmtDate(group.gEnd));
    attachGroupHover(dur, group);
    row.appendChild(dur);

    // Клик по заголовку (кроме бейджа длительности) — toggle свёрнутости.
    label.addEventListener('click', () => {
        if (expandedGroups.has(group.key)) expandedGroups.delete(group.key);
        else expandedGroups.add(group.key);
        renderGantt();
    });

    rowsEl.appendChild(row);
}

// Пустое состояние: только ось не строим (домен может быть вырожден) —
// показываем текстовую заглушку.
function renderEmpty(canvas) {
    const empty = document.createElement('div');
    empty.className = 'gantt-empty';
    empty.textContent = 'Нет задач in_progress/closed';
    canvas.appendChild(empty);
}

// Overlay-слой коммитов: один абсолютный контейнер во всю высоту контейнера
// строк (.gantt-rows), внутри — вертикальные черты .gantt-commit на позициях
// из state.gitCommits. rowsEl — relative-родитель (.gantt-rows): overlay с
// top/bottom:0 покрывает полную высоту всех строк, поэтому черты доходят до
// самого низа даже при прокрутке #gantt-canvas.
// commit.ts в СЕКУНДАХ → *1000 для сравнения с доменом в мс. Вне окна — skip.
function renderCommits(rowsEl, t0, span) {
    const commits = Array.isArray(state.gitCommits) ? state.gitCommits : [];
    if (commits.length === 0) return;

    const t1 = t0 + span;
    const overlay = document.createElement('div');
    overlay.className = 'gantt-commits-overlay';
    overlay.style.position = 'absolute';
    overlay.style.left = '0';
    overlay.style.top = '0';
    overlay.style.right = '0';
    overlay.style.bottom = '0';
    overlay.style.pointerEvents = 'none';

    for (const c of commits) {
        if (!c) continue;
        const tsMs = Number(c.ts) * 1000;
        if (!Number.isFinite(tsMs)) continue;
        if (tsMs < t0 || tsMs > t1) continue;
        const leftPct = ((tsMs - t0) / span) * 100;
        const line = document.createElement('div');
        line.className = 'gantt-commit';
        line.style.position = 'absolute';
        line.style.left = leftPct + '%';
        line.style.pointerEvents = 'auto';
        const hash = String(c.hash || '');
        const subject = String(c.subject || '');
        line.dataset.hash = hash;
        line.dataset.subject = subject;
        // Нативный title — мгновенный fallback, пока грузится кастомный попап.
        line.title = hash.slice(0, 7) + ' ' + subject;
        attachCommitHover(line);
        overlay.appendChild(line);
    }

    rowsEl.appendChild(overlay);
}

// ---- Hover-попап деталей коммита (.gantt-commit-popover) ----
//
// Один shared попап-элемент на всю страницу: ленивая инициализация, аппендится
// в document.body, переиспользуется для всех черт. Детали коммита кэшируются
// по полному hash (включая null/ошибку — чтобы не повторять fetch).

// hash -> commit detail | null (закэшированный «нет данных»/ошибка).
const detailCache = new Map();
// Задержка перед показом попапа (мс) и грейс-период перед скрытием (мс).
const POPOVER_SHOW_DELAY = 120;
const POPOVER_HIDE_DELAY = 150;
// Отступ попапа от черты коммита и от краёв вьюпорта (px).
const POPOVER_GAP = 10;
const POPOVER_MARGIN = 8;

let popoverEl = null;       // shared DOM-элемент попапа
let showTimer = null;       // таймер показа (mouseenter на черте)
let hideTimer = null;       // таймер скрытия (mouseleave с грейсом)
let popoverToken = 0;       // монотонный счётчик для отмены устаревших fetch

// Лениво создаёт shared попап-элемент и вешает на него hover-листенеры
// (наведение на сам попап отменяет скрытие).
function ensurePopover() {
    if (popoverEl) return popoverEl;
    const el = document.createElement('div');
    el.className = 'gantt-commit-popover';
    el.hidden = true;
    el.addEventListener('mouseenter', () => {
        if (hideTimer !== null) {
            clearTimeout(hideTimer);
            hideTimer = null;
        }
    });
    el.addEventListener('mouseleave', () => scheduleHide());
    document.body.appendChild(el);
    popoverEl = el;
    return el;
}

// Скрывает попап (с грейс-периодом). Повторный hover отменяет таймер.
function scheduleHide() {
    if (hideTimer !== null) clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
        hideTimer = null;
        if (popoverEl) popoverEl.hidden = true;
    }, POPOVER_HIDE_DELAY);
}

// Навешивает hover-обработчики на одну черту коммита.
function attachCommitHover(line) {
    line.addEventListener('mouseenter', () => {
        if (hideTimer !== null) {
            clearTimeout(hideTimer);
            hideTimer = null;
        }
        if (showTimer !== null) clearTimeout(showTimer);
        showTimer = setTimeout(() => {
            showTimer = null;
            openPopoverFor(line);
        }, POPOVER_SHOW_DELAY);
    });
    line.addEventListener('mouseleave', () => {
        if (showTimer !== null) {
            clearTimeout(showTimer);
            showTimer = null;
        }
        scheduleHide();
    });
}

// ---- Hover-попап разбивки группы по задачам ----
// Переиспользует shared-попап (ensurePopover/positionPopover/scheduleHide) и
// те же таймеры показа/скрытия, что и попап коммита.

// Навешивает hover на бейдж длительности группы → попап со списком задач.
function attachGroupHover(el, group) {
    el.addEventListener('mouseenter', () => {
        if (hideTimer !== null) { clearTimeout(hideTimer); hideTimer = null; }
        if (showTimer !== null) clearTimeout(showTimer);
        showTimer = setTimeout(() => {
            showTimer = null;
            renderGroupPopover(group);
            positionPopover(el);
        }, POPOVER_SHOW_DELAY);
    });
    el.addEventListener('mouseleave', () => {
        if (showTimer !== null) { clearTimeout(showTimer); showTimer = null; }
        scheduleHide();
    });
}

// Рендерит содержимое попапа группы: шапка (subject коммита + общая длительность
// + диапазон дат) и список задач (id, title, полное описание ≤800 симв.,
// длительность). Всё через textContent (без innerHTML).
function renderGroupPopover(group) {
    const el = ensurePopover();
    el.innerHTML = '';

    const head = document.createElement('div');
    head.className = 'gantt-popover-head';
    const titleSpan = document.createElement('span');
    titleSpan.className = 'gantt-popover-subject';
    titleSpan.textContent = group.commit
        ? String(group.commit.subject || group.commit.hash || '')
        : 'Без коммита / в работе';
    head.appendChild(titleSpan);
    el.appendChild(head);

    const meta = document.createElement('div');
    meta.className = 'gantt-popover-date';
    meta.textContent = fmtDuration(group.totalMs) + (group.hasOngoing ? '+' : '')
        + ' · ' + fmtDate(group.gStart) + ' → '
        + (group.hasOngoing ? 'в работе' : fmtDate(group.gEnd))
        + ' · задач: ' + group.tasks.length;
    el.appendChild(meta);

    const list = document.createElement('div');
    list.className = 'gantt-group-tasks';
    for (const v of group.tasks) {
        const item = document.createElement('div');
        item.className = 'gantt-group-task';

        const top = document.createElement('div');
        top.className = 'gantt-group-task-head';
        const id = document.createElement('span');
        id.className = 'gantt-group-task-id';
        id.textContent = String(v.issue.id || '');
        top.appendChild(id);
        const durMs = (v.status === 'closed') ? (v.end - v.start) : NaN;
        const dur = document.createElement('span');
        dur.className = 'gantt-group-task-dur';
        dur.textContent = (v.status === 'closed') ? fmtDuration(durMs) : 'в работе';
        top.appendChild(dur);
        item.appendChild(top);

        const title = document.createElement('div');
        title.className = 'gantt-group-task-title';
        title.textContent = String(v.issue.title || '');
        item.appendChild(title);

        const descRaw = String(v.issue.description || '').trim();
        if (descRaw) {
            const desc = document.createElement('pre');
            desc.className = 'gantt-group-task-desc';
            desc.textContent = descRaw.length > 800 ? descRaw.slice(0, 799) + '…' : descRaw;
            item.appendChild(desc);
        }

        list.appendChild(item);
    }
    el.appendChild(list);
}

// Показывает попап для черты: из кэша мгновенно, иначе fetch детали.
function openPopoverFor(line) {
    const hash = String(line.dataset.hash || '');
    const subject = String(line.dataset.subject || '');
    if (!hash) return;

    if (detailCache.has(hash)) {
        renderPopover(detailCache.get(hash), hash, subject);
        positionPopover(line);
        return;
    }

    // Помечаем токеном: если за время fetch пользователь навёлся на другую
    // черту, устаревший ответ не перерисует попап.
    const token = ++popoverToken;
    // Сразу покажем минимальный fallback, пока грузятся детали.
    renderPopover(null, hash, subject);
    positionPopover(line);

    fetchCommitDetail(hash).then((detail) => {
        detailCache.set(hash, detail);
        if (token !== popoverToken) return;
        if (popoverEl && popoverEl.hidden) return;
        renderPopover(detail, hash, subject);
        positionPopover(line);
    }).catch(() => {
        detailCache.set(hash, null);
    });
}

// Загружает детали коммита. cwd из sessionCwdOrNull() (если null — без path).
// Возвращает json.commit (объект или null). Сетевые/non-ok → null.
async function fetchCommitDetail(hash) {
    try {
        const cwd = sessionCwdOrNull();
        const params = [];
        if (cwd) params.push('path=' + encodeURIComponent(cwd));
        params.push('hash=' + encodeURIComponent(hash));
        const url = '/api/git/commit?' + params.join('&');
        const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/git/commit failed:', r.status);
            return null;
        }
        const json = await r.json();
        return (json && typeof json.commit === 'object') ? json.commit : null;
    } catch (e) {
        console.warn('fetchCommitDetail failed', e);
        return null;
    }
}

// Рендерит содержимое попапа. detail===null → минимальный fallback
// (hash7 + subject из dataset). Всё через textContent (без innerHTML).
function renderPopover(detail, hash, fallbackSubject) {
    const el = ensurePopover();
    el.innerHTML = '';

    const head = document.createElement('div');
    head.className = 'gantt-popover-head';
    const hashSpan = document.createElement('span');
    hashSpan.className = 'gantt-popover-hash';
    hashSpan.textContent = hash.slice(0, 7);
    head.appendChild(hashSpan);
    if (detail && Number.isFinite(Number(detail.ts))) {
        const dateSpan = document.createElement('span');
        dateSpan.className = 'gantt-popover-date';
        dateSpan.textContent = fmtDate(Number(detail.ts) * 1000);
        head.appendChild(dateSpan);
    }
    el.appendChild(head);

    if (detail && detail.author) {
        const author = document.createElement('div');
        author.className = 'gantt-popover-author';
        author.textContent = String(detail.author);
        el.appendChild(author);
    }

    const subjText = (detail && detail.subject) ? String(detail.subject) : String(fallbackSubject || '');
    if (subjText) {
        const subj = document.createElement('div');
        subj.className = 'gantt-popover-subject';
        subj.textContent = subjText;
        el.appendChild(subj);
    }

    if (detail && detail.body && String(detail.body).trim()) {
        const body = document.createElement('pre');
        body.className = 'gantt-popover-body';
        let txt = String(detail.body).trim();
        if (txt.length > 800) txt = txt.slice(0, 799) + '…';
        body.textContent = txt;
        el.appendChild(body);
    }

    const files = (detail && Array.isArray(detail.files)) ? detail.files : [];
    if (files.length > 0) {
        const list = document.createElement('div');
        list.className = 'gantt-commit-files';
        for (const f of files) {
            if (!f) continue;
            const row = document.createElement('div');
            row.className = 'gantt-file';
            const badge = document.createElement('span');
            const st = String(f.status || '').trim();
            const letter = st ? st[0].toUpperCase() : '?';
            badge.className = 'gantt-file-status status-' + letter;
            badge.textContent = st || '?';
            const path = document.createElement('span');
            path.className = 'gantt-file-path';
            path.textContent = String(f.path || '');
            row.appendChild(badge);
            row.appendChild(path);
            list.appendChild(row);
        }
        el.appendChild(list);
    }
}

// Позиционирует попап у черты коммита, не вылезая за вьюпорт.
// fixed-координаты относительно вьюпорта; клампим по window.innerWidth/Height.
function positionPopover(line) {
    const el = ensurePopover();
    el.hidden = false;
    el.style.position = 'fixed';
    el.style.left = '0px';
    el.style.top = '0px';

    const anchor = line.getBoundingClientRect();
    const pop = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;

    // По горизонтали: справа от черты, иначе слева, иначе клампим.
    let left = anchor.right + POPOVER_GAP;
    if (left + pop.width > vw - POPOVER_MARGIN) {
        left = anchor.left - POPOVER_GAP - pop.width;
    }
    left = clamp(left, POPOVER_MARGIN, Math.max(POPOVER_MARGIN, vw - pop.width - POPOVER_MARGIN));

    // По вертикали: выравниваем по верху черты, клампим в вьюпорт.
    let top = anchor.top;
    top = clamp(top, POPOVER_MARGIN, Math.max(POPOVER_MARGIN, vh - pop.height - POPOVER_MARGIN));

    el.style.left = left + 'px';
    el.style.top = top + 'px';
}

// ---- Загрузка коммитов и управление переключателем диапазона ----

// Загружает git-коммиты корня текущей сессии для активного диапазона.
// cwd = sessionCwdOrNull(); since/until берутся из ganttWindow() (тот же
// хелпер, что и у renderGantt), оба в секундах Unix или null:
//   - since=null  → нижняя граница не ограничена ('all');
//   - until=null  → верхняя граница не ограничена (now; 'today'/N дней);
//   - 'yesterday' задаёт оба, ограничивая окно ровно вчерашними сутками.
// rows для since/until не нужны (для 'all' оба null независимо от задач).
// URL: /api/git/commits?path=<enc cwd>[&since=<unix>][&until=<unix>]; если
// cwd null — без path. При ok → state.gitCommits = json.commits||[]; при
// ошибке/non-ok → [] + warn. В конце всегда вызывает renderGantt().
// Epoch-счётчик для защиты от stale-response гонок: при быстром переключении
// сессии/диапазона запросы перекрываются, и поздний ответ старого запроса мог
// перезатереть state.gitCommits данными не той сессии/окна. Запоминаем epoch и
// ключ (cwd|since|until) на входе и сверяем перед записью в state.
let _gitCommitsEpoch = 0;
export async function fetchGitCommits() {
    const epoch = ++_gitCommitsEpoch;
    const cwd = sessionCwdOrNull();
    const win = ganttWindow(null);
    const reqKey = `${cwd || ''}|${win.since}|${win.until}`;
    try {
        const params = [];
        if (cwd) params.push('path=' + encodeURIComponent(cwd));
        if (win.since !== null) params.push('since=' + win.since);
        if (win.until !== null) params.push('until=' + win.until);
        const qs = params.length ? '?' + params.join('&') : '';
        const url = '/api/git/commits' + qs;
        const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
        // Пока ждали ответ, мог уйти более свежий запрос (или сменился cwd/окно).
        // Тогда наш ответ устарел — молча выходим, не трогая state/рендер.
        if (epoch !== _gitCommitsEpoch || reqKey !== `${sessionCwdOrNull() || ''}|${ganttWindow(null).since}|${ganttWindow(null).until}`) {
            return;
        }
        if (!r.ok) {
            console.warn('GET /api/git/commits failed:', r.status);
            state.gitCommits = [];
        } else {
            const json = await r.json();
            if (epoch !== _gitCommitsEpoch) return; // повторная сверка после await json()
            state.gitCommits = Array.isArray(json && json.commits) ? json.commits : [];
        }
    } catch (e) {
        if (epoch !== _gitCommitsEpoch) return;
        console.warn('fetchGitCommits failed', e);
        state.gitCommits = [];
    }
    if (epoch !== _gitCommitsEpoch) return;
    renderGantt();
}

// Навешивает обработчики на кнопки переключения диапазона #gantt-range.
// Идемпотентно: повторный вызов не вешает дублирующих листенеров (guard
// через dataset-флаг на самом контейнере).
// По клику: state.ganttRange = именованный диапазон ('all'|'today'|'yesterday')
// как есть, иначе Number(data-range) (число дней); переключение класса .active,
// затем fetchGitCommits() (since/until изменились → перезагрузка + рендер).
export function initGanttControls() {
    const root = $ganttRange;
    if (!root) return;
    if (root.dataset.ganttBound === '1') return;
    root.dataset.ganttBound = '1';

    const buttons = root.querySelectorAll('button[data-range]');
    buttons.forEach((btn) => {
        btn.addEventListener('click', () => {
            const ds = btn.dataset.range;
            state.ganttRange = (ds === 'all' || ds === 'today' || ds === 'yesterday')
                ? ds
                : Number(ds);
            buttons.forEach((b) => b.classList.toggle('active', b === btn));
            fetchGitCommits();
        });
    });
}
