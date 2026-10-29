// tmux-web — плагин forge-tracker: каркас панели «Мои задачи».
//
// ДВЕ ТОЧКИ ВХОДА, как у Telegram / VK / Битрикса:
//
//   initTrackerGate()  ← js/tracker/gate.js, зовётся из js/core/bootstrap.js
//                        ОДИН РАЗ на загрузке страницы. Живёт в отдельном
//                        крошечном модуле НАМЕРЕННО: гейт нужен всегда, а этот
//                        файл со всей панелью — только тем, кто вкладку
//                        откроет. Сокет в гейте НЕ поднимается — см. ниже.
//   initTracker()      ← js/tabs/tabs.js, на КАЖДОМ входе на вкладку; сам
//                        модуль подгружается динамическим import'ом при первом
//                        открытии. Идемпотентна (флаг local.wired): слушатели
//                        навешивает ровно один раз, а состояние перечитывает
//                        всегда.
//
// ПОЧЕМУ СОКЕТ ПОДНИМАЕТСЯ ПРИ ОТКРЫТИИ ВКЛАДКИ, А НЕ В ГЕЙТЕ. У мессенджеров
// наоборот: их WS постоянный, потому что бейдж непрочитанного и тосты обязаны
// работать с любой вкладки. Трекеру считать нечего — задачи не приходят
// извне, их заводит сам пользователь, и пока панель закрыта, сообщать не о
// чем. Держать ради этого открытый сокет и polling-фолбэк на каждой загрузке
// devforge — плата без выгоды.
//
// ФИЛЬТР ЖИВЁТ В ОДНОМ МЕСТЕ. Директория (`~/today` и соседи), выбранный
// список и выбранный тег — это ТРИ независимых координаты одной выборки, и
// хранятся они здесь, а режим (list · kanban · gantt · report) их только
// отрисовывает по-своему. Поэтому переключение режима фильтр не сбрасывает —
// это отдельный критерий приёмки.
//
// ЖИВОЕ ВРЕМЯ ОБНОВЛЯЕТСЯ ТЕКСТОМ, А НЕ ПЕРЕРИСОВКОЙ. Раз в секунду тикер
// (js/tracker/timer.js) зовёт `tick`, и тот переписывает содержимое считанного
// числа узлов: значения в блоке `// RUNNING`, колонку времени у задач с идущим
// таймером, суммы `TODAY`/`WEEK` в статус-баре и живые числа открытой карточки.
// Полная отрисовка раз в секунду стоила бы прокрутки ленты, начатого
// drag'n'drop и фокуса в полях карточки.
//
// РЕЖИМЫ ВКЛЮЧАЮТСЯ ПО ОДНОМУ, И СПИСОК ВКЛЮЧЁННЫХ ЖИВЁТ ЗДЕСЬ (`MODES`).
// Кнопка невключённого режима `disabled` в разметке, а сохранённый в
// localStorage режим применяется, только если он есть в `MODES`: пустой экран
// за живой кнопкой (или за localStorage от другой версии) хуже честного «пока
// нельзя». Сейчас работают все четыре: list, kanban, gantt и report.
//
// ПЕРЕНОС В TASKS/TODO ДЕЛАЕТ ФРОНТ, А НЕ ПЛАГИН. Кнопка `[→ tasks/todo]`
// открывает диалог выбора корня (js/tracker/to-todo.js) и оттуда зовёт
// существующий `POST /api/todos` хоста. Своих роутов для этого у forge-tracker
// нет и не будет: задача КОПИРУЕТСЯ, в трекере после переноса не меняется
// ничего.
//
// У ГАНТТА ЕСТЬ СВОЙ ЗАПРОС, У ОСТАЛЬНЫХ РЕЖИМОВ — НЕТ. Лента и доска целиком
// рисуются из снимка `/api/tracker/state`, а полосы периода снимок не несёт:
// их считает `GET /api/tracker/gantt?from&to` по своим правилам попадания в
// окно. Поэтому вход в режим, листание периода и приход нового снимка ведут к
// запросу диаграммы — см. блок «режим gantt» ниже.

import {
    $trkRunningBadge, $tracker,
    $trkStore, $trkToday, $trkReload,
    $trkDirs, $trkListsTitle, $trkLists, $trkListAdd, $trkTags,
    $trkSessionLine, $trkModes, $trkGanttNav, $trkReportNav,
    $trkRunning, $trkContent,
    $trkPrompt, $trkPromptInput, $trkPromptPreview,
    $trkConn, $trkStatusInfo,
    $trkPush, $trkPushNode, $trkPushRoots, $trkPushError,
    $trkPushClose, $trkPushCancel, $trkPushOk,
    $trkMs, $trkMsTitle, $trkMsDate, $trkMsHint, $trkMsError,
    $trkMsClose, $trkMsCancel, $trkMsDelete, $trkMsOk,
} from '../core/dom.js';
// Перенос задачи в TODO делает ФРОНТ существующим роутом хоста — крейт
// forge-tracker про TodoStore не знает. Отсюда три чужих импорта: список
// корней (сессии), корень по умолчанию и сам `POST /api/todos`.
import { state } from '../core/state.js';
import { currentTodosPath } from '../tasks/render.js';
import { createTodoForPath } from '../tasks/crud.js';
import * as api from './api.js';
import { isTrackerEnabled, trackerStatus } from './gate.js';
import { connectTrackerWs, disconnectTrackerWs } from './ws.js';
import { parsePrompt, previewOf, toCreatePayload } from './parse.js';
import { renderList, filterTasks, fmtDuration, planOrder } from './render-list.js';
import { renderKanban, COLUMNS } from './render-kanban.js';
import {
    renderGantt, buildDays, dayKey, dayStart, addDays, isOver, shortDate,
} from './render-gantt.js';
import {
    renderReport, windowFor, shiftAnchor, windowLabel, csvFileName,
    reportToCsv, isEmptyReport, fmtHm, dayKey as rDayKey, PERIODS,
} from './render-report.js';
import {
    initTrackerModals, openTaskModal, closeTaskModal, teardownTrackerModals,
    onTrackerSnapshot, paintLive,
} from './modals.js';
import {
    clockFromSnapshot, serverNow, dayRolledOver,
    liveTaskSeconds, liveItemSeconds, liveTotals,
    renderRunning, tickRunning, startTicker, stopTicker, isTicking,
} from './timer.js';
import {
    initTrackerPush, openPushDialog, closePushDialog, teardownTrackerPush,
} from './to-todo.js';
import {
    initTrackerMilestone, openMilestoneDialog, closeMilestoneDialog,
    teardownTrackerMilestone,
} from './milestone.js';

const LS_DIR = 'forge.tracker.dir';
const LS_MODE = 'forge.tracker.mode';
const LS_GSPAN = 'forge.tracker.gantt.span';
const LS_RPERIOD = 'forge.tracker.report.period';

/** Длины окна Гантта, между которыми переключает шкала в шапке. */
const GANTT_SPANS = [7, 14, 30];
/** Длина окна по умолчанию — две недели, как на макете (01.09 — 14.09). */
const GANTT_SPAN_DEFAULT = 14;

/**
 * Режимы, у которых есть отрисовка.
 *
 * Единственный источник истины на две проверки: восстановление режима из
 * localStorage и `setMode`. Кнопки остальных режимов `disabled` в index.html,
 * и снимать атрибут надо ровно тогда же, когда режим появляется здесь.
 */
const MODES = new Set(['list', 'kanban', 'gantt', 'report']);

