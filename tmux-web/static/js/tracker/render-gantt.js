// tmux-web — плагин forge-tracker: режим `gantt`.
//
// Экран по макету (~/Documents/ForgeTracker.pen, «Экран — GANTT»):
//
//     // NODE                        01  02  03  04 …  14    est   факт
//     // MILESTONES                          ▼ today  ◆ …
//     // work — 4
//     P1  Свести дизайн трекера      ████████▒▒▒▒▒▒▒▒◆        ~4ч   5:12
//     P0  Flood control VK: код 9    ████████◆                ~1ч   4:38   ← красный
//
// СЕТКА — ЭТО СИМВОЛЫ, А НЕ ПИКСЕЛИ. Одна колонка дня = `CELL` знаков
// моноширинного шрифта, и полоса задачи строится как СТРОКА такой же длины,
// а не как absolute-блок поверх контейнера. Причина простая: шапка дней,
// дорожка вех и все полосы обязаны стоять ровно друг под другом при любой
// ширине окна и любом масштабе шрифта. Пиксельная раскладка требует, чтобы
// три разных механизма (ширина ячейки шапки, left/width полосы, позиция
// ромба) давали одно и то же число; символьная — не требует ничего, потому
// что выравнивание делает сам шрифт. Побочная выгода: полоса становится
// ЧИСТОЙ ФУНКЦИЕЙ строки, и её можно проверить node-тестом без DOM
// (tests/frontend/tracker_gantt.test.mjs).
//
// ЦВЕТ ВНУТРИ ОДНОЙ ПОЛОСЫ РАЗНЫЙ, ПОЭТОМУ СТРОКА РЕЖЕТСЯ НА ПРОГОНЫ.
// `buildTrack` возвращает не строку, а массив `[{text, cls}]` — подряд идущие
// знаки одного цвета склеены в один прогон, и на 14 дней их обычно 3-4. Так
// в DOM попадает несколько `<span>`, а не 56 штук по одному на знак.
//
// ЧТО СЧИТАЕТ СЕРВЕР, А ЧТО ФРОНТ. Полосы и вехи окна приходят из
// `GET /api/tracker/gantt?from&to` (plugins/tracker/src/report.rs::build_gantt):
// сервер решает, какая задача попала в окно, и он же складывает факт. Фронт
// НЕ пересчитывает суммы — он лишь раскладывает уже посчитанное по клеткам.
// Единственное, что здесь вычисляется заново, — признак превышения оценки:
// правило одно в одно как в `build_gantt` (`факт > оценка`, без оценки —
// никогда), но берётся от ЖИВОГО времени задачи, иначе полоса идущего таймера
// краснела бы только после следующего запроса.
//
// ЗАДАЧА БЕЗ `start` И БЕЗ `due` ПОЛОСЫ НЕ ПОЛУЧАЕТ — и это осознанно.
// Нарисовать её «где-то по факту» нечем: в данных Гантта нет разбивки времени
// по дням, а выдумать день значит соврать. Такая задача остаётся строкой с
// колонками `est` и `факт` и явной подписью `— без плана —` в дорожке: видно,
// что она есть, и видно, чего ей не хватает.
//
// НИКАКОГО innerHTML: заголовок задачи и подпись вехи — пользовательский
// текст и попадают в DOM только через textContent.

import { fmtDuration, parseTs, sortTasks } from './render-list.js';

/** Знаков моноширинной сетки на один день. Взято из макета (`01  02  03`). */
export const CELL = 4;

/**
 * Часов в «дне плана».
 *
 * Нужно ровно в одном случае: у задачи есть оценка и ровно одна из дат
 * (`start` ИЛИ `due`), и длину полосы больше взять неоткуда. Восемь — это
 * рабочий день; календарные 24 растянули бы часовую задачу в точку, а
 * недельная оценка схлопнулась бы в трое суток.
 */
const HOURS_PER_DAY = 8;

/** Символы сетки — те же, что в макете. */
const GLYPH = {
    fact: '█',
    plan: '▒',
    due: '◆',
    milestone: '◆',
    today: '▼',
    blank: ' ',
};

// ------------------------------------------------------------------ календарь

