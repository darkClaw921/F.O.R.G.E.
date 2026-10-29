// tmux-web — плагин forge-tracker: режим `report`.
//
// Экран по макету (~/Documents/ForgeTracker.pen, «Экран — ~/report (сводка
// времени)»):
//
//     // TOTAL      // AVG/DAY    // CLOSED     // MAX FOCUS
//     21:47         3:07          9/17          2:14
//
//     // BY DAY — bar = 15 мин
//     пн 01.09  █████████████░░░  3:08
//     вс 07.09  █████████░░░░░░░  2:12      ← сегодня, акцентом
//
//     // BY LIST                    // TOP NODES
//     work  ▓▓▓▓▓▓▓▓▓▓▓▓ 10:02 48%   Свести дизайн трекера [work] 5:12
//
// ВСЕ ЧИСЛА ПРИХОДЯТ ПОСЧИТАННЫМИ. Правило суток (день локальный, отрезок
// через полночь режется по границе), правило активного отрезка (идущий таймер
// считается «до сейчас») и правило чек-листа (время пункта входит во время
// задачи ОДИН раз) живут в plugins/tracker/src/report.rs и покрыты там
// тестами. Здесь нет ни одной строки, которая складывала бы секунды: экран
// получает `rows` и `kpi` из `GET /api/tracker/report?from&to&group_by=…` и
// только раскладывает их в знаки. Вторая редакция тех же правил на фронте
// рано или поздно разошлась бы с первой молча — и как раз в сводке, где
// расхождение заметить труднее всего.
//
// ТРИ РАЗРЕЗА — ТРИ ЗАПРОСА ЗА ОДНО И ТО ЖЕ ОКНО. Ось группировки у роута
// одна на ответ (`day` | `task` | `list`), а экран показывает все три сразу.
// KPI при этом от оси не зависят и одинаковы во всех трёх ответах — берём их
// из ответа `day` и не сверяем: сервер считает их одной функцией.
//
// ПОЛОСЫ — СИМВОЛЬНЫЕ, как на Гантте. `// BY DAY` меряет абсолютной шкалой
// (один знак = 15 минут, как написано в самом заголовке блока), `// BY LIST` —
// относительной (самый долгий список занимает всю ширину). Разница не
// случайна: у дней сравнивать надо с рабочим днём, а у списков — друг с
// другом.
//
// НИКАКОГО innerHTML: имена списков и заголовки задач — пользовательский
// текст и попадают в DOM только через textContent.

import { fmtDuration } from './render-list.js';

/** Периоды переключателя в шапке — в том порядке, что в макете. */
export const PERIODS = ['day', 'week', 'month'];

/** Один знак полосы `// BY DAY` — 15 минут (подпись блока это и говорит). */
export const BAR_UNIT = 900;
/** Ширина полосы дня в знаках: 16 × 15 мин = 4 часа на всю полосу. */
export const BAR_WIDTH = 16;
/** Ширина полосы списка в знаках. */
export const LIST_BAR_WIDTH = 12;

const DOW = ['вс', 'пн', 'вт', 'ср', 'чт', 'пт', 'сб'];

/** Локальная полночь дня, которому принадлежит `date`. */
export function dayStart(date) {
    return new Date(date.getFullYear(), date.getMonth(), date.getDate(), 0, 0, 0, 0);
}

/** Сдвиг на `n` суток от локальной полуночи. */
export function addDays(date, n) {
    return new Date(date.getFullYear(), date.getMonth(), date.getDate() + n, 0, 0, 0, 0);
}

