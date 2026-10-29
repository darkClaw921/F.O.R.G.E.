// tmux-web — плагин forge-tracker: карточка задачи («NODE» в макете).
//
// Экран «Экран — NODE (карточка задачи)» из ~/Documents/ForgeTracker.pen:
// шапка с чекбоксом, приоритетом и заголовком, блок `// FIELDS`, заметки,
// блок `// CHECKLIST — 2/5 · 40%` с полосой прогресса и таймером у каждого
// пункта и блок `// TIME` со списком отрезков; внизу DELETE · MARK DONE · SAVE.
//
// ЧАСТИЧНОЕ РЕДАКТИРОВАНИЕ НЕ ЗАТИРАЕТ НЕЗАДАННОЕ. Это главное требование к
// модулю, и держится оно на двух вещах:
//
//   1. в PATCH уходят ТОЛЬКО поля, которые человек действительно изменил —
//      форма сравнивается со снимком задачи, снятым при открытии;
//   2. пустое поле означает СБРОС и уезжает как `null`, а не как `""`.
//      Модель различает «не передали» и «передали null» специально
//      (`Option<Option<T>>` + `some_option` в models.rs), и карточка —
//      единственное место фронта, где эта разница видна пользователю.
//
// ФОРМА И ЖИВЫЕ БЛОКИ ПЕРЕРИСОВЫВАЮТСЯ ПОРОЗНЬ. Чек-лист и лента отрезков
// меняются от чужих действий (кадры WS) и от собственных кнопок карточки, а
// поля `// FIELDS` и `// NOTES` в это время могут быть наполовину набраны.
// Поэтому `onTrackerSnapshot` перерисовывает ТОЛЬКО два живых блока и никогда
// не трогает форму: иначе первый же кадр стирал бы набранный текст. По той же
// причине блок, в котором прямо сейчас открыт inline-редактор (правка пункта
// или отрезка), пропускается до конца правки — см. `local.busy`.
//
// ВРЕМЯ ПУНКТА — ЭТО ВРЕМЯ ЗАДАЧИ. Таймер пункта заводит обычный отрезок с
// заполненными `task_id` и `item_id` (plugins/tracker/src/routes/items.rs),
// поэтому сумма задачи включает его РОВНО ОДИН РАЗ. Карточка нигде не
// складывает время пунктов со временем задачи — оба числа приходят из снимка
// (`task_seconds`, `item_seconds`), и второе является частью первого.
//
// ОДИН АКТИВНЫЙ ОТРЕЗОК НА ЗАДАЧУ — ЯВНОЕ ПРАВИЛО КНОПКИ ПУНКТА. Стор второй
// отрезок по той же задаче не заведёт: `start` вернёт уже идущий. Молча
// показать это пользователю нельзя — он нажал `[▸]` на пункте и ждёт, что
// время пойдёт на ЭТОТ пункт. Поэтому карточка сначала ОСТАНАВЛИВАЕТ идущий
// отрезок задачи (чей бы он ни был) и только потом стартует таймер пункта:
// накопленное не теряется, оно остаётся в закрытом отрезке.
//
// ОШИБКИ СЕТИ ПОКАЗЫВАЮТСЯ В КАРТОЧКЕ, а не пишутся в консоль: человек нажал
// SAVE и обязан узнать, сохранилось ли. Карточка при ошибке НЕ закрывается —
// иначе набранный текст пропал бы вместе с ней.

import {
    $trkModal, $trkModalNode, $trkModalBody, $trkModalError,
    $trkModalClose, $trkModalDelete, $trkModalDone, $trkModalSave,
    $trkModalPush,
} from '../core/dom.js';
import * as api from './api.js';
import { fmtDuration, parseTs } from './render-list.js';
import {
    ESTIMATE_CUSTOM, ESTIMATE_PRESETS, estimateChoice, estimateValue,
    fromDateInput, fromDueInput, fromLocalInput, toDateInput, toDueInput, toLocalInput,
} from './fields.js';
import { entryElapsed } from './timer.js';
import {
    addTag, knownTags, moveSuggest, popTag, removeTagAt, splitTags, suggestTags,
} from './tags.js';
import { isPushOpen } from './to-todo.js';
import { isMilestoneOpen } from './milestone.js';

// Конверсии дат и оценки живут в fields.js (там нет DOM — значит они покрыты
// node-тестом). Ре-экспорт оставлен, чтобы уже написанный код и e2e могли
// звать их через карточку, как раньше.
export {
    toLocalInput, fromLocalInput, toDateInput, fromDateInput, toDueInput, fromDueInput,
};

const STATUSES = [
    ['inbox', 'inbox'],
    ['today', 'today'],
    ['doing', 'doing'],
    ['waiting', 'waiting'],
    ['done', 'done'],
];

const local = {
    /** Зависимости от main.js: снимок и перезапрос состояния. */
    deps: null,
    /** Слушатели навешены (идемпотентность init). */
    wired: false,
    /** Задача, открытая сейчас (снимок НА МОМЕНТ ОТКРЫТИЯ). */
    task: null,
    /** Поля формы. */
    fields: null,
    /** Контейнеры живых блоков: перерисовываются отдельно от формы. */
    sections: null,
    /** Незаконченный ввод нового пункта — переживает перерисовку блока. */
    draftItem: '',
    /**
     * Открытые inline-редакторы. Пока они открыты, свой блок не
     * перерисовывается: кадр WS не должен стирать наполовину набранную правку.
     */
    busy: { item: null, entry: null },
};

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = text;
    return n;
}

/** Строка `ключ — значение — пояснение` блока `// FIELDS`. */
function field(key, control, extra) {
    const row = el('div', 'trk-field');
    row.appendChild(el('div', 'trk-field-key', key));
    const val = el('div', 'trk-field-val');
    val.appendChild(control);
    row.appendChild(val);
    if (extra) row.appendChild(el('div', 'trk-field-extra', extra));
    return row;
}

function input(value, placeholder) {
    const i = document.createElement('input');
    i.type = 'text';
    i.spellcheck = false;
    i.value = value == null ? '' : String(value);
    if (placeholder) i.placeholder = placeholder;
    return i;
}