/** Локальный ключ дня `YYYY-MM-DD` (та же форма, что у голой даты в сторе). */
export function dayKey(date) {
    const d = date instanceof Date ? date : new Date(date);
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Локальная полночь переданного дня. */
export function dayStart(date) {
    const d = date instanceof Date ? new Date(date.getTime()) : new Date(date);
    d.setHours(0, 0, 0, 0);
    return d;
}

/** Сдвиг на `n` суток от локальной полночи (переход через DST безопасен). */
export function addDays(date, n) {
    const d = dayStart(date);
    d.setDate(d.getDate() + n);
    return d;
}

/**
 * Ось дней окна — ЧИСТАЯ функция.
 *
 * @param {Date|number|string} from — первый день окна
 * @param {number} span — сколько дней в окне
 * @param {Date} [today] — «сегодня» (параметр ради тестов, а не ради красоты)
 * @returns {Array<{key,label,date,today,weekend}>}
 */
export function buildDays(from, span, today) {
    const first = dayStart(from);
    const todayKey = dayKey(today || new Date());
    const out = [];
    for (let i = 0; i < Math.max(0, span); i += 1) {
        const d = addDays(first, i);
        const wd = d.getDay();
        out.push({
            key: dayKey(d),
            label: String(d.getDate()).padStart(2, '0'),
            date: d,
            today: dayKey(d) === todayKey,
            weekend: wd === 0 || wd === 6,
        });
    }
    return out;
}

/**
 * Окно, содержащее указанный день.
 *
 * Якорь — ПЕРВЫЙ день окна, а не «сегодня по центру»: человек листает окна
 * встык (`[◀]`/`[▶]` двигают ровно на длину окна), и центрирование давало бы
 * при каждом шаге новый, ни с чем не совпадающий отрезок.
 */
export function windowFor(anchor, span) {
    const from = dayStart(anchor);
    return { from, to: addDays(from, Math.max(1, span)) };
}

/**
 * Смещение даты от начала окна в сутках — МОЖЕТ БЫТЬ отрицательным и больше
 * длины окна.
 *
 * Именно поэтому это не «индекс в массиве»: полоса задачи, начавшейся до окна
 * и кончающейся внутри, обязана быть обрезана слева, а не потеряна. Разность
 * берётся между локальными полуночами и округляется — иначе переход на летнее
 * время внутри окна сдвинул бы всё правее него на день.
 */
export function dayOffset(days, raw) {
    const d = parseTs(raw);
    if (!d || !days.length) return null;
    const ms = dayStart(d).getTime() - days[0].date.getTime();
    return Math.round(ms / 86400000);
}

/** Индекс дня в оси или `-1` (смещение, попавшее внутрь окна). */
export function dayIndexOf(days, raw) {
    const off = dayOffset(days, raw);
    return off === null || off < 0 || off >= days.length ? -1 : off;
}

// ------------------------------------------------------------------- полосы

/**
 * Плановый отрезок задачи в индексах дней — ЧИСТАЯ функция.
 *
 * Правила, в порядке убывания надёжности источника:
 *
 *   `start` и `due`   → ровно эти дни (что человек задал, то и рисуем);
 *   только `start`    → от начала вперёд на длину оценки;
 *   только `due`      → назад от срока на длину оценки (срок — правый край);
 *   ни того, ни другого → полосы нет.
 *
 * Длина оценки — `ceil(оценка / 8ч)`, минимум один день. Без оценки одиночная
 * дата даёт полосу в один день: это честнее, чем растянуть её наугад.
 *
 * Отрезок обрезается по окну; полностью выпавший из окна даёт `null`.
 *
 * @returns {{from:number,to:number}|null} включительные индексы
 */
export function planSpan(bar, days) {
    if (!bar || !days.length) return null;
    const s = dayOffset(days, bar.start);
    const e = dayOffset(days, bar.due);
    if (s === null && e === null) return null;

    const est = Number(bar.estimate_min) || 0;
    const len = est > 0 ? Math.max(1, Math.ceil(est / (60 * HOURS_PER_DAY))) : 1;

    let from;
    let to;
    if (s !== null && e !== null) {
        // Даты могли быть заданы «наоборот»: чинить стор не наше дело, но и
        // рисовать полосу отрицательной длины нельзя.
        from = Math.min(s, e);
        to = Math.max(s, e);
    } else if (s !== null) {
        from = s;
        to = s + len - 1;
    } else {
        to = e;
        from = e - len + 1;
    }

    // Полоса целиком мимо окна — её нет. Частично — обрезается по краю.
    if (to < 0 || from > days.length - 1) return null;
    from = Math.max(0, from);
    to = Math.min(days.length - 1, to);
    return { from, to };
}

/**
 * Превышена ли оценка — ПРАВИЛО ОДНО НА ФРОНТ И БЭК.
 *
 * Копия `build_gantt` (plugins/tracker/src/report.rs): без оценки превышения
 * не бывает никогда — «перерасход» без плана не имеет смысла. Считается здесь
 * заново только затем, чтобы полоса идущей задачи краснела в тот же миг, а не
 * после следующего `GET /api/tracker/gantt`.
 */
export function isOver(bar, seconds) {
    const est = Number(bar && bar.estimate_min) || 0;
    if (est <= 0) return false;
    return Number(seconds || 0) > est * 60;
}

/** Склейка соседних знаков одного класса в прогоны. */
function runsOf(chars, classes) {
    const out = [];
    for (let i = 0; i < chars.length; i += 1) {
        const cls = classes[i] || '';
        const last = out[out.length - 1];
        if (last && last.cls === cls) last.text += chars[i];
        else out.push({ text: chars[i], cls });
    }
    return out;
}

/**
 * Строка полосы задачи — ЧИСТАЯ функция.
 *
 * Заливка: доля `факт / оценка` от длины полосы. Без оценки полоса заливается
 * целиком, если по задаче есть хоть секунда, — иначе остаётся плановой.
 * Ненулевой факт всегда даёт минимум один знак `█`: полоса, где работа была,
 * но не видна, вводит в заблуждение сильнее, чем округление.
 *
 * Превышение окрашивает ВСЮ полосу и убирает плановый хвост: план уже сорван,
 * и показывать в нём «остаток» нечего.
 *
 * Ромб срока `◆` ставится последним знаком клетки своего дня и перекрывает
 * знак полосы: он должен быть виден и тогда, когда срок пришёлся на середину
 * полосы (плановое начало позже срока — законное состояние стора).
 *
 * @returns {Array<{text:string,cls:string}>}
 */
export function buildTrack(bar, days, opts) {
    const o = opts || {};
    const width = days.length * CELL;
    const chars = new Array(width).fill(GLYPH.blank);
    const classes = new Array(width).fill('');

    const span = planSpan(bar, days);
    const seconds = Number(o.seconds !== undefined ? o.seconds : (bar && bar.actual_seconds)) || 0;
    const over = o.over !== undefined ? !!o.over : isOver(bar, seconds);

    if (span) {
        const start = span.from * CELL;
        const total = (span.to - span.from + 1) * CELL;
        const est = Number(bar.estimate_min) || 0;
        let filled;
        if (over) {
            filled = total;
        } else if (est > 0) {
            filled = Math.round((seconds / (est * 60)) * total);
            filled = Math.max(0, Math.min(total, filled));
            if (seconds > 0 && filled === 0) filled = 1;
        } else {
            filled = seconds > 0 ? total : 0;
        }
        for (let i = 0; i < total; i += 1) {
            const factPart = i < filled;
            chars[start + i] = factPart ? GLYPH.fact : GLYPH.plan;
            classes[start + i] = over ? 'over' : (factPart ? 'fact' : 'plan');
        }
    }

    const dueIdx = dayIndexOf(days, bar && bar.due);
    if (dueIdx >= 0) {
        const at = dueIdx * CELL + CELL - 1;
        chars[at] = GLYPH.due;
        classes[at] = over ? 'over' : 'due';
    }

    return runsOf(chars, classes);
}

/**
 * Строка дорожки `// MILESTONES` — ЧИСТАЯ функция.
 *
 * `▼` — сегодня, `◆` — веха. Обе метки живут в клетке своего дня; если в один
 * день попало несколько меток, каждая занимает следующий свободный знак этой
 * же клетки. Больше `CELL` меток в один день не поместится — лишние не рисуются
 * в дорожке, но остаются в списке подписей под ней, поэтому не пропадают.
 *
 * Веха вне окна не рисуется: сервер её и не пришлёт (`build_gantt` фильтрует
 * по окну), но проверка тут своя — дорожка не должна зависеть от того, кто её
 * наполнил.
 */
export function buildLane(milestones, days, today) {
    const width = days.length * CELL;
    const chars = new Array(width).fill(GLYPH.blank);
    const classes = new Array(width).fill('');

    const put = (idx, glyph, cls) => {
        if (idx < 0 || idx >= days.length) return false;
        for (let k = 0; k < CELL; k += 1) {
            const at = idx * CELL + k;
            if (chars[at] === GLYPH.blank) {
                chars[at] = glyph;
                classes[at] = cls;
                return true;
            }
        }
        return false;
    };

    const todayIdx = days.findIndex((d) => d.key === dayKey(today || new Date()));
    put(todayIdx, GLYPH.today, 'today');

    (Array.isArray(milestones) ? milestones : []).forEach((m) => {
        if (!m) return;
        put(dayIndexOf(days, m.date), GLYPH.milestone, 'ms');
    });

    return runsOf(chars, classes);
}

// -------------------------------------------------------------- группировка

/**
 * Разложить полосы по спискам — ЧИСТАЯ функция.
 *
 * Порядок групп — порядок списков в сторе (`order` держит сервер), задачи без
 * списка идут последней группой `// без списка`. Внутри группы порядок тот же,
 * что в ленте и на доске: ручной `order`, затем момент создания. Своей
 * сортировки «по дате начала» здесь НЕТ намеренно — иначе одна и та же задача
 * стояла бы в трёх режимах на трёх разных местах.
 *
 * @param {Array} bars — полосы из `GET /api/tracker/gantt`
 * @param {Map} tasksById — задачи снимка (за `order` и `priority`)
 * @param {Array} lists — списки снимка в их порядке
 */
export function groupBars(bars, tasksById, lists) {
    const byList = new Map();
    const order = [];
    (Array.isArray(lists) ? lists : []).forEach((l) => {
        if (l && l.id) { byList.set(l.id, []); order.push(l); }
    });
    const orphan = [];

    (Array.isArray(bars) ? bars : []).forEach((b) => {
        if (!b) return;
        const bucket = b.list_id && byList.has(b.list_id) ? byList.get(b.list_id) : orphan;
        bucket.push(b);
    });

    const sortBucket = (arr) => {
        const tasks = arr.map((b) => {
            const t = tasksById.get(b.task_id);
            return t || { id: b.task_id, title: b.title, order: null, created_at: null };
        });
        const sorted = sortTasks(tasks);
        const byId = new Map(arr.map((b) => [b.task_id, b]));
        return sorted.map((t) => byId.get(t.id)).filter(Boolean);
    };

    const groups = [];
    order.forEach((l) => {
        const arr = byList.get(l.id);
        if (arr.length) groups.push({ id: l.id, name: l.name, bars: sortBucket(arr) });
    });
    if (orphan.length) groups.push({ id: null, name: 'без списка', bars: sortBucket(orphan) });
    return groups;
}

// -------------------------------------------------------------------- DOM

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = text;
    return n;
}

