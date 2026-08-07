// tmux-web — Bitrix24 plugin: колонка списка диалогов `#b24-contacts`.
//
// ============================================================================
// ОДИН СПИСОК, ЧЕТЫРЕ ДИРЕКТОРИИ (Р2, Вариант A)
// ============================================================================
//
// Главное отличие от VK: там `all|unread|important|unanswered` — это СЕРВЕРНЫЙ
// параметр `filter`, то есть отдельный запрос и отдельный список. Здесь портал
// отдаёт ОДИН список (`im.recent.get`), а разделение на `~/all`, `~/chats`,
// `~/tasks`, `~/unread` выводится из полей строки (`kind`, `unread_count`).
//
// Поэтому:
//   • переключение директории — ЛОКАЛЬНАЯ операция, сетевого запроса не стоит;
//   • второго списка (вроде `dirDialogs` у VK) здесь нет и не будет — он был бы
//     копией того же массива;
//   • правило попадания в директорию (`dirMatches` в format.js) — построчная
//     копия серверного `dir_matches`, иначе список менялся бы сам при
//     прокрутке.
//
// ============================================================================
// ГРАФ ИМПОРТОВ
// ============================================================================
//
// Колонка НЕ импортирует ленту (`chat.js`, Ф5): выбор диалога уходит наружу
// колбэком `onSelect`, который раздаёт точка сборки `main.js`. Обратный импорт
// сделал бы граф циклическим и утянул бы ленту в тесты колонки.
//
// ============================================================================
// БЕЛЫЙ СПИСОК
// ============================================================================
//
// Применяется в ДВУХ местах этого модуля — `visibleSource()` (сам список) и
// `renderDirs()` (счётчики директорий). Полный перечень всех точек применения
// — в шапке `js/bitrix/visibility.js`; пропуск любой из них означает протечку
// скрытого диалога.

import {
    $b24Contacts, $b24DialogList, $b24DirAllCount, $b24DirChatsCount, $b24DirList,
    $b24DirTasksCount, $b24DirUnreadCount, $b24NodesTitle,
} from '../core/dom.js';
import * as b24Api from './api.js';
import {
    dialogMark, dirCounts, dirMatches, displayName, findDialog, kindGlyph,
    mergeDialogs, nodesTitle, previewText, sortDialogs, taskOfDialog,
} from './format.js';
import { attachAvatar } from './media.js';
import { b24State, currentVisibilityFilter } from './state.js';
import { visibleDialogs } from './visibility.js';

/** Сколько строк просим за раз. */
export const DIALOG_PAGE = 40;

/** За сколько пикселей до низа колонки начинается догрузка. */
export const SCROLL_TAIL_PX = 120;

/**
 * Сколько строк в директории считается «похоже на пустоту».
 *
 * Ниже этого числа переключение директории догружает следующую страницу
 * ОБЩЕГО списка: пустая колонка читается как «тут ничего нет», а не как «ещё
 * не доехало».
 */
export const DIR_MIN_ROWS = 5;

/** Счётчики директорий по ключу — заполняются `initDialogs`. */
const DIR_COUNT_NODES = {};

const local = {
    /** Слушатели навешены (init идемпотентен). */
    inited: false,
    /** Идёт загрузка страницы — второй запрос не начинаем. */
    loading: false,
    /** Есть ли ещё страницы по данным сервера. */
    hasMore: false,
    /** Колбэк выбора строки (раздаёт main.js). */
    onSelect: null,
    /** Колбэк смены директории (раздаёт main.js): `~/tasks` тянет карточки. */
    onDir: null,
    /**
     * Колбэк «счётчики портала обновились» (раздаёт main.js).
     *
     * Нужен, потому что бейдж вкладки живёт в точке сборки, а счётчики
     * приезжают ЗДЕСЬ — ответом `GET /api/bitrix/filters`. Обратный импорт
     * (`dialogs.js` → `main.js`) замкнул бы граф модулей, поэтому связь
     * колбэком, как и у выбора строки.
     *
     * Без него бейдж обновлялся бы ТОЛЬКО кадром `counters` из WS: на свежей
     * загрузке страницы он оставался бы пустым до первого кадра, хотя число
     * уже получено по REST.
     */
    onCounters: null,
    /** Поколение загрузки: ответ прошлой директории не должен перерисовать новую. */
    generation: 0,
};

// ---------------------------------------------------------------------------
// Источники данных
// ---------------------------------------------------------------------------