function select(options, value) {
    const s = document.createElement('select');
    options.forEach(([v, label]) => {
        const o = document.createElement('option');
        o.value = v;
        o.textContent = label;
        if (String(v) === String(value)) o.selected = true;
        s.appendChild(o);
    });
    return s;
}

/**
 * Редактор тегов поля `tags`: чипы `#forge ×` плюс поле ввода с подсказками.
 *
 * ОТДЕЛЬНОЙ СУЩНОСТИ «ТЕГ» В МОДЕЛИ НЕТ — и это не упущение, а устройство:
 * тег живёт ровно как строка в `task.tags` (`plugins/tracker/src/models.rs`),
 * ни таблицы тегов, ни эндпоинтов `/api/tracker/tags` не существует. Поэтому
 * пункт «создать новый: #имя» НЕ ходит в сеть и за ним не стоит никакого
 * CRUD: «создать» здесь значит «добавить в этот список строку, которой ещё
 * не было ни у одной задачи». Искать пропавший запрос не нужно.
 *
 * ПОДСКАЗКИ БЕРУТСЯ ИЗ СНИМКА `/api/tracker/state` — из тех же задач, что
 * питают блок `// TAGS` в сайдбаре (`main.js`). Второй источник намеренно не
 * заведён: две копии одного списка разъезжаются, и карточка предлагала бы
 * теги, которых в сайдбаре уже нет.
 *
 * Вся логика без DOM (нормализация, дедуп, фильтр, ветка «создать») живёт в
 * `tags.js` и покрыта tests/frontend/tracker_tags.test.mjs — здесь только
 * разметка и клавиши.
 *
 * @param {string[]} initial теги задачи НА МОМЕНТ ОТКРЫТИЯ карточки
 * @param {string[]} known все теги снимка (`knownTags`)
 * @returns {{node: HTMLElement, getTags: function(): string[],
 *   closeSuggest: function(): boolean}} `getTags` дозабирает недобавленный
 *   ввод (человек набрал тег и нажал SAVE — терять набранное нельзя),
 *   `closeSuggest` возвращает `true`, если список был открыт (слой Esc).
 */
function tagEditor(initial, known) {
    const state = { tags: Array.isArray(initial) ? initial.slice() : [], index: -1, items: [] };

    const box = el('div', 'trk-tags-edit');
    const wrap = el('div', 'trk-tags-input-wrap');
    const inp = document.createElement('input');
    inp.type = 'text';
    inp.spellcheck = false;
    inp.autocomplete = 'off';
    inp.className = 'trk-tags-input';
    inp.placeholder = '+ тег';
    const menu = el('div', 'trk-tags-suggest');
    menu.hidden = true;
    wrap.appendChild(inp);
    wrap.appendChild(menu);
    box.appendChild(wrap);

    function paintChips() {
        Array.from(box.querySelectorAll('.trk-chip')).forEach((n) => n.remove());
        state.tags.forEach((tag, i) => {
            const chip = el('span', 'trk-chip');
            chip.appendChild(el('span', 'trk-chip-name', `#${tag}`));
            const x = el('button', 'trk-chip-x', '×');
            x.type = 'button';
            x.title = `снять #${tag}`;
            x.addEventListener('click', () => {
                state.tags = removeTagAt(state.tags, i);
                paintChips();
                paintSuggest();
                inp.focus();
            });
            chip.appendChild(x);
            box.insertBefore(chip, wrap);
        });
    }

    /** Закрыть список. Возвращает `true`, если он был открыт. */
    function closeSuggest() {
        if (menu.hidden) return false;
        menu.textContent = '';
        menu.hidden = true;
        state.index = -1;
        return true;
    }

    function paintSuggest(open) {
        const s = suggestTags(known, inp.value, state.tags);
        state.items = s.items.map((t) => ({ tag: t, create: false }));
        // Пункт «создать новый» всегда последний: сперва то, что уже есть,
        // и только потом заведение ещё одной строки.
        if (s.create) state.items.push({ tag: s.create, create: true });
        const show = open === undefined ? !menu.hidden : !!open;
        if (!show || !state.items.length) {
            // Содержимое стирается вместе со скрытием: спрятанный, но живой
            // список — это узлы, которые видит querySelector и не видит
            // человек (на этом уже спотыкалась живая проба).
            menu.textContent = '';
            menu.hidden = true;
            state.index = -1;
            return;
        }
        if (state.index >= state.items.length) state.index = -1;
        menu.textContent = '';
        state.items.forEach((it, i) => {
            const row = el(
                'button',
                `trk-suggest${it.create ? ' trk-suggest-new' : ''}${i === state.index ? ' active' : ''}`,
                it.create ? `создать новый: #${it.tag}` : `#${it.tag}`,
            );
            row.type = 'button';
            // mousedown гасится, чтобы поле не потеряло фокус до click:
            // иначе blur успел бы закрыть список, и клик попал бы в пустоту.
            row.addEventListener('mousedown', (ev) => ev.preventDefault());
            row.addEventListener('click', () => commit(it.tag));
            menu.appendChild(row);
        });
        menu.hidden = false;
    }

    /** Добавить набранное (можно несколько через пробел/запятую) и очистить поле. */
    function commit(raw) {
        splitTags(raw).forEach((t) => { state.tags = addTag(state.tags, t).tags; });
        inp.value = '';
        state.index = -1;
        paintChips();
        paintSuggest(true);
        inp.focus();
    }

    inp.addEventListener('focus', () => paintSuggest(true));
    inp.addEventListener('input', () => paintSuggest(true));
    // Уход фокуса закрывает только СПИСОК: набранное остаётся в поле и
    // доедет до PATCH через getTags — человек его видит и не теряет.
    inp.addEventListener('blur', () => closeSuggest());
    inp.addEventListener('keydown', (ev) => {
        const picked = state.index >= 0 ? state.items[state.index] : null;
        switch (ev.key) {
        case 'Enter':
            ev.preventDefault();
            commit(picked ? picked.tag : inp.value);
            return;
        case ' ':
        case ',':
            // Пробел и запятая — разделители тегов, а не символы тега.
            ev.preventDefault();
            if (inp.value.trim()) commit(picked ? picked.tag : inp.value);
            return;
        case 'Backspace':
            if (inp.value !== '') return;
            ev.preventDefault();
            state.tags = popTag(state.tags);
            paintChips();
            paintSuggest();
            return;
        case 'ArrowDown':
        case 'ArrowUp':
            ev.preventDefault();
            if (menu.hidden) paintSuggest(true);
            state.index = moveSuggest(state.index, ev.key === 'ArrowDown' ? 1 : -1, state.items.length);
            paintSuggest();
            return;
        case 'Escape':
            // Esc снимает РОВНО один слой. Открытый список — верхний слой
            // поверх карточки, поэтому событие до неё не доходит (её
            // слушатель висит на document, см. onKeydown ниже).
            if (!closeSuggest()) return;
            ev.preventDefault();
            ev.stopPropagation();
            return;
        default:
        }
    });
    // Клик по пустому месту рамки — это «хочу дописать тег».
    box.addEventListener('click', (ev) => { if (ev.target === box) inp.focus(); });

    paintChips();
    return {
        node: box,
        getTags() {
            // Набранное, но не подтверждённое Enter'ом, всё равно уезжает в
            // PATCH: человек видит его в поле и ждёт, что SAVE его сохранит.
            if (inp.value.trim()) commit(inp.value);
            return state.tags.slice();
        },
        closeSuggest,
    };
}

