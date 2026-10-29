// tmux-web — плагин forge-tracker: режим `kanban`.
//
// Доска по макету (~/Documents/ForgeTracker.pen, экран «Экран — KANBAN»): пять
// колонок в ряд, в каждой — карточки в терминальной рамке
//
//     ┌────────────────────────────────┐
//     │ P1  [work]              #forge │
//     │ Свести дизайн трекера          │
//     │ est ~4ч   0:42:19   [▸]    [→] │
//     └────────────────────────────────┘
//
// КОЛОНКИ — ЭТО `TaskStatus`, А НЕ СВОЙ СПРАВОЧНИК. Значения `inbox · today ·
// doing · waiting · done` заданы в plugins/tracker/src/models.rs
// (`TaskStatus::ALL`) и проверяются сервером в `parse_status`. Отсюда следует
// главное свойство доски: перенос карточки — это один `PATCH
// /api/tracker/tasks/:id/status`, а не выдуманное поле «колонка», которое
// пришлось бы держать в согласии со статусом.
//
// ЧТО ЗДЕСЬ ЧИСТОЕ, А ЧТО ТРОГАЕТ DOM. `COLUMNS` и `groupByStatus` — данные и
// чистая функция, они покрыты node-тестом (tests/frontend/tracker_kanban.test.mjs).
// `renderKanban` строит узлы и навешивает обработчики, но САМ ничего не
// решает: и перенос, и старт таймера, и открытие карточки уходят колбэками в
// main.js — ровно как в render-list.js.
//
// ЧАСЫ ЗДЕСЬ НЕ ТИКАЮТ — и это не забывчивость, а то же правило, что в ленте:
// один тикер на панель (js/tracker/timer.js) раз в секунду переписывает ТЕКСТ
// узлов `[data-trk-task-time]`, где бы они ни лежали. Поэтому у времени
// карточки тот же самый data-атрибут, что у колонки времени в ленте, и
// отдельного таймера доске не нужно. Перерисовывать доску раз в секунду
// нельзя: это рвало бы начатый drag'n'drop и прокрутку колонок.
//
// НИКАКОГО innerHTML: заголовок задачи — пользовательский текст и попадает в
// DOM только через textContent.

import { fmtDuration, parseTs, sortTasks } from './render-list.js';

/**
 * Колонки доски — в том порядке, в каком они стоят в макете.
 *
 * `id` обязан совпадать со строковым представлением `TaskStatus` (см.
 * `TaskStatus::as_str`): именно это значение уходит в `PATCH .../status`.
 */
export const COLUMNS = [
    { id: 'inbox', label: '// inbox' },
    { id: 'today', label: '// today' },
    { id: 'doing', label: '// doing' },
    { id: 'waiting', label: '// waiting' },
    { id: 'done', label: '// done' },
];

/** Множество известных статусов — для отсева мусора из будущих версий стора. */
const KNOWN = new Set(COLUMNS.map((c) => c.id));

/**
 * Разложить задачи по колонкам — ЧИСТАЯ функция.
 *
 * Внутри колонки порядок тот же, что в ленте: ручной `order`, затем момент
 * создания (`sortTasks`). Сортировать `done` по времени выполнения, как это
 * делает `splitDone` в ленте, здесь НЕЛЬЗЯ: колонку `done` тоже можно
 * перетаскивать, и своя сортировка молча отменяла бы перестановку.
 *
 * Задача с неизвестным статусом (стор новее фронта) попадает в `inbox`, а не
 * пропадает с доски: потерянная задача хуже задачи не в той колонке.
 *
 * @param {Array} tasks — уже отфильтрованная выборка
 * @returns {Object<string, Array>} ключ — id колонки, значение — её задачи
 */