function btn(cls, text, title) {
    const b = el('button', cls, text);
    b.type = 'button';
    if (title) b.title = title;
    return b;
}

/** Оценка в короткой записи `~4ч` — та же форма, что на карточке доски. */
function estLabel(bar) {
    const m = Number(bar.estimate_min) || 0;
    if (!m) return '—';
    const h = Math.floor(m / 60);
    const min = m % 60;
    if (h && min) return `~${h}ч${min}м`;
    if (h) return `~${h}ч`;
    return `~${min}м`;
}

/** Короткая дата `05.09` для подписи вехи. */
export function shortDate(raw) {
    const d = parseTs(raw);
    if (!d) return String(raw || '');
    const p = (n) => String(n).padStart(2, '0');
    return `${p(d.getDate())}.${p(d.getMonth() + 1)}`;
}

/** Ячейка дорожки: прогоны символов, каждый своим цветом. */
function trackCell(runs) {
    const cell = el('div', 'trk-g-track');
    runs.forEach((r) => {
        if (!r.text) return;
        cell.appendChild(el('span', r.cls ? `trk-g-${r.cls}` : null, r.text));
    });
    return cell;
}

/** Шапка сетки: `// NODE` + номера дней + `est` + `факт`. */
function headRow(days) {
    const row = el('div', 'trk-g-row trk-g-head');
    row.appendChild(el('div', 'trk-g-name', '// NODE'));
    const cell = el('div', 'trk-g-track');
    days.forEach((d) => {
        const s = el(
            'span',
            `trk-g-day${d.today ? ' today' : ''}${d.weekend ? ' weekend' : ''}`,
            d.label.padEnd(CELL, ' '),
        );
        cell.appendChild(s);
    });
    row.appendChild(cell);
    row.appendChild(el('div', 'trk-g-est', 'est'));
    row.appendChild(el('div', 'trk-g-factcol', 'факт'));
    return row;
}