/**
 * Список, из которого рисуется колонка: загруженные строки, пропущенные через
 * белый список (точка 1 из перечня в visibility.js).
 *
 * @returns {Array<object>}
 */
function visibleSource() {
    return visibleDialogs(b24State.dialogs, currentVisibilityFilter());
}

/** Строки активной директории. */
function currentRows() {
    return visibleSource().filter((d) => dirMatches(b24State.activeDir, d));
}

// ---------------------------------------------------------------------------
// Загрузка
// ---------------------------------------------------------------------------

/**
 * Загружает директории и счётчики (`GET /api/bitrix/filters`).
 *
 * Дёшево: после холодного старта сервер отвечает из снимка счётчиков, не
 * обращаясь к порталу (Р3).
 *
 * @returns {Promise<void>}
 */
export async function loadFilters() {
    try {
        const resp = await b24Api.getFilters();
        const c = (resp && resp.counters) || {};
        b24State.counters = {
            ...b24State.counters,
            total: Number(c.total) || 0,
            chat: Number(c.chat) || 0,
            dialog: Number(c.dialog) || 0,
            tasks: Number(c.tasks) || 0,
            notify: Number(c.notify) || 0,
        };
        renderDirs();
        // Бейдж вкладки — та же цифра, но пишет её ЕДИНСТВЕННЫЙ писатель
        // (`main.js::recomputeUnread`), поэтому здесь только сигнал.
        if (local.onCounters) local.onCounters();
    } catch (_) {
        // Директории статичны, а счётчики — украшение: без них колонка
        // работает, и показывать ошибку не за что.
    }
}

/**
 * Загружает страницу списка.
 *
 * @param {{append?: boolean}} [opts] — `append` догружает хвост, иначе список
 *   перезагружается с нуля
 * @returns {Promise<void>}
 */
export async function loadDialogs(opts = {}) {
    if (local.loading) return;
    const append = !!opts.append;
    if (append && !local.hasMore) return;

    local.loading = true;
    const generation = ++local.generation;
    try {
        const offset = append ? b24State.dialogs.length : 0;
        // ДИРЕКТОРИЯ НА СЕРВЕР НЕ УЕЗЖАЕТ, хотя роут её принимает.
        //
        // Список у портала ОДИН (Р2), и в состоянии он тоже один. Попроси мы
        // страницу уже отфильтрованной, в `b24State.dialogs` осталась бы
        // только текущая директория — и счётчики соседних мгновенно показали
        // бы ноль, потому что считаются по загруженным строкам. Серверный
        // `filter` остаётся в контракте для клиентов, которым нужна
        // постраничная выборка по одной директории.
        const resp = await b24Api.getDialogs({ limit: DIALOG_PAGE, offset });
        if (generation !== local.generation) return;

        const page = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
        b24State.dialogs = append ? mergeDialogs(b24State.dialogs, page) : mergeDialogs([], page);
        local.hasMore = !!(resp && resp.has_more);
        renderAll();
        renderDirs();
    } catch (_) {
        // Ошибку показывает общий индикатор связи (`setMode`) — колонка не
        // должна дублировать её собственным текстом на месте списка.
    } finally {
        if (generation === local.generation) local.loading = false;
    }
}

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

/** Перерисовывает счётчики директорий и подсветку активной. */
export function renderDirs() {
    // Счётчики считаются ПО ВИДИМЫМ строкам (точка 2 из visibility.js).
    const counts = dirCounts(visibleSource());
    for (const [key, node] of Object.entries(DIR_COUNT_NODES)) {
        if (!node) continue;
        const n = counts[key] || 0;
        node.textContent = n > 0 ? String(n) : '';
    }
    if ($b24DirList) {
        for (const li of Array.from($b24DirList.children || [])) {
            if (!li || !li.dataset) continue;
            li.classList.toggle('active', li.dataset.dir === b24State.activeDir);
        }
    }
}

/**
 * Полностью перерисовывает список.
 *
 * Полный рендер, а не точечные правки: строк в колонке десятки, а не тысячи, и
 * инкрементальное обновление стоило бы второго индекса узлов, который
 * разъезжается с данными при первой же ошибке.
 */
export function renderAll() {
    if (!$b24DialogList) return;
    const rows = currentRows();

    $b24DialogList.textContent = '';
    for (const d of rows) {
        $b24DialogList.appendChild(buildRow(d));
    }
    if ($b24NodesTitle) {
        $b24NodesTitle.textContent = nodesTitle(b24State.activeDir, rows.length);
    }
    highlightActive();
}