export function groupByStatus(tasks) {
    const by = {};
    COLUMNS.forEach((c) => { by[c.id] = []; });
    (Array.isArray(tasks) ? tasks : []).forEach((t) => {
        if (!t) return;
        const st = KNOWN.has(t.status) ? t.status : 'inbox';
        by[st].push(t);
    });
    COLUMNS.forEach((c) => { by[c.id] = sortTasks(by[c.id]); });
    return by;
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

/** Оценка `est ~4ч` — в макете подпись входит в текст. */
function estLabel(task) {
    const m = Number(task.estimate_min);
    if (!m) return 'est —';
    const h = Math.floor(m / 60);
    const min = m % 60;
    if (h && min) return `est ~${h}ч${min}м`;
    if (h) return `est ~${h}ч`;
    return `est ~${min}м`;
}

/** Срок в короткой форме `08.09` / `08.09 18:00`; пусто, если срока нет. */
function dueLabel(task) {
    const d = parseTs(task.due);
    if (!d) return '';
    const p = (n) => String(n).padStart(2, '0');
    const day = `${p(d.getDate())}.${p(d.getMonth() + 1)}`;
    const bare = /^\d{4}-\d{2}-\d{2}$/.test(String(task.due).trim());
    return bare ? day : `${day} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * Одна карточка.
 *
 * Отличия от макета — ровно два, и оба «по наличию»: срок и прогресс
 * чек-листа рисуются, только когда они у задачи есть. На карточках макета их
 * не было, потому что там их не было и у задач; молча скрывать заведённый
 * человеком срок доска не вправе — в ленте он виден, и доска обязана
 * показывать ту же задачу так же.
 */
function card(task, view) {
    const running = view.runningIds.has(task.id);
    const box = el('div', 'trk-kb-card');
    box.dataset.taskId = task.id;
    box.draggable = true;
    if (running) box.classList.add('running');
    if (task.status === 'done') box.classList.add('done');

    const top = el('div', 'trk-kb-top');
    const prio = Number.isFinite(Number(task.priority)) ? Number(task.priority) : 2;
    top.appendChild(el('span', `trk-kb-prio trk-prio-${prio}`, `P${prio}`));
    top.appendChild(el('span', 'trk-kb-list', listLabel(task, view.listsById)));
    const pr = view.progress[task.id];
    if (pr && pr.total) {
        top.appendChild(el('span', 'trk-kb-progress', `${pr.done}/${pr.total}`));
    }
    top.appendChild(el('span', 'trk-kb-tags',
        (Array.isArray(task.tags) ? task.tags : []).map((t) => `#${t}`).join(' ')));
    box.appendChild(top);

    box.appendChild(el('div', 'trk-kb-title', task.title || ''));

    const bottom = el('div', 'trk-kb-bottom');
    bottom.appendChild(el('span', 'trk-kb-est', estLabel(task)));

    const dueText = dueLabel(task);
    if (dueText) {
        const due = el('span', 'trk-kb-due', dueText);
        const dueAt = parseTs(task.due);
        if (dueAt && task.status !== 'done' && dueAt < view.now) due.classList.add('overdue');
        bottom.appendChild(due);
    }

    // Тот же data-атрибут, что у колонки времени в ленте: общий тикер панели
    // (js/tracker/timer.js) находит узел по нему и переписывает только текст.
    const secs = view.taskSeconds[task.id];
    const time = el('span', 'trk-kb-time', secs ? fmtDuration(secs) : '—');
    time.dataset.trkTaskTime = task.id;
    bottom.appendChild(time);

    const toggle = btn('trk-kb-toggle', running ? '[▮]' : '[▸]',
        running ? 'Остановить таймер' : 'Запустить таймер');
    toggle.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onTimer(task, running);
    });
    bottom.appendChild(toggle);

    const push = btn('trk-kb-push', '[→]', 'Скопировать в колонку TODO вкладки Tasks');
    push.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onPush(task);
    });
    bottom.appendChild(push);

    box.appendChild(bottom);
    box.addEventListener('click', () => view.onOpen(task));
    return box;
}

/** Колонка целиком: шапка `// name N [+]` и стопка карточек. */
function column(meta, tasks, view) {
    const col = el('div', 'trk-kb-col');
    col.dataset.status = meta.id;

    const head = el('div', 'trk-kb-head');
    head.appendChild(el('span', 'trk-kb-name', meta.label));
    head.appendChild(el('span', 'trk-kb-count', String(tasks.length)));
    const add = btn('trk-kb-add', '[+]', `Новая задача в // ${meta.id}`);
    add.addEventListener('click', (ev) => {
        ev.stopPropagation();
        view.onAdd(meta.id);
    });
    head.appendChild(add);
    col.appendChild(head);

    // Карточки лежат в своём прокручиваемом контейнере: доска не должна
    // растягиваться по самой длинной колонке, иначе переносить карточку в
    // короткую колонку пришлось бы через прокрутку всей страницы.
    const body = el('div', 'trk-kb-cards');
    body.dataset.status = meta.id;
    if (!tasks.length) {
        body.appendChild(el('div', 'trk-kb-empty', '— пусто —'));
    }
    tasks.forEach((t) => body.appendChild(card(t, view)));
    col.appendChild(body);
    return col;
}