/** Локальный ключ дня `YYYY-MM-DD` — тот же формат, что в строках отчёта. */
export function dayKey(date) {
    const p = (n) => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${p(date.getMonth() + 1)}-${p(date.getDate())}`;
}

/**
 * Дата из ключа `YYYY-MM-DD` как ЛОКАЛЬНАЯ полночь.
 *
 * Через `new Date("2026-09-01")` так нельзя: голая дата читается как UTC, и в
 * зонах западнее Гринвича день уезжает на сутки назад — та же ловушка, что в
 * render-list.js и render-gantt.js.
 */
export function dateOfKey(key) {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(key || '').trim());
    if (!m) return null;
    return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), 0, 0, 0, 0);
}

/**
 * Окно периода — ЧИСТАЯ функция.
 *
 * Границы всегда попадают на локальную полночь и уходят в запрос строками
 * `YYYY-MM-DD`: сервер трактует голую дату как локальную полночь (`parse_ts`),
 * поэтому окно экрана и окно расчёта совпадают знак в знак.
 *
 * @param {string} period — `day` | `week` | `month`
 * @param {Date} anchor — любой момент внутри искомого периода
 * @returns {{from: Date, to: Date, period: string}} полуоткрытое `[from, to)`
 */
export function windowFor(period, anchor) {
    const a = dayStart(anchor instanceof Date ? anchor : new Date());
    if (period === 'day') return { period, from: a, to: addDays(a, 1) };
    if (period === 'month') {
        const from = new Date(a.getFullYear(), a.getMonth(), 1, 0, 0, 0, 0);
        const to = new Date(a.getFullYear(), a.getMonth() + 1, 1, 0, 0, 0, 0);
        return { period, from, to };
    }
    // Неделя начинается с понедельника — та же граница, что у `WEEK` в
    // статус-баре и у окна по умолчанию в routes/report.rs.
    const back = (a.getDay() + 6) % 7;
    const from = addDays(a, -back);
    return { period: 'week', from, to: addDays(from, 7) };
}

/** Сдвиг окна на один период назад (`-1`) или вперёд (`+1`). */
export function shiftAnchor(period, anchor, dir) {
    const a = dayStart(anchor instanceof Date ? anchor : new Date());
    if (period === 'day') return addDays(a, dir);
    if (period === 'month') return new Date(a.getFullYear(), a.getMonth() + dir, 1, 0, 0, 0, 0);
    return addDays(a, dir * 7);
}

/** `01.09` — короткая дата подписи периода. */
export function shortDate(date) {
    const p = (n) => String(n).padStart(2, '0');
    return `${p(date.getDate())}.${p(date.getMonth() + 1)}`;
}

/** Подпись окна в шапке: `week 01.09 — 07.09`. */
export function windowLabel(win) {
    const last = addDays(win.to, -1);
    if (win.period === 'day') return `day ${shortDate(win.from)}`;
    return `${win.period} ${shortDate(win.from)} — ${shortDate(last)}`;
}

/** `Ч:ММ` — формат чисел сводки из макета (`21:47`, `3:07`). */
export function fmtHm(seconds) {
    const total = Math.max(0, Math.floor(Number(seconds) || 0));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    return `${h}:${String(m).padStart(2, '0')}`;
}

/** `+3:12` / `−0:40` / `±0:00` — расхождение с прошлым периодом. */
export function fmtDelta(seconds) {
    const v = Math.round(Number(seconds) || 0);
    if (v === 0) return '±0:00';
    return `${v > 0 ? '+' : '−'}${fmtHm(Math.abs(v))}`;
}

/** Подпись дня строки `// BY DAY`: `пн 01.09`. */
export function dayRowLabel(key) {
    const d = dateOfKey(key);
    if (!d) return String(key || '');
    return `${DOW[d.getDay()]} ${shortDate(d)}`;
}

/**
 * Полоса дня — АБСОЛЮТНАЯ шкала: один знак = [`BAR_UNIT`] секунд.
 *
 * Заполненных знаков — округлённое до ближайшего число единиц; ноль секунд
 * даёт пустую полосу, а перебор через ширину не ломает вёрстку: полоса
 * упирается в потолок и получает признак `over`, по которому отрисовка ставит
 * акцентный цвет. Обрезать полосу молча было бы враньём в другую сторону.
 */
export function dayBar(seconds, width = BAR_WIDTH, unit = BAR_UNIT) {
    const secs = Math.max(0, Math.floor(Number(seconds) || 0));
    const units = Math.round(secs / unit);
    const filled = Math.min(width, units);
    return {
        text: '█'.repeat(filled) + '░'.repeat(Math.max(0, width - filled)),
        filled,
        over: units > width,
    };
}

/**
 * Полоса списка — ОТНОСИТЕЛЬНАЯ шкала: самая долгая строка занимает всю
 * ширину, остальные — свою долю от неё.
 *
 * Ненулевое время всегда даёт хотя бы один знак: строка с полосой нулевой
 * длины читается как «ничего не было», а это не так.
 */
export function shareBar(seconds, max, width = LIST_BAR_WIDTH) {
    const secs = Math.max(0, Math.floor(Number(seconds) || 0));
    const top = Math.max(0, Math.floor(Number(max) || 0));
    if (!secs || !top) return '';
    const n = Math.max(1, Math.min(width, Math.round((secs / top) * width)));
    return '▓'.repeat(n);
}

/** Доля строки в процентах (целых); нулевой итог даёт `0`. */
export function pct(seconds, total) {
    const t = Number(total) || 0;
    if (t <= 0) return 0;
    return Math.round(((Number(seconds) || 0) / t) * 100);
}

/**
 * Четыре блока шапки — ЧИСТАЯ функция над `kpi` из ответа сервера.
 *
 * Своей арифметики здесь нет: и среднее, и «закрыто», и «максимальный фокус»
 * посчитаны в report.rs. Единственное, что делается тут, — подписи.
 */
export function kpiCards(kpi, view) {
    const k = kpi || {};
    const period = (view && view.period) || 'week';
    const word = { day: 'за день', week: 'за неделю', month: 'за месяц' }[period] || 'за период';

    const prev = k.prev_total_seconds;
    const totalHint = (prev === null || prev === undefined)
        ? `${k.days || 1} дн. в периоде`
        : `${fmtDelta((Number(k.total_seconds) || 0) - Number(prev))} к прошлой`;

    const focusTask = view && view.tasksById && k.max_focus_task_id
        ? view.tasksById.get(k.max_focus_task_id)
        : null;
    const focusParts = [];
    if (k.max_focus_day) {
        const d = dateOfKey(k.max_focus_day);
        if (d) focusParts.push(DOW[d.getDay()]);
    }
    if (focusTask) {
        focusParts.push(focusTask.tags && focusTask.tags.length
            ? `#${focusTask.tags[0]}`
            : focusTask.title);
    }

    return [
        {
            id: 'total',
            label: '// TOTAL',
            value: fmtHm(k.total_seconds),
            hint: totalHint,
        },
        {
            id: 'avg',
            label: '// AVG/DAY',
            value: fmtHm(k.avg_day_seconds),
            hint: `на ${k.days || 1} дн. периода`,
        },
        {
            id: 'closed',
            label: '// CLOSED',
            value: `${k.closed_tasks || 0}/${k.touched_tasks || 0}`,
            hint: `задач ${word}`,
        },
        {
            id: 'focus',
            label: '// MAX FOCUS',
            value: fmtHm(k.max_focus_seconds),
            hint: focusParts.length ? focusParts.join(' · ') : 'самый долгий отрезок',
        },
    ];
}

/** Экран пуст, когда за период не набежало ни секунды. */
export function isEmptyReport(view) {
    const rows = (view && view.byDay && view.byDay.rows) || [];
    const total = Number(view && view.byDay && view.byDay.total_seconds) || 0;
    return total <= 0 && rows.length === 0;
}

// ------------------------------------------------------------------------ CSV
//
// ФОРМАТ ЗАФИКСИРОВАН: разделитель `;`, кодировка UTF-8 С BOM, время в виде
// `Ч:ММ:СС` И отдельной колонкой целых минут. Точка с запятой и BOM — ради
// Excel с русской локалью: он делит по `;`, а без BOM читает файл как cp1251 и
// показывает кракозябры вместо кириллицы. Минуты числом нужны сводным
// таблицам: строку `3:08` они складывать не умеют, а дробные минуты уткнулись
// бы в десятичный разделитель, который у той же локали — запятая.

/** Экранирование поля CSV: кавычки удваиваются, поле берётся в кавычки. */
function csvField(value) {
    const s = value === null || value === undefined ? '' : String(value);
    return `"${s.replace(/"/g, '""')}"`;
}

/** Имя файла выгрузки: `tracker-report-2026-09-01_2026-09-07.csv`. */
export function csvFileName(win) {
    const last = addDays(win.to, -1);
    return `tracker-report-${dayKey(win.from)}_${dayKey(last)}.csv`;
}

/**
 * Весь показанный экран в CSV — ЧИСТАЯ функция.
 *
 * Выгружается ровно то и ровно за тот период, что на экране: сначала четыре
 * KPI, затем три разреза теми же строками и в том же порядке. Никакого
 * пересчёта — те же числа из тех же ответов сервера.
 */
export function reportToCsv(view) {
    const head = ['Раздел', 'Ключ', 'Название', 'Время', 'Минуты', 'Доля %'];
    const lines = [head.map(csvField).join(';')];
    const push = (section, key, name, seconds, share) => {
        lines.push([
            section,
            key,
            name,
            fmtDuration(seconds),
            String(Math.round((Number(seconds) || 0) / 60)),
            share === null || share === undefined ? '' : String(share),
        ].map(csvField).join(';'));
    };

    lines.push(csvField(`Период: ${windowLabel(view)}`));
    kpiCards(view.kpi, view).forEach((c) => {
        lines.push([
            'KPI',
            c.id,
            `${c.label.replace('// ', '')} (${c.hint})`,
            c.value,
            '',
            '',
        ].map(csvField).join(';'));
    });

    const sections = [
        ['По дням', view.byDay, (r) => dayRowLabel(r.key)],
        ['По спискам', view.byList, (r) => r.label],
        ['По задачам', view.byTask, (r) => r.label],
    ];
    sections.forEach(([name, report, label]) => {
        const rows = (report && report.rows) || [];
        const total = Number(report && report.total_seconds) || 0;
        rows.forEach((r) => push(name, r.key, label(r), r.seconds, pct(r.seconds, total)));
    });

    return lines.join('\r\n');
}

// ------------------------------------------------------------------- отрисовка

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = text;
    return n;
}

