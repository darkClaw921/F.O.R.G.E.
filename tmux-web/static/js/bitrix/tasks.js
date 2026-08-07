// tmux-web — Bitrix24 plugin: карточка задачи над лентой (Ф10).
//
// ============================================================================
// ПОЧЕМУ ЭТО ШАПКА, А НЕ ВТОРАЯ ВКЛАДКА (Р2)
// ============================================================================
//
// Чат задачи Битрикс24 — обычный IM-чат с entity_type="TASKS_TASK".
// Комментарии задачи и сообщения её чата — ОДИН И ТОТ ЖЕ поток
// (task.commentitem.getlist на «новых задачах» возвращает [] — legacy-форум
// мёртв). Поэтому «Задачи» здесь не второй мессенджер, а:
//
//   • директория `~/tasks` ОДНОГО списка диалогов (js/bitrix/dialogs.js);
//   • карточка-шапка `#b24-task-card` НАД той же лентой (этот модуль).
//
// Второй вкладки в `#settings-bar` нет намеренно.
//
// ============================================================================
// КНОПКИ СТРОЯТСЯ СТРОГО ПО `task.actions` И НИКОГДА — ПО СПИСКУ ВО ФРОНТЕ
// ============================================================================
//
// Портал присылает готовую матрицу прав, и она ДИНАМИЧНА: у ждущей задачи
// разрешён `start` и запрещён `pause`, после «Начать» — ровно наоборот.
// Захардкоженный набор кнопок означал бы «Начать» на уже выполняющейся задаче,
// то есть вызов, который портал молча проигнорирует (именно так `defer` на
// выполняющейся задаче оказался no-op при разведке).
//
// Отсюда правило: ПОСЛЕ КАЖДОГО ДЕЙСТВИЯ карточка перерисовывается ответом
// сервера, а лента перезапрашивается — портал пишет в чат задачи системное
// сообщение о смене статуса, и оно обязано появиться в переписке.
//
// ============================================================================
// ВЕТКА «ЗАДАЧА БЕЗ ЧАТА» (Р20)
// ============================================================================
//
// `dialog_id` карточки бывает пустым: у свежесозданной задачи чата может ещё
// не быть. Это ОБЯЗАТЕЛЬНАЯ ветка, а не редкий случай. В ней карточка
// показывается целиком, а вместо ленты идёт внятная подпись «обсуждение ещё не
// начато»; поле ввода блокируется — писать некуда.
//
// ============================================================================
// XSS-ИНВАРИАНТ
// ============================================================================
//
// `innerHTML` запрещён полностью: заголовок и описание задачи пишут люди.
// Только `createElement` + `textContent`.

import { $b24Input, $b24Prompt, $b24Send, $b24TaskCard } from '../core/dom.js';
import * as b24Api from './api.js';
import { errorLabel, taskActionLabel, taskDeadlineLabel, taskNoChatNote } from './format.js';
import { b24State } from './state.js';

/** Сколько задач тянем в директорию `~/tasks` за раз (страница платформы). */
export const TASK_PAGE = 50;

const local = {
    /** Слушатель кнопок навешен (init идемпотентен). */
    inited: false,
    /** Задача, показанная в шапке прямо сейчас (строкой) либо null. */
    activeTaskId: null,
    /** Идёт действие — второй клик по кнопкам не начинаем. */
    busy: false,
    /** Поколение загрузки карточки: ответ прошлой задачи не рисует новую. */
    generation: 0,
    /** Колбэк «перезапросить ленту диалога» (раздаёт main.js). */
    onReloadFeed: null,
    /** Колбэк «перерисовать колонку» (раздаёт main.js). */
    onTasksLoaded: null,
};

// ---------------------------------------------------------------------------
// Загрузка
// ---------------------------------------------------------------------------

/**
 * Загружает список задач в `b24State.tasks`.
 *
 * Нужен колонке: строка чата задачи получает статус-точку и бейдж
 * непрочитанных комментариев только тогда, когда карточка задачи известна.
 * Список диалогов такой информации не несёт вовсе — у него другой источник
 * (`im.recent.get`).
 *
 * Ошибка гасится молча: статус-точки — украшение, а не содержимое, и колонка
 * без них полностью работоспособна.
 *
 * @param {{filter?: string, start?: number}} [opts]
 * @returns {Promise<void>}
 */
export async function loadTasks(opts = {}) {
    try {
        const resp = await b24Api.getTasks({ filter: opts.filter, start: opts.start });
        const page = Array.isArray(resp && resp.tasks) ? resp.tasks : [];
        for (const task of page) {
            if (task && task.id != null) b24State.tasks.set(String(task.id), task);
        }
        if (typeof local.onTasksLoaded === 'function') local.onTasksLoaded();
    } catch (_) {
        // Статус-точки — украшение: колонка работает и без них.
    }
}

