// tmux-web — плагин forge-tracker: режим `list`.
//
// Отрисовка ленты задач по макету (~/Documents/ForgeTracker.pen, экран
// «FORGE://tracker»): строка задачи это ряд колонок фиксированной ширины
//
//     [ ] P1 Название …                 [work] #forge  ~4ч  0:42:19 [▸] [→ tasks/todo]
//
// а невыполненные и сделанные разделены заголовком `// DONE TODAY — 2 · 1:39:13`.
//
// ЧТО ЗДЕСЬ ЧИСТОЕ, А ЧТО ТРОГАЕТ DOM. `filterTasks`, `sortTasks`,
// `splitDone` и `fmtDuration` — чистые функции без DOM и без сети: на них
// держится вся логика выборки, и покрываются они node-тестом. `renderList`
// строит узлы и навешивает обработчики, но САМ ничего не решает — все
// действия уходят в колбэки из main.js.
//
// НИКАКОГО innerHTML. Заголовок задачи — пользовательский текст, и он
// попадает в DOM только через textContent. Это то же правило, что в
// js/messenger/markdown.js, и здесь оно даже строже: разметки у трекера нет
// вовсе.
//
// ЧАСЫ ЗДЕСЬ НЕ ТИКАЮТ. Время задачи берётся из снимка (`task_seconds`,
// посчитано сервером на момент `now`) и меняется по событиям WS. Живой
// секундомер добавляет к нему прошедшее время, переписывая ТЕКСТ узла
// `[data-trk-task-time]` — см. js/tracker/timer.js. Своего таймера у ленты
// нет намеренно: один тикер на панель вместо таймера на строку.

/** Заголовки пустых состояний: у каждой директории свой текст. */
const EMPTY = {
    today: {
        cap: '// ~/today — 0 NODES',
        line: 'на сегодня пусто. напиши первую задачу в строке ниже:',
    },
    week: {
        cap: '// ~/week — 0 NODES',
        line: 'на этой неделе задач нет. напиши первую в строке ниже:',
    },
    inbox: {
        cap: '// ~/inbox — 0 NODES',
        line: 'входящие разобраны. новое — в строке ниже:',
    },
    all: {
        cap: '// ~/all — 0 NODES',
        line: 'нет задач. напиши первую в строке ниже:',
    },
};

/** Примеры синтаксиса под пустым состоянием (текст из макета). */
const EMPTY_EXAMPLES = [
    'tracker:~$ Свести дизайн трекера !1 #forge ~4ч @work',
    'tracker:~$ Созвон с командой !2 #meet ~1ч due:18:00',
];

const EMPTY_HINT = '!N приоритет · #тег · ~оценка · @список · due:время';