/**
 * Дорожка вех: строка меток и подписи-чипы под ней.
 *
 * ПОЧЕМУ ПОДПИСИ ВЫНЕСЕНЫ В ОТДЕЛЬНУЮ СТРОКУ. В макете `▼ today`,
 * `◆ дизайн принят` и `◆ релиз 0.9` стоят прямо в дорожке текстом. На живых
 * данных так нельзя: подпись длиннее клетки дня, и две вехи на соседних днях
 * либо наезжают друг на друга, либо уводят метку от своего дня. Поэтому в
 * дорожке остаются только сами метки (они точно над своим днём), а подписи
 * идут чипами под ней — и заодно становятся кнопками правки вехи.
 *
 * @returns {Array<HTMLElement>} строка сетки и полоса чипов
 */
function laneBlock(days, view) {
    const out = [];

    const row = el('div', 'trk-g-row trk-g-ms-row');
    row.appendChild(el('div', 'trk-g-name', '// MILESTONES'));
    row.appendChild(trackCell(buildLane(view.milestones, days, view.now)));
    row.appendChild(el('div', 'trk-g-est', ''));
    row.appendChild(el('div', 'trk-g-factcol', ''));
    out.push(row);

    const chips = el('div', 'trk-g-chips');
    chips.appendChild(el('span', 'trk-g-chip-today', `▼ today ${shortDate(dayKey(view.now))}`));
    (view.milestones || []).forEach((m) => {
        if (!m) return;
        const c = btn('trk-g-chip', `◆ ${m.title} · ${shortDate(m.date)}`, 'изменить или удалить веху');
        c.addEventListener('click', () => view.onEditMilestone && view.onEditMilestone(m));
        chips.appendChild(c);
    });
    const add = btn('trk-g-chip trk-g-chip-add', '[+ веха]', 'создать веху');
    add.addEventListener('click', () => view.onAddMilestone && view.onAddMilestone());
    chips.appendChild(add);
    out.push(chips);

    return out;
}