/**
 * Собирает строку списка. Только `createElement`/`textContent` — `innerHTML` в
 * панели запрещён полностью.
 *
 * @param {object} d — DialogDto
 * @returns {object} узел `li`
 */
function buildRow(d) {
    const li = document.createElement('li');
    li.className = 'b24-dialog';
    li.dataset.dialogId = String(d.id);
    li.dataset.kind = String(d.kind || 'chat');
    li.tabIndex = 0;
    if (d.muted) li.classList.add('b24-muted');
    if (d.pinned) li.classList.add('b24-pinned');

    li.appendChild(buildAvatar(d));

    const name = document.createElement('div');
    name.className = 'b24-dialog-name';

    const head = document.createElement('div');
    head.className = 'b24-dialog-head';
    const title = document.createElement('span');
    title.className = 'b24-dialog-title';
    title.textContent = displayName(d);
    head.appendChild(title);
    if (d.kind === 'task') {
        // СТАТУС-ТОЧКА ЗАДАЧИ. Её источник — НЕ строка списка (`im.recent.get`
        // ничего про статусы не знает), а карточка из `tasks.task.list`,
        // загруженная при входе в директорию `~/tasks`. Пока карточки нет,
        // точка нейтральна — это честнее, чем угадать статус.
        const task = taskOfDialog(b24State.tasks, d);
        const dot = document.createElement('span');
        dot.className = `b24-task-dot b24-task-dot-${(task && task.status_key) || 'unknown'}`;
        dot.title = (task && task.status_label) || 'Чат задачи';
        dot.setAttribute('aria-hidden', 'true');
        head.appendChild(dot);

        const badge = document.createElement('span');
        badge.className = 'b24-dialog-task';
        badge.textContent = 'TASK';
        badge.title = d.task_id ? `Задача #${d.task_id}` : 'Чат задачи';
        head.appendChild(badge);
        if (task && task.overdue) {
            const late = document.createElement('span');
            late.className = 'b24-dialog-overdue';
            late.textContent = '!';
            late.title = 'Задача просрочена';
            head.appendChild(late);
        }
        // Непрочитанные КОММЕНТАРИИ задачи — отдельное число портала
        // (`newCommentsCount`), не совпадающее с непрочитанными сообщениями
        // чата: портал считает их разными методами.
        const fresh = Number(task && task.new_comments_count) || 0;
        if (fresh > 0) {
            const badgeNew = document.createElement('span');
            badgeNew.className = 'b24-dialog-task-new';
            badgeNew.textContent = String(fresh);
            badgeNew.title = `Новых комментариев: ${fresh}`;
            head.appendChild(badgeNew);
        }
    }
    if (d.muted) {
        const mute = document.createElement('span');
        mute.className = 'b24-muted-mark';
        mute.textContent = '·';
        mute.title = 'Уведомления заглушены';
        head.appendChild(mute);
    }
    name.appendChild(head);

    const preview = previewText(d);
    if (preview) {
        const line = document.createElement('div');
        line.className = 'b24-dialog-preview';
        line.textContent = preview;
        name.appendChild(line);
    }
    li.appendChild(name);

    const mark = document.createElement('span');
    mark.className = (Number(d.unread_count) || 0) > 0 ? 'b24-dialog-unread' : 'b24-dialog-age';
    mark.textContent = dialogMark(d);
    li.appendChild(mark);
    li.appendChild(buildRowActions(d));
    return li;
}

/**
 * Кнопки строки: закрепить, пометить непрочитанным, скрыть (Р14).
 *
 * ЭТИМ ОБЪЁМ УПРАВЛЕНИЯ ЧАТАМИ ИСЧЕРПАН. Создание чатов, переименование,
 * приглашение участников и выход у портала есть, но в v1 их нет намеренно:
 * панель — это рабочее место разработчика, а не второй веб-мессенджер.
 *
 * Кнопки помечены `data-b24-act`, и делегированный слушатель проверяет их
 * ДО выбора диалога: клик по «скрыть» не должен заодно открывать ленту чата,
 * который сейчас исчезнет.
 *
 * @param {object} d — DialogDto
 * @returns {object} узел
 */