/** Момент в форме `08.09.2026 14:02`. */
function fmtMoment(raw) {
    const d = parseTs(raw);
    if (!d) return '—';
    const p = (n) => String(n).padStart(2, '0');
    return `${p(d.getDate())}.${p(d.getMonth() + 1)}.${d.getFullYear()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** `14:02` или `running` для незакрытого отрезка. */
function fmtRange(entry) {
    const a = parseTs(entry.started_at);
    const b = parseTs(entry.stopped_at);
    const p = (n) => String(n).padStart(2, '0');
    const from = a ? `${p(a.getHours())}:${p(a.getMinutes())}` : '??:??';
    if (!b) return `${from} — running`;
    return `${from} — ${p(b.getHours())}:${p(b.getMinutes())}`;
}

function showError(message) {
    if (!$trkModalError) return;
    if (!message) {
        $trkModalError.hidden = true;
        $trkModalError.textContent = '';
        return;
    }
    $trkModalError.textContent = message;
    $trkModalError.hidden = false;
}

/**
 * Ошибка сервера человеческим языком.
 *
 * Контракт ошибок плагина машинный и англоязычный
 * (plugins/tracker/src/routes/error.rs), а карточку читает человек. Переводим
 * ровно те случаи, которые он реально способен получить руками; всё
 * остальное показываем как есть — молчать хуже, чем показать техническую
 * строку.
 */
function humanError(e) {
    const msg = String((e && e.message) || e || '');
    if (/stopped_at must be later than started_at/i.test(msg)) {
        return 'конец отрезка должен быть позже начала';
    }
    if (/^bad started_at/i.test(msg)) return 'не разобрать начало отрезка';
    if (/^bad stopped_at/i.test(msg)) return 'не разобрать конец отрезка';
    if (e && e.code === 'conflict') {
        return `по задаче уже идёт таймер (${msg})`;
    }
    return msg;
}

function btn(cls, text, title) {
    const b = el('button', cls, text);
    b.type = 'button';
    if (title) b.title = title;
    return b;
}

/** `spent 5:12:44 · 130% от оценки` — подпись справа от поля estimate. */
function spentLabel(seconds, estimateMin) {
    const spent = `spent ${fmtDuration(seconds)}`;
    if (!estimateMin) return spent;
    return `${spent} · ${Math.round((seconds / 60) / estimateMin * 100)}% от оценки`;
}

// ------------------------------------------------------------------ отрисовка

function renderBody(task, snap) {
    $trkModalBody.textContent = '';
    showError('');

    const lists = Array.isArray(snap.lists) ? snap.lists : [];
    const seconds = (snap.task_seconds && snap.task_seconds[task.id]) || 0;
    const running = activeEntryOf(snap, task.id);

    // --- шапка: заголовок и таймер задачи ---
    const title = input(task.title, 'заголовок задачи');
    const head = el('div', 'trk-modal-section');
    head.appendChild(field('title', title));

    // Таймер задачи есть и в ленте, и здесь: карточку открывают, чтобы «сесть
    // за задачу», и уходить ради старта обратно в список незачем.
    const timerRow = el('div', 'trk-node-timer');
    const timerBtn = btn('trk-node-play', running ? '[▮]' : '[▸]',
        running ? 'Остановить таймер задачи' : 'Запустить таймер задачи');
    timerBtn.addEventListener('click', () => toggleTaskTimer(task.id, !!running));
    timerRow.appendChild(timerBtn);
    const timerVal = el('span', 'trk-node-time', fmtDuration(seconds));
    timerVal.dataset.trkModalTotal = task.id;
    timerRow.appendChild(timerVal);
    timerRow.appendChild(el('span', 'trk-node-hint',
        running ? 'таймер задачи идёт' : 'общее время задачи'));
    head.appendChild(field('timer', timerRow));
    $trkModalBody.appendChild(head);

    // --- // FIELDS ---
    const fields = el('div', 'trk-modal-section');
    fields.appendChild(el('div', 'trk-section-title', '// FIELDS'));

    const listSel = select(
        [['', '— inbox —'], ...lists.map((l) => [l.id, l.name])],
        task.list_id || '',
    );
    fields.appendChild(field('list', listSel));

    const statusSel = select(STATUSES, task.status || 'inbox');
    fields.appendChild(field('status', statusSel));

    const prioSel = select(
        [0, 1, 2, 3, 4].map((n) => [String(n), `P${n}`]),
        String(Number.isFinite(Number(task.priority)) ? Number(task.priority) : 2),
    );
    fields.appendChild(field('priority', prioSel));

    // Оценка: типовые длительности + «своё». Числовое поле показывается
    // только в режиме «своё», но существует всегда — так значение не из
    // списка (`~90м` из prompt-строки) открывается КАК ЕСТЬ.
    const estBox = el('div', 'trk-est');
    const estSel = select(
        [['', '— без оценки —'], ...ESTIMATE_PRESETS.map(([m, label]) => [String(m), label]),
            [ESTIMATE_CUSTOM, 'своё…']],
        '',
    );
    const estCustom = document.createElement('input');
    estCustom.type = 'number';
    estCustom.min = '1';
    estCustom.step = '5';
    estCustom.placeholder = 'минут';
    const estChoice = estimateChoice(task.estimate_min);
    estSel.value = estChoice.mode;
    estCustom.value = estChoice.custom;
    const syncEst = () => { estCustom.hidden = estSel.value !== ESTIMATE_CUSTOM; };
    syncEst();
    estSel.addEventListener('change', () => {
        syncEst();
        if (estSel.value === ESTIMATE_CUSTOM) estCustom.focus();
    });
    estBox.appendChild(estSel);
    estBox.appendChild(estCustom);
    const estRow = field('estimate', estBox, spentLabel(seconds, task.estimate_min));
    const estExtraNode = estRow.querySelector('.trk-field-extra');
    fields.appendChild(estRow);

    // start — БЕЗ времени: Гантт рисует плановую полосу по дням, времени
    // начала не показывает никто (см. fields.js).
    const start = document.createElement('input');
    start.type = 'date';
    start.value = toDateInput(task.start);
    fields.appendChild(field('start', start));

    // due — СО временем: `due:18:00` из prompt-строки и «сегодня 18:00» из
    // макета обязаны быть видны и правимы.
    const due = document.createElement('input');
    due.type = 'datetime-local';
    due.value = toDueInput(task.due);
    fields.appendChild(field('due', due));

    // Теги — чипы с крестиком плюс поле с подсказками из УЖЕ существующих
    // тегов снимка (тот же источник, что у блока `// TAGS` в сайдбаре).
    const tags = tagEditor(task.tags, knownTags(snap.tasks));
    fields.appendChild(field('tags', tags.node));

    fields.appendChild(field(
        'created',
        el('div', null, fmtMoment(task.created_at)),
        `updated ${fmtMoment(task.updated_at)}`,
    ));
    $trkModalBody.appendChild(fields);

    // --- // NOTES ---
    const notesBox = el('div', 'trk-modal-section');
    notesBox.appendChild(el('div', 'trk-section-title', '// NOTES'));
    const notes = document.createElement('textarea');
    notes.rows = 4;
    notes.spellcheck = false;
    notes.value = task.notes == null ? '' : String(task.notes);
    notesBox.appendChild(notes);
    $trkModalBody.appendChild(notesBox);

    // --- // CHECKLIST и // TIME: живые блоки, перерисовываются отдельно ---
    const check = el('div', 'trk-modal-section');
    $trkModalBody.appendChild(check);
    const time = el('div', 'trk-modal-section');
    $trkModalBody.appendChild(time);

    local.fields = {
        title, listSel, statusSel, prioSel, estSel, estCustom, start, due, tags, notes,
    };
    local.sections = { check, time, timerBtn, timerVal, estExtraNode };
    paintChecklist(snap);
    paintTime(snap);
}