function block(title) {
    const b = el('section', 'trk-r-block');
    b.appendChild(el('div', 'trk-r-block-title', title));
    return b;
}

function kpiRow(view) {
    const row = el('div', 'trk-r-kpi');
    kpiCards(view.kpi, view).forEach((c) => {
        const card = el('div', 'trk-r-card');
        card.dataset.trkKpi = c.id;
        card.appendChild(el('div', 'trk-r-card-label', c.label));
        card.appendChild(el('div', 'trk-r-card-value', c.value));
        card.appendChild(el('div', 'trk-r-card-hint', c.hint));
        row.appendChild(card);
    });
    return row;
}

function byDayBlock(view) {
    const b = block('// BY DAY — bar = 15 мин');
    const rows = (view.byDay && view.byDay.rows) || [];
    if (!rows.length) {
        b.appendChild(el('div', 'trk-r-empty', 'ни одного отрезка за период'));
        return b;
    }
    rows.forEach((r) => {
        const line = el('div', 'trk-r-day');
        if (r.key === view.todayKey) line.classList.add('today');
        line.dataset.trkRDay = r.key;
        line.appendChild(el('span', 'trk-r-day-label', dayRowLabel(r.key)));
        const bar = dayBar(r.seconds);
        const barNode = el('span', 'trk-r-bar', bar.text);
        if (bar.over) barNode.classList.add('over');
        line.appendChild(barNode);
        line.appendChild(el('span', 'trk-r-day-time', fmtHm(r.seconds)));
        b.appendChild(line);
    });
    return b;
}