/** Пустой снимок: панель обязана рисоваться и до первого ответа сервера. */
function emptySnapshot() {
    return {
        lists: [], tasks: [], items: [], milestones: [], entries: [], active: [],
        task_seconds: {}, item_seconds: {}, progress: {},
        totals: { today_seconds: 0, week_seconds: 0 },
        now: new Date().toISOString(),
    };
}

const local = {
    /** Слушатели панели навешены. */
    wired: false,
    /** Последний снимок состояния. */
    snap: emptySnapshot(),
    /** Активная директория: today | week | inbox | all. */
    dir: 'today',
    /** Фильтр по списку (id) или null. */
    listId: null,
    /** Фильтр по тегу или null. */
    tag: null,
    /** Режим отображения. */
    mode: 'list',
    /** Сокет открыт. */
    wsOpen: false,
    /** Таймер временного сообщения в статус-баре. */
    noteTimer: null,
    /** Опора времени: серверное `now` снимка и момент его получения. */
    clock: null,
    /** Состав идущих отрезков, по которому нарисован блок `// RUNNING`. */
    runningKey: '',
    /** Запрос свежего снимка после полуночи уже отправлен. */
    rolloverAsked: false,
    /** Карточку доски сейчас тащат — отрисовка отложена. */
    dragging: false,
    /** Пока тащили, пришёл повод перерисовать. */
    renderPending: false,

    // ---- режим gantt -----------------------------------------------------
    /** Ответ `GET /api/tracker/gantt` за текущее окно или null (ещё не пришёл). */
    gantt: null,
    /** Первый день окна Гантта (Date, локальная полночь). */
    gAnchor: null,
    /** Длина окна Гантта в сутках. */
    gSpan: GANTT_SPAN_DEFAULT,
    /** Свёрнутые группы Гантта: id списка, `''` — «без списка». */
    gCollapsed: new Set(),
    /** Порядковый номер запроса Гантта — против гонки ответов при листании. */
    gSeq: 0,

    // ---- режим report ----------------------------------------------------
    /** Три ответа `GET /api/tracker/report` за одно окно или null. */
    report: null,
    /** Длина периода сводки: day | week | month. */
    rPeriod: 'week',
    /** Любой день внутри показанного периода (Date, локальная полночь). */
    rAnchor: null,
    /** Порядковый номер запроса сводки — против гонки ответов при листании. */
    rSeq: 0,
};

// ------------------------------------------------------------------ утилиты

function readLs(key, fallback) {
    try { return localStorage.getItem(key) || fallback; } catch (_) { return fallback; }
}
function writeLs(key, value) {
    try { localStorage.setItem(key, value); } catch (_) { /* приватный режим */ }
}

/** Множество id задач с идущим таймером. */
function runningIds(snap) {
    const s = new Set();
    (Array.isArray(snap.active) ? snap.active : []).forEach((e) => {
        if (e && e.task_id) s.add(e.task_id);
    });
    return s;
}

/**
 * Временная строка в статус-баре вместо тоста: панель своего тоста не имеет.
 *
 * Необязательное действие рисуется кнопкой прямо в строке — так «тост» умеет
 * увести на вкладку Tasks после переноса. Такому сообщению нужен свой срок
 * жизни: обычные 4 секунды человек не успеет использовать как ссылку.
 *
 * @param {string} text текст сообщения
 * @param {{label: string, run: function(): void, ttl?: number}} [action]
 */
function note(text, action) {
    if (!$trkStatusInfo) return;
    if (local.noteTimer) { clearTimeout(local.noteTimer); local.noteTimer = null; }
    $trkStatusInfo.textContent = text;
    let ttl = 4000;
    if (action && action.label && typeof action.run === 'function') {
        const link = el('button', 'trk-note-link', action.label);
        link.type = 'button';
        link.addEventListener('click', action.run);
        $trkStatusInfo.appendChild(document.createTextNode(' '));
        $trkStatusInfo.appendChild(link);
        if (Number(action.ttl) > 0) ttl = Number(action.ttl);
    }
    local.noteTimer = setTimeout(() => {
        local.noteTimer = null;
        renderStatusbar();
    }, ttl);
}

/** Ошибка операции: в статус-бар, а не в консоль — человек нажал кнопку. */
function fail(prefix, e) {
    note(`${prefix}: ${(e && e.message) || e}`);
}

// ------------------------------------------------------------------ отрисовка

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = text;
    return n;
}

/** Сколько задач попадает в директорию — для счётчиков левой колонки. */
function dirCount(dir) {
    return filterTasks(local.snap.tasks, {
        dir,
        listId: null,
        tag: null,
        now: new Date(),
        runningIds: runningIds(local.snap),
    }).filter((t) => t.status !== 'done').length;
}

function renderSidebar() {
    if ($trkDirs) {
        Array.from($trkDirs.querySelectorAll('.trk-nav')).forEach((li) => {
            const d = li.dataset.dir;
            li.classList.toggle('active', d === local.dir && !local.listId);
            const c = li.querySelector('.trk-nav-count');
            if (c) c.textContent = String(dirCount(d));
        });
    }

    const lists = Array.isArray(local.snap.lists) ? local.snap.lists : [];
    if ($trkListsTitle) {
        $trkListsTitle.textContent = `// LISTS — ${lists.length} NODES`;
    }
    if ($trkLists) {
        $trkLists.textContent = '';
        lists.forEach((l) => {
            const li = el('li', 'trk-nav');
            li.dataset.listId = l.id;
            if (local.listId === l.id) li.classList.add('active');
            const mark = el('span', 'trk-list-mark', '▪');
            if (l.color) mark.style.color = l.color;
            li.appendChild(mark);
            li.appendChild(el('span', 'trk-nav-name', l.name));
            const n = local.snap.tasks.filter(
                (t) => t && t.list_id === l.id && t.status !== 'done',
            ).length;
            li.appendChild(el('span', 'trk-nav-count', `[${n}]`));
            li.title = 'клик — фильтр, двойной клик — переименовать';
            li.addEventListener('click', () => {
                local.listId = local.listId === l.id ? null : l.id;
                renderAll();
            });
            li.addEventListener('dblclick', (ev) => { ev.preventDefault(); renameList(l); });
            $trkLists.appendChild(li);
        });
    }

    if ($trkTags) {
        $trkTags.textContent = '';
        const seen = new Map();
        local.snap.tasks.forEach((t) => {
            (Array.isArray(t.tags) ? t.tags : []).forEach((tag) => {
                seen.set(tag, (seen.get(tag) || 0) + 1);
            });
        });
        [...seen.keys()].sort().forEach((tag) => {
            const b = el('button', 'trk-tag', `#${tag}`);
            b.type = 'button';
            if (local.tag === tag) b.classList.add('active');
            b.addEventListener('click', () => {
                local.tag = local.tag === tag ? null : tag;
                renderAll();
            });
            $trkTags.appendChild(b);
        });
    }
}

/**
 * Живые суммы за сегодня и за неделю: серверная база плюс натикавшее.
 *
 * Одно место на титлбар и статус-бар — иначе `TODAY` наверху и `TODAY` внизу
 * разъехались бы на секунду.
 */
function totalsNow() {
    return liveTotals(local.snap, local.clock);
}