function buildRowActions(d) {
    const box = document.createElement('span');
    box.className = 'b24-dialog-actions';

    const pin = document.createElement('button');
    pin.className = 'b24-dialog-btn';
    pin.type = 'button';
    pin.dataset.b24Act = 'pin';
    pin.title = d.pinned ? 'Открепить' : 'Закрепить';
    pin.setAttribute('aria-label', pin.title);
    pin.textContent = d.pinned ? '⌾' : '⌖';
    box.appendChild(pin);

    // Ручное «непрочитано» есть только у Битрикс24 (ни у VK, ни у Telegram
    // такого метода нет). Требует идентификатора сообщения — без последнего
    // сообщения кнопки нет вовсе, иначе она отвечала бы 400.
    if (d.last_message && d.last_message.id) {
        const unread = document.createElement('button');
        unread.className = 'b24-dialog-btn';
        unread.type = 'button';
        unread.dataset.b24Act = 'unread';
        unread.title = 'Пометить непрочитанным';
        unread.setAttribute('aria-label', unread.title);
        unread.textContent = '•';
        box.appendChild(unread);
    }

    const hide = document.createElement('button');
    hide.className = 'b24-dialog-btn';
    hide.type = 'button';
    hide.dataset.b24Act = 'hide';
    hide.title = 'Скрыть из списка';
    hide.setAttribute('aria-label', hide.title);
    hide.textContent = '✕';
    box.appendChild(hide);

    return box;
}

/**
 * Выполняет действие над строкой и перезагружает список.
 *
 * Перезагрузка, а не локальная правка поля: закрепление меняет ПОРЯДОК списка,
 * скрытие — его состав, а ручное «непрочитано» — счётчики директорий. Собрать
 * всё это на клиенте значило бы завести вторую модель сортировки, которая
 * разъедется с серверной.
 *
 * @param {string} act — `pin` | `unread` | `hide`
 * @param {string} dialogId
 * @returns {Promise<void>}
 */
async function runRowAction(act, dialogId) {
    const d = findDialog(b24State.dialogs, dialogId);
    if (!d) return;
    try {
        if (act === 'pin') {
            await b24Api.pinDialog(dialogId, !d.pinned);
        } else if (act === 'unread') {
            const last = d.last_message && d.last_message.id;
            if (!last) return;
            await b24Api.markUnread(dialogId, last);
        } else if (act === 'hide') {
            const ask = (typeof globalThis.confirm === 'function') ? globalThis.confirm : null;
            // Обратной операции у портала нет: диалог вернётся сам, от первого
            // же нового сообщения, — и об этом сказано в вопросе.
            if (ask && !ask('Скрыть диалог из списка? Он вернётся от нового сообщения.')) return;
            await b24Api.hideDialog(dialogId);
        } else {
            return;
        }
    } catch (_) {
        // Отказ портала виден по тому, что список не изменился: заводить
        // тост ради трёх редких кнопок дороже, чем он стоит.
        return;
    }
    await loadDialogs();
}

/**
 * Аватар строки.
 *
 * Адрес берётся ТОЛЬКО из `avatar_url`, а его отдаёт бэкенд как ПУТЬ ПРОКСИ
 * `/api/bitrix/media/avatar/:user_id` (Р6). Внешних адресов (CDN портала) в
 * DOM не появляется никогда — инвариант держится единообразно с VK и Telegram.
 *
 * Прокси реализуется в Ф8, до неё путь честно отдаёт 404: `onerror` прячет
 * картинку, и остаётся кружок с инициалами на фирменном цвете портала.
 *
 * @param {object} d
 * @returns {object} узел
 */
function buildAvatar(d) {
    const box = document.createElement('span');
    box.className = 'b24-avatar';
    if (d.color) box.style.borderColor = String(d.color);

    const initials = document.createElement('span');
    initials.className = 'b24-avatar-initials';
    initials.textContent = kindGlyph(d);
    box.appendChild(initials);

    const url = typeof d.avatar_url === 'string' ? d.avatar_url : '';
    // Только собственный путь плагина: любой другой адрес — чужой хост в DOM.
    if (url.startsWith('/api/bitrix/media/avatar/')) {
        const img = document.createElement('img');
        img.className = 'b24-avatar-img';
        img.alt = '';
        // `hidden` до успеха: пока байты не приехали, виден кружок с
        // инициалами, а не пустая рамка.
        img.hidden = true;
        box.appendChild(img);
        // НЕ `img.src = url`: путь `/api/*` в remote-mode требует
        // `Authorization: Bearer`, а атрибут `src` заголовков не несёт. Байты
        // приходят через подменённый `window.fetch` и становятся `blob:`-адресом
        // (см. js/bitrix/media.js). Идентификатор берётся из САМОГО пути — он и
        // есть контракт прокси.
        attachAvatar(img, url.slice('/api/bitrix/media/avatar/'.length));
    }
    return box;
}

