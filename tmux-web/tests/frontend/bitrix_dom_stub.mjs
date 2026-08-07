// ВСПОМОГАТЕЛЬНЫЙ МОДУЛЬ (не тест): фейковый DOM для тестов панели «Битрикс24».
//
// Запускать напрямую незачем — он импортируется из bitrix_login.test.mjs ДО
// первого импорта модулей js/bitrix/*.
//
// Почему стаб обязан стоять раньше: `core/dom.js` резолвит все ref'ы ОДИН раз
// на верхнем уровне модуля. Импортируй он позже — каждый `$b24*` оказался бы
// null, и тест проверял бы пустоту, зеленея по случайности.
//
// Стаб намеренно уже, чем `vk_dom_stub.mjs`: панели Битрикс24 в Фазе 3
// нужен только экран подключения, а копировать чужие 400 строк ради этого
// значило бы завести второй набор фейковых узлов, который разъедется с
// первым. Здесь ровно тот срез DOM, которым пользуются login.js и main.js:
// поиск по id, hidden/value/disabled, textContent и addEventListener.
//
// Идентификаторы взяты из блока `$b24*` в `static/js/core/dom.js` — их
// соответствие разметке проверяет отдельный страж bitrix_contract.test.mjs.

/** Все созданные узлы по id. */
export const els = new Map();

function makeText(data) {
    return { nodeType: 3, tagName: null, parentNode: null, textContent: String(data) };
}

function detach(node) {
    const p = node.parentNode;
    if (!p) return;
    const i = p.childNodes.indexOf(node);
    if (i >= 0) p.childNodes.splice(i, 1);
    node.parentNode = null;
}