function byListBlock(view) {
    const b = block('// BY LIST');
    const rows = (view.byList && view.byList.rows) || [];
    const total = Number(view.byList && view.byList.total_seconds) || 0;
    if (!rows.length) {
        b.appendChild(el('div', 'trk-r-empty', 'списков в периоде нет'));
        return b;
    }
    const max = rows.reduce((m, r) => Math.max(m, Number(r.seconds) || 0), 0);
    rows.forEach((r) => {
        const line = el('div', 'trk-r-list');
        line.dataset.trkRList = r.key;
        line.appendChild(el('span', 'trk-r-list-name', r.label));
        line.appendChild(el('span', 'trk-r-bar', shareBar(r.seconds, max)));
        line.appendChild(el('span', 'trk-r-list-time', fmtHm(r.seconds)));
        line.appendChild(el('span', 'trk-r-list-pct', `${pct(r.seconds, total)}%`));
        b.appendChild(line);
    });
    return b;
}

function byTaskBlock(view) {
    const b = block('// TOP NODES');
    const rows = ((view.byTask && view.byTask.rows) || []).slice(0, view.topLimit || 8);
    if (!rows.length) {
        b.appendChild(el('div', 'trk-r-empty', 'задач с временем в периоде нет'));
        return b;
    }
    rows.forEach((r) => {
        const task = view.tasksById ? view.tasksById.get(r.key) : null;
        const line = el('div', 'trk-r-node');
        line.dataset.trkRTask = r.key;
        const title = el('span', 'trk-r-node-title', r.label);
        line.appendChild(title);
        const listName = task && task.list_id && view.listsById
            ? (view.listsById.get(task.list_id) || {}).name
            : null;
        line.appendChild(el('span', 'trk-r-node-list', listName ? `[${listName}]` : ''));
        line.appendChild(el('span', 'trk-r-node-time', fmtHm(r.seconds)));
        if (task && view.onOpen) {
            line.classList.add('clickable');
            line.addEventListener('click', () => view.onOpen(task));
        }
        b.appendChild(line);
    });
    return b;
}

/**
 * Отрисовка экрана сводки.
 *
 * Пустой период — это ОТДЕЛЬНОЕ состояние, а не четыре нуля: нули читаются как
 * «сломалось», а человеку надо сказать, что именно за этот период времени нет,
 * и предложить сдвинуть окно.
 */
export function renderReport(container, view) {
    if (!container) return;
    container.textContent = '';
    if (!view || !view.byDay) {
        container.appendChild(el('div', 'trk-empty-hint', 'сводка загружается…'));
        return;
    }

    const wrap = el('div', 'trk-r-wrap');
    wrap.appendChild(kpiRow(view));

    if (isEmptyReport(view)) {
        const empty = el('div', 'trk-r-none');
        empty.appendChild(el('div', 'trk-r-none-title', `// НЕТ ДАННЫХ — ${windowLabel(view)}`));
        empty.appendChild(el('div', 'trk-r-none-hint',
            'за этот период не записано ни одного отрезка. Сдвиньте период кнопками [◀] [▶] или запустите таймер задачи.'));
        wrap.appendChild(empty);
        container.appendChild(wrap);
        return;
    }

    wrap.appendChild(byDayBlock(view));
    const split = el('div', 'trk-r-split');
    split.appendChild(byListBlock(view));
    split.appendChild(byTaskBlock(view));
    wrap.appendChild(split);
    container.appendChild(wrap);
}