// ---------------------------------------------------------------- // CHECKLIST

/** Полоса прогресса из макета: `████████░░░░░░░░░░░░`, 20 делений. */
export function progressBar(done, total, width) {
    const w = width || 20;
    if (!total) return '░'.repeat(w);
    const filled = Math.round((Math.max(0, Math.min(done, total)) / total) * w);
    return '█'.repeat(filled) + '░'.repeat(w - filled);
}

/** Прогресс `n/m · %` — считается так же, как `ProgressDto` на сервере. */
export function progressOf(items) {
    const total = items.length;
    const done = items.filter((i) => i && i.done).length;
    return { done, total, percent: total ? Math.round((done / total) * 100) : 0 };
}

/** Пункты задачи из снимка, в порядке `order`. */
function itemsOf(snap, taskId) {
    return (Array.isArray(snap && snap.items) ? snap.items : [])
        .filter((i) => i && i.task_id === taskId)
        .sort((a, b) => (Number(a.order) || 0) - (Number(b.order) || 0));
}

/** Идущий отрезок задачи (он же — единственный, инвариант стора). */
function activeEntryOf(snap, taskId) {
    return (Array.isArray(snap && snap.active) ? snap.active : [])
        .find((e) => e && e.task_id === taskId) || null;
}

/** `08.09 10:30` для отмеченного пункта, `идёт с 14:02` для идущего. */
function itemWhen(item, running) {
    const p = (n) => String(n).padStart(2, '0');
    if (running) {
        const s = parseTs(running.started_at);
        return s ? `идёт с ${p(s.getHours())}:${p(s.getMinutes())}` : 'идёт';
    }
    const d = parseTs(item.done_at);
    if (!d) return '';
    return `${p(d.getDate())}.${p(d.getMonth() + 1)} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function paintChecklist(snap) {
    const box = local.sections && local.sections.check;
    if (!box || !local.task) return;
    const taskId = local.task.id;
    const items = itemsOf(snap, taskId);
    // Прогресс берём серверный, а при его отсутствии (задача только что
    // создана и в снимок ещё не попала) считаем по тем же правилам.
    const pr = (snap.progress && snap.progress[taskId]) || progressOf(items);
    const itemSeconds = snap.item_seconds || {};
    const active = activeEntryOf(snap, taskId);

    box.textContent = '';
    const head = el('div', 'trk-check-head');
    head.appendChild(el('div', 'trk-section-title',
        `// CHECKLIST — ${pr.done}/${pr.total} · ${pr.percent}%`));
    if (pr.total) head.appendChild(el('span', 'trk-check-bar', progressBar(pr.done, pr.total)));
    head.appendChild(el('span', 'trk-grow'));
    const addBtn = btn('trk-mini-link', '[+ пункт]', 'Добавить пункт чек-листа');
    head.appendChild(addBtn);
    box.appendChild(head);

    items.forEach((item) => box.appendChild(itemRow(item, {
        running: active && active.item_id === item.id ? active : null,
        seconds: Number(itemSeconds[item.id]) || 0,
        taskId,
    })));

    // Строка ввода нового пункта. Её значение переживает перерисовку блока:
    // кадр WS о чужой правке не должен стирать набранный текст.
    const add = el('div', 'trk-item trk-item-add');
    add.appendChild(el('span', 'trk-item-box', '[+]'));
    const addInput = input(local.draftItem, 'новый пункт, Enter — добавить');
    addInput.addEventListener('input', () => { local.draftItem = addInput.value; });
    addInput.addEventListener('keydown', (ev) => {
        if (ev.key === 'Enter') {
            ev.preventDefault();
            addItem(taskId, addInput.value);
        }
    });
    add.appendChild(addInput);
    box.appendChild(add);
    addBtn.addEventListener('click', () => addInput.focus());

    box.appendChild(el('div', 'trk-check-note',
        'пункты можно вести таймером отдельно — их время суммируется в общее время задачи'));

    installItemDnd(box);
    box.__trkTaskId = taskId;
}