function paintTotals(totals) {
    if ($trkToday) {
        $trkToday.textContent = totals.today_seconds
            ? `TODAY ${fmtDuration(totals.today_seconds)}`
            : '';
    }
    if (!$trkStatusInfo || local.noteTimer) return;
    const st = trackerStatus();
    const store = (st && st.store_path) || '~/.config/forge/tracker.json';
    const parts = ['UTF-8', `GLOBAL JSON ${store}`];
    // В режиме доски статус-бар макета говорит, сколько у неё колонок —
    // число берётся из самого списка колонок, а не пишется цифрой.
    if (local.mode === 'kanban') parts.push(`KANBAN ${COLUMNS.length} COLUMNS`);
    if (local.mode === 'report') {
        const win = reportWindow();
        parts.push(`REPORT ${windowLabel(win)}`);
    }
    if (local.mode === 'gantt') {
        const days = ganttDays();
        const bars = ganttBars();
        const over = bars.filter((b) => isOver(b, secondsOf(b))).length;
        parts.push(`GANTT ${shortDate(dayKey(days[0].date))}—${shortDate(dayKey(days[days.length - 1].date))}`);
        parts.push(`${over} OVERRUN`);
    }
    parts.push(`TODAY ${fmtDuration(totals.today_seconds)}`);
    parts.push(`WEEK ${fmtDuration(totals.week_seconds)}`);
    $trkStatusInfo.textContent = parts.join(' · ');
}

function renderHeader(visible) {
    const active = Array.isArray(local.snap.active) ? local.snap.active : [];
    if ($trkStore) {
        $trkStore.textContent = `● ${active.length} TIMERS · GLOBAL STORE`;
    }
    if ($trkRunningBadge) {
        $trkRunningBadge.textContent = active.length ? String(active.length) : '';
        $trkRunningBadge.hidden = active.length === 0;
    }
    if ($trkGanttNav) $trkGanttNav.hidden = local.mode !== 'gantt';
    if ($trkReportNav) $trkReportNav.hidden = local.mode !== 'report';
    if ($trkSessionLine && local.mode === 'report') {
        // Шапка сводки по макету: период и его итог. Числа берутся из ответа
        // сервера, а не пересчитываются здесь.
        const win = reportWindow();
        const kpi = (local.report && local.report.kpi) || null;
        const tail = [windowLabel(win)];
        if (kpi) {
            tail.push(`TOTAL ${fmtHm(kpi.total_seconds)}`);
            tail.push(`${kpi.closed_tasks}/${kpi.touched_tasks} CLOSED`);
            tail.push(`AVG/DAY ${fmtHm(kpi.avg_day_seconds)}`);
        } else {
            tail.push('сводка загружается…');
        }
        $trkSessionLine.textContent = `REPORT ▸ ${tail.join(' · ')}`;
    } else if ($trkSessionLine && local.mode === 'gantt') {
        // Шапка Гантта по макету: сколько узлов и вех показано и какая шкала.
        // Кнопки [◀] [today] [▶] живут рядом настоящими кнопками (#trk-gantt-nav),
        // а не текстом в этой строке.
        const days = ganttDays();
        const bars = ganttBars();
        const ms = (local.gantt && local.gantt.milestones) || [];
        const tail = [
            `${bars.length} NODES`,
            `${ms.length} MILESTONES`,
            'scale: day',
            `${shortDate(dayKey(days[0].date))}—${shortDate(dayKey(days[days.length - 1].date))}`,
            'срок и оценка задают длину, факт — заливку',
        ].join(' · ');
        $trkSessionLine.textContent = `GANTT ▸ ${tail}`;
    } else if ($trkSessionLine) {
        const listName = local.listId
            ? (local.snap.lists.find((l) => l.id === local.listId) || {}).name
            : null;
        const parts = [`TRACKER ▸ ~/${local.dir}`];
        if (local.mode === 'kanban') parts.push('kanban');
        if (listName) parts.push(`@${listName}`);
        if (local.tag) parts.push(`#${local.tag}`);
        parts.push(`${visible.length} NODES`);
        const seconds = visible.reduce(
            (acc, t) => acc + (Number(local.snap.task_seconds[t.id]) || 0), 0,
        );
        parts.push(`⏱ ${fmtDuration(seconds)}`);
        if (active.length) parts.push(`◇ ${active.length} RUNNING`);
        if (local.mode === 'kanban') parts.push('drag ↔ меняет статус');
        $trkSessionLine.textContent = parts.join(' · ');
    }
    if ($trkModes) {
        Array.from($trkModes.querySelectorAll('.trk-mode')).forEach((b) => {
            b.classList.toggle('active', b.dataset.mode === local.mode);
        });
    }
}

function renderStatusbar() {
    if ($trkConn) {
        const active = Array.isArray(local.snap.active) ? local.snap.active : [];
        // TRACKING горит, когда идёт хотя бы один отрезок; OFFLINE — когда
        // потерян сокет. Между ними ONLINE: соединение есть, таймеров нет.
        if (!local.wsOpen) {
            $trkConn.textContent = 'OFFLINE';
            $trkConn.className = 'offline';
        } else if (active.length) {
            $trkConn.textContent = 'TRACKING';
            $trkConn.className = 'tracking';
        } else {
            $trkConn.textContent = 'ONLINE';
            $trkConn.className = '';
        }
    }
    paintTotals(totalsNow());
}

/**
 * Блок `// RUNNING — N` над лентой.
 *
 * Перерисовывается только при СМЕНЕ СОСТАВА идущих отрезков: цифры внутри
 * обновляет тикер, трогая текстовые узлы. Иначе кнопка `[stop]` пересобиралась
 * бы под курсором каждую секунду.
 */
function renderRunningBlock(force) {
    if (!$trkRunning) return;
    const active = Array.isArray(local.snap.active) ? local.snap.active : [];
    const key = active.map((e) => `${e.id}:${e.item_id || ''}`).join('|');
    if (!force && key === local.runningKey) return;
    local.runningKey = key;
    renderRunning($trkRunning, {
        entries: active,
        tasksById: new Map((local.snap.tasks || []).map((t) => [t.id, t])),
        itemsById: new Map((local.snap.items || []).map((i) => [i.id, i])),
        nowMs: serverNow(local.clock),
        onStop: (entry) => toggleTimer({ id: entry.task_id }, true),
        onOpen: (task) => openTaskModal(task),
    });
}

/** Заголовок разделителя ленты: `08.09.2026 · ~/today`. */
function dayLabel() {
    const d = new Date();
    const p = (n) => String(n).padStart(2, '0');
    return `${p(d.getDate())}.${p(d.getMonth() + 1)}.${d.getFullYear()} · ~/${local.dir}`;
}

/**
 * Начало и конец переноса карточки.
 *
 * Пока карточку тащат, панель НЕ перерисовывается: узел под курсором исчез бы,
 * и браузер оборвал бы перенос. Кадр, пришедший в это время из другого окна,
 * не теряется — он превращается в отложенную отрисовку, которая случится сразу
 * после того, как кнопку мыши отпустят.
 */
function setDragging(on) {
    local.dragging = !!on;
    if (!local.dragging && local.renderPending) {
        local.renderPending = false;
        renderAll();
    }
}