/** Секунды → `0:42:19`. */
export function fmtDuration(seconds) {
    const total = Math.max(0, Math.floor(Number(seconds) || 0));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    return `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

/** Локальная полночь дня, которому принадлежит `date`. */
function dayStart(date) {
    return new Date(date.getFullYear(), date.getMonth(), date.getDate(), 0, 0, 0, 0);
}

/** Начало локальной недели (понедельник) — та же граница, что в report.rs. */
function weekStart(date) {
    const d = dayStart(date);
    // getDay(): 0 — воскресенье. Сдвигаем так, чтобы неделя начиналась в
    // понедельник (сервер считает так же, и расхождение дало бы разные
    // суммы в статус-баре и в выборке ~/week).
    const shift = (d.getDay() + 6) % 7;
    d.setDate(d.getDate() - shift);
    return d;
}

/**
 * Разбор отметки времени задачи: RFC 3339 или голая дата `YYYY-MM-DD`.
 *
 * Голая дата трактуется как ЛОКАЛЬНАЯ полночь — ровно как `parse_ts` в
 * plugins/tracker/src/report.rs. Через `new Date("2026-09-08")` так нельзя:
 * там дата читается как UTC, и в положительных поясах срок уезжал бы на день
 * назад.
 */
export function parseTs(raw) {
    if (!raw) return null;
    const s = String(raw).trim();
    const bare = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
    if (bare) {
        return new Date(Number(bare[1]), Number(bare[2]) - 1, Number(bare[3]), 0, 0, 0, 0);
    }
    const d = new Date(s);
    return Number.isNaN(d.getTime()) ? null : d;
}

/**
 * Попадает ли задача в директорию — ЧИСТАЯ функция.
 *
 * Директории это ПРЕДИКАТЫ поверх одного снимка, а не серверные запросы:
 *
 *   `~/today` — сделать сегодня: колонка `today` или `doing`, либо срок не
 *               позже конца сегодняшнего дня, либо по задаче идёт таймер;
 *   `~/week`  — то же плюс `waiting` и сроки до конца текущей недели;
 *   `~/inbox` — не разобранное: колонка `inbox`;
 *   `~/all`   — всё.
 *
 * Выполненные сюда ПОПАДАЮТ: их отделяет `splitDone`, потому что блок
 * `// DONE TODAY` в макете живёт внутри той же выборки, а не рядом с ней.
 */
export function inDir(task, dir, ctx) {
    if (!task) return false;
    if (dir === 'all') return true;
    const now = (ctx && ctx.now) || new Date();
    const running = !!(ctx && ctx.runningIds && ctx.runningIds.has(task.id));

    if (dir === 'inbox') {
        return task.status === 'inbox' && task.status !== 'done';
    }

    const horizon = dir === 'week'
        ? (() => { const d = weekStart(now); d.setDate(d.getDate() + 7); return d; })()
        : (() => { const d = dayStart(now); d.setDate(d.getDate() + 1); return d; })();

    if (task.status === 'done') {
        // Сделанное показываем в той директории, к чьему окну относится
        // момент выполнения: «сделано сегодня» не должно исчезать из ~/today
        // от того, что срок у задачи не стоял.
        const doneAt = parseTs(task.done_at);
        if (!doneAt) return false;
        const from = dir === 'week' ? weekStart(now) : dayStart(now);
        return doneAt >= from && doneAt < horizon;
    }

    if (running) return true;
    if (task.status === 'today' || task.status === 'doing') return true;
    if (dir === 'week' && task.status === 'waiting') return true;
    const due = parseTs(task.due);
    return !!due && due < horizon;
}

/**
 * Выборка задач: директория + фильтр по списку + фильтр по тегу.
 *
 * @param {Array} tasks — все задачи снимка
 * @param {{dir: string, listId: (string|null), tag: (string|null),
 *          now: Date, runningIds: Set<string>}} ctx
 */
export function filterTasks(tasks, ctx) {
    const list = Array.isArray(tasks) ? tasks : [];
    return list.filter((t) => {
        if (!t) return false;
        if (ctx.listId && t.list_id !== ctx.listId) return false;
        if (ctx.tag && !(Array.isArray(t.tags) && t.tags.includes(ctx.tag))) return false;
        return inDir(t, ctx.dir, ctx);
    });
}

/**
 * Порядок ленты: сначала ручной `order`, затем момент создания.
 *
 * Приоритет в сортировку НЕ входит намеренно: в макете P0 стоит второй
 * строкой, то есть порядок ленты задаёт человек перетаскиванием, а приоритет
 * — это метка, а не сортировка. Автосортировка по приоритету обесценила бы
 * drag'n'drop.
 */
export function sortTasks(tasks) {
    return [...tasks].sort((a, b) => {
        const ao = Number(a.order) || 0;
        const bo = Number(b.order) || 0;
        if (ao !== bo) return ao - bo;
        return String(a.created_at || '').localeCompare(String(b.created_at || ''));
    });
}

/**
 * Полный порядок задач после перестановки — ЧИСТАЯ функция.
 *
 * Порядок в трекере ГЛОБАЛЬНЫЙ (поле `order`), а видна всегда лишь выборка —
 * лента одной директории или одна колонка доски. Поэтому новый список
 * собирается из ВСЕХ задач снимка, и в нём переставляется ровно одна:
 * отправить порядок одной лишь выборки нельзя, сервер увёл бы недостающие id
 * в хвост, то есть невидимые задачи молча перемешались бы.
 *
 * Функция общая для ленты и доски: обе умеют «положить перед/после соседки»,
 * и правило пересборки у них обязано быть одно.
 *
 * @param {Array} tasks — все задачи снимка
 * @param {string} movedId — кого двигаем
 * @param {string} targetId — соседка, относительно которой кладём
 * @param {boolean} after — класть ПОСЛЕ соседки
 * @returns {Array<string>|null} полный порядок id либо `null`, когда двигать
 *          нечего (соседки нет или она же и есть перемещаемая задача)
 */
export function planOrder(tasks, movedId, targetId, after) {
    if (!movedId || !targetId || movedId === targetId) return null;
    const ids = sortTasks(tasks || []).map((t) => t.id).filter((id) => id !== movedId);
    let at = ids.indexOf(targetId);
    if (at < 0) return null;
    if (after) at += 1;
    ids.splice(at, 0, movedId);
    return ids;
}

/** Делит выборку на «в работе» и «сделано» (последние — свежие сверху). */
export function splitDone(tasks) {
    const open = [];
    const done = [];
    for (const t of tasks) (t.status === 'done' ? done : open).push(t);
    done.sort((a, b) => String(b.done_at || '').localeCompare(String(a.done_at || '')));
    return { open, done };
}

// ------------------------------------------------------------------- отрисовка

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

/** Метка списка `[work]` или пусто. */
function listLabel(task, listsById) {
    if (!task.list_id) return '';
    const l = listsById.get(task.list_id);
    return l ? `[${l.name}]` : '';
}

/** Срок в короткой форме `08.09` / `08.09 18:00`. */
function dueLabel(task) {
    const d = parseTs(task.due);
    if (!d) return '';
    const day = `${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')}`;
    const bare = /^\d{4}-\d{2}-\d{2}$/.test(String(task.due).trim());
    if (bare) return day;
    return `${day} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/** Оценка `~4ч` из минут. */
function estLabel(task) {
    const m = Number(task.estimate_min);
    if (!m) return '—';
    const h = Math.floor(m / 60);
    const min = m % 60;
    if (h && min) return `~${h}ч${min}м`;
    if (h) return `~${h}ч`;
    return `~${min}м`;
}

/** Одна строка задачи. */
function taskRow(task, view) {
    const running = view.runningIds.has(task.id);
    const row = el('div', 'trk-task');
    row.dataset.taskId = task.id;
    row.draggable = true;
    if (running) row.classList.add('running');
    if (task.status === 'done') row.classList.add('done');

    const box = btn('trk-task-box', task.status === 'done' ? '[x]' : '[ ]',
        task.status === 'done' ? 'Вернуть в работу' : 'Отметить выполненной');
    box.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onToggleDone(task);
    });
    row.appendChild(box);

    const prio = Number.isFinite(Number(task.priority)) ? Number(task.priority) : 2;
    row.appendChild(el('span', `trk-task-prio trk-prio-${prio}`, `P${prio}`));

    row.appendChild(el('span', 'trk-task-title', task.title || ''));
    row.appendChild(el('span', 'trk-task-list', listLabel(task, view.listsById)));
    row.appendChild(el('span', 'trk-task-tags',
        (Array.isArray(task.tags) ? task.tags : []).map((t) => `#${t}`).join(' ')));
    row.appendChild(el('span', 'trk-task-est', estLabel(task)));

    const due = el('span', 'trk-task-due', dueLabel(task));
    const dueAt = parseTs(task.due);
    if (dueAt && task.status !== 'done' && dueAt < view.now) due.classList.add('overdue');
    row.appendChild(due);

    // Прогресс чек-листа считает сервер (`progress` в снимке); наполняют его
    // пункты чек-листа, которые заводятся в карточке задачи.
    const pr = view.progress[task.id];
    row.appendChild(el('span', 'trk-task-progress',
        pr && pr.total ? `${pr.done}/${pr.total}` : ''));

    // Время помечено `data-trk-task-time`: пока по задаче идёт таймер, текст
    // этого узла раз в секунду переписывает общий тикер (js/tracker/timer.js),
    // не трогая остальную строку. Перерисовывать ленту каждую секунду нельзя —
    // это сбрасывало бы прокрутку и начатый drag'n'drop.
    const secs = view.taskSeconds[task.id];
    const time = el('span', 'trk-task-time', secs ? fmtDuration(secs) : '—');
    time.dataset.trkTaskTime = task.id;
    row.appendChild(time);

    const toggle = btn('trk-task-toggle', running ? '[▮]' : '[▸]',
        running ? 'Остановить таймер' : 'Запустить таймер');
    toggle.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onTimer(task, running);
    });
    row.appendChild(toggle);

    // Кнопка переноса отрисована по макету, поведение — фаза 8. До тех пор
    // она честно говорит об этом, а не молчит на клик.
    const push = btn('trk-task-push', '[→ tasks/todo]', 'Скопировать в колонку TODO вкладки Tasks');
    push.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onPush(task);
    });
    row.appendChild(push);

    const rm = btn('trk-task-rm', '[rm]', 'Удалить задачу');
    rm.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onDelete(task);
    });
    row.appendChild(rm);

    row.addEventListener('click', () => view.onOpen(task));

    return row;
}