/** Одна строка пункта чек-листа. */
function itemRow(item, ctx) {
    const row = el('div', 'trk-item');
    row.dataset.itemId = item.id;
    row.draggable = true;
    if (item.done) row.classList.add('done');
    if (ctx.running) row.classList.add('running');

    const box = btn('trk-item-box', item.done ? '[x]' : '[ ]',
        item.done ? 'Снять отметку' : 'Отметить пункт');
    box.addEventListener('click', () => toggleItem(ctx.taskId, item));
    row.appendChild(box);

    if (local.busy.item === item.id) {
        // Переименование: поле встаёт НА МЕСТО текста, остальные колонки
        // остаются — строка не «прыгает» под курсором.
        const edit = input(item.title, 'текст пункта');
        edit.className = 'trk-item-edit';
        edit.addEventListener('keydown', (ev) => {
            if (ev.key === 'Enter') { ev.preventDefault(); renameItem(ctx.taskId, item, edit.value); }
            if (ev.key === 'Escape') { ev.stopPropagation(); cancelItemEdit(); }
        });
        edit.addEventListener('blur', () => renameItem(ctx.taskId, item, edit.value));
        row.appendChild(edit);
        setTimeout(() => { edit.focus(); edit.select(); }, 0);
    } else {
        const title = el('span', 'trk-item-title', item.title || '');
        title.title = 'клик — переименовать';
        title.addEventListener('click', () => {
            local.busy.item = item.id;
            paintChecklist(currentSnapshot());
        });
        row.appendChild(title);
    }

    row.appendChild(el('span', 'trk-item-when', itemWhen(item, ctx.running)));

    const time = el('span', 'trk-item-time', ctx.seconds ? fmtDuration(ctx.seconds) : '—');
    time.dataset.trkItemTime = item.id;
    row.appendChild(time);

    const play = btn('trk-item-play', ctx.running ? '[▮]' : '[▸]',
        ctx.running ? 'Остановить таймер пункта' : 'Запустить таймер пункта');
    play.addEventListener('click', () => toggleItemTimer(ctx.taskId, item, !!ctx.running));
    row.appendChild(play);

    const rm = btn('trk-item-rm', '[rm]', 'Удалить пункт (потраченное время останется у задачи)');
    rm.addEventListener('click', () => removeItem(ctx.taskId, item));
    row.appendChild(rm);

    return row;
}

/**
 * Перетаскивание пунктов.
 *
 * Обработчики навешиваются на КОНТЕЙНЕР один раз, а строки пересоздаются на
 * каждой перерисовке — поэтому актуальный список пунктов читается из
 * `box.__trkTaskId` и свежего снимка, а не из замыкания (та же ловушка, что в
 * js/tracker/render-list.js).
 */
function installItemDnd(box) {
    if (box.dataset.dndInstalled) return;
    box.dataset.dndInstalled = '1';
    let dragId = null;

    box.addEventListener('dragstart', (ev) => {
        const row = ev.target.closest && ev.target.closest('.trk-item[data-item-id]');
        if (!row) return;
        dragId = row.dataset.itemId;
        try { ev.dataTransfer.setData('text/plain', dragId); } catch (_) { /* Safari */ }
        if (ev.dataTransfer) ev.dataTransfer.effectAllowed = 'move';
    });
    box.addEventListener('dragover', (ev) => {
        if (!dragId) return;
        const row = ev.target.closest && ev.target.closest('.trk-item[data-item-id]');
        if (!row || row.dataset.itemId === dragId) return;
        ev.preventDefault();
        if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'move';
    });
    box.addEventListener('drop', (ev) => {
        if (!dragId) return;
        const row = ev.target.closest && ev.target.closest('.trk-item[data-item-id]');
        const moved = dragId;
        dragId = null;
        if (!row) return;
        ev.preventDefault();
        const targetId = row.dataset.itemId;
        if (!targetId || targetId === moved) return;
        const rect = row.getBoundingClientRect();
        const after = (ev.clientY - rect.top) > rect.height / 2;
        reorderItems(box.__trkTaskId, moved, targetId, after);
    });
    box.addEventListener('dragend', () => { dragId = null; });
}

// -------------------------------------------------------------------- // TIME

/** Снимок из main.js — единственный источник данных живых блоков. */
function currentSnapshot() {
    return (local.deps && local.deps.snapshot && local.deps.snapshot()) || {};
}

function paintTime(snap) {
    const box = local.sections && local.sections.time;
    if (!box || !local.task) return;
    const taskId = local.task.id;
    const entries = (Array.isArray(snap.entries) ? snap.entries : [])
        .filter((e) => e && e.task_id === taskId)
        .sort((a, b) => String(b.started_at || '').localeCompare(String(a.started_at || '')));
    const seconds = (snap.task_seconds && snap.task_seconds[taskId]) || 0;
    const itemsById = new Map(itemsOf(snap, taskId).map((i) => [i.id, i]));

    box.textContent = '';
    const head = el('div', 'trk-check-head');
    head.appendChild(el('div', 'trk-section-title',
        `// TIME — ${entries.length} ENTRIES · ${fmtDuration(seconds)}`));
    head.appendChild(el('span', 'trk-grow'));
    const manual = btn('trk-mini-link', '[+ manual]', 'Добавить отрезок вручную');
    manual.addEventListener('click', () => {
        local.busy.entry = 'new';
        paintTime(currentSnapshot());
    });
    head.appendChild(manual);
    box.appendChild(head);

    if (local.busy.entry === 'new') {
        const now = new Date();
        const hourAgo = new Date(now.getTime() - 60 * 60 * 1000);
        box.appendChild(entryForm({
            started_at: hourAgo.toISOString(),
            stopped_at: now.toISOString(),
            note: '',
        }, itemsById, null));
    }

    if (!entries.length && local.busy.entry !== 'new') {
        box.appendChild(el('div', 'trk-empty-hint', 'отрезков нет'));
    }

    entries.forEach((e) => {
        if (local.busy.entry === e.id) {
            box.appendChild(entryForm(e, itemsById, e.id));
            return;
        }
        box.appendChild(entryRow(e, itemsById, snap));
    });
}