function renderAll() {
    if (local.dragging) {
        local.renderPending = true;
        return;
    }
    const now = new Date();
    const running = runningIds(local.snap);
    const visible = filterTasks(local.snap.tasks, {
        dir: local.dir,
        listId: local.listId,
        tag: local.tag,
        now,
        runningIds: running,
    });

    renderSidebar();
    renderHeader(visible);
    renderStatusbar();
    renderRunningBlock();

    // Доска живёт в том же контейнере, что и лента, но раскладка у неё другая:
    // страница не прокручивается, прокручивается каждая колонка. Разводит их
    // класс на контейнере, а не второй контейнер — иначе тикеру пришлось бы
    // искать `[data-trk-task-time]` в двух местах.
    if ($trkContent) {
        $trkContent.classList.toggle('trk-kanban', local.mode === 'kanban');
        $trkContent.classList.toggle('trk-gantt', local.mode === 'gantt');
        $trkContent.classList.toggle('trk-report', local.mode === 'report');
    }

    if (!MODES.has(local.mode)) {
        // Режимы 7 и 9 ещё не написаны, и их кнопки disabled — сюда попасть
        // можно только через сохранённый localStorage от будущей версии.
        if ($trkContent) {
            $trkContent.textContent = '';
            $trkContent.appendChild(el('div', 'trk-empty-hint', `режим ${local.mode} ещё не реализован`));
        }
        return;
    }

    const listsById = new Map((local.snap.lists || []).map((l) => [l.id, l]));

    if (local.mode === 'gantt') {
        renderGanttMode();
        return;
    }

    if (local.mode === 'report') {
        renderReportMode();
        return;
    }

    if (local.mode === 'kanban') {
        renderKanban($trkContent, {
            tasks: visible,
            listsById,
            taskSeconds: local.snap.task_seconds || {},
            progress: local.snap.progress || {},
            runningIds: running,
            now,
            onTimer: toggleTimer,
            onOpen: (task) => openTaskModal(task),
            onPush: openPushDialog,
            onAdd: addToColumn,
            onMove: moveTask,
            onDrag: setDragging,
        });
        return;
    }

    renderList($trkContent, {
        tasks: visible,
        listsById,
        taskSeconds: local.snap.task_seconds || {},
        progress: local.snap.progress || {},
        runningIds: running,
        dir: local.dir,
        now,
        dayLabel: dayLabel(),
        onToggleDone: toggleDone,
        onTimer: toggleTimer,
        onOpen: (task) => openTaskModal(task),
        onPush: openPushDialog,
        onDelete: removeTask,
        onReorder: reorder,
    });
}

// ----------------------------------------------------------------- режим gantt
//
// ОКНО ЖИВЁТ ЗДЕСЬ, ДАННЫЕ ОКНА — НА СЕРВЕРЕ. `GET /api/tracker/gantt?from&to`
// сам решает, какая задача попала в период (плановая дата внутри ЛИБО отрезок
// времени внутри) и складывает факт. Повторять этот отбор на фронте значило бы
// завести вторую редакцию правил, которая рано или поздно разъедется с
// report.rs.
//
// НО ФИЛЬТР ПАНЕЛИ ОСТАЁТСЯ ОБЩИМ. Директория, список и тег — три координаты
// ОДНОЙ выборки на все режимы (см. шапку файла), поэтому пришедшие полосы
// пересекаются с видимыми задачами снимка. Отсюда же следует, что смена
// периода фильтр не трогает: она меняет только `from`/`to` запроса.
//
// ~/today В ГАНТТЕ ПОЧТИ ПУСТ — И ЭТО НЕ ПОЛОМКА. Предикат `~/today` построен
// на статусах и сроке «сегодня», а диаграмма показывает недели. Разумная
// директория для этого режима — `~/all`.

/** Видимая выборка панели — та же, что у ленты и доски. */
function visibleTasks() {
    return filterTasks(local.snap.tasks, {
        dir: local.dir,
        listId: local.listId,
        tag: local.tag,
        now: new Date(),
        runningIds: runningIds(local.snap),
    });
}

/** Ось дней текущего окна Гантта. */
function ganttDays() {
    const anchor = local.gAnchor || defaultAnchor();
    return buildDays(anchor, local.gSpan, new Date());
}

/** Первый день окна по умолчанию: сегодня стоит на трети окна слева. */
function defaultAnchor() {
    return addDays(dayStart(new Date()), -Math.floor(local.gSpan / 3));
}

/** Живое время задачи: то же число, что показывает лента и тикает тикер. */
function secondsOf(bar) {
    const s = local.snap.task_seconds && local.snap.task_seconds[bar.task_id];
    return s !== undefined ? Number(s) : Number(bar.actual_seconds) || 0;
}

/** Полосы окна, пересечённые с видимой выборкой панели. */
function ganttBars() {
    const bars = (local.gantt && local.gantt.bars) || [];
    const ids = new Set(visibleTasks().map((t) => t.id));
    return bars.filter((b) => b && ids.has(b.task_id));
}

function renderGanttMode() {
    if (!$trkContent) return;
    if (!local.gantt) {
        $trkContent.textContent = '';
        $trkContent.appendChild(el('div', 'trk-empty-hint', 'диаграмма загружается…'));
        return;
    }
    renderGantt($trkContent, {
        bars: ganttBars(),
        milestones: (local.gantt.milestones || []).slice(),
        days: ganttDays(),
        tasksById: new Map((local.snap.tasks || []).map((t) => [t.id, t])),
        lists: local.snap.lists || [],
        taskSeconds: local.snap.task_seconds || {},
        runningIds: runningIds(local.snap),
        collapsed: local.gCollapsed,
        now: new Date(),
        onOpen: (task) => openTaskModal(task),
        onToggleGroup: (key) => {
            if (local.gCollapsed.has(key)) local.gCollapsed.delete(key);
            else local.gCollapsed.add(key);
            renderAll();
        },
        onAddMilestone: addMilestone,
        onEditMilestone: editMilestone,
    });
}

/**
 * Запросить полосы и вехи окна.
 *
 * Ответы нумеруются: пока летит запрос за неделю назад, человек успевает
 * нажать `[◀]` ещё раз, и пришедший позже ответ на СТАРОЕ окно затёр бы новое.
 */
export async function refreshGantt() {
    if (local.mode !== 'gantt') return;
    const days = ganttDays();
    const from = dayKey(days[0].date);
    const to = dayKey(addDays(days[days.length - 1].date, 1));
    const seq = (local.gSeq += 1);
    try {
        const data = await api.getGantt({ from, to });
        if (seq !== local.gSeq) return;
        local.gantt = data || { bars: [], milestones: [] };
        renderAll();
    } catch (e) {
        if (seq !== local.gSeq) return;
        fail('диаграмма', e);
    }
}

/** Сдвиг окна ровно на его длину: окна идут встык, без нахлёста. */
function shiftGantt(dir) {
    const days = ganttDays();
    local.gAnchor = addDays(days[0].date, dir * local.gSpan);
    renderAll();
    refreshGantt();
}

/** `[today]` — вернуться к окну, содержащему сегодняшний день. */
function ganttToday() {
    local.gAnchor = defaultAnchor();
    renderAll();
    refreshGantt();
}

/** Смена длины окна; сегодняшний день остаётся в кадре. */
function setGanttSpan(span) {
    if (!GANTT_SPANS.includes(span)) return;
    local.gSpan = span;
    writeLs(LS_GSPAN, String(span));
    local.gAnchor = defaultAnchor();
    paintGanttNav();
    renderAll();
    refreshGantt();
}

/** Подсветка активной шкалы в шапке. */
function paintGanttNav() {
    if (!$trkGanttNav) return;
    Array.from($trkGanttNav.querySelectorAll('[data-gspan]')).forEach((b) => {
        b.classList.toggle('active', Number(b.dataset.gspan) === local.gSpan);
    });
}

