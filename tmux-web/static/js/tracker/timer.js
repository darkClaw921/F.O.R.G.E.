// tmux-web — плагин forge-tracker: живые часы и блок `// RUNNING`.
//
// ОДИН ТИКЕР НА ВСЮ ПАНЕЛЬ, А НЕ ТАЙМЕР НА СТРОКУ. Идущих отрезков может быть
// сколько угодно (параллельные таймеры по разным задачам — штатный сценарий
// трекера), и `setInterval` на каждую строку означал бы N таймеров, которые
// надо по одному снимать при перерисовке ленты. Одна отрисовка ленты — и
// половина таймеров осиротела бы, продолжая жить в фоне. Поэтому тикер здесь
// ровно один: раз в секунду он зовёт колбэк, а тот обновляет ТЕКСТ уже
// существующих узлов. Перерисовки DOM каждую секунду тоже нет — она сбрасывала
// бы фокус в полях, прокрутку ленты и начатый drag'n'drop.
//
// ВРЕМЯ СЧИТАЕТСЯ ОТ ОПОРНОЙ ТОЧКИ, А НЕ НАКОПЛЕНИЕМ ШАГОВ. Наивный
// `seconds += 1` на каждый тик расходится с реальностью в первый же раз, когда
// вкладку скрыли (браузер душит фоновые таймеры) или машину усыпили. Здесь
// каждое значение вычисляется заново: `сейчас − started_at`. «Сейчас» берётся
// не с часов браузера, а восстанавливается из снимка:
//
//     serverNow = snap.now + (Date.now() − момент получения снимка)
//
// Поле `now` кладёт в снимок сервер (см. plugins/tracker/src/dto.rs) именно
// ради этого: часы браузера и сервера расходятся на секунды и минуты, и без
// опорной точки секундомер прыгал бы при каждом новом снимке.
//
// СУММЫ НЕ ПЕРЕСЧИТЫВАЮТСЯ С НУЛЯ. Правила учёта (время пункта чек-листа
// входит в сумму задачи ровно один раз, окно дня и недели — локальное) живут
// в plugins/tracker/src/report.rs и покрыты тестами. Фронт берёт посчитанное
// сервером значение на момент `snap.now` и ДОБАВЛЯЕТ к нему то, что натикало
// после — по одному разу на каждый идущий отрезок. Это не дубль правил, а их
// продолжение во времени.
//
// ПОЛНОЧЬ. Добавка «сколько прошло с момента снимка» верна, пока снимок и
// «сейчас» лежат в одном локальном дне. После полуночи `TODAY` обязан
// обнулиться, а такой арифметикой этого не выразить — поэтому переход суток
// не досчитывается, а честно требует свежий снимок (`dayRolledOver`).

import { fmtDuration, parseTs } from './render-list.js';

/** Период тика. Секунда — цена деления в макете (`0:42:19`). */
const TICK_MS = 1000;

const local = {
    /** Идентификатор setInterval или null. */
    handle: null,
    /** Колбэк тика. */
    onTick: null,
};

// -------------------------------------------------------------- опорная точка

/**
 * Опора для пересчёта: серверное «сейчас» снимка и момент его получения.
 *
 * ЧИСТАЯ функция от снимка и текущих часов браузера — берётся ровно один раз,
 * при применении снимка, и дальше не меняется до следующего снимка.
 *
 * @param {object} snap — снимок состояния (поле `now`)
 * @param {number} [clientMs] — `Date.now()` на момент получения
 * @returns {{snapNowMs: number, baseClientMs: number}}
 */
export function clockFromSnapshot(snap, clientMs) {
    const base = Number.isFinite(clientMs) ? clientMs : Date.now();
    const d = parseTs(snap && snap.now);
    // Снимка без `now` быть не должно, но если сервер его почему-то не прислал,
    // опорой становятся часы браузера: расхождение в пару секунд лучше, чем
    // NaN во всех значениях панели.
    return { snapNowMs: d ? d.getTime() : base, baseClientMs: base };
}

/**
 * Серверное «сейчас» на текущий момент.
 *
 * @param {{snapNowMs: number, baseClientMs: number}} clock
 * @param {number} [clientMs] — `Date.now()`
 */