/** Одна строка ленты отрезков. */
function entryRow(entry, itemsById, snap) {
    const row = el('div', 'trk-entry');
    row.dataset.entryId = entry.id;
    const running = !entry.stopped_at;
    if (running) row.classList.add('running');

    row.appendChild(el('span', 'trk-entry-mark', running ? '●' : '·'));
    const d = parseTs(entry.started_at);
    row.appendChild(el('span', 'trk-entry-day', d
        ? `${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')}`
        : '—'));
    row.appendChild(el('span', 'trk-entry-range', fmtRange(entry)));

    const item = entry.item_id && itemsById.get(entry.item_id);
    const note = [item ? `▸ ${item.title}` : '', entry.note || ''].filter(Boolean).join(' · ');
    row.appendChild(el('span', 'trk-entry-note', note));
    row.appendChild(el('span', 'trk-entry-manual', entry.source === 'manual' ? '[manual]' : ''));

    const value = el('span', 'trk-entry-value', fmtDuration(entrySeconds(entry, snap)));
    value.dataset.trkEntryTime = entry.id;
    row.appendChild(value);

    const edit = btn('trk-entry-edit', '[edit]', 'Поправить время отрезка');
    edit.addEventListener('click', () => {
        local.busy.entry = entry.id;
        paintTime(currentSnapshot());
    });
    row.appendChild(edit);

    const rm = btn('trk-entry-rm', '[rm]', 'Удалить отрезок');
    rm.addEventListener('click', () => removeEntry(entry));
    row.appendChild(rm);

    return row;
}

/**
 * Форма отрезка: и правка существующего, и добавление вручную.
 *
 * Пустое поле конца означает «отрезок идёт»: так закрывается забытый таймер
 * (заполнить конец) и так же вручную заведённый отрезок можно оставить
 * активным. Отправляется это как `stopped_at: null` — модель различает
 * «не передали» и «передали null».
 */
function entryForm(entry, itemsById, entryId) {
    const form = el('div', 'trk-entry-form');
    const from = document.createElement('input');
    from.type = 'datetime-local';
    from.value = toLocalInput(entry.started_at);
    const to = document.createElement('input');
    to.type = 'datetime-local';
    to.value = toLocalInput(entry.stopped_at);
    const note = input(entry.note || '', 'комментарий');
    const itemSel = select(
        [['', '— вся задача —'], ...[...itemsById.values()].map((i) => [i.id, i.title])],
        entry.item_id || '',
    );

    form.appendChild(el('span', 'trk-entry-form-key', 'from'));
    form.appendChild(from);
    form.appendChild(el('span', 'trk-entry-form-key', 'to'));
    form.appendChild(to);
    form.appendChild(itemSel);
    form.appendChild(note);

    const ok = btn('trk-btn trk-btn-primary', entryId ? 'SAVE' : 'ADD');
    ok.addEventListener('click', () => submitEntry(entryId, {
        started_at: fromLocalInput(from.value),
        stopped_at: fromLocalInput(to.value),
        item_id: itemSel.value || null,
        note: note.value.trim() || null,
    }));
    form.appendChild(ok);

    const cancel = btn('trk-btn', 'CANCEL');
    cancel.addEventListener('click', () => {
        local.busy.entry = null;
        showError('');
        paintTime(currentSnapshot());
    });
    form.appendChild(cancel);
    return form;
}

/** Длительность одного отрезка в секундах на момент снимка. */
function entrySeconds(entry, snap) {
    const nowMs = (parseTs(snap && snap.now) || new Date()).getTime();
    return entryElapsed(entry, nowMs);
}

// -------------------------------------------------------- действия чек-листа

/** Общий хвост любой мутации: показать ошибку или перечитать состояние. */
async function mutate(what, fn) {
    try {
        await fn();
    } catch (e) {
        showError(`${what}: ${humanError(e)}`);
        return false;
    }
    showError('');
    // Кадр WS всё равно придёт, но перезапрос делает карточку независимой от
    // сокета: с закрытым сокетом кнопки обязаны работать.
    if (local.deps && local.deps.refresh) await local.deps.refresh();
    return true;
}

async function addItem(taskId, raw) {
    const title = String(raw || '').trim();
    if (!title) return;
    local.draftItem = '';
    await mutate('пункт не добавлен', () => api.createItem(taskId, { title }));
}

async function toggleItem(taskId, item) {
    await mutate('отметка не сохранена', () => api.toggleItem(taskId, item.id));
}

function cancelItemEdit() {
    local.busy.item = null;
    paintChecklist(currentSnapshot());
}

async function renameItem(taskId, item, raw) {
    if (local.busy.item !== item.id) return;
    const title = String(raw || '').trim();
    local.busy.item = null;
    if (!title || title === item.title) {
        paintChecklist(currentSnapshot());
        return;
    }
    await mutate('пункт не переименован', () => api.patchItem(taskId, item.id, { title }));
}

async function removeItem(taskId, item) {
    // eslint-disable-next-line no-alert
    if (!window.confirm(`Удалить пункт «${item.title}»? Потраченное на него время останется у задачи.`)) return;
    await mutate('пункт не удалён', () => api.deleteItem(taskId, item.id));
}

/** Новый порядок пунктов: полный список id задачи с перемещённым элементом. */
async function reorderItems(taskId, movedId, targetId, after) {
    if (!taskId) return;
    const ids = itemsOf(currentSnapshot(), taskId).map((i) => i.id).filter((id) => id !== movedId);
    let at = ids.indexOf(targetId);
    if (at < 0) return;
    if (after) at += 1;
    ids.splice(at, 0, movedId);
    await mutate('порядок не сохранён', () => api.reorderItems(taskId, ids));
}