// --------------------------------------------------------------- режим report
//
// СЧИТАЕТ СЕРВЕР, ЭКРАН ТОЛЬКО ПОКАЗЫВАЕТ. Здесь живут ровно две вещи: окно
// периода (какие `from`/`to` уйдут в запрос) и склейка трёх ответов в один
// вид. Ни одна сумма на фронте не складывается — правило суток, правило
// идущего отрезка и правило чек-листа заданы в plugins/tracker/src/report.rs
// и покрыты его тестами. Поэтому «21:47» в шапке сводки и `WEEK 21:47` в
// статус-баре — это одно и то же число из одного расчёта, а не два похожих.
//
// ТРИ ЗАПРОСА, ОДНО ОКНО. У роута одна ось группировки на ответ, а экран
// показывает дни, списки и задачи сразу, поэтому за окно уходит три запроса
// параллельно. KPI одинаковы во всех трёх — берутся из ответа `day`.
//
// ФИЛЬТР ПАНЕЛИ СВОДКУ НЕ ТРОГАЕТ — и это осознанное отличие от ленты, доски
// и Гантта. Сводка отвечает на вопрос «куда ушло моё время», и вычесть из неё
// задачи, не попавшие в `~/today` или в выбранный тег, значило бы показать
// сумму, которая не сходится ни с `TODAY`, ни с `WEEK` в статус-баре. Сумма,
// которая не сходится с соседней строкой того же окна, хуже отсутствующей.

/** Текущее окно сводки: `{period, from, to}` с локальными границами суток. */
function reportWindow() {
    return windowFor(local.rPeriod, local.rAnchor || new Date());
}

/** Вид для render-report.js: три ответа плюс справочники панели. */
function reportView() {
    const win = reportWindow();
    if (!local.report) return { ...win };
    return {
        ...win,
        byDay: local.report.byDay,
        byList: local.report.byList,
        byTask: local.report.byTask,
        kpi: local.report.kpi,
        todayKey: rDayKey(new Date()),
        tasksById: new Map((local.snap.tasks || []).map((t) => [t.id, t])),
        listsById: new Map((local.snap.lists || []).map((l) => [l.id, l])),
        onOpen: (task) => openTaskModal(task),
    };
}

function renderReportMode() {
    if (!$trkContent) return;
    renderReport($trkContent, local.report ? reportView() : null);
}

/**
 * Запросить сводку за текущее окно — три разреза сразу.
 *
 * Ответы нумеруются по той же причине, что у Гантта: пока летит запрос за
 * прошлую неделю, человек успевает нажать `[◀]` ещё раз, и пришедший позже
 * ответ на СТАРОЕ окно затёр бы новое.
 */
export async function refreshReport() {
    if (local.mode !== 'report') return;
    const win = reportWindow();
    const from = rDayKey(win.from);
    const to = rDayKey(win.to);
    const seq = (local.rSeq += 1);
    try {
        const [byDay, byList, byTask] = await Promise.all([
            api.getReport({ from, to, group_by: 'day' }),
            api.getReport({ from, to, group_by: 'list' }),
            api.getReport({ from, to, group_by: 'task' }),
        ]);
        if (seq !== local.rSeq) return;
        local.report = {
            byDay: byDay || { rows: [], total_seconds: 0 },
            byList: byList || { rows: [], total_seconds: 0 },
            byTask: byTask || { rows: [], total_seconds: 0 },
            kpi: (byDay && byDay.kpi) || null,
        };
        renderAll();
    } catch (e) {
        if (seq !== local.rSeq) return;
        fail('сводка', e);
    }
}

/** Сдвиг периода на его длину: периоды идут встык, без нахлёста. */
function shiftReport(dir) {
    const win = reportWindow();
    local.rAnchor = shiftAnchor(local.rPeriod, win.from, dir);
    local.report = null;
    renderAll();
    refreshReport();
}

/** `[today]` — вернуться к периоду, в котором лежит сегодняшний день. */
function reportToday() {
    local.rAnchor = null;
    local.report = null;
    renderAll();
    refreshReport();
}

/** Смена длины периода; сегодняшний день остаётся внутри окна. */
function setReportPeriod(period) {
    if (!PERIODS.includes(period)) return;
    local.rPeriod = period;
    writeLs(LS_RPERIOD, period);
    // Длина периода изменилась — держаться за прежний якорь незачем: человек
    // переключает day/week/month, чтобы увидеть текущий день, неделю, месяц.
    local.rAnchor = null;
    local.report = null;
    paintReportNav();
    renderAll();
    refreshReport();
}

/** Подсветка активного периода в шапке. */
function paintReportNav() {
    if (!$trkReportNav) return;
    Array.from($trkReportNav.querySelectorAll('[data-rperiod]')).forEach((b) => {
        b.classList.toggle('active', b.dataset.rperiod === local.rPeriod);
    });
}

/**
 * Выгрузка показанного в CSV.
 *
 * Файл собирается ИЗ ТЕХ ЖЕ данных, что нарисованы: второго запроса нет, и
 * разойтись с экраном нечему. Пустой период файла не даёт вовсе — пустой CSV
 * человек унесёт в таблицу и решит, что данные потерялись; честнее сказать в
 * статус-баре, что выгружать нечего.
 */