/** Подсвечивает активную строку. */
export function highlightActive() {
    if (!$b24DialogList) return;
    const active = String(b24State.activeDialogId == null ? '' : b24State.activeDialogId);
    for (const li of Array.from($b24DialogList.children || [])) {
        if (!li || !li.dataset) continue;
        li.classList.toggle('active', li.dataset.dialogId === active);
    }
}

// ---------------------------------------------------------------------------
// Действия
// ---------------------------------------------------------------------------

/**
 * Переключает директорию.
 *
 * Локальная операция плюс ОДИН запрос страницы: серверу директория нужна для
 * честного `has_more`, но пользователь видит новый список мгновенно, ещё до
 * ответа.
 *
 * @param {string} dir
 */
export function selectDir(dir) {
    const next = ['all', 'chats', 'tasks', 'unread'].includes(dir) ? dir : 'all';
    if (b24State.activeDir === next) return;
    b24State.activeDir = next;
    // Сетевого запроса ЗА СПИСКОМ здесь нет вовсе: он один и уже загружен (Р2).
    renderAll();
    renderDirs();
    // Единственное, что директория действительно догружает, — КАРТОЧКИ ЗАДАЧ
    // (Ф10): статусы и сроки живут в `tasks.task.list`, а не в списке
    // диалогов, и вне `~/tasks` показывать их негде.
    if (typeof local.onDir === 'function') local.onDir(next);
    // Единственное исключение — директория выглядит пустой, а список
    // догружен не весь: тогда пустота на экране означала бы «здесь ничего
    // нет», хотя строки просто не доехали.
    if (local.hasMore && currentRows().length < DIR_MIN_ROWS) {
        loadDialogs({ append: true });
    }
}

/**
 * Выбирает диалог и сообщает об этом наружу.
 *
 * @param {string} id
 */
export function selectDialog(id) {
    const key = String(id == null ? '' : id);
    if (!key) return;
    b24State.activeDialogId = key;
    highlightActive();
    if (typeof local.onSelect === 'function') {
        local.onSelect(key, findDialog(b24State.dialogs, key));
    }
}

/** Ставит счётчик непрочитанного строке (WS-кадры Ф7). */
export function setUnread(id, count) {
    const d = findDialog(b24State.dialogs, id);
    if (!d) return;
    d.unread_count = Math.max(0, Number(count) || 0);
    renderAll();
    renderDirs();
}

/**
 * Применяет кадр `dialogs_patch` (Ф7).
 *
 * Патч, а не полный список: у портала бывают тысячи чатов, и слать их целиком
 * на каждое изменение означало бы мегабайты по сокету. Слияние идёт тем же
 * `mergeDialogs`, что и догрузка страницы, — второй порядок сортировки
 * разъехался бы с первым при первой же правке.
 *
 * `removed` сервер сегодня не заполняет никогда (портал об исчезновении строк
 * не сообщает — он просто перестаёт их отдавать), но обрабатывается здесь:
 * иначе появление этого поля в будущем потребовало бы правки фронта.
 *
 * @param {Array<object>} dialogs — изменившиеся строки
 * @param {Array<string>} [removed] — исчезнувшие идентификаторы
 * @returns {number} сколько строк в колонке после применения
 */
export function applyDialogsPatch(dialogs, removed) {
    const page = Array.isArray(dialogs) ? dialogs : [];
    if (page.length > 0) {
        b24State.dialogs = mergeDialogs(b24State.dialogs, page);
    }
    const gone = Array.isArray(removed) ? removed.map(String) : [];
    if (gone.length > 0) {
        b24State.dialogs = b24State.dialogs.filter((d) => !gone.includes(String(d.id)));
    }
    if (page.length > 0 || gone.length > 0) {
        renderAll();
        renderDirs();
    }
    return b24State.dialogs.length;
}

/**
 * Поднимает строку списка по пришедшему сообщению (кадр `message_new`).
 *
 * Делается ЛОКАЛЬНО, без запроса к порталу: строка уже загружена, а всё нужное
 * (текст, автор, дата) лежит в самом кадре. Запрос списка на каждое сообщение
 * стоил бы половины бюджета в 2 rps ровно там, где его и так не хватает.
 *
 * Счётчик растёт только у ЧУЖОГО сообщения и только когда диалог не открыт: своё
 * непрочитанным не бывает, а открытую ленту пользователь читает глазами (отметку
 * шлёт сама лента).
 *
 * @param {object} msg — MessageDto из кадра
 * @param {boolean} counts — учитывать ли сообщение в непрочитанном
 * @returns {boolean} строка нашлась (иначе список нужно перезапросить)
 */