export function serverNow(clock, clientMs) {
    const now = Number.isFinite(clientMs) ? clientMs : Date.now();
    if (!clock) return now;
    return clock.snapNowMs + (now - clock.baseClientMs);
}

/**
 * Сколько натикало ПОСЛЕ снимка, в секундах (никогда не отрицательное).
 *
 * Именно эта величина добавляется к каждой серверной сумме, в которую входит
 * идущий отрезок.
 */
export function sinceSnapshot(clock, clientMs) {
    if (!clock) return 0;
    return Math.max(0, (serverNow(clock, clientMs) - clock.snapNowMs) / 1000);
}

/**
 * Сменились ли локальные сутки между снимком и «сейчас».
 *
 * `true` означает «арифметика добавки больше не действует для TODAY» — нужен
 * свежий снимок с сервера, а не досчёт на клиенте.
 */
export function dayRolledOver(clock, clientMs) {
    if (!clock) return false;
    const a = new Date(clock.snapNowMs);
    const b = new Date(serverNow(clock, clientMs));
    return a.getFullYear() !== b.getFullYear()
        || a.getMonth() !== b.getMonth()
        || a.getDate() !== b.getDate();
}

// ------------------------------------------------------------- живые значения

/** Длительность отрезка в секундах на момент `nowMs` (серверная шкала). */
export function entryElapsed(entry, nowMs) {
    if (!entry) return 0;
    const a = parseTs(entry.started_at);
    if (!a) return 0;
    const b = parseTs(entry.stopped_at);
    const end = b ? b.getTime() : nowMs;
    return Math.max(0, (end - a.getTime()) / 1000);
}

/** Активные отрезки снимка (защита от снимка без поля). */
function activeOf(snap) {
    return Array.isArray(snap && snap.active) ? snap.active.filter(Boolean) : [];
}

/**
 * Живые суммы по задачам: серверная база плюс добавка на каждый идущий отрезок.
 *
 * Отрезков по одной задаче не больше одного (инвариант стора), но код не
 * закладывается на это: добавка считается по фактическому числу активных
 * отрезков задачи. Если инвариант когда-нибудь ослабят, сумма останется верной.
 *
 * @returns {Object<string, number>} только те задачи, у которых есть добавка
 */
export function liveTaskSeconds(snap, clock, clientMs) {
    const delta = sinceSnapshot(clock, clientMs);
    const out = {};
    if (!delta) return out;
    const base = (snap && snap.task_seconds) || {};
    activeOf(snap).forEach((e) => {
        if (!e.task_id) return;
        const prev = out[e.task_id];
        out[e.task_id] = (prev === undefined ? (Number(base[e.task_id]) || 0) : prev) + delta;
    });
    return out;
}

/** Живые суммы по пунктам чек-листа — та же арифметика, что и по задачам. */
export function liveItemSeconds(snap, clock, clientMs) {
    const delta = sinceSnapshot(clock, clientMs);
    const out = {};
    if (!delta) return out;
    const base = (snap && snap.item_seconds) || {};
    activeOf(snap).forEach((e) => {
        if (!e.item_id) return;
        const prev = out[e.item_id];
        out[e.item_id] = (prev === undefined ? (Number(base[e.item_id]) || 0) : prev) + delta;
    });
    return out;
}

/**
 * Живые суммы статус-бара за сегодня и за неделю.
 *
 * Каждый идущий отрезок добавляет одну и ту же дельту: время, прошедшее с
 * момента снимка, попадает и в сегодняшний день, и в текущую неделю. Пока
 * сутки не сменились (см. `dayRolledOver`), это ровно то же, что посчитал бы
 * сервер.
 */
export function liveTotals(snap, clock, clientMs) {
    const totals = (snap && snap.totals) || {};
    const today = Number(totals.today_seconds) || 0;
    const week = Number(totals.week_seconds) || 0;
    const add = sinceSnapshot(clock, clientMs) * activeOf(snap).length;
    return { today_seconds: today + add, week_seconds: week + add };
}

// ------------------------------------------------------------------- тикер

/**
 * Запустить общий тик. Идемпотентна: второй вызов заменяет колбэк, но не
 * заводит второй интервал.
 *
 * @param {function(): void} onTick
 */
export function startTicker(onTick) {
    local.onTick = typeof onTick === 'function' ? onTick : null;
    if (local.handle !== null) return;
    local.handle = setInterval(() => {
        if (local.onTick) local.onTick();
    }, TICK_MS);
}