function exportReportCsv() {
    if (!local.report) { note('сводка ещё не загрузилась'); return; }
    const view = reportView();
    if (isEmptyReport(view)) {
        note(`за период ${windowLabel(view)} времени не записано — выгружать нечего`);
        return;
    }
    // BOM обязателен: без него Excel с русской локалью читает файл как cp1251
    // и показывает кракозябры вместо кириллицы.
    const blob = new Blob(['\ufeff', reportToCsv(view)], {
        type: 'text/csv;charset=utf-8',
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = csvFileName(view);
    document.body.appendChild(a);
    a.click();
    a.remove();
    // Освобождать ссылку сразу нельзя: Safari успевает не начать скачивание.
    setTimeout(() => URL.revokeObjectURL(url), 10000);
    note(`выгружено: ${csvFileName(view)}`);
}

// -------------------------------------------------------------------- вехи
//
// CRUD вех живёт в собственном диалоге (js/tracker/milestone.js): подпись,
// дата НАТИВНЫМ пикером и явная кнопка DELETE. До этого здесь стояли два
// `window.prompt` подряд — дату приходилось набирать руками (единственное
// место панели без пикера), удаление пряталось за «сотри подпись», а
// браузерная модалка вдобавок невидима для автоматизации.
//
// Сервер на любую правку вехи шлёт кадр `reload` (точечного кадра для вех
// нет), панель перечитывает снимок, а снимок тянет за собой `refreshGantt`.
// Поэтому созданная веха появляется на своём дне без перезагрузки страницы и
// во всех открытых окнах сразу — диалог сам ничего не перерисовывает.

/** Дата вехи по умолчанию — середина текущего окна. */
function midWindowDate() {
    const days = ganttDays();
    return dayKey(days[Math.floor(days.length / 2)].date);
}

/** `[+ веха]` на дорожке Гантта. */
function addMilestone() {
    openMilestoneDialog(null, midWindowDate());
}

/** Клик по чипу вехи: правка подписи, даты или удаление. */
function editMilestone(ms) {
    openMilestoneDialog(ms, midWindowDate());
}

// -------------------------------------------------------------------- действия

async function toggleDone(task) {
    const next = task.status === 'done' ? 'inbox' : 'done';
    try {
        await api.setTaskStatus(task.id, next);
    } catch (e) {
        fail('смена статуса', e);
    }
}

async function toggleTimer(task, running) {
    try {
        if (running) await api.stopTimer(task.id);
        else await api.startTimer(task.id);
    } catch (e) {
        fail('таймер', e);
    }
}

async function removeTask(task) {
    const title = task.title || task.id;
    // eslint-disable-next-line no-alert
    if (!window.confirm(`Удалить задачу «${title}» вместе с её временем?`)) return;
    try {
        await api.deleteTask(task.id);
    } catch (e) {
        fail('удаление', e);
    }
}

/**
 * Перестановка задачи в ленте.
 *
 * Полный порядок собирает `planOrder` (js/tracker/render-list.js): порядок в
 * трекере ГЛОБАЛЬНЫЙ, а видна только выборка, поэтому отправлять порядок одной
 * лишь выборки нельзя — сервер увёл бы недостающие id в хвост.
 */
async function reorder(movedId, targetId, after) {
    const ids = planOrder(local.snap.tasks || [], movedId, targetId, after);
    if (!ids) return;
    try {
        await api.reorderTasks(ids);
    } catch (e) {
        fail('перестановка', e);
    }
}

/** Применить порядок к снимку локально — для оптимистичной отрисовки доски. */
function applyOrderLocally(ids) {
    const pos = new Map(ids.map((id, i) => [id, i]));
    (local.snap.tasks || []).forEach((t) => {
        if (t && pos.has(t.id)) t.order = pos.get(t.id);
    });
}

/**
 * Перенос карточки на доске: смена колонки и/или перестановка внутри неё.
 *
 * ОПТИМИСТИЧНО, С ОТКАТОМ — как в js/tasks/crud.js. Карточка обязана
 * оказаться под курсором в момент, когда её отпустили: ждать ответа сервера,
 * а до тех пор рисовать её на прежнем месте, значит показывать заведомо
 * неверное состояние. Поэтому снимок правится сразу, а при отказе сервера
 * возвращается ровно то, что было (статус, `done_at` и порядок), и следом
 * запрашивается свежий снимок — на случай, если отказ был вызван чужой
 * правкой из другого окна.
 *
 * `done_at` двигает СЕРВЕР (`update_task` в plugins/tracker/src/store.rs):
 * перевод в `done` его проставляет, любой обратный перенос снимает. Здесь он
 * заполняется лишь предварительно, чтобы карточка сразу выглядела как
 * выполненная; истина придёт со следующим снимком.
 *
 * @param {string} movedId — что перенесли
 * @param {string} status — колонка, в которую отпустили
 * @param {string|null} targetId — соседка, относительно которой положили
 * @param {boolean} after — положили ПОСЛЕ соседки
 */
async function moveTask(movedId, status, targetId, after) {
    const task = (local.snap.tasks || []).find((t) => t && t.id === movedId);
    if (!task) return;

    const prevStatus = task.status;
    const prevDoneAt = task.done_at;
    const prevOrder = (local.snap.tasks || []).map((t) => [t.id, t.order]);
    const changesStatus = !!status && status !== prevStatus;
    const ids = planOrder(local.snap.tasks || [], movedId, targetId, after);
    if (!changesStatus && !ids) return;

    if (changesStatus) {
        task.status = status;
        task.done_at = status === 'done' ? new Date().toISOString() : null;
    }
    if (ids) applyOrderLocally(ids);
    renderAll();

    try {
        if (changesStatus) await api.setTaskStatus(movedId, status);
        if (ids) await api.reorderTasks(ids);
    } catch (e) {
        task.status = prevStatus;
        task.done_at = prevDoneAt;
        const back = new Map(prevOrder);
        (local.snap.tasks || []).forEach((t) => {
            if (t && back.has(t.id)) t.order = back.get(t.id);
        });
        renderAll();
        fail('перенос', e);
        // Свежий снимок нужен, когда отказал СЕРВЕР (конфликт, удалённая
        // задача, чужая правка из другого окна) — тогда наше представление
        // устарело. При сетевой ошибке (`status === 0`) запрашивать нечего:
        // он всё равно упадёт и лишь затрёт сообщение об отказе своим.
        if (e && e.status) refreshTracker();
    }
}

/**
 * `[+]` в шапке колонки: новая задача сразу в этой колонке.
 *
 * Строка разбирается тем же `parsePrompt`, что и главное поле ввода, — иначе у
 * доски завёлся бы свой, более бедный синтаксис. Статус задаёт колонка, в
 * шапке которой нажали, а не текущая директория.
 */
async function addToColumn(status) {
    // eslint-disable-next-line no-alert
    const raw = window.prompt(`Новая задача в // ${status}`);
    if (raw === null) return;
    const parsed = parsePrompt(raw);
    if (!parsed.ok) return;
    let listId = null;
    try {
        listId = await resolveList(parsed.list);
    } catch (e) {
        fail('список', e);
        return;
    }
    try {
        await api.createTask(toCreatePayload(parsed, listId, status));
    } catch (e) {
        fail('создание задачи', e);
    }
}

async function renameList(list) {
    // eslint-disable-next-line no-alert
    const name = window.prompt('Имя списка', list.name);
    if (name === null) return;
    const trimmed = name.trim();
    if (!trimmed) {
        // eslint-disable-next-line no-alert
        if (!window.confirm(`Удалить список «${list.name}»? Задачи останутся, но уедут в inbox.`)) return;
        try {
            await api.deleteList(list.id);
            if (local.listId === list.id) local.listId = null;
        } catch (e) { fail('удаление списка', e); }
        return;
    }
    if (trimmed === list.name) return;
    try {
        await api.patchList(list.id, { name: trimmed });
    } catch (e) { fail('переименование', e); }
}

async function addList() {
    // eslint-disable-next-line no-alert
    const name = window.prompt('Имя нового списка');
    if (!name || !name.trim()) return;
    try {
        await api.createList({ name: name.trim() });
    } catch (e) { fail('создание списка', e); }
}

/**
 * Список по имени: найти или СОЗДАТЬ.
 *
 * Решение зафиксировано в шапке js/tracker/parse.js: `@новый` создаёт список.
 * Альтернатива — молча положить задачу в inbox, то есть сделать не то, что
 * написал человек. Сравнение имён — без учёта регистра: два списка `work` и
 * `Work` человек не различит, а стор различит.
 */
async function resolveList(name) {
    if (!name) return null;
    const wanted = name.trim().toLowerCase();
    if (!wanted) return null;
    const found = (local.snap.lists || []).find(
        (l) => String(l.name || '').trim().toLowerCase() === wanted,
    );
    if (found) return found.id;
    const created = await api.createList({ name: name.trim() });
    return created && created.id ? created.id : null;
}

async function submitPrompt(ev) {
    if (ev) ev.preventDefault();
    if (!$trkPromptInput) return;
    const parsed = parsePrompt($trkPromptInput.value);
    if (!parsed.ok) {
        // Пустая строка задачу не создаёт — и молча, без ошибки: это просто
        // Enter в пустом поле.
        return;
    }
    let listId = null;
    try {
        listId = await resolveList(parsed.list);
    } catch (e) {
        fail('список', e);
        return;
    }
    // Директория задаёт колонку создаваемой задачи: `~/today` обязан
    // показать только что созданную задачу, а не потерять её во входящих.
    const status = local.dir === 'today' ? 'today' : null;
    try {
        await api.createTask(toCreatePayload(parsed, listId, status));
    } catch (e) {
        fail('создание задачи', e);
        return;
    }
    $trkPromptInput.value = '';
    renderPreview();
}

function renderPreview() {
    if (!$trkPromptPreview || !$trkPromptInput) return;
    const raw = $trkPromptInput.value;
    if (!raw.trim()) {
        $trkPromptPreview.hidden = true;
        $trkPromptPreview.textContent = '';
        return;
    }
    const parsed = parsePrompt(raw);
    $trkPromptPreview.textContent = '';
    previewOf(parsed).forEach((p) => {
        const box = el('span', 'trk-pv');
        box.appendChild(el('span', null, `${p.key}: `));
        const v = el('span', p.key === 'title' ? 'trk-pv-title' : null, p.value);
        box.appendChild(v);
        $trkPromptPreview.appendChild(box);
    });
    $trkPromptPreview.hidden = false;
}

function setDir(dir) {
    local.dir = dir;
    local.listId = null;
    writeLs(LS_DIR, dir);
    renderAll();
}

/**
 * Смена режима.
 *
 * Режим переживает уход с вкладки и перезагрузку страницы (localStorage), а
 * фильтр при смене режима НЕ сбрасывается: директория, список и тег — это
 * координаты выборки, а режим лишь способ её показать.
 */
function setMode(mode) {
    if (!MODES.has(mode)) return;
    local.mode = mode;
    writeLs(LS_MODE, mode);
    renderAll();
    // Гантт и сводка — единственные режимы со своим запросом: лента и доска
    // рисуются из уже имеющегося снимка, а полосы окна и суммы периода снимок
    // не несёт.
    if (mode === 'gantt') refreshGantt();
    if (mode === 'report') { paintReportNav(); refreshReport(); }
}

// ------------------------------------------------------------------- состояние

/** Полный перезапрос снимка (открытие вкладки, кадр `reload`, кнопка ⏻). */
export async function refreshTracker() {
    try {
        const snap = await api.getState();
        if (snap) applySnapshot(snap);
    } catch (e) {
        fail('состояние', e);
    }
}

function applySnapshot(snap) {
    local.snap = {
        ...emptySnapshot(),
        ...snap,
        task_seconds: snap.task_seconds || {},
        item_seconds: snap.item_seconds || {},
        progress: snap.progress || {},
    };
    // Опорная точка берётся РОВНО ЗДЕСЬ и держится до следующего снимка: все
    // живые значения считаются от неё, а не от часов браузера.
    local.clock = clockFromSnapshot(local.snap);
    local.rolloverAsked = false;
    renderAll();
    onTrackerSnapshot(local.snap);
    syncTicker();
    // Снимок пришёл — значит что-то изменилось; полосы окна считает сервер, и
    // единственный способ их обновить — спросить заново. Кадра «веха
    // изменилась» в протоколе нет: сервер шлёт общий `reload`, который и
    // приводит сюда.
    if (local.mode === 'gantt') refreshGantt();
    // Сводка живёт по тем же правилам: её числа считает сервер, и обновить их
    // можно только запросом.
    if (local.mode === 'report') refreshReport();
}

/**
 * Тикер работает ТОЛЬКО когда есть что тикать и панель видна.
 *
 * Держать интервал при нуле активных отрезков незачем, а держать его после
 * ухода с вкладки — прямая утечка: браузер душит фоновые таймеры, но не
 * снимает их.
 */
function syncTicker() {
    const active = Array.isArray(local.snap.active) ? local.snap.active : [];
    const visible = !!$tracker && !$tracker.hidden
        && (typeof document === 'undefined' || document.visibilityState !== 'hidden');
    if (active.length && visible) startTicker(tick);
    else stopTicker();
}

/**
 * Один тик: только текст уже существующих узлов.
 *
 * Всё считается ЗАНОВО от опорной точки (`сейчас − started_at`), а не
 * прибавлением секунды: после скрытой вкладки или сна машины накопление шагов
 * разъехалось бы с реальностью, а пересчёт — нет.
 */
function tick() {
    // После полуночи добавка «сколько прошло с момента снимка» перестаёт
    // описывать TODAY: сумма дня обязана начаться заново. Досчитать это на
    // клиенте нельзя, поэтому просим у сервера свежий снимок — один раз.
    if (dayRolledOver(local.clock)) {
        if (!local.rolloverAsked) {
            local.rolloverAsked = true;
            refreshTracker();
        }
        return;
    }

    const nowMs = serverNow(local.clock);
    const byTask = liveTaskSeconds(local.snap, local.clock);
    const byItem = liveItemSeconds(local.snap, local.clock);

    tickRunning($trkRunning, local.snap.active || [], nowMs);

    if ($trkContent) {
        Array.from($trkContent.querySelectorAll('[data-trk-task-time]')).forEach((node) => {
            const secs = byTask[node.dataset.trkTaskTime];
            if (secs === undefined) return;
            node.textContent = fmtDuration(secs);
        });
    }

    paintTotals(totalsNow());
    paintLive(local.snap, nowMs, { task: byTask, item: byItem });
}

/**
 * Точечное применение кадра `task_upsert`.
 *
 * Сама задача применяется мгновенно, но следом запрашивается свежий снимок:
 * этот же кадр сервер шлёт при ЛЮБОЙ мутации чек-листа (см.
 * plugins/tracker/src/routes/items.rs), а её видимый результат — прогресс
 * `n/m` и список пунктов — живёт в агрегатах снимка, а не в самой задаче.
 * Без перезапроса чек-лист во второй вкладке не менялся бы вовсе.
 */
function upsertTask(task) {
    if (!task || !task.id) return;
    const arr = local.snap.tasks || [];
    const i = arr.findIndex((t) => t && t.id === task.id);
    if (i >= 0) arr[i] = task;
    else arr.push(task);
    renderAll();
    refreshTracker();
}

function removeTaskLocal(id) {
    const arr = local.snap.tasks || [];
    const i = arr.findIndex((t) => t && t.id === id);
    if (i < 0) return;
    arr.splice(i, 1);
    delete local.snap.task_seconds[id];
    delete local.snap.progress[id];
    renderAll();
    onTrackerSnapshot(local.snap);
}

/**
 * Точечное применение кадра `entry_upsert`.
 *
 * Суммы (`task_seconds`, `totals`) пересчитывает СЕРВЕР, а не фронт: правила
 * учёта (время пункта чек-листа входит в сумму задачи ровно один раз, идущий
 * таймер считается «по сейчас») живут в report.rs и покрыты тестами.
 * Дублировать их в JS — гарантированный разъезд, поэтому после изменения
 * отрезка запрашивается свежий снимок.
 */
function upsertEntry(entry) {
    if (!entry || !entry.id) return;
    const arr = local.snap.entries || [];
    const i = arr.findIndex((e) => e && e.id === entry.id);
    if (i >= 0) arr[i] = entry;
    else arr.push(entry);
    refreshTracker();
}

function removeEntry(id) {
    const arr = local.snap.entries || [];
    const i = arr.findIndex((e) => e && e.id === id);
    if (i >= 0) arr.splice(i, 1);
    refreshTracker();
}

const wsHandlers = {
    snapshot: (msg) => applySnapshot(msg),
    taskUpsert: upsertTask,
    taskRemoved: removeTaskLocal,
    entryUpsert: upsertEntry,
    entryRemoved: removeEntry,
    reload: () => refreshTracker(),
    open: () => { local.wsOpen = true; renderStatusbar(); },
    close: () => { local.wsOpen = false; renderStatusbar(); },
};

// ------------------------------------------------------------------- точки входа

/**
 * Инициализация панели. Зовётся на КАЖДОМ входе на вкладку и идемпотентна:
 * слушатели навешиваются один раз, состояние перечитывается всегда.
 */
export function initTracker() {
    if (!isTrackerEnabled()) return;

    if (!local.wired) {
        local.wired = true;

        if ($trkDirs) {
            $trkDirs.addEventListener('click', (ev) => {
                const li = ev.target.closest && ev.target.closest('.trk-nav');
                if (!li || !li.dataset.dir) return;
                setDir(li.dataset.dir);
            });
        }
        if ($trkListAdd) $trkListAdd.addEventListener('click', addList);
        if ($trkModes) {
            $trkModes.addEventListener('click', (ev) => {
                const b = ev.target.closest && ev.target.closest('.trk-mode');
                if (!b || b.disabled || !b.dataset.mode) return;
                setMode(b.dataset.mode);
            });
        }
        if ($trkGanttNav) {
            $trkGanttNav.addEventListener('click', (ev) => {
                const b = ev.target.closest && ev.target.closest('button');
                if (!b) return;
                if (b.dataset.gspan) { setGanttSpan(Number(b.dataset.gspan)); return; }
                if (b.dataset.gnav === 'prev') shiftGantt(-1);
                else if (b.dataset.gnav === 'next') shiftGantt(1);
                else if (b.dataset.gnav === 'today') ganttToday();
            });
        }
        if ($trkReportNav) {
            $trkReportNav.addEventListener('click', (ev) => {
                const b = ev.target.closest && ev.target.closest('button');
                if (!b) return;
                if (b.dataset.rperiod) { setReportPeriod(b.dataset.rperiod); return; }
                if (b.dataset.rnav === 'prev') shiftReport(-1);
                else if (b.dataset.rnav === 'next') shiftReport(1);
                else if (b.dataset.rnav === 'today') reportToday();
                else if (b.dataset.rnav === 'csv') exportReportCsv();
            });
        }
        if ($trkReload) $trkReload.addEventListener('click', () => refreshTracker());
        if ($trkPrompt) $trkPrompt.addEventListener('submit', submitPrompt);
        if ($trkPromptInput) $trkPromptInput.addEventListener('input', renderPreview);

        initTrackerModals({
            snapshot: () => local.snap,
            refresh: refreshTracker,
            push: openPushDialog,
        });

        // Диалог переноса получает узлы и внешние сервисы снаружи: сам он
        // ни `core/dom.js`, ни вкладку Tasks не импортирует — так его чистые
        // функции остаются проверяемыми в голом node.
        initTrackerPush({
            dom: {
                $trkPush, $trkPushNode, $trkPushRoots, $trkPushError,
                $trkPushClose, $trkPushCancel, $trkPushOk,
            },
            snapshot: () => local.snap,
            sessions: () => (Array.isArray(state.sessions) ? state.sessions : []),
            activePath: () => currentTodosPath(),
            create: createTodoForPath,
            note,
            // Вкладки грузят панель трекера динамически; статический импорт
            // tabs.js отсюда завёл бы цикл, поэтому он тоже динамический.
            openTasks: () => {
                import('../tabs/tabs.js')
                    .then((m) => m.switchTab('tasks'))
                    .catch((e) => fail('переход на Tasks не удался', e));
            },
        });

        // Диалог вехи — по тому же правилу, что и диалог переноса: узлы и
        // сервисы снаружи, чтобы чистая часть модуля оставалась проверяемой
        // в голом node.
        initTrackerMilestone({
            dom: {
                $trkMs, $trkMsTitle, $trkMsDate, $trkMsHint, $trkMsError,
                $trkMsClose, $trkMsCancel, $trkMsDelete, $trkMsOk,
            },
            create: (body) => api.createMilestone(body),
            patch: (id, body) => api.patchMilestone(id, body),
            remove: (id) => api.deleteMilestone(id),
            note,
        });

        // Скрытое окно тикать не должно: браузер душит фоновые интервалы, а
        // при возврате значение всё равно пересчитывается от опорной точки.
        document.addEventListener('visibilitychange', onVisibility);

        local.dir = readLs(LS_DIR, 'today');
        // Режим из localStorage применяется ТОЛЬКО если он реализован: кнопки
        // невключённого режима disabled, и восстановление такого режима дало бы
        // пустой экран без выхода.
        const savedMode = readLs(LS_MODE, 'list');
        local.mode = MODES.has(savedMode) ? savedMode : 'list';

        const savedSpan = Number(readLs(LS_GSPAN, String(GANTT_SPAN_DEFAULT)));
        local.gSpan = GANTT_SPANS.includes(savedSpan) ? savedSpan : GANTT_SPAN_DEFAULT;

        const savedPeriod = readLs(LS_RPERIOD, 'week');
        local.rPeriod = PERIODS.includes(savedPeriod) ? savedPeriod : 'week';
    }

    // Окно Гантта режим НЕ переживает: человек уходит на другую вкладку и
    // возвращается, чтобы посмотреть, что сейчас, — а не туда, куда пролистал
    // полчаса назад. Сама принадлежность к режиму (`local.mode`) при этом
    // сохраняется, как и требует критерий приёмки.
    local.gAnchor = defaultAnchor();
    // Период сводки, как и окно Гантта, при возврате на вкладку встаёт на
    // «сейчас»: человек возвращается посмотреть текущую неделю, а не ту, до
    // которой долистал полчаса назад. Сама длина периода (day/week/month)
    // переживает и уход, и перезагрузку.
    local.rAnchor = null;
    local.report = null;

    connectTrackerWs(wsHandlers);
    refreshTracker();
    paintGanttNav();
    paintReportNav();
    renderAll();
    syncTicker();
    if (local.mode === 'gantt') refreshGantt();
    if (local.mode === 'report') refreshReport();
}

/**
 * Окно ушло в фон или вернулось.
 *
 * Возврат не «догоняет» время шагами: свежий снимок берётся у сервера, а до
 * его прихода значения всё равно считаются от опорной точки — то есть уже
 * верны.
 */
function onVisibility() {
    if (document.visibilityState === 'hidden') {
        stopTicker();
        return;
    }
    if (!$tracker || $tracker.hidden) return;
    refreshTracker();
    syncTicker();
}

/** Уход с вкладки: закрыть сокет, остановить тик, погасить карточку. */
export function suspendTracker() {
    if (!isTrackerEnabled()) return;
    closePushDialog();
    closeMilestoneDialog();
    closeTaskModal();
    disconnectTrackerWs();
    stopTicker();
    local.wsOpen = false;
    // Уход с вкладки посреди переноса: `dragend` в этом случае может и не
    // прийти, а застрявший флаг навсегда заморозил бы отрисовку панели.
    local.dragging = false;
    local.renderPending = false;
}

/** Возврат на вкладку из фона. */
export function resumeTracker() {
    if (!isTrackerEnabled()) return;
    if (!$tracker || $tracker.hidden) return;
    connectTrackerWs(wsHandlers);
    refreshTracker();
    syncTicker();
}

/** beforeunload: снять всё, что переживает страницу. */
export function teardownTracker() {
    disconnectTrackerWs();
    teardownTrackerModals();
    teardownTrackerPush();
    teardownTrackerMilestone();
    stopTicker();
    document.removeEventListener('visibilitychange', onVisibility);
    local.wired = false;
    if (local.noteTimer) { clearTimeout(local.noteTimer); local.noteTimer = null; }
}

/** Снимок состояния панели для e2e и диагностики. */
export function _debugState() {
    return {
        enabled: isTrackerEnabled(),
        dir: local.dir,
        listId: local.listId,
        tag: local.tag,
        mode: local.mode,
        wsOpen: local.wsOpen,
        ganttSpan: local.gSpan,
        ganttFrom: local.gAnchor ? dayKey(local.gAnchor) : null,
        ganttBars: local.gantt ? (local.gantt.bars || []).length : null,
        ganttMilestones: local.gantt ? (local.gantt.milestones || []).length : null,
        reportPeriod: local.rPeriod,
        reportFrom: rDayKey(reportWindow().from),
        reportTo: rDayKey(reportWindow().to),
        reportKpi: (local.report && local.report.kpi) || null,
        reportDays: local.report ? (local.report.byDay.rows || []).length : null,
        reportLists: local.report ? (local.report.byList.rows || []).length : null,
        reportTasks: local.report ? (local.report.byTask.rows || []).length : null,
        tasks: (local.snap.tasks || []).length,
        lists: (local.snap.lists || []).length,
        items: (local.snap.items || []).length,
        active: (local.snap.active || []).length,
        ticking: isTicking(),
        totals: totalsNow(),
    };
}