export function bumpDialog(msg, counts) {
    if (!msg) return false;
    const d = findDialog(b24State.dialogs, msg.dialog_id);
    if (!d) return false;

    d.last_message = {
        id: Number(msg.id) || 0,
        text: String(msg.text || ''),
        author_id: String((msg.sender && msg.sender.id) || '0'),
        author_name: (msg.sender && msg.sender.name) || null,
        date: msg.date || null,
        out: !!msg.out,
        system: !!msg.system,
        has_file: Array.isArray(msg.media) && msg.media.length > 0,
    };
    d.date_update = msg.date || d.date_update;
    if (counts) d.unread_count = Math.max(0, Number(d.unread_count) || 0) + 1;

    b24State.dialogs = sortDialogs(b24State.dialogs);
    renderAll();
    renderDirs();
    return true;
}

/** Полный сброс колонки — при отвязке портала. */
export function resetDialogs() {
    local.hasMore = false;
    local.loading = false;
    local.generation += 1;
    b24State.activeDir = 'all';
    if ($b24DialogList) $b24DialogList.textContent = '';
    if ($b24NodesTitle) $b24NodesTitle.textContent = nodesTitle('all', 0);
    renderDirs();
}

// ---------------------------------------------------------------------------
// Инициализация
// ---------------------------------------------------------------------------

/**
 * Навешивает слушатели колонки. Идемпотентна: вкладку открывают много раз, а
 * второй набор слушателей означал бы двойные запросы на каждый клик.
 *
 * @param {{onSelect?: function, onDir?: function}} [handlers]
 */
export function initDialogs(handlers = {}) {
    local.onSelect = typeof handlers.onSelect === 'function' ? handlers.onSelect : null;
    local.onDir = typeof handlers.onDir === 'function' ? handlers.onDir : null;
    local.onCounters = typeof handlers.onCounters === 'function' ? handlers.onCounters : null;

    DIR_COUNT_NODES.all = $b24DirAllCount;
    DIR_COUNT_NODES.chats = $b24DirChatsCount;
    DIR_COUNT_NODES.tasks = $b24DirTasksCount;
    DIR_COUNT_NODES.unread = $b24DirUnreadCount;

    if (local.inited) return;
    local.inited = true;

    // Делегирование: строки списка перерисовываются целиком, и слушатель на
    // каждой из них пришлось бы вешать заново после каждого рендера.
    if ($b24DirList) {
        $b24DirList.addEventListener('click', (e) => {
            const li = closestWithData(e.target, 'dir');
            if (li) selectDir(li.dataset.dir);
        });
    }
    if ($b24DialogList) {
        $b24DialogList.addEventListener('click', (e) => {
            const li = closestWithData(e.target, 'dialogId');
            if (!li) return;
            // Кнопка строки проверяется РАНЬШЕ выбора: клик по «скрыть» не
            // должен заодно открывать ленту чата, который сейчас исчезнет.
            const btn = closestWithData(e.target, 'b24Act');
            if (btn) {
                runRowAction(btn.dataset.b24Act, li.dataset.dialogId);
                return;
            }
            selectDialog(li.dataset.dialogId);
        });
        $b24DialogList.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter' && e.key !== ' ') return;
            const li = closestWithData(e.target, 'dialogId');
            if (!li) return;
            e.preventDefault();
            selectDialog(li.dataset.dialogId);
        });
    }
    if ($b24Contacts) {
        $b24Contacts.addEventListener('scroll', () => {
            const el = $b24Contacts;
            const left = el.scrollHeight - el.scrollTop - el.clientHeight;
            if (left <= SCROLL_TAIL_PX) loadDialogs({ append: true });
        });
    }
}

/** Ближайший предок с нужным `data-`полем (замена `closest` без селекторов). */
function closestWithData(node, field) {
    let cur = node;
    for (let i = 0; cur && i < 8; i += 1) {
        if (cur.dataset && cur.dataset[field] != null) return cur;
        cur = cur.parentNode;
    }
    return null;
}

/** Снимок для e2e и диагностики. */
export function _debugDialogsState() {
    return {
        inited: local.inited,
        loading: local.loading,
        hasMore: local.hasMore,
        dir: b24State.activeDir,
        loaded: b24State.dialogs.length,
        visible: currentRows().length,
        activeDialogId: b24State.activeDialogId,
    };
}