/** Одна строка задачи. */
function barRow(bar, days, view) {
    const running = view.runningIds.has(bar.task_id);
    const seconds = view.taskSeconds[bar.task_id] !== undefined
        ? Number(view.taskSeconds[bar.task_id])
        : Number(bar.actual_seconds) || 0;
    const over = isOver(bar, seconds);

    const row = el('div', `trk-g-row trk-g-task${running ? ' running' : ''}${over ? ' over' : ''}`);
    row.dataset.taskId = bar.task_id;

    const name = el('div', 'trk-g-name');
    name.appendChild(el('span', 'trk-g-prio', `P${priorityOf(view, bar)}`));
    name.appendChild(el('span', 'trk-g-title', bar.title || '(без имени)'));
    name.title = bar.title || '';
    name.addEventListener('click', () => view.onOpen && view.onOpen(taskOf(view, bar)));
    row.appendChild(name);

    // Ни полосы, ни ромба срока в окне — рисовать нечего, и вместо строки из
    // 56 пробелов честнее сказать словами, чего задаче не хватает.
    if (!planSpan(bar, days) && dayIndexOf(days, bar.due) < 0) {
        const cell = el('div', 'trk-g-track trk-g-noplan', '— без плана —');
        row.appendChild(cell);
    } else {
        row.appendChild(trackCell(buildTrack(bar, days, { seconds, over })));
    }

    row.appendChild(el('div', 'trk-g-est', estLabel(bar)));

    const fact = el('div', 'trk-g-factcol', seconds ? fmtDuration(seconds) : '—');
    // Тот же data-атрибут, что в ленте и на доске: общий тикер панели
    // (js/tracker/timer.js) найдёт его сам, своего таймера Гантту не нужно.
    if (seconds || running) fact.dataset.trkTaskTime = bar.task_id;
    row.appendChild(fact);

    return row;
}