/**
 * Перетаскивание карточек.
 *
 * Наружу отдаётся ОДНО событие `onMove(movedId, status, targetId, after)`:
 * колонка, в которую отпустили, и — если отпустили над карточкой — соседка и
 * сторона. Что из этого стало сменой статуса, а что перестановкой, решает
 * main.js: только у него есть все задачи снимка, а порядок в трекере
 * ГЛОБАЛЬНЫЙ (поле `order`), а не «внутри колонки».
 *
 * Обработчики навешиваются на контейнер ОДИН раз, а `view` пересоздаётся
 * каждой отрисовкой — поэтому актуальный лежит в `container.__trkKbView` и
 * читается оттуда, а не из замыкания (та же ловушка, что в render-list.js).
 */
function installDnd(container) {
    let dragId = null;
    let overCol = null;
    const current = () => container.__trkKbView || {};

    const highlight = (col) => {
        if (overCol === col) return;
        if (overCol) overCol.classList.remove('drop');
        overCol = col;
        if (overCol) overCol.classList.add('drop');
    };
    // Пока карточку тащат, доску перерисовывать НЕЛЬЗЯ: кадр `task_upsert` из
    // другого окна пересобрал бы узлы, и браузер оборвал бы перенос на
    // полпути (узел под курсором просто исчезает). Поэтому о начале и конце
    // переноса сообщается наверх, а main.js откладывает отрисовку.
    const report = (on) => {
        const fn = current().onDrag;
        if (typeof fn === 'function') fn(on);
    };
    const reset = () => {
        const was = dragId !== null;
        dragId = null;
        highlight(null);
        if (was) report(false);
    };

    container.addEventListener('dragstart', (ev) => {
        const box = ev.target.closest && ev.target.closest('.trk-kb-card');
        if (!box) return;
        dragId = box.dataset.taskId;
        try { ev.dataTransfer.setData('text/plain', dragId); } catch (_) { /* Safari */ }
        if (ev.dataTransfer) ev.dataTransfer.effectAllowed = 'move';
        box.classList.add('dragging');
        report(true);
    });

    container.addEventListener('dragover', (ev) => {
        if (!dragId) return;
        const col = ev.target.closest && ev.target.closest('.trk-kb-col');
        if (!col) return;
        // preventDefault — единственный способ сказать браузеру «сюда можно»;
        // без него drop не наступает вовсе.
        ev.preventDefault();
        if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'move';
        highlight(col);
    });

    container.addEventListener('dragleave', (ev) => {
        // Уход за пределы доски: подсветку снимаем, а внутри доски её снимет
        // следующий dragover соседней колонки.
        if (ev.target === container) highlight(null);
    });

    container.addEventListener('drop', (ev) => {
        if (!dragId) return;
        const col = ev.target.closest && ev.target.closest('.trk-kb-col');
        const moved = dragId;
        reset();
        if (!col) return;
        ev.preventDefault();
        const status = col.dataset.status;
        const over = ev.target.closest && ev.target.closest('.trk-kb-card');
        let targetId = null;
        let after = false;
        if (over && over.dataset.taskId !== moved) {
            targetId = over.dataset.taskId;
            const box = over.getBoundingClientRect();
            after = (ev.clientY - box.top) > box.height / 2;
        } else if (!over) {
            // Отпустили в пустоту колонки — значит, в конец: соседкой берём
            // последнюю карточку, иначе перестановки не выйдет вовсе.
            const cards = Array.from(col.querySelectorAll('.trk-kb-card'))
                .filter((c) => c.dataset.taskId !== moved);
            if (cards.length) {
                targetId = cards[cards.length - 1].dataset.taskId;
                after = true;
            }
        }
        const fn = current().onMove;
        if (typeof fn === 'function') fn(moved, status, targetId, after);
    });

    container.addEventListener('dragend', () => {
        Array.from(container.querySelectorAll('.trk-kb-card.dragging'))
            .forEach((c) => c.classList.remove('dragging'));
        reset();
    });
}

/**
 * Отрисовать доску.
 *
 * @param {HTMLElement} container — #trk-content
 * @param {object} view — данные и колбэки: `tasks` (уже отфильтрованные),
 *   `listsById`, `taskSeconds`, `progress`, `runningIds` (Set), `now`, и
 *   колбэки `onTimer`, `onOpen`, `onPush`, `onAdd`, `onMove`.
 */
export function renderKanban(container, view) {
    if (!container) return;
    container.textContent = '';

    const by = groupByStatus(view.tasks);
    const board = el('div', 'trk-kb-board');
    COLUMNS.forEach((meta) => board.appendChild(column(meta, by[meta.id], view)));
    container.appendChild(board);

    container.__trkKbView = view;
    if (!container.dataset.kbDndInstalled) {
        installDnd(container);
        container.dataset.kbDndInstalled = '1';
    }
}