/** Пустое состояние директории — терминальный текст из макета. */
function emptyBlock(dir) {
    const meta = EMPTY[dir] || EMPTY.all;
    const box = el('div', null);
    box.id = 'trk-empty';
    box.appendChild(el('div', 'trk-empty-cap', meta.cap));
    box.appendChild(el('div', 'trk-empty-line', meta.line));
    EMPTY_EXAMPLES.forEach((ex) => box.appendChild(el('div', 'trk-empty-ex', ex)));
    box.appendChild(el('div', 'trk-empty-hint', EMPTY_HINT));
    return box;
}

/**
 * Перетаскивание строк.
 *
 * Порядок ГЛОБАЛЬНЫЙ (поле `order` задачи), а видно в ленте только выборку,
 * поэтому наружу отдаётся не «новый порядок видимого», а пара
 * «кого» + «перед кем»: пересобрать полный список умеет только main.js,
 * у которого есть все задачи снимка.
 */
function installDnd(container) {
    let dragId = null;
    // Обработчики навешиваются ОДИН раз на контейнер, а `view` пересоздаётся
    // на каждой отрисовке — поэтому читаем его отсюда, а не из замыкания:
    // иначе drop после первой же перерисовки звал бы устаревший колбэк.
    const current = () => container.__trkView || {};

    container.addEventListener('dragstart', (ev) => {
        const row = ev.target.closest && ev.target.closest('.trk-task');
        if (!row) return;
        dragId = row.dataset.taskId;
        try { ev.dataTransfer.setData('text/plain', dragId); } catch (_) { /* Safari */ }
        if (ev.dataTransfer) ev.dataTransfer.effectAllowed = 'move';
    });
    container.addEventListener('dragover', (ev) => {
        if (!dragId) return;
        const row = ev.target.closest && ev.target.closest('.trk-task');
        if (!row || row.dataset.taskId === dragId) return;
        ev.preventDefault();
        if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'move';
    });
    container.addEventListener('drop', (ev) => {
        if (!dragId) return;
        const row = ev.target.closest && ev.target.closest('.trk-task');
        if (!row) { dragId = null; return; }
        ev.preventDefault();
        const targetId = row.dataset.taskId;
        const moved = dragId;
        dragId = null;
        if (!targetId || targetId === moved) return;
        // Куда именно класть — решает половина строки, над которой отпустили.
        const box = row.getBoundingClientRect();
        const after = (ev.clientY - box.top) > box.height / 2;
        const fn = current().onReorder;
        if (typeof fn === 'function') fn(moved, targetId, after);
    });
    container.addEventListener('dragend', () => { dragId = null; });
}