/**
 * Старт/стоп таймера пункта.
 *
 * Правило одного активного отрезка на задачу здесь ЯВНОЕ: если по задаче уже
 * что-то идёт (её собственный таймер или таймер другого пункта), этот отрезок
 * сначала закрывается, и только потом стартует таймер выбранного пункта. Без
 * такого переключения сервер вернул бы прежний отрезок, и человек, нажавший
 * `[▸]` на пункте, увидел бы, что ничего не изменилось.
 */
async function toggleItemTimer(taskId, item, running) {
    await mutate('таймер пункта', async () => {
        if (running) {
            await api.stopItemTimer(taskId, item.id);
            return;
        }
        const active = activeEntryOf(currentSnapshot(), taskId);
        if (active) await api.stopTimer(taskId);
        await api.startItemTimer(taskId, item.id);
    });
}

/** Старт/стоп таймера всей задачи из карточки. */
async function toggleTaskTimer(taskId, running) {
    await mutate('таймер задачи', () => (
        running ? api.stopTimer(taskId) : api.startTimer(taskId)
    ));
}

// --------------------------------------------------------- ручные отрезки

/**
 * Сохранить форму отрезка: правка существующего или новый вручную.
 *
 * Перевёрнутый интервал сервер отвергает кодом 400 — форма при этом остаётся
 * открытой с набранными значениями, а причина показывается по-русски.
 */
async function submitEntry(entryId, form) {
    if (!local.task) return;
    if (!form.started_at) {
        showError('отрезок не сохранён: не задано начало');
        return;
    }
    const ok = await mutate('отрезок не сохранён', () => {
        if (entryId) {
            // `stopped_at: null` — законное значение: так забытый таймер снова
            // становится активным. Поэтому поле уезжает ВСЕГДА.
            return api.patchEntry(entryId, {
                started_at: form.started_at,
                stopped_at: form.stopped_at,
                item_id: form.item_id,
                note: form.note,
            });
        }
        return api.createEntry({
            task_id: local.task.id,
            started_at: form.started_at,
            stopped_at: form.stopped_at,
            item_id: form.item_id,
            note: form.note,
        });
    });
    if (!ok) return;
    local.busy.entry = null;
    paintTime(currentSnapshot());
}

async function removeEntry(entry) {
    // eslint-disable-next-line no-alert
    if (!window.confirm('Удалить отрезок? Время будет вычтено из сумм.')) return;
    await mutate('отрезок не удалён', () => api.deleteEntry(entry.id));
}

// ------------------------------------------------------------- живые обновления

/**
 * Новый снимок состояния: перерисовать ТОЛЬКО живые блоки.
 *
 * Форму (`// FIELDS`, `// NOTES`) не трогаем никогда — в ней может быть
 * набранный текст. Блок с открытым inline-редактором пропускаем по той же
 * причине.
 */
export function onTrackerSnapshot(snap) {
    if (!isTaskModalOpen() || !local.task) return;
    const fresh = (Array.isArray(snap && snap.tasks) ? snap.tasks : [])
        .find((t) => t && t.id === local.task.id);
    if (!fresh) {
        // Задачу удалили в другой вкладке — карточке нечего показывать.
        closeTaskModal();
        return;
    }
    if (!local.busy.item) paintChecklist(snap);
    if (!local.busy.entry) paintTime(snap);
    paintLive(snap, (parseTs(snap.now) || new Date()).getTime());
}

/**
 * Тик секундомера внутри карточки: только текстовые узлы.
 *
 * @param {object} snap — текущий снимок
 * @param {number} nowMs — серверное «сейчас» на текущий момент
 * @param {{task: Object<string, number>, item: Object<string, number>}} [live]
 *        живые суммы из js/tracker/timer.js
 */
export function paintLive(snap, nowMs, live) {
    if (!isTaskModalOpen() || !local.task || !$trkModalBody) return;
    const taskId = local.task.id;
    const liveTask = (live && live.task) || {};
    const liveItem = (live && live.item) || {};

    const total = liveTask[taskId] !== undefined
        ? liveTask[taskId]
        : ((snap.task_seconds && snap.task_seconds[taskId]) || 0);
    const s = local.sections || {};
    if (s.timerVal) s.timerVal.textContent = fmtDuration(total);
    if (s.estExtraNode) {
        s.estExtraNode.textContent = spentLabel(total, local.task.estimate_min);
    }

    Array.from($trkModalBody.querySelectorAll('[data-trk-item-time]')).forEach((node) => {
        const id = node.dataset.trkItemTime;
        if (liveItem[id] === undefined) return;
        node.textContent = fmtDuration(liveItem[id]);
    });

    const active = activeEntryOf(snap, taskId);
    if (!active) return;
    Array.from($trkModalBody.querySelectorAll('[data-trk-entry-time]')).forEach((node) => {
        if (node.dataset.trkEntryTime !== active.id) return;
        node.textContent = fmtDuration(entryElapsed(active, nowMs));
    });
}

// ------------------------------------------------------------------ сохранение

/** Строка формы → значение поля или `null` (сброс). */
function orNull(raw) {
    const s = String(raw == null ? '' : raw).trim();
    return s === '' ? null : s;
}

/**
 * Тело PATCH: только изменённое.
 *
 * ЧИСТАЯ функция — форму описывает объект `form`, задачу до правки `task`.
 * Именно здесь живёт разница «не передали» / «передали null», поэтому она
 * вынесена из обработчика и проверяема отдельно.
 */
export function buildPatch(task, form) {
    const patch = {};
    const title = String(form.title || '').trim();
    if (title && title !== task.title) patch.title = title;

    const listId = orNull(form.list_id);
    if (listId !== (task.list_id || null)) patch.list_id = listId;

    if (form.status && form.status !== task.status) patch.status = form.status;

    const prio = Number(form.priority);
    if (Number.isFinite(prio) && prio !== Number(task.priority)) patch.priority = prio;

    const notes = orNull(form.notes);
    if (notes !== (task.notes || null)) patch.notes = notes;

    const estRaw = orNull(form.estimate_min);
    const est = estRaw === null ? null : Number(estRaw);
    const estOld = task.estimate_min == null ? null : Number(task.estimate_min);
    if (est === null || Number.isFinite(est)) {
        if (est !== estOld) patch.estimate_min = est;
    }

    const start = orNull(form.start);
    if (start !== (task.start || null)) patch.start = start;

    const due = orNull(form.due);
    if (due !== (task.due || null)) patch.due = due;

    // Список тегов приходит из редактора чипов уже нормализованным; строка
    // (старая форма поля `#тег #тег`, prompt-строка, e2e) тоже принимается.
    const tags = Array.isArray(form.tags) ? splitTags(form.tags.join(' ')) : splitTags(form.tags);
    const oldTags = Array.isArray(task.tags) ? task.tags : [];
    if (tags.join(' ') !== oldTags.join(' ')) patch.tags = tags;

    return patch;
}