/** Остановить тик (уход с вкладки, скрытие окна, выгрузка страницы). */
export function stopTicker() {
    if (local.handle !== null) {
        clearInterval(local.handle);
        local.handle = null;
    }
    local.onTick = null;
}

/** Идёт ли тик прямо сейчас — для e2e и диагностики. */
export function isTicking() {
    return local.handle !== null;
}

// ------------------------------------------------------- блок `// RUNNING`

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = text;
    return n;
}

/** `started 14:02` из отметки начала отрезка. */
function startedLabel(entry) {
    const d = parseTs(entry.started_at);
    if (!d) return '';
    const p = (n) => String(n).padStart(2, '0');
    return `started ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * Отрисовать блок `// RUNNING — N`.
 *
 * Вызывается на изменение состава идущих отрезков, а НЕ каждую секунду: цифры
 * между отрисовками обновляет `tickRunning`, трогая только текстовые узлы.
 *
 * @param {HTMLElement} container — #trk-running
 * @param {{entries: Array, tasksById: Map, itemsById: Map, nowMs: number,
 *          onStop: function(object): void, onOpen: function(object): void}} view
 */
export function renderRunning(container, view) {
    if (!container) return;
    container.textContent = '';
    const entries = (view && Array.isArray(view.entries) ? view.entries : []).filter(Boolean);
    if (!entries.length) {
        container.hidden = true;
        return;
    }
    container.hidden = false;
    container.appendChild(el('div', 'trk-section-title', `// RUNNING — ${entries.length}`));

    entries.forEach((entry) => {
        const row = el('div', 'trk-run');
        row.dataset.entryId = entry.id;
        row.appendChild(el('span', 'trk-run-dot', '●'));

        const task = view.tasksById && view.tasksById.get(entry.task_id);
        const title = el('span', 'trk-run-task', (task && task.title) || entry.task_id);
        if (task && typeof view.onOpen === 'function') {
            title.classList.add('clickable');
            title.title = 'открыть карточку';
            title.addEventListener('click', () => view.onOpen(task));
        }
        row.appendChild(title);

        // Пункт чек-листа показывается рядом с задачей: в блоке `// RUNNING`
        // человек обязан видеть, ЧТО именно идёт, а не только «где-то в этой
        // задаче». Время всё равно принадлежит задаче — см. dto.rs.
        const item = entry.item_id && view.itemsById && view.itemsById.get(entry.item_id);
        row.appendChild(el('span', 'trk-run-item', item ? `▸ ${item.title}` : ''));

        row.appendChild(el('span', 'trk-run-started', startedLabel(entry)));

        const value = el('span', 'trk-run-value', fmtDuration(entryElapsed(entry, view.nowMs)));
        value.dataset.trkRun = entry.id;
        row.appendChild(value);

        const stop = el('button', 'trk-run-stop', '[stop]');
        stop.type = 'button';
        stop.title = 'Остановить этот таймер';
        stop.addEventListener('click', (ev) => {
            ev.stopPropagation();
            if (typeof view.onStop === 'function') view.onStop(entry);
        });
        row.appendChild(stop);

        container.appendChild(row);
    });
}

/**
 * Обновить цифры уже отрисованного блока.
 *
 * Только текст — никаких перестроений: блок с кнопками `[stop]` не должен
 * пересобираться под курсором раз в секунду.
 */
export function tickRunning(container, entries, nowMs) {
    if (!container || container.hidden) return;
    // Идём от УЗЛОВ к отрезкам, а не наоборот: селектор по значению `id` в
    // атрибуте потребовал бы экранирования (`CSS.escape` есть не везде), а
    // идущих отрезков единицы — перебор дешевле.
    const byId = new Map();
    (Array.isArray(entries) ? entries : []).forEach((e) => {
        if (e && e.id) byId.set(e.id, e);
    });
    Array.from(container.querySelectorAll('[data-trk-run]')).forEach((node) => {
        const entry = byId.get(node.dataset.trkRun);
        if (entry) node.textContent = fmtDuration(entryElapsed(entry, nowMs));
    });
}

/** Состояние тикера для e2e и диагностики. */
export function _debugTimerState() {
    return { ticking: isTicking(), hasCallback: !!local.onTick };
}