/**
 * Отрисовать ленту режима `list`.
 *
 * @param {HTMLElement} container — #trk-content
 * @param {object} view — данные и колбэки:
 *   `tasks` (уже отфильтрованные), `listsById`, `taskSeconds`, `progress`,
 *   `runningIds` (Set), `dir`, `now`, `dayLabel`, и колбэки `onToggleDone`,
 *   `onTimer`, `onOpen`, `onPush`, `onDelete`, `onReorder`.
 */
export function renderList(container, view) {
    if (!container) return;
    container.textContent = '';

    const sorted = sortTasks(view.tasks);
    const { open, done } = splitDone(sorted);

    if (!open.length && !done.length) {
        container.appendChild(emptyBlock(view.dir));
        return;
    }

    if (view.dayLabel) {
        container.appendChild(el('div', 'trk-divider', `—— ${view.dayLabel} ——`));
    }
    open.forEach((t) => container.appendChild(taskRow(t, view)));

    if (done.length) {
        const seconds = done.reduce((acc, t) => acc + (Number(view.taskSeconds[t.id]) || 0), 0);
        const cap = view.dir === 'week' ? '// DONE THIS WEEK' : '// DONE';
        container.appendChild(el('div', 'trk-divider trk-divider-left',
            `${cap} — ${done.length} · ${fmtDuration(seconds)}`));
        done.forEach((t) => container.appendChild(taskRow(t, view)));
    }

    // Актуальный view кладём ДО установки обработчиков: они читают его
    // отсюда (см. `installDnd`), а не из замыкания.
    container.__trkView = view;
    if (!container.dataset.dndInstalled) {
        installDnd(container);
        container.dataset.dndInstalled = '1';
    }
}