async function save() {
    if (!local.task || !local.fields) return;
    const f = local.fields;
    const patch = buildPatch(local.task, {
        title: f.title.value,
        list_id: f.listSel.value,
        status: f.statusSel.value,
        priority: f.prioSel.value,
        notes: f.notes.value,
        // Значения пикеров приводятся к форме МОДЕЛИ до сравнения: buildPatch
        // сличает строки, и `null` от пустого поля означает СБРОС.
        estimate_min: estimateValue(f.estSel.value, f.estCustom.value),
        start: fromDateInput(f.start.value),
        due: fromDueInput(f.due.value),
        tags: f.tags.getTags(),
    });
    if (!Object.keys(patch).length) {
        closeTaskModal();
        return;
    }
    try {
        await api.patchTask(local.task.id, patch);
    } catch (e) {
        showError(`SAVE не удался: ${e.message}`);
        return;
    }
    closeTaskModal();
    if (local.deps && local.deps.refresh) local.deps.refresh();
}

async function markDone() {
    if (!local.task) return;
    const next = local.task.status === 'done' ? 'inbox' : 'done';
    try {
        await api.setTaskStatus(local.task.id, next);
    } catch (e) {
        showError(`смена статуса не удалась: ${e.message}`);
        return;
    }
    closeTaskModal();
    if (local.deps && local.deps.refresh) local.deps.refresh();
}

async function removeTask() {
    if (!local.task) return;
    const title = local.task.title || local.task.id;
    // eslint-disable-next-line no-alert
    if (!window.confirm(`Удалить задачу «${title}» вместе с её временем?`)) return;
    try {
        await api.deleteTask(local.task.id);
    } catch (e) {
        showError(`DELETE не удался: ${e.message}`);
        return;
    }
    closeTaskModal();
    if (local.deps && local.deps.refresh) local.deps.refresh();
}

// -------------------------------------------------------------------- открытие

/** Esc закрывает карточку. Слушатель живёт на документе, снимается в teardown. */
function onKeydown(ev) {
    if (ev.key !== 'Escape') return;
    // Поверх карточки может стоять диалог переноса. Esc обязан снимать РОВНО
    // один слой: закрыв обоих сразу, мы потеряли бы недописанную правку из-за
    // отменённого переноса.
    if (isPushOpen() || isMilestoneOpen()) return;
    if (!$trkModal || $trkModal.hidden) return;
    // Ещё один слой ВНУТРИ карточки — открытый список подсказок тегов. Его
    // Esc обычно перехватывает сам ввод (слушатель на поле срабатывает
    // раньше document и гасит всплытие); эта ветка — на случай, когда фокус
    // уже ушёл из поля, а список ещё виден.
    if (local.fields && local.fields.tags && local.fields.tags.closeSuggest()) {
        ev.stopPropagation();
        return;
    }
    ev.stopPropagation();
    closeTaskModal();
}

/**
 * Навесить слушатели карточки. Идемпотентна: зовётся из initTracker на каждом
 * входе на вкладку.
 *
 * @param {{snapshot: function(): object, refresh: function(): void,
 *   push: function(object): void}} deps
 */
export function initTrackerModals(deps) {
    local.deps = deps || local.deps;
    if (local.wired) return;
    local.wired = true;
    if ($trkModalClose) $trkModalClose.addEventListener('click', closeTaskModal);
    if ($trkModalSave) $trkModalSave.addEventListener('click', save);
    if ($trkModalDone) $trkModalDone.addEventListener('click', markDone);
    if ($trkModalDelete) $trkModalDelete.addEventListener('click', removeTask);
    // Перенос в TODO — не мутация задачи, поэтому кнопка ничего не сохраняет и
    // карточку не закрывает: копируется задача В ТОМ ВИДЕ, В КАКОМ ОНА
    // СОХРАНЕНА, а не набранная в форме правка.
    if ($trkModalPush) {
        $trkModalPush.addEventListener('click', () => {
            if (!local.task) return;
            if (local.deps && typeof local.deps.push === 'function') {
                local.deps.push(local.task);
            }
        });
    }
    if ($trkModal) {
        // Клик мимо карточки закрывает её; клик ВНУТРИ — нет (проверяем
        // именно target, а не всплытие: иначе закрывалось бы от любого клика).
        $trkModal.addEventListener('click', (ev) => {
            if (ev.target === $trkModal) closeTaskModal();
        });
    }
    document.addEventListener('keydown', onKeydown);
}

/** Открыть карточку задачи. */
export function openTaskModal(task) {
    if (!$trkModal || !task) return;
    const snap = currentSnapshot();
    local.task = task;
    local.busy = { item: null, entry: null };
    local.draftItem = '';
    if ($trkModalNode) {
        $trkModalNode.textContent = `▸ NODE ${String(task.id).slice(0, 8)}`;
    }
    if ($trkModalDone) {
        $trkModalDone.textContent = task.status === 'done' ? 'REOPEN' : 'MARK DONE';
    }
    renderBody(task, snap);
    $trkModal.hidden = false;
    if (local.fields && local.fields.title) local.fields.title.focus();
}

/** Закрыть карточку. */
export function closeTaskModal() {
    if (!$trkModal) return;
    $trkModal.hidden = true;
    local.task = null;
    local.fields = null;
    local.sections = null;
    local.busy = { item: null, entry: null };
    local.draftItem = '';
    showError('');
}

/** Открыта ли карточка. */
export function isTaskModalOpen() {
    return !!$trkModal && !$trkModal.hidden;
}

/** Снять слушатель Esc (beforeunload / teardown панели). */
export function teardownTrackerModals() {
    document.removeEventListener('keydown', onKeydown);
    local.wired = false;
}