export function makeEl(tag) {
    const node = {
        nodeType: 1,
        tagName: String(tag || 'div').toLowerCase(),
        childNodes: [],
        parentNode: null,
        dataset: {},
        style: {},
        attributes: {},
        listeners: new Map(),
        className: '',
        hidden: false,
        disabled: false,
        value: '',
    };

    node.classList = {
        add(...cs) {
            const set = new Set(node.className.split(/\s+/).filter(Boolean));
            cs.forEach((c) => set.add(c));
            node.className = Array.from(set).join(' ');
        },
        remove(...cs) {
            const set = new Set(node.className.split(/\s+/).filter(Boolean));
            cs.forEach((c) => set.delete(c));
            node.className = Array.from(set).join(' ');
        },
        contains(c) {
            return node.className.split(/\s+/).filter(Boolean).includes(c);
        },
        toggle(c, on) {
            const has = node.classList.contains(c);
            const want = on === undefined ? !has : !!on;
            if (want) node.classList.add(c);
            else node.classList.remove(c);
            return want;
        },
    };

    Object.defineProperty(node, 'textContent', {
        get: () => node.childNodes.map((c) => c.textContent).join(''),
        set(v) {
            node.childNodes.slice().forEach(detach);
            const t = String(v == null ? '' : v);
            if (t !== '') node.appendChild(makeText(t));
        },
    });

    node.appendChild = (child) => {
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    /**
     * Вставка ПЕРЕД узлом — ею лента догружает историю сверху.
     *
     * `ref === null` означает «в конец», ровно как в браузере: `chat.js`
     * рассчитывает на это, когда лента пуста.
     */
    node.insertBefore = (child, ref) => {
        detach(child);
        child.parentNode = node;
        const i = ref ? node.childNodes.indexOf(ref) : -1;
        if (i >= 0) node.childNodes.splice(i, 0, child);
        else node.childNodes.push(child);
        return child;
    };
    node.removeChild = (child) => {
        detach(child);
        return child;
    };
    /** Только элементы (браузерный `children` не включает текстовые узлы). */
    Object.defineProperty(node, 'children', {
        get: () => node.childNodes.filter((c) => c.nodeType === 1),
    });
    Object.defineProperty(node, 'firstChild', {
        get: () => node.childNodes[0] || null,
    });
    // Геометрия прокрутки: у фейкового DOM её нет вовсе, поэтому тесты ленты
    // задают её руками (`$log.scrollHeight = 900`). Значения по умолчанию —
    // нули, и при них `isAtBottom` честно отвечает «внизу».
    node.scrollTop = 0;
    node.scrollHeight = 0;
    node.clientHeight = 0;
    node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
    node.getAttribute = (k) => (k in node.attributes ? node.attributes[k] : null);
    node.removeAttribute = (k) => { delete node.attributes[k]; };
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.removeEventListener = (type, fn) => {
        const arr = node.listeners.get(type) || [];
        const i = arr.indexOf(fn);
        if (i >= 0) arr.splice(i, 1);
    };
    /** Сколько слушателей навешено — так ловится удвоение при повторном init. */
    node.listenerCount = (type) => (node.listeners.get(type) || []).length;
    node.focusCount = 0;
    node.clickCount = 0;
    node.tabIndex = -1;
    node.focus = () => { node.focusCount += 1; active = node; };
    node.blur = () => { if (active === node) active = null; };
    node.scrollIntoView = () => {};
    /** Самоудаление — им закрывает себя тост (js/echo/notifications.js). */
    node.remove = () => detach(node);
    node.click = () => { node.clickCount += 1; dispatch(node, 'click', { target: node }); };

    // ------------------------------------------------------------------
    // Срез селекторов (Ф11: ими пользуются keynav.js, media.js и tasks.js).
    //
    // Поддержаны ровно три формы, встречающиеся в коде панели: `.class`,
    // `#id` и имя тега. Полноценный парсер CSS здесь был бы отдельной
    // библиотекой с собственными багами — по той же причине `keynav.js`
    // фильтрует системные строки по `classList`, а не селектором `:not(…)`.
    // ------------------------------------------------------------------
    node.matches = (sel) => {
        const s = String(sel || '');
        if (!s) return false;
        if (s.charAt(0) === '.') return node.classList.contains(s.slice(1));
        if (s.charAt(0) === '#') return node.attributes.id === s.slice(1);
        return node.tagName === s.toLowerCase();
    };
    node.closest = (sel) => {
        let cur = node;
        while (cur) {
            if (cur.nodeType === 1 && cur.matches && cur.matches(sel)) return cur;
            cur = cur.parentNode;
        }
        return null;
    };
    node.querySelector = (sel) => {
        for (const c of node.childNodes) {
            if (c.nodeType !== 1) continue;
            if (c.matches(sel)) return c;
            const found = c.querySelector(sel);
            if (found) return found;
        }
        return null;
    };
    node.querySelectorAll = (sel) => {
        const out = [];
        for (const c of node.childNodes) {
            if (c.nodeType !== 1) continue;
            if (c.matches(sel)) out.push(c);
            out.push(...c.querySelectorAll(sel));
        }
        return out;
    };
    node.contains = (other) => {
        let cur = other;
        while (cur) { if (cur === node) return true; cur = cur.parentNode; }
        return false;
    };
    return node;
}

/**
 * Узел, который сейчас «в фокусе».
 *
 * Фейковый DOM фокуса не имеет вовсе, а `keynav.js` принимает по нему ВСЕ
 * решения — и о зоне курсора, и о владении клавиатурой. Поэтому `focus()`
 * пишет сюда, а `document.activeElement` читает отсюда.
 */
let active = null;

/** Ставит фокус явно (в том числе на `document.body` и в никуда). */
export function setActive(node) {
    active = node || null;
}

/** Текущий «сфокусированный» узел. */
export function activeEl() {
    return active;
}

/**
 * Доставка события ОДНОМУ узлу-слушателю.
 *
 * Всплытие не эмулируется: login.js вешает слушателей на конкретные узлы.
 *
 * @param {object} node
 * @param {string} type
 * @param {object} [ev]
 * @returns {object} само событие
 */
export function dispatch(node, type, ev) {
    const e = Object.assign({
        type,
        target: node,
        defaultPrevented: false,
        preventDefault() { this.defaultPrevented = true; },
        stopPropagation() {},
    }, ev || {});
    (node.listeners.get(type) || []).slice().forEach((fn) => fn(e));
    return e;
}

/** Создаёт узел с id и кладёт его в реестр. */
export function el(id, tag) {
    const node = makeEl(tag || 'div');
    node.attributes.id = id;
    els.set(id, node);
    return node;
}

// ---------------------------------------------------------------------------
// Разметка: те id секции Битрикс24, которых касается Фаза 3.
// ---------------------------------------------------------------------------

const $body = makeEl('body');
export const $panel = el('bitrix');
export const $toggle = el('bitrix-toggle', 'button');
export const $unread = el('b24-unread-badge', 'span');
el('b24-titlebar');
export const $selfUser = el('b24-self-user', 'span');
export const $logoutBtn = el('b24-logout', 'button');
export const $mode = el('b24-mode', 'span');

// Лента (Ф5): ровно те узлы, которых касаются chat.js, attachments.js и
// botkb.js. Прокрутка у фейкового DOM своя — тест задаёт `scrollHeight` и
// `clientHeight` руками.
export const $log = el('b24-log', 'ul');
export const $sessionHead = el('b24-session-head');
export const $input = el('b24-input', 'input');
export const $typing = el('b24-typing');
// Цикл переписки (Ф6): строка режима ввода (цитата/правка) и счётчик символов.
export const $promptNote = el('b24-prompt-note');
export const $charCount = el('b24-charcount', 'span');
export const $prompt = el('b24-prompt', 'form');
export const $send = el('b24-send', 'button');
// Отправка файлов (Ф9): три входа (кнопка ⇪, скрытый input, оверлей дропзоны)
// и контейнер очереди над строкой ввода.
export const $attach = el('b24-attach', 'button');
export const $fileInput = el('b24-file-input', 'input');
export const $dropzone = el('b24-dropzone');
export const $uploads = el('b24-uploads');
export const $taskCard = el('b24-task-card');
export const $contacts = el('b24-contacts', 'aside');
export const $dialogList = el('b24-dialog-list', 'ul');
export const $dirList = el('b24-dir-list', 'ul');
export const $nodesTitle = el('b24-nodes-title');
// Четыре ЛОКАЛЬНЫЕ директории — ровно те же, что в index.html. Ячейки
// счётчиков нужны целиком: `dialogs.js` резолвит их через `core/dom.js` один
// раз при импорте, и без них точка «счётчики директорий» из белого списка
// проверялась бы записью в никуда.
export const $dirCounts = {};
for (const dir of ['all', 'chats', 'tasks', 'unread']) {
    const li = makeEl('li');
    li.className = dir === 'all' ? 'b24-dir active' : 'b24-dir';
    li.dataset.dir = dir;
    const count = el(`b24-dir-${dir}-count`, 'span');
    count.className = 'b24-dir-count';
    li.appendChild(count);
    $dirList.appendChild(li);
    $dirCounts[dir] = count;
}
// Ф11. Тумблер темы и кнопка ☰ — цели действий `ui.theme` и мобильной
// раскладки; подложка и лайтбокс — узлы, по состоянию которых keynav решает,
// молчать ему или работать.
export const $themeToggle = el('b24-theme-toggle', 'button');
export const $contactsToggle = el('b24-contacts-toggle', 'button');
export const $contactsScrim = el('b24-contacts-scrim');
export const $lightbox = el('b24-lightbox');

// Общий стек тостов (js/echo/notifications.js). Он НЕ принадлежит панели:
// уведомления Echo, Telegram, VK и Битрикс24 падают в один контейнер, и
// четвёртого стека в проекте не заводится. Без этого узла `notify` уходит в
// console и тест не может отличить «тост показан» от «тост подавлен».
export const $toasts = el('echo-toasts');

export const $login = el('b24-login');
export const $loginStep = el('b24-login-step-webhook');
export const $hint = el('b24-login-hint');
export const $webhook = el('b24-webhook', 'input');
export const $submit = el('b24-login-webhook-submit', 'button');
export const $status = el('b24-login-status');
export const $error = el('b24-login-error');
export const $countdown = el('b24-login-countdown');
export const $logoutNote = el('b24-logout-note');

$body.appendChild($panel);
// Дерево панели: `keynav.js` проверяет `#bitrix.contains(document.activeElement)`
// и ищет строки внутри `#b24-log` / `#b24-dialog-list`, поэтому узлы обязаны
// быть СВЯЗАНЫ, а не просто лежать в реестре по id.
$panel.appendChild($contacts);
$contacts.appendChild($dirList);
$contacts.appendChild($dialogList);
$panel.appendChild($taskCard);
$panel.appendChild($log);
$panel.appendChild($prompt);
$prompt.appendChild($input);
$panel.appendChild($lightbox);
$panel.appendChild($contactsScrim);
$panel.appendChild($themeToggle);
$panel.appendChild($contactsToggle);
$login.hidden = true;
$error.hidden = true;
$countdown.hidden = true;
$logoutBtn.hidden = true;
$taskCard.hidden = true;
$lightbox.hidden = true;
$contactsScrim.hidden = true;

export const $document = makeEl('#document');
$document.getElementById = (id) => els.get(id) || null;
$document.createElement = (tag) => makeEl(tag);
$document.createTextNode = (t) => makeText(t);
$document.body = $body;
$document.documentElement = makeEl('html');
$document.visibilityState = 'visible';
// Фокуса у фейкового DOM нет — отдаём тот узел, которому его поставил
// `focus()` (см. [`setActive`]).
Object.defineProperty($document, 'activeElement', { get: () => active });
$document.querySelector = (sel) => $body.querySelector(sel);
$document.querySelectorAll = (sel) => $body.querySelectorAll(sel);
globalThis.document = $document;

globalThis.location = {
    protocol: 'http:', host: 'localhost', hash: '', href: 'http://localhost/',
    origin: 'http://localhost', pathname: '/', search: '',
};
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
globalThis.history = { replaceState() {} };
// Тост ставит класс появления через rAF; в Node его нет вовсе.
globalThis.requestAnimationFrame = (fn) => setTimeout(fn, 0);
globalThis.window = {
    fetch: (...args) => globalThis.fetch(...args),
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    setTimeout, clearTimeout,
};

// ---------------------------------------------------------------------------
// Сеть
// ---------------------------------------------------------------------------

/** Ответ в том виде, в каком его читает messenger/http-envelope.js::call. */
export function jsonRes(status, obj) {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: { get: (h) => (String(h).toLowerCase() === 'content-type' ? 'application/json' : null) },
        json: async () => obj,
        text: async () => JSON.stringify(obj),
    };
}

/** Журнал запросов: [{url, method, body}]. */
export const netCalls = [];

let netHandler = () => Promise.resolve(jsonRes(200, {}));

/** @param {(url: string, init: object) => Promise<object>} fn */
export function setNet(fn) {
    netHandler = fn;
}

globalThis.fetch = (url, init) => {
    let body = null;
    try { body = init && init.body ? JSON.parse(init.body) : null; } catch (_) { body = init && init.body; }
    netCalls.push({ url: String(url), method: (init && init.method) || 'GET', body });
    return netHandler(String(url), init || {});
};

/** Даёт микрозадачам доехать (несколько тиков — цепочки await в submit). */
export async function flush(times) {
    for (let i = 0; i < (times || 6); i += 1) {
        // eslint-disable-next-line no-await-in-loop
        await Promise.resolve();
    }
}