/** Задача снимка по полосе (для колбэка открытия карточки). */
function taskOf(view, bar) {
    return view.tasksById.get(bar.task_id) || { id: bar.task_id, title: bar.title };
}
/** Приоритет из снимка: в полосе Гантта его нет, а в макете он есть. */
function priorityOf(view, bar) {
    const t = view.tasksById.get(bar.task_id);
    const p = t && Number(t.priority);
    return Number.isFinite(p) ? p : 3;
}

/**
 * Отрисовать режим `gantt`.
 *
 * @param {HTMLElement} container — `#trk-content`
 * @param {object} view
 * @param {Array} view.bars — полосы окна (уже отфильтрованные выборкой панели)
 * @param {Array} view.milestones — вехи окна
 * @param {Array} view.days — ось дней (`buildDays`)
 * @param {Map} view.tasksById — задачи снимка
 * @param {Array} view.lists — списки снимка в их порядке
 * @param {object} view.taskSeconds — живые суммы по задачам
 * @param {Set} view.runningIds — задачи с идущим таймером
 * @param {Set} view.collapsed — id свёрнутых групп (`''` — «без списка»)
 * @param {Date} view.now
 * @param {Function} view.onOpen, view.onToggleGroup
 * @param {Function} view.onAddMilestone, view.onEditMilestone
 */
export function renderGantt(container, view) {
    if (!container) return;
    container.textContent = '';
    const days = view.days || [];

    const grid = el('div', 'trk-g-grid');
    grid.appendChild(headRow(days));
    laneBlock(days, view).forEach((n) => grid.appendChild(n));

    const groups = groupBars(view.bars, view.tasksById, view.lists);
    if (!groups.length) {
        grid.appendChild(el('div', 'trk-g-empty', 'в этом окне ничего нет — сдвиньте период или снимите фильтр'));
    }

    groups.forEach((g) => {
        const key = g.id || '';
        const collapsed = view.collapsed.has(key);
        const head = el('div', 'trk-g-group');
        const b = btn('trk-g-group-btn', `${collapsed ? '▸' : '▾'} // ${g.name} — ${g.bars.length}`);
        b.addEventListener('click', () => view.onToggleGroup && view.onToggleGroup(key));
        head.appendChild(b);
        grid.appendChild(head);
        if (collapsed) return;
        g.bars.forEach((bar) => grid.appendChild(barRow(bar, days, view)));
    });

    const scroller = el('div', 'trk-g-scroll');
    scroller.appendChild(grid);
    container.appendChild(scroller);

    const legend = el('div', 'trk-g-legend');
    [
        ['fact', '█', 'факт'],
        ['plan', '▒', 'план по оценке'],
        ['over', '█', 'превышение оценки'],
        ['run', '▌', 'полоса слева — идёт таймер'],
        ['due', '◆', 'веха / срок задачи'],
        ['today', '▼', 'сегодня'],
    ].forEach(([cls, glyph, text]) => {
        const item = el('span', 'trk-g-leg');
        item.appendChild(el('span', `trk-g-${cls}`, glyph));
        item.appendChild(el('span', null, text));
        legend.appendChild(item);
    });
    container.appendChild(legend);
}