/**
 * Показывает карточку для открытого диалога.
 *
 * Для обычного чата карточка ПРЯЧЕТСЯ и очищается: оставленная шапка чужой
 * задачи над лентой переписки — самая дорогая из возможных ошибок этого
 * модуля, потому что кнопки действий в ней рабочие.
 *
 * @param {?object} dialog — DialogDto
 * @returns {Promise<void>}
 */
export async function showTaskFor(dialog) {
    const taskId = dialog && dialog.kind === 'task' && dialog.task_id != null
        ? String(dialog.task_id)
        : '';
    if (!taskId) {
        hideTaskCard();
        return;
    }
    const generation = ++local.generation;
    local.activeTaskId = taskId;

    // Известную карточку рисуем сразу — шапка не должна мигать пустотой, пока
    // летит запрос; свежая версия заменит её следом.
    const known = b24State.tasks.get(taskId);
    if (known) renderTaskCard(known);

    await refreshTask(taskId, generation);
}

/**
 * Перечитывает карточку у сервера и перерисовывает шапку.
 *
 * @param {string} taskId
 * @param {number} [generation]
 * @returns {Promise<?object>} TaskDto либо null
 */
export async function refreshTask(taskId, generation = local.generation) {
    const key = String(taskId == null ? '' : taskId);
    if (!key) return null;
    try {
        const task = await b24Api.getTask(key);
        if (generation !== local.generation) return null;
        if (task && task.id != null) b24State.tasks.set(String(task.id), task);
        if (String(local.activeTaskId) === key) renderTaskCard(task);
        return task;
    } catch (err) {
        if (generation !== local.generation) return null;
        // Карточку не показали — но лента задачи самодостаточна: в ней есть и
        // системные сообщения о смене статуса, и комментарии. Поэтому вместо
        // пустой шапки рисуем строку ошибки, а ленту не трогаем.
        if (String(local.activeTaskId) === key) renderTaskError(err);
        return null;
    }
}

/**
 * Применяет WS-кадр `task_update` (карточка изменилась в другой вкладке).
 *
 * @param {?object} task — TaskDto
 */
export function applyTaskUpdate(task) {
    if (!task || task.id == null) return;
    const key = String(task.id);
    b24State.tasks.set(key, task);
    if (String(local.activeTaskId) === key) renderTaskCard(task);
    if (typeof local.onTasksLoaded === 'function') local.onTasksLoaded();
}

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

/** Прячет шапку и забывает активную задачу. */
export function hideTaskCard() {
    local.activeTaskId = null;
    local.generation += 1;
    if (!$b24TaskCard) return;
    $b24TaskCard.textContent = '';
    $b24TaskCard.hidden = true;
    setPromptEnabled(true, null);
}

/** Строка ошибки вместо карточки: лента при этом остаётся рабочей. */
function renderTaskError(err) {
    if (!$b24TaskCard) return;
    $b24TaskCard.textContent = '';
    $b24TaskCard.hidden = false;
    const line = document.createElement('div');
    line.className = 'b24-task-error';
    line.textContent = `// карточка задачи недоступна: ${errorLabel(err && err.code)}`;
    $b24TaskCard.appendChild(line);
}

/**
 * Рисует карточку задачи.
 *
 * @param {?object} task — TaskDto
 */
export function renderTaskCard(task) {
    if (!$b24TaskCard) return;
    if (!task) {
        hideTaskCard();
        return;
    }
    $b24TaskCard.textContent = '';
    $b24TaskCard.hidden = false;
    $b24TaskCard.dataset.taskId = String(task.id);
    $b24TaskCard.dataset.status = String(task.status_key || 'unknown');

    $b24TaskCard.appendChild(buildHead(task));
    $b24TaskCard.appendChild(buildMeta(task));

    const actions = buildActions(task);
    if (actions) $b24TaskCard.appendChild(actions);

    const note = taskNoChatNote(task);
    if (note) {
        const box = document.createElement('div');
        box.className = 'b24-task-note';
        box.textContent = note;
        $b24TaskCard.appendChild(box);
    }
    // Поле ввода живо ровно тогда, когда есть куда писать (Р20).
    setPromptEnabled(!!task.dialog_id, note);
}

/** Заголовок: статус-точка, номер, название, ссылка на портал. */
function buildHead(task) {
    const head = document.createElement('div');
    head.className = 'b24-task-head';

    const dot = document.createElement('span');
    dot.className = `b24-task-dot b24-task-dot-${task.status_key || 'unknown'}`;
    dot.title = task.status_label || '';
    dot.setAttribute('aria-hidden', 'true');
    head.appendChild(dot);

    const num = document.createElement('span');
    num.className = 'b24-task-id';
    num.textContent = `#${task.id}`;
    head.appendChild(num);

    const title = document.createElement('span');
    title.className = 'b24-task-title';
    title.textContent = task.title || '(без названия)';
    head.appendChild(title);

    if (task.overdue) {
        const late = document.createElement('span');
        late.className = 'b24-task-overdue';
        late.textContent = 'ПРОСРОЧЕНО';
        late.title = 'Срок вышел, а задача не закрыта';
        head.appendChild(late);
    }

    // Ссылка «открыть в Битриксе». Абсолютный адрес собирает СЕРВЕР из хоста
    // портала: во фронте его взять неоткуда — хост часть вебхука и наружу
    // больше нигде не уезжает. Кода вебхука в адресе нет никогда.
    if (task.url) {
        const link = document.createElement('a');
        link.className = 'b24-task-link';
        link.href = String(task.url);
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
        link.textContent = 'открыть в Битриксе ↗';
        head.appendChild(link);
    }
    return head;
}

/** Строка полей: статус, приоритет, срок, постановщик, ответственный. */
function buildMeta(task) {
    const meta = document.createElement('div');
    meta.className = 'b24-task-meta';

    addField(meta, 'статус', task.status_label);
    addField(meta, 'приоритет', task.priority_label, `b24-task-prio-${task.priority_key || 'normal'}`);
    const deadline = taskDeadlineLabel(task);
    if (deadline) addField(meta, 'срок', deadline, task.overdue ? 'b24-task-late' : '');
    addField(meta, 'постановщик', task.created_by_name || task.created_by);
    addField(meta, 'ответственный', task.responsible_name || task.responsible_id);
    if (task.group_name) addField(meta, 'группа', task.group_name);
    if (Number(task.new_comments_count) > 0) {
        addField(meta, 'новых комментариев', String(task.new_comments_count), 'b24-task-new');
    }
    return meta;
}

/** Одно поле «подпись: значение». */
function addField(host, label, value, extraClass) {
    const text = value == null ? '' : String(value);
    if (!text) return;
    const box = document.createElement('span');
    box.className = extraClass ? `b24-task-field ${extraClass}` : 'b24-task-field';
    const cap = document.createElement('span');
    cap.className = 'b24-task-field-label';
    cap.textContent = `${label}:`;
    const val = document.createElement('span');
    val.className = 'b24-task-field-value';
    val.textContent = text;
    box.appendChild(cap);
    box.appendChild(val);
    host.appendChild(box);
}

/**
 * Ряд кнопок — СТРОГО по `task.actions`.
 *
 * Сервер уже отфильтровал матрицу двумя ситами: «портал разрешил ПРЯМО
 * СЕЙЧАС» и «панель умеет выполнить». Фронт не добавляет своего сита и не
 * подставляет ни одной кнопки от себя — иначе появилась бы кнопка, за которой
 * нет действия.
 *
 * @returns {?object} узел либо null, если действий нет
 */
function buildActions(task) {
    const list = Array.isArray(task.actions) ? task.actions : [];
    if (list.length === 0) return null;

    const row = document.createElement('div');
    row.className = 'b24-task-actions';
    for (const action of list) {
        const key = String(action);
        const btn = document.createElement('button');
        btn.className = 'b24-task-btn';
        btn.type = 'button';
        btn.dataset.b24Task = key;
        btn.textContent = taskActionLabel(key);
        btn.title = `Действие «${taskActionLabel(key)}» над задачей #${task.id}`;
        btn.disabled = local.busy;
        row.appendChild(btn);
    }
    return row;
}

/**
 * Включает или блокирует поле ввода.
 *
 * У задачи без чата (Р20) писать физически некуда: диалога не существует, и
 * отправка ушла бы в никуда с ошибкой портала. Поэтому поле блокируется с
 * объяснением, а не молча ломается на первом Enter.
 *
 * @param {boolean} enabled
 * @param {?string} reason — подсказка для заблокированного поля
 */
function setPromptEnabled(enabled, reason) {
    const on = !!enabled;
    if ($b24Input) {
        $b24Input.disabled = !on;
        $b24Input.placeholder = on ? 'сообщение' : 'обсуждение ещё не начато';
        if (reason) $b24Input.title = reason;
        else $b24Input.removeAttribute('title');
    }
    if ($b24Send) $b24Send.disabled = !on;
    if ($b24Prompt) $b24Prompt.classList.toggle('b24-prompt-locked', !on);
}

// ---------------------------------------------------------------------------
// Действия
// ---------------------------------------------------------------------------

/**
 * Выполняет действие над активной задачей.
 *
 * После успеха:
 *   1. карточка перерисовывается ответом сервера — матрица УЖЕ другая;
 *   2. лента перезапрашивается — портал написал в чат задачи системное
 *      сообщение о смене статуса, и оно обязано появиться в переписке.
 *
 * Второй шаг нельзя заменить ожиданием realtime: события задач портал вебхуку
 * не присылает вовсе, а фолбэк-синхронизация приедет через минуту.
 *
 * @param {string} action — ключ действия портала
 * @returns {Promise<void>}
 */
export async function runTaskAction(action) {
    const taskId = local.activeTaskId;
    const key = String(action == null ? '' : action);
    if (!taskId || !key || local.busy) return;

    local.busy = true;
    setButtonsDisabled(true);
    try {
        const resp = await b24Api.taskAction(taskId, key);
        const task = resp && resp.task;
        if (task && task.id != null) {
            b24State.tasks.set(String(task.id), task);
            if (String(local.activeTaskId) === String(task.id)) renderTaskCard(task);
        } else {
            // Портал действие принял, а карточку сервер перечитать не смог:
            // спрашиваем сами, а не оставляем шапку со старым статусом.
            await refreshTask(taskId);
        }
        if (typeof local.onTasksLoaded === 'function') local.onTasksLoaded();

        const dialogId = (resp && resp.dialog_id) || (task && task.dialog_id) || null;
        if (dialogId && typeof local.onReloadFeed === 'function') {
            local.onReloadFeed(String(dialogId));
        }
    } catch (err) {
        renderActionError(err);
    } finally {
        local.busy = false;
        setButtonsDisabled(false);
    }
}

/**
 * Гасит или отпускает кнопки, не пересобирая карточку.
 *
 * Обход по `data-b24-task`, а не селектором: в панели селекторы не
 * используются вовсе (`closestWithData` в dialogs.js существует ровно поэтому)
 * — тогда и фейковый DOM тестов остаётся простым, и опечатка в классе не даёт
 * молча пустую выборку.
 */
function setButtonsDisabled(disabled) {
    if (!$b24TaskCard) return;
    const stack = [$b24TaskCard];
    while (stack.length > 0) {
        const node = stack.pop();
        if (node.dataset && node.dataset.b24Task) node.disabled = !!disabled;
        for (const child of (node.childNodes || [])) stack.push(child);
    }
}

/**
 * Показывает отказ действия строкой В КАРТОЧКЕ.
 *
 * `409 invalid_state` здесь — не сбой, а нормальный ответ: матрица могла
 * измениться в чужом браузере, и кнопка на экране устарела. Поэтому карточка
 * тут же перечитывается — пользователь увидит актуальный набор кнопок.
 */
function renderActionError(err) {
    if (!$b24TaskCard) return;
    const line = document.createElement('div');
    line.className = 'b24-task-error';
    line.textContent = `// ${errorLabel(err && err.code)}`;
    $b24TaskCard.appendChild(line);
    if (err && err.code === 'invalid_state' && local.activeTaskId) {
        refreshTask(local.activeTaskId);
    }
}

// ---------------------------------------------------------------------------
// Инициализация
// ---------------------------------------------------------------------------

/**
 * Навешивает делегированный слушатель кнопок карточки. Идемпотентна.
 *
 * Делегирование обязательно: карточка пересобирается после КАЖДОГО действия
 * (матрица динамична), и слушатели на самих кнопках пришлось бы вешать заново
 * каждый раз.
 *
 * @param {{onReloadFeed?: function, onTasksLoaded?: function}} [handlers]
 */
export function initTasks(handlers = {}) {
    local.onReloadFeed = typeof handlers.onReloadFeed === 'function' ? handlers.onReloadFeed : null;
    local.onTasksLoaded = typeof handlers.onTasksLoaded === 'function' ? handlers.onTasksLoaded : null;

    if (local.inited) return;
    local.inited = true;
    if (!$b24TaskCard) return;
    $b24TaskCard.addEventListener('click', (e) => {
        let node = e.target;
        for (let i = 0; node && i < 6; i += 1) {
            if (node.dataset && node.dataset.b24Task) {
                e.preventDefault();
                runTaskAction(node.dataset.b24Task);
                return;
            }
            node = node.parentNode;
        }
    });
}

/** Сброс при logout и уходе со страницы. */
export function resetTasks() {
    local.activeTaskId = null;
    local.busy = false;
    local.generation += 1;
    hideTaskCard();
}

/** Снимок для e2e и диагностики. */
export function _debugTasksState() {
    return {
        inited: local.inited,
        activeTaskId: local.activeTaskId,
        busy: local.busy,
        known: b24State.tasks.size,
        cardVisible: !!($b24TaskCard && !$b24TaskCard.hidden),
    };
}
