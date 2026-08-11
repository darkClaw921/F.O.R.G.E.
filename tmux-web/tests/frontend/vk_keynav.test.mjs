// Регресс-тест клавиатурной навигации по панели VK (js/vk/keynav.js).
//
// Чистая логика комбинаций покрыта соседними vk_keys.test.mjs (реестр) и
// messenger_shared_parity.test.mjs (движок). Здесь проверяется вторая половина
// фичи, которую чистыми функциями не поймать: курсор по беседам и сообщениям,
// выполнение действий, шпаргалка, ГЕЙТЫ (поле ввода, лайтбокс, CAPTCHA,
// модалка, выключенный тумблер) и снятие слушателя при teardown.
//
// Отдельная и обязательная секция — СОСУЩЕСТВОВАНИЕ ДВУХ ПАНЕЛЕЙ: в одном
// процессе поднимаются обе (Telegram и VK), и проверяется, что владение
// клавиатурой взаимоисключающе, а при обеих скрытых панелях его нет ни у кого
// (то есть глобальный vim из static/hotkeys.js работает).
//
// Почему свой процесс и свой фейковый DOM (а не общий vk_dom_stub.mjs):
// `core/dom.js` резолвит ref'ы ОДИН раз на верхнем уровне модуля, поэтому стаб
// обязан стоять раньше первого импорта; кроме того, keynav ходит по дереву
// СОСТАВНЫМИ селекторами (`#vk-dialog-list .vk-dialog`), спрашивает
// `document.activeElement` и вешает слушатель в capture-фазе — общий стаб
// такого не умеет, а расширять его под одного потребителя значило бы рисковать
// пятнадцатью соседними тестами.
//
// Запуск: node tmux-web/tests/frontend/vk_keynav.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM
// ---------------------------------------------------------------------------

/**
 * Разбор ОДНОГО простого селектора: `tag`, `#id`, `.class`, `.a.b`, `tag[attr]`.
 */
function matchesSimple(node, sel) {
    if (!node || node.nodeType !== 1) return false;
    const m = String(sel).trim().match(/^([a-zA-Z]*)(#[-\w]+)?((?:\.[-\w]+)*)(?:\[([-\w]+)\])?$/);
    if (!m) return false;
    const [, tag, id, classes, attr] = m;
    if (tag && node.tagName !== tag.toLowerCase()) return false;
    if (id && node.attributes.id !== id.slice(1)) return false;
    for (const c of (classes || '').split('.').filter(Boolean)) {
        if (!node.classList.contains(c)) return false;
    }
    if (attr && !(attr in node.attributes)) return false;
    return true;
}

/**
 * Полный селектор: список через запятую, каждый — цепочка потомков через пробел.
 *
 * Комбинаторы `>`/`+` не поддержаны намеренно: keynav.js ими не пользуется, а
 * лишний код в стенде — это лишний источник собственных багов.
 */
function matchesSel(node, sel) {
    for (const alt of String(sel).split(',')) {
        const parts = alt.trim().split(/\s+/).filter(Boolean);
        if (!parts.length) continue;
        if (!matchesSimple(node, parts[parts.length - 1])) continue;
        // Предки проверяем справа налево: каждому предыдущему звену должен
        // найтись предок выше по дереву.
        let cur = node.parentNode;
        let i = parts.length - 2;
        while (i >= 0 && cur) {
            if (matchesSimple(cur, parts[i])) i -= 1;
            cur = cur.parentNode;
        }
        if (i < 0) return true;
    }
    return false;
}

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

let activeElement = null;

function makeEl(tag) {
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
        title: '',
        type: '',
        value: '',
        disabled: false,
        tabIndex: -1,
        scrollTop: 0,
        scrollHeight: 1000,
        clientHeight: 400,
        focusCount: 0,
        clickCount: 0,
        scrollIntoViewCount: 0,
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
    Object.defineProperty(node, 'children', {
        get: () => node.childNodes.filter((c) => c.nodeType === 1),
    });
    Object.defineProperty(node, 'firstChild', {
        get: () => node.childNodes[0] || null,
    });
    Object.defineProperty(node, 'previousElementSibling', {
        get() {
            if (!node.parentNode) return null;
            const sibs = node.parentNode.children;
            const i = sibs.indexOf(node);
            return i > 0 ? sibs[i - 1] : null;
        },
    });
    Object.defineProperty(node, 'src', {
        get: () => ('src' in node.attributes ? node.attributes.src : ''),
        set(v) { node.attributes.src = String(v); },
    });

    node.appendChild = (child) => {
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.insertBefore = (child, ref) => {
        detach(child);
        child.parentNode = node;
        const i = ref ? node.childNodes.indexOf(ref) : -1;
        if (i < 0) node.childNodes.push(child);
        else node.childNodes.splice(i, 0, child);
        return child;
    };
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
    node.after = (next) => {
        const p = node.parentNode;
        if (!p) return;
        detach(next);
        next.parentNode = p;
        const i = p.childNodes.indexOf(node);
        if (i >= 0) p.childNodes.splice(i + 1, 0, next);
        else p.childNodes.push(next);
    };
    node.replaceWith = (next) => {
        const p = node.parentNode;
        if (!p) return;
        const i = p.childNodes.indexOf(node);
        detach(next);
        next.parentNode = p;
        p.childNodes.splice(i, 1, next);
        node.parentNode = null;
    };
    node.contains = (other) => {
        let cur = other;
        while (cur) {
            if (cur === node) return true;
            cur = cur.parentNode;
        }
        return false;
    };
    node.closest = (sel) => {
        let cur = node;
        while (cur) {
            if (matchesSel(cur, sel)) return cur;
            cur = cur.parentNode;
        }
        return null;
    };
    node.matches = (sel) => matchesSel(node, sel);
    node.querySelectorAll = (sel) => {
        const out = [];
        const walk = (n) => n.childNodes.forEach((c) => {
            if (c.nodeType !== 1) return;
            if (matchesSel(c, sel)) out.push(c);
            walk(c);
        });
        walk(node);
        return out;
    };
    node.querySelector = (sel) => node.querySelectorAll(sel)[0] || null;
    node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
    node.getAttribute = (k) => (k in node.attributes ? node.attributes[k] : null);
    node.removeAttribute = (k) => { delete node.attributes[k]; };
    node.hasAttribute = (k) => k in node.attributes;
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.removeEventListener = (type, fn) => {
        const arr = node.listeners.get(type) || [];
        const i = arr.indexOf(fn);
        if (i >= 0) arr.splice(i, 1);
    };
    // Фокус настоящий: keynav спрашивает document.activeElement, и «счётчик
    // вызовов» вместо позиции фокуса сделал бы зону всегда одной и той же.
    node.focus = () => { node.focusCount += 1; activeElement = node; };
    node.blur = () => { if (activeElement === node) activeElement = null; };
    node.click = () => { node.clickCount += 1; dispatch(node, 'click', { target: node }); };
    node.scrollIntoView = () => { node.scrollIntoViewCount += 1; };
    node.getBoundingClientRect = () => ({ top: 0, left: 0, width: 100, height: 20, bottom: 20, right: 100 });

    return node;
}

function dispatch(node, type, ev) {
    const e = Object.assign({
        type,
        target: node,
        defaultPrevented: false,
        propagationStopped: false,
        preventDefault() { this.defaultPrevented = true; },
        stopPropagation() { this.propagationStopped = true; },
    }, ev || {});
    (node.listeners.get(type) || []).slice().forEach((fn) => fn(e));
    return e;
}

const els = new Map();
function el(id, tag) {
    const node = makeEl(tag || 'div');
    node.attributes.id = id;
    els.set(id, node);
    return node;
}

// ── дерево панели VK (упрощённая копия index.html) ──
const $body = makeEl('body');
const $panel = el('vk');
const $contacts = el('vk-contacts', 'aside');
const $dirList = el('vk-dir-list', 'ul');
const $dialogList = el('vk-dialog-list', 'ul');
const $communityList = el('vk-community-list', 'ul');
const $log = el('vk-log');
const $composer = makeEl('div');
const $prompt = el('vk-prompt', 'form');
const $input = el('vk-input', 'input');
const $send = el('vk-send', 'button');
const $attach = el('vk-attach', 'button');
const $theme = el('vk-theme-toggle', 'button');
const $lightbox = el('vk-lightbox');
const $login = el('vk-login');
// CAPTCHA — отличие VK: отдельный оверлей ВНЕ #vk-login, всплывает на любой
// метод API, а не только на вход.
const $captcha = el('vk-captcha');
// Попап анализа чата: тот же класс гейта, что логин и CAPTCHA. Внутри него два
// поля ввода (вопрос модели и заметка о беседе), и vim-навигация обязана
// уступать им клавиатуру целиком.
const $analysis = el('vk-analysis');
el('vk-session-head');
el('vk-file-input', 'input');
el('vk-dropzone');
el('vk-lightbox-img', 'img');
el('vk-lightbox-prev', 'button');
el('vk-lightbox-next', 'button');
el('vk-lightbox-close', 'button');
el('vk-lightbox-caption');
el('vk-typing');
el('vk-toggle', 'button');
el('vk-unread-badge', 'span');
el('vk-self-user', 'span');
el('vk-logout', 'button');
el('vk-mode', 'span');
el('vk-lat', 'span');
el('vk-status-right', 'span');
el('vk-nodes-title');
el('vk-contacts-toggle', 'button');
el('vk-contacts-scrim');
el('vk-login-step-token');
el('vk-login-hint', 'p');
el('vk-token', 'input');
el('vk-login-token-submit', 'button');
el('vk-login-status');
el('vk-login-error');
el('vk-login-countdown');
el('vk-captcha-img', 'img');
el('vk-captcha-key', 'input');
el('vk-captcha-submit', 'button');
el('vk-captcha-cancel', 'button');
el('vk-captcha-error');

$lightbox.hidden = true;
$login.hidden = true;
$captcha.hidden = true;
$analysis.hidden = true;
$panel.appendChild($contacts);
$contacts.appendChild($dirList);
$contacts.appendChild($dialogList);
$contacts.appendChild($communityList);
$panel.appendChild($log);
$panel.appendChild($composer);
$panel.appendChild($login);
$panel.appendChild($captcha);
$panel.appendChild($lightbox);
$composer.appendChild($prompt);
$prompt.appendChild($input);
$prompt.appendChild($send);
$prompt.appendChild($attach);
$panel.appendChild($theme);
$body.appendChild($panel);

const $spacer = makeEl('div');
$spacer.className = 'vk-log-spacer';
$log.appendChild($spacer);

// ── дерево панели Telegram: нужно ТОЛЬКО для проверки сосуществования ──
const $tgPanel = el('telegram');
const $tgContacts = el('tg-contacts', 'aside');
const $tgDialogList = el('tg-dialog-list', 'ul');
const $tgLog = el('tg-log');
const $tgLogin = el('tg-login');
const $tgLightbox = el('tg-lightbox');
el('tg-dir-list', 'ul');
el('tg-channel-list', 'ul');
el('tg-prompt', 'form');
el('tg-input', 'input');
el('tg-send', 'button');
el('tg-attach', 'button');
el('tg-theme-toggle', 'button');
el('tg-session-head');
el('tg-file-input', 'input');
el('tg-dropzone');
el('tg-lightbox-img', 'img');
el('tg-lightbox-close', 'button');
el('tg-lightbox-caption');
el('tg-typing');
$tgLogin.hidden = true;
$tgLightbox.hidden = true;
$tgPanel.hidden = true;
$tgPanel.appendChild($tgContacts);
$tgContacts.appendChild($tgDialogList);
$tgPanel.appendChild($tgLog);
$tgPanel.appendChild($tgLogin);
$tgPanel.appendChild($tgLightbox);
$body.appendChild($tgPanel);

const $document = makeEl('#document');
$document.getElementById = (id) => els.get(id) || null;
$document.createElement = (tag) => makeEl(tag);
$document.createTextNode = (t) => makeText(t);
$document.body = $body;
$document.documentElement = makeEl('html');
$document.visibilityState = 'visible';
$document.querySelector = (sel) => $body.querySelector(sel);
$document.querySelectorAll = (sel) => $body.querySelectorAll(sel);
Object.defineProperty($document, 'activeElement', { get: () => activeElement });
globalThis.document = $document;

globalThis.location = {
    protocol: 'http:', host: 'localhost', hash: '', href: 'http://localhost/',
    origin: 'http://localhost', pathname: '/', search: '',
};
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
globalThis.history = { replaceState() {} };

/** Настройки, которые видит keynav через публичный контракт. */
const userSettings = {};
globalThis.window = {
    fetch: () => Promise.resolve({ ok: true, status: 200, json: async () => ({}) }),
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    setTimeout, clearTimeout,
    innerWidth: 1400,
    ForgeApp: { state: { userSettings } },
};
globalThis.innerWidth = 1400;
globalThis.fetch = globalThis.window.fetch;
// media.js/upload.js трогают их только внутри функций, но модули грузятся
// целиком — пусть глобали существуют.
globalThis.Blob = class Blob { constructor() { this.size = 0; this.type = ''; } };
globalThis.IntersectionObserver = class IntersectionObserver {
    constructor(cb) { this.cb = cb; }

    observe() {}

    unobserve() {}

    disconnect() {}
};

/** Что уехало в буфер обмена. */
const clipboard = [];
// `navigator` в node — getter-only свойство globalThis, присвоением его не
// подменить; определяем через defineProperty.
Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: {
        clipboard: {
            writeText: (t) => { clipboard.push(String(t)); return Promise.resolve(); },
        },
    },
});

// ---------------------------------------------------------------------------
// Модули
// ---------------------------------------------------------------------------

const base = new URL('../../static/js/vk/', import.meta.url).href;
const keynav = await import(`${base}keynav.js`);
const reply = await import(`${base}reply.js`);
const { vkState } = await import(`${base}state.js`);
// Панель Telegram поднимается ТОЛЬКО ради секции «сосуществование».
const tgBase = new URL('../../static/js/telegram/', import.meta.url).href;
const tgKeynav = await import(`${tgBase}keynav.js`);
const { tgState } = await import(`${tgBase}state.js`);

// ---------------------------------------------------------------------------
// Мини-фреймворк
// ---------------------------------------------------------------------------

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

// ---------------------------------------------------------------------------
// Хелперы сценариев
// ---------------------------------------------------------------------------

/** Нажатие клавиши: доставляем документным слушателям, как это делает браузер. */
function press(code, key, mods, target) {
    return dispatch($document, 'keydown', {
        code,
        key: key === undefined ? '' : key,
        target: target || activeElement || $body,
        ctrlKey: !!(mods && mods.ctrl),
        altKey: !!(mods && mods.alt),
        shiftKey: !!(mods && mods.shift),
        metaKey: !!(mods && mods.meta),
    });
}

/** Строка беседы в колонке (как её строит dialogs.js::buildRow). */
function addDialog(list, id, cls, unreadText) {
    const li = makeEl('li');
    li.className = cls;
    li.dataset.dialogId = String(id);
    li.tabIndex = 0;
    const unread = makeEl('span');
    unread.className = 'vk-dialog-unread';
    unread.textContent = unreadText || '';
    li.appendChild(unread);
    li._vk = { unread };
    li.addEventListener('click', () => { opened.push(String(id)); });
    list.appendChild(li);
    return li;
}

/**
 * Строка сообщения в ленте (как её строит chat.js::buildMessageRow).
 *
 * Ключ курсора — `cmid`, а не `id`: у VK ответы адресуются
 * `conversation_message_id`, и он же нумеруется В КАЖДОЙ беседе с единицы.
 */
function addMessage(cmid, text, opts) {
    const li = makeEl('li');
    li.className = 'vk-line';
    const textEl = makeEl('span');
    textEl.className = 'vk-line-text';
    textEl.textContent = text;
    li.appendChild(textEl);
    li._vk = {
        text: textEl,
        suffix: makeEl('span'),
        msg: {
            id: 90000 + cmid, // глобальный id ЗАВЕДОМО не равен cmid
            cmid,
            dialog_id: String(vkState.activeDialogId || '100'),
            text,
            out: false,
            date: 1700000000,
            sender: { id: '7', name: 'Вася' },
            attachments: [],
        },
    };
    // Вложения: N карточек, у каждой OPEN и SAVE (media.js::buildMediaCard).
    const attachCount = (opts && opts.attachments) || 0;
    if (attachCount) {
        const group = makeEl('div');
        group.className = 'vk-media-group';
        for (let i = 0; i < attachCount; i += 1) {
            const card = makeEl('div');
            card.className = 'vk-media-card vk-media-photo';
            card.dataset.index = String(i);
            const bodyEl = makeEl('div');
            bodyEl.className = 'vk-media-body';
            const foot = makeEl('div');
            foot.className = 'vk-media-foot';
            const openBtn = makeEl('button');
            openBtn.className = 'vk-media-btn';
            openBtn.textContent = 'OPEN';
            openBtn.disabled = !!(opts && opts.mediaDisabled);
            const saveBtn = makeEl('button');
            saveBtn.className = 'vk-media-btn';
            saveBtn.textContent = 'SAVE';
            foot.appendChild(openBtn);
            foot.appendChild(saveBtn);
            card.appendChild(bodyEl);
            card.appendChild(foot);
            group.appendChild(card);
            li._vk.msg.attachments.push({ type: 'photo' });
        }
        li.appendChild(group);
    }
    if (opts && opts.button) {
        const kb = makeEl('div');
        kb.className = 'vk-kb';
        const btn = makeEl('button');
        btn.className = 'vk-kb-btn';
        btn.textContent = 'кнопка';
        kb.appendChild(btn);
        li.appendChild(kb);
        li._vk.kb = kb;
    }
    if (opts && opts.quote) {
        const q = makeEl('div');
        q.className = 'vk-reply-quote';
        q.dataset.replyTo = String(opts.quote);
        q.addEventListener('click', () => { jumped.push(Number(opts.quote)); });
        li.appendChild(q);
    }
    $log.appendChild(li);
    return li;
}

function cursorNode() {
    return $panel.querySelector('.' + keynav.KB_CURSOR_CLASS);
}

/** cmid сообщения под курсором (или null). */
function cursorMsgId() {
    const node = cursorNode();
    return node && node._vk && node._vk.msg ? node._vk.msg.cmid : null;
}

/** id беседы под курсором (или null). */
function cursorDialogId() {
    const node = cursorNode();
    return node && node.dataset ? node.dataset.dialogId || null : null;
}

function resetLog() {
    $log.childNodes.slice().forEach((n) => { if (n !== $spacer) detach(n); });
    $log.scrollTop = 0;
}

const opened = [];
const jumped = [];

// ---------------------------------------------------------------------------
// Стартовое состояние
// ---------------------------------------------------------------------------

vkState.activeDialogId = '100';
vkState.dialogs = [
    { id: '100', title: 'Первый', kind: 'user' },
    { id: '200', title: 'Второй', kind: 'user' },
];

const $d100 = addDialog($dialogList, '100', 'vk-dialog', '');
const $d200 = addDialog($dialogList, '200', 'vk-dialog', '3');
const $d300 = addDialog($communityList, '-70', 'vk-community', '');

const $dirAll = makeEl('li');
$dirAll.className = 'vk-dir active';
$dirAll.dataset.dir = 'all';
$dirAll.addEventListener('click', () => { dirClicks.push('all'); });
const $dirUnread = makeEl('li');
$dirUnread.className = 'vk-dir';
$dirUnread.dataset.dir = 'unread';
$dirUnread.addEventListener('click', () => { dirClicks.push('unread'); });
$dirList.appendChild($dirAll);
$dirList.appendChild($dirUnread);
const dirClicks = [];

addMessage(1, 'первое');
addMessage(2, 'второе');
addMessage(3, 'третье');

const notices = [];
keynav.initKeynav({ onNotice: (t) => notices.push(t) });

// ---------------------------------------------------------------------------
// 1. Гейты: кто владеет клавиатурой
// ---------------------------------------------------------------------------

eq(keynav.hotkeysEnabled(), true, 'гейт: без сохранённых настроек хоткеи ВКЛЮЧЕНЫ');
userSettings.vk_hotkeys_enabled = false;
eq(keynav.hotkeysEnabled(), false, 'гейт: тумблер выключает');
userSettings.vk_hotkeys_enabled = true;
eq(keynav.hotkeysEnabled(), true, 'гейт: тумблер включает обратно');
delete userSettings.vk_hotkeys_enabled;
eq(keynav.hotkeysEnabled(), true, 'гейт: отсутствие поля = включено (дефолт backend)');
// Гейт дефолта — не косметика: `=== true` вместо `!== false` выключил бы фичу
// у всех, кто ни разу не открывал настройки.
userSettings.vk_hotkeys_enabled = 'да';
eq(keynav.hotkeysEnabled(), true, 'гейт: любое значение кроме false = включено');
delete userSettings.vk_hotkeys_enabled;

eq(keynav.panelVisible(), true, 'гейт: панель видна');
$panel.hidden = true;
eq(keynav.panelVisible(), false, 'гейт: скрытая панель');
eq(keynav.vkOwnsKeyboard(), false, 'гейт: скрытая панель клавиатурой не владеет');
$panel.hidden = false;

// Оверлей логина закрывает панель целиком — навигация по беседам там бессмысленна.
$login.hidden = false;
eq(keynav.panelVisible(), false, 'гейт: открытый логин = панель не готова');
eq(keynav.vkOwnsKeyboard(), false, 'гейт: при открытом логине владения нет');
$login.hidden = true;

// CAPTCHA — гейт, которого у Telegram нет вовсе: оверлей всплывает на ЛЮБОЙ
// метод API, и пока пользователь набирает код, клавиши принадлежат его полю.
$captcha.hidden = false;
eq(keynav.panelVisible(), false, 'гейт: открытая CAPTCHA = панель не готова');
eq(keynav.vkOwnsKeyboard(), false, 'гейт: при открытой CAPTCHA владения нет');
{
    keynav.clearCursor();
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'CAPTCHA: нажатие не перехвачено');
    ok(cursorNode() === null, 'CAPTCHA: курсор не появился');
}
$captcha.hidden = true;

// Попап анализа: без этого гейта буквы `j` и `k` листали бы список бесед
// вместо того, чтобы попадать в поле вопроса, — вопрос стало бы физически не
// набрать.
$analysis.hidden = false;
eq(keynav.panelVisible(), false, 'гейт: открытый попап анализа = панель не готова');
eq(keynav.vkOwnsKeyboard(), false, 'гейт: при открытом попапе анализа владения нет');
{
    keynav.clearCursor();
    const eDown = press('KeyJ', 'j');
    eq(eDown.defaultPrevented, false, 'анализ: `j` не перехвачен — буква уходит в textarea');
    const eUp = press('KeyK', 'k');
    eq(eUp.defaultPrevented, false, 'анализ: `k` не перехвачен');
    ok(cursorNode() === null, 'анализ: курсор по беседам не появился');
}
$analysis.hidden = true;
eq(keynav.panelVisible(), true, 'гейт: закрытый попап анализа возвращает навигацию');

activeElement = null;
eq(keynav.vkOwnsKeyboard(), true, 'гейт: фокус нигде — панель владеет');
$log.focus();
eq(keynav.vkOwnsKeyboard(), true, 'гейт: фокус внутри панели — владеет');
// Ключевое: уйдя фокусом в сайдбар сессий, пользователь возвращает себе vim.
const $outside = makeEl('div');
$body.appendChild($outside);
$outside.focus();
eq(keynav.vkOwnsKeyboard(), false, 'гейт: фокус вне панели — НЕ владеет');
activeElement = null;
userSettings.vk_hotkeys_enabled = false;
eq(keynav.vkOwnsKeyboard(), false, 'гейт: выключённые хоткеи не владеют клавиатурой');
delete userSettings.vk_hotkeys_enabled;

// Контракт для hotkeys.js обязан быть опубликован — иначе vim не узнает,
// что должен молчать.
ok(globalThis.window.ForgeApp.vkKeys, 'контракт: ForgeApp.vkKeys опубликован');
eq(typeof globalThis.window.ForgeApp.vkKeys.owns, 'function', 'контракт: owns() есть');
eq(typeof globalThis.window.ForgeApp.vkKeys.enabled, 'function', 'контракт: enabled() есть');
eq(typeof globalThis.window.ForgeApp.vkKeys.invalidate, 'function', 'контракт: invalidate() есть');
// Имя контракта — часть публичного договора с hotkeys.js::PANEL_KEY_CONTRACTS.
eq(globalThis.window.ForgeApp.vkKeys.owns(), keynav.vkOwnsKeyboard(),
    'контракт: owns() отвечает то же, что vkOwnsKeyboard()');

// ---------------------------------------------------------------------------
// 2. Курсор по беседам
// ---------------------------------------------------------------------------

press('KeyD', 'в'); // РУССКАЯ раскладка: key — кириллица, code — физическая D
eq(cursorDialogId(), '100', 'беседы: d ставит курсор на активную беседу');
eq(keynav._debugKeynavState().activeZone, 'dialogs', 'беседы: зона — список бесед');

press('KeyJ', 'о');
eq(cursorDialogId(), '200', 'беседы: j — следующая (русская раскладка)');
press('KeyJ', 'j');
eq(cursorDialogId(), '-70', 'беседы: j доходит до сообществ (обе колонки — один список)');
press('KeyJ', 'j');
eq(cursorDialogId(), '-70', 'беседы: на последней j никуда не уводит');
press('KeyK', 'л');
eq(cursorDialogId(), '200', 'беседы: k — предыдущая (русская раскладка)');

// Курсор хранится идентификатором, а не ссылкой: после перерисовки колонки
// (dialogs.js пересоздаёт <li> целиком) он обязан найтись заново.
const stale = $d200;
detach($d200);
const $d200b = addDialog($dialogList, '200', 'vk-dialog', '3');
press('KeyJ', 'j');
eq(cursorDialogId(), '-70', 'беседы: после перерисовки курсор нашёлся по id и шагнул дальше');
eq($panel.querySelectorAll('.' + keynav.KB_CURSOR_CLASS).length, 1,
    'беседы: подсвечена ровно одна строка, дублей после перерисовки нет');
ok(stale.parentNode === null, 'беседы: старая строка действительно выброшена из дерева');
ok($d200b.parentNode === $dialogList, 'беседы: пересозданная строка на месте');
ok($d100 !== null, 'беседы: первая строка на месте');

press('KeyK', 'k');
eq(cursorDialogId(), '200', 'беседы: вернулись на пересозданную строку');
const beforeOpen = opened.length;
press('Enter', 'Enter');
eq(opened.length, beforeOpen + 1, 'беседы: Enter кликает по строке');
eq(opened[opened.length - 1], '200', 'беседы: открылась именно беседа под курсором');

// Непрочитанные.
press('KeyD', 'd');
press('KeyU', 'u');
eq(cursorDialogId(), '200', 'беседы: u ведёт к непрочитанной');
press('KeyU', 'u');
eq(cursorDialogId(), '200', 'беседы: u по кругу возвращается к той же');
{
    // Когда непрочитанных нет — честная подсказка, а не тишина.
    const badge = $d200b._vk.unread;
    badge.textContent = '';
    const before = notices.length;
    press('KeyU', 'u');
    eq(notices.length, before + 1, 'беседы: без непрочитанных показываем подсказку');
    ok(/Непрочитанных бесед нет/.test(notices[notices.length - 1]),
        'беседы: текст подсказки про беседы, а не про чаты');
    badge.textContent = '3';
}

// Серверные фильтры VK (аналог папок Telegram): каждое переключение — запрос.
const dirBefore = dirClicks.length;
press('BracketRight', ']');
eq(dirClicks.length, dirBefore + 1, 'фильтры: ] переключает');
eq(dirClicks[dirClicks.length - 1], 'unread', 'фильтры: ] идёт к следующему');
$dirUnread.classList.add('active');
$dirAll.classList.remove('active');
press('BracketLeft', '[');
eq(dirClicks[dirClicks.length - 1], 'all', 'фильтры: [ идёт к предыдущему');
$dirAll.classList.add('active');
$dirUnread.classList.remove('active');

// ---------------------------------------------------------------------------
// 3. Курсор по сообщениям (ключ — cmid)
// ---------------------------------------------------------------------------

press('KeyM', 'ь');
eq(cursorMsgId(), 3, 'лента: m ставит курсор на последнее сообщение');
eq(keynav._debugKeynavState().activeZone, 'log', 'лента: зона — лента');
eq(keynav._debugKeynavState().msgCursor, 3, 'лента: курсор хранится cmid-ом, а не глобальным id');
press('KeyK', 'k');
eq(cursorMsgId(), 2, 'лента: k — выше');
press('KeyK', 'k');
eq(cursorMsgId(), 1, 'лента: k — ещё выше');

// Верх ленты: вместо тупика докручиваем вверх, чтобы сработала штатная
// подгрузка истории (chat.js::bindScroll).
$log.scrollTop = 500;
press('KeyK', 'k');
eq(cursorMsgId(), 1, 'лента: на первом сообщении курсор остаётся');
ok($log.scrollTop < 500, 'лента: k у верхней границы прокручивает ленту вверх');

press('KeyJ', 'j');
eq(cursorMsgId(), 2, 'лента: j — ниже');

// G — в конец: и лента, и курсор.
$log.scrollTop = 0;
press('KeyG', 'П', { shift: true });
eq($log.scrollTop, $log.scrollHeight, 'лента: ⇧G прокручивает в самый низ');
press('KeyJ', 'j');
eq(cursorMsgId(), 3, 'лента: после ⇧G курсор тоже внизу — j не возвращает наверх');

// g — подгрузка истории выше (прокрутка вверх).
$log.scrollTop = 800;
press('KeyG', 'п');
ok($log.scrollTop < 800, 'лента: g прокручивает вверх');

// ---------------------------------------------------------------------------
// 4. Действия над сообщением
// ---------------------------------------------------------------------------

press('KeyM', 'm');
eq(cursorMsgId(), 3, 'действия: курсор на последнем');
press('KeyR', 'к');
{
    const target = reply.replyTarget('100');
    ok(target !== null, 'ответ: r выбрал цель ответа');
    eq(target && target.cmid, 3, 'ответ: цель адресована cmid-ом сообщения под курсором');
}
// Фокус после r обязан уйти в поле ввода: ответ — это начало набора.
ok(activeElement === $input, 'ответ: фокус ушёл в поле ввода');
reply.clearReplyTarget();

// Копирование.
$log.focus();
press('KeyM', 'm');
const clipBefore = clipboard.length;
press('KeyY', 'н');
eq(clipboard.length, clipBefore + 1, 'копирование: y положил текст в буфер');
eq(clipboard[clipboard.length - 1], 'третье', 'копирование: скопирован текст сообщения под курсором');

// Цитата.
resetLog();
addMessage(10, 'без цитаты');
const $withQuote = addMessage(11, 'с цитатой', { quote: 10 });
press('KeyM', 'm');
eq(cursorMsgId(), 11, 'цитата: курсор на сообщении с цитатой');
const jumpBefore = jumped.length;
press('KeyO', 'щ');
eq(jumped.length, jumpBefore + 1, 'цитата: o кликает по цитате');
eq(jumped[jumped.length - 1], 10, 'цитата: переход к нужному оригиналу');
ok($withQuote !== null, 'цитата: строка на месте');

// ---------------------------------------------------------------------------
// 5. Вложения: v ведёт к ним, j/k ходят по НИМ, Enter открывает N-е
// ---------------------------------------------------------------------------
//
// Действие, которого у Telegram нет: там media — ОДИН объект, а у VK на
// сообщении до десяти вложений, и без этого шага третье фото галереи с
// клавиатуры было бы не открыть.

resetLog();
const $gallery = addMessage(20, 'галерея', { attachments: 3 });
press('KeyM', 'm');
const $cards = $gallery.querySelectorAll('.vk-media-card');
eq($cards.length, 3, 'вложения: в строке три карточки');
const openBtns = $cards.map((c) => c.querySelectorAll('.vk-media-btn')[0]);

press('KeyV', 'м'); // русская раскладка
ok(activeElement === openBtns[0], 'вложения: v ставит фокус на первое вложение');
eq(keynav._debugKeynavState().activeZone, 'media', 'вложения: зона — вложения');

press('KeyJ', 'j');
ok(activeElement === openBtns[1], 'вложения: j — следующее вложение');
press('KeyJ', 'j');
ok(activeElement === openBtns[2], 'вложения: j — третье вложение');
press('KeyJ', 'j');
ok(activeElement === openBtns[2], 'вложения: на последнем j никуда не уводит');
press('KeyK', 'k');
ok(activeElement === openBtns[1], 'вложения: k — предыдущее');

// Шаг курсора — по ВЛОЖЕНИЯМ, а не по кнопкам: у карточки их две (OPEN и
// SAVE), и хождение по обеим означало бы два нажатия j на одно вложение.
{
    const allBtns = $gallery.querySelectorAll('.vk-media-btn');
    eq(allBtns.length, 6, 'вложения: кнопок вдвое больше карточек (OPEN+SAVE)');
}

// Enter по сфокусированной кнопке мы НЕ перехватываем: <button> браузер
// активирует сам, а свой click рядом с нативным дал бы два открытия.
{
    const before = openBtns[1].clickCount;
    const e = press('Enter', 'Enter', {}, openBtns[1]);
    eq(e.defaultPrevented, false, 'вложения: Enter на кнопке отдан браузеру');
    eq(openBtns[1].clickCount, before, 'вложения: своего клика не добавляем');
}

// Escape из зоны вложений возвращает в ленту.
{
    const e = press('Escape', 'Escape', {}, openBtns[1]);
    eq(e.defaultPrevented, true, 'вложения: Escape перехвачен');
    eq(keynav._debugKeynavState().activeZone, 'log', 'вложения: Escape вернул в ленту');
}

// А со СТРОКИ Enter открывает первое вложение — самое ожидаемое действие.
$log.focus();
press('KeyM', 'm');
{
    const before = openBtns[0].clickCount;
    const e = press('Enter', 'Enter');
    eq(e.defaultPrevented, true, 'вложения: Enter со строки перехвачен');
    eq(openBtns[0].clickCount, before + 1, 'вложения: Enter со строки открывает первое вложение');
}

// У сообщения без вложений — честная подсказка, а не тишина.
resetLog();
addMessage(21, 'просто текст');
press('KeyM', 'm');
{
    const before = notices.length;
    press('KeyV', 'v');
    eq(notices.length, before + 1, 'вложения: без вложений показываем подсказку');
    ok(/вложений/.test(notices[notices.length - 1]), 'вложения: текст подсказки про вложения');
}

// Отключённая кнопка OPEN (у ссылки/опроса файла нет) — Enter со строки её не
// нажимает: съеденная клавиша без единого следствия хуже, чем неперехваченная.
resetLog();
const $noFile = addMessage(22, 'опрос', { attachments: 1, mediaDisabled: true });
press('KeyM', 'm');
{
    const btn = $noFile.querySelectorAll('.vk-media-btn')[0];
    const before = btn.clickCount;
    const e = press('Enter', 'Enter');
    eq(btn.clickCount, before, 'вложения: отключённая OPEN не нажимается');
    eq(e.defaultPrevented, false, 'вложения: событие не погашено — действия не было');
}

// ---------------------------------------------------------------------------
// 6. Кнопки сообществ
// ---------------------------------------------------------------------------

resetLog();
const $withBtn = addMessage(30, 'меню', { button: true });
press('KeyM', 'm');
press('KeyB', 'и');
const $btn = $withBtn.querySelector('.vk-kb-btn');
ok(activeElement === $btn, 'кнопки: b ставит фокус на кнопку сообщения');
{
    const btnBefore = $btn.clickCount;
    const e = press('Enter', 'Enter', {}, $btn);
    eq(e.defaultPrevented, false, 'кнопки: Enter на кнопке отдан браузеру');
    eq($btn.clickCount, btnBefore, 'кнопки: своего клика не добавляем — дубля нажатия не будет');
}
$log.focus();
press('KeyM', 'm');
{
    const btnBefore = $btn.clickCount;
    const e = press('Enter', 'Enter');
    eq(e.defaultPrevented, true, 'кнопки: Enter со строки перехвачен');
    eq($btn.clickCount, btnBefore + 1, 'кнопки: Enter со строки нажимает первую кнопку сообщения');
}

resetLog();
addMessage(40, 'просто текст');
press('KeyM', 'm');
{
    const before = notices.length;
    press('KeyB', 'b');
    eq(notices.length, before + 1, 'кнопки: без кнопок показываем подсказку');
}

// ---------------------------------------------------------------------------
// 7. Панель: вложение, тема
// ---------------------------------------------------------------------------

{
    const before = $attach.clickCount;
    press('KeyA', 'ф');
    eq($attach.clickCount, before + 1, 'панель: a открывает выбор файла');
}
{
    const before = $theme.clickCount;
    press('KeyT', 'е');
    eq($theme.clickCount, before + 1, 'панель: t переключает тему');
}

// ---------------------------------------------------------------------------
// 8. Поле ввода
// ---------------------------------------------------------------------------

resetLog();
addMessage(50, 'раз');
addMessage(51, 'два');
press('KeyM', 'm');
eq(cursorMsgId(), 51, 'ввод: исходно курсор на последнем');

press('KeyI', 'ш');
ok(activeElement === $input, 'ввод: i ставит фокус в поле');

// Главное правило: в поле ввода одиночные клавиши обязаны доходить до поля.
$input.tagName = 'input';
const evJ = press('KeyJ', 'о', {}, $input);
eq(evJ.defaultPrevented, false, 'ввод: j в поле НЕ перехвачен');
eq(cursorMsgId(), 51, 'ввод: курсор от j в поле не сдвинулся');
const evG = press('KeyG', 'П', { shift: true }, $input);
eq(evG.defaultPrevented, false, 'ввод: ⇧G в поле — это заглавная буква, не хоткей');

// Escape из поля возвращает в ленту.
const evEsc = press('Escape', 'Escape', {}, $input);
eq(evEsc.defaultPrevented, true, 'ввод: Escape в поле перехвачен');
eq(keynav._debugKeynavState().activeZone, 'log', 'ввод: Escape вернул в ленту');

// А вот при выбранной цели ответа Escape обязан достаться chat.js — он снимает
// полосу «отвечаю Васе». Перехватив его здесь, мы отменили бы не то.
press('KeyM', 'm');
press('KeyR', 'r');
{
    const e = press('Escape', 'Escape', {}, $input);
    eq(e.defaultPrevented, false, 'ввод: Escape с активным ответом отдан chat.js');
}
reply.clearReplyTarget();

// ---------------------------------------------------------------------------
// 9. Шпаргалка
// ---------------------------------------------------------------------------

$log.focus();
eq($panel.querySelectorAll('.vk-keys-help').length, 0, 'шпаргалка: изначально закрыта');
{
    const e = press('Slash', '?', { shift: true }, $input);
    eq(e.defaultPrevented, false, 'шпаргалка: «?» в поле ввода — обычный символ');
    eq($panel.querySelectorAll('.vk-keys-help').length, 0, 'шпаргалка: из поля не открылась');
}
press('Slash', '?', { shift: true });
eq($panel.querySelectorAll('.vk-keys-help').length, 1, 'шпаргалка: ? открывает');
{
    const card = $panel.querySelector('.vk-keys-help-card');
    ok(card.textContent.includes('ГОРЯЧИЕ КЛАВИШИ'), 'шпаргалка: заголовок на месте');
    ok(card.querySelectorAll('.vk-keys-help-row').length >= 10,
        'шпаргалка: перечислены все действия');
    ok(card.textContent.includes('Настройки'), 'шпаргалка: сказано, где переназначать');
    ok(card.textContent.includes('вложениям'), 'шпаргалка: есть действие про вложения');
    // Шпаргалка рисуется ВНУТРИ панели (z-index 40), а не поверх страницы —
    // иначе она перекрыла бы соседние вкладки devforge.
    ok($panel.contains($panel.querySelector('.vk-keys-help')),
        'шпаргалка: живёт внутри #vk');
}
press('Slash', '?', { shift: true });
eq($panel.querySelectorAll('.vk-keys-help').length, 0, 'шпаргалка: ? закрывает');
press('Slash', '?', { shift: true });
press('Escape', 'Escape');
eq($panel.querySelectorAll('.vk-keys-help').length, 0, 'шпаргалка: Escape закрывает');

// Переназначенная клавиша обязана попасть в шпаргалку — иначе она врёт.
userSettings.vk_hotkeys = { 'msg.reply': 'Ctrl+r' };
keynav.invalidateBindings();
press('Slash', '?', { shift: true });
{
    const card = $panel.querySelector('.vk-keys-help-card');
    ok(card.textContent.includes('Ctrl'), 'шпаргалка: показывает переназначенную комбинацию');
}
press('Escape', 'Escape');

// ---------------------------------------------------------------------------
// 10. Переназначение и его применение
// ---------------------------------------------------------------------------

resetLog();
addMessage(60, 'раз');
addMessage(61, 'два');
$log.focus();
press('KeyM', 'm');
{
    const e = press('KeyR', 'r');
    eq(e.defaultPrevented, false, 'переназначение: освободившаяся r ничего не делает');
    eq(reply.replyTarget('100'), null, 'переназначение: ответ не выбран');
}
{
    const e = press('KeyR', 'r', { ctrl: true });
    eq(e.defaultPrevented, true, 'переназначение: Ctrl+r перехвачена');
    ok(reply.replyTarget('100') !== null, 'переназначение: Ctrl+r выбрала цель ответа');
}
reply.clearReplyTarget();

// Конфликт: переопределение забрало клавишу у навигации — та обязана
// замолчать, а не работать «через раз».
userSettings.vk_hotkeys = { 'msg.copy': 'j' };
keynav.invalidateBindings();
$log.focus();
press('KeyM', 'm');
{
    const before = cursorMsgId();
    const e = press('KeyJ', 'j');
    eq(cursorMsgId(), before, 'конфликт: j больше не двигает курсор');
    eq(e.defaultPrevented, true, 'конфликт: j ушла победителю (копированию)');
}
delete userSettings.vk_hotkeys;
keynav.invalidateBindings();

// Кэш привязок ключуется по СОДЕРЖИМОМУ карты переопределений, а не по ссылке
// на объект настроек. Отсюда важное свойство: сохранённые настройки действуют
// сразу, БЕЗ ПЕРЕЗАГРУЗКИ страницы и даже без явного invalidate.
userSettings.vk_hotkeys = { 'msg.copy': 'j' };
{
    $log.focus();
    press('KeyM', 'm');
    const before = cursorMsgId();
    press('KeyJ', 'j');
    eq(cursorMsgId(), before, 'кэш: новая привязка действует сразу, без invalidate');
}
keynav.invalidateBindings();
{
    const before = cursorMsgId();
    press('KeyJ', 'j');
    eq(cursorMsgId(), before, 'кэш: invalidate не воскрешает старую привязку');
}
delete userSettings.vk_hotkeys;
keynav.invalidateBindings();
{
    $log.focus();
    press('KeyM', 'm');
    const before = cursorMsgId();
    press('KeyK', 'k');
    ok(cursorMsgId() !== before, 'кэш: после снятия переопределения навигация вернулась');
}

// ---------------------------------------------------------------------------
// 11. Гейты во время работы
// ---------------------------------------------------------------------------

userSettings.vk_hotkeys_enabled = false;
{
    resetLog();
    addMessage(70, 'раз');
    addMessage(71, 'два');
    keynav.clearCursor();
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'выключено: нажатие не перехвачено');
    ok(cursorNode() === null, 'выключено: курсор не появился');
}
delete userSettings.vk_hotkeys_enabled;

// Лайтбокс перекрывает панель и живёт по своим правилам (стрелки листают
// галерею вложений).
$lightbox.hidden = false;
{
    keynav.clearCursor();
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'лайтбокс: нажатие не перехвачено');
    ok(cursorNode() === null, 'лайтбокс: курсор не появился');
}
$lightbox.hidden = true;

// Модалка настроек: там записывают новые комбинации, наши клавиши обязаны
// молчать — иначе запись 'j' сразу же дёрнула бы навигацию.
{
    const $overlay = makeEl('div');
    $overlay.className = 'modal-overlay';
    $body.appendChild($overlay);
    keynav.clearCursor();
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'модалка: нажатие не перехвачено');
    ok(cursorNode() === null, 'модалка: курсор не появился');
    detach($overlay);
}

// Скрытая панель (пользователь ушёл на другую вкладку devforge).
$panel.hidden = true;
{
    keynav.clearCursor();
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'скрытая панель: нажатие не перехвачено');
}
$panel.hidden = false;

// ---------------------------------------------------------------------------
// 12. СОСУЩЕСТВОВАНИЕ ДВУХ ПАНЕЛЕЙ
// ---------------------------------------------------------------------------
//
// Обе панели держат по слушателю в capture на document и по контракту в
// window.ForgeApp. Конфликт невозможен по построению: switchTab держит
// непогашенным ровно один пейн, а panelVisible() каждой панели смотрит на
// СВОЙ #telegram / #vk. Проверяем это явно — цена ошибки здесь в том, что
// одна панель молча ворует клавиши у другой.

tgState.activeDialogId = '900';
tgKeynav.initKeynav({});
activeElement = null;

// Обе скрыты (открыт терминал) — владения нет ни у кого, работает vim.
$panel.hidden = true;
$tgPanel.hidden = true;
eq(keynav.vkOwnsKeyboard(), false, 'сосуществование: обе скрыты — VK не владеет');
eq(tgKeynav.telegramOwnsKeyboard(), false, 'сосуществование: обе скрыты — Telegram не владеет');

// Открыт только Telegram.
$tgPanel.hidden = false;
eq(tgKeynav.telegramOwnsKeyboard(), true, 'сосуществование: видимый #telegram владеет');
eq(keynav.vkOwnsKeyboard(), false, 'сосуществование: при видимом #telegram VK НЕ владеет');

// Открыт только VK.
$tgPanel.hidden = true;
$panel.hidden = false;
eq(keynav.vkOwnsKeyboard(), true, 'сосуществование: видимый #vk владеет');
eq(tgKeynav.telegramOwnsKeyboard(), false, 'сосуществование: при видимом #vk Telegram НЕ владеет');

// Открыт VK, но фокус в сайдбаре сессий — пользователь возвращает себе vim.
$outside.focus();
eq(keynav.vkOwnsKeyboard(), false, 'сосуществование: фокус в сайдбаре — клик вернул vim');
eq(tgKeynav.telegramOwnsKeyboard(), false, 'сосуществование: и Telegram тоже не владеет');
activeElement = null;

// Открыт VK, но его CAPTCHA поверх — владения нет, хотя панель видима.
$captcha.hidden = false;
eq(keynav.vkOwnsKeyboard(), false, 'сосуществование: CAPTCHA снимает владение у видимой панели');
$captcha.hidden = true;

// Нажатие при видимом Telegram и скрытом VK не двигает курсор VK.
$tgPanel.hidden = false;
$panel.hidden = true;
{
    keynav.clearCursor();
    resetLog();
    addMessage(80, 'раз');
    press('KeyM', 'm');
    ok(cursorNode() === null, 'сосуществование: скрытый VK на нажатия не реагирует');
}
$tgPanel.hidden = true;
$panel.hidden = false;

// ---------------------------------------------------------------------------
// 13. Сброс и teardown
// ---------------------------------------------------------------------------

resetLog();
addMessage(80, 'раз');
press('KeyM', 'm');
ok(cursorNode() !== null, 'сброс: курсор поставлен');
keynav.resetKeynav();
ok(cursorNode() === null, 'сброс: resetKeynav снял подсветку');
eq(keynav._debugKeynavState().zone, null, 'сброс: зона очищена');

press('Slash', '?', { shift: true });
eq($panel.querySelectorAll('.vk-keys-help').length, 1, 'teardown: шпаргалка открыта перед снятием');
keynav.teardownKeynav();
eq($panel.querySelectorAll('.vk-keys-help').length, 0, 'teardown: шпаргалка закрыта');
eq(keynav._debugKeynavState().bound, false, 'teardown: слушатель снят с флага');
{
    resetLog();
    addMessage(90, 'раз');
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'teardown: нажатия больше не обрабатываются');
    ok(cursorNode() === null, 'teardown: курсор не ставится');
}

// Повторный init поднимает всё обратно — вкладку можно закрыть и открыть.
keynav.initKeynav({ onNotice: (t) => notices.push(t) });
{
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, true, 'повторный init: нажатия снова работают');
    ok(cursorNode() !== null, 'повторный init: курсор ставится');
}
// Второй вызов initKeynav не должен вешать второй слушатель: иначе каждое
// нажатие выполнялось бы дважды.
keynav.initKeynav({});
{
    // Слушателей на документе ровно два: наш и телеграмный (панели живут
    // рядом, каждая гасится собственным panelVisible()).
    eq($document.listeners.get('keydown').length, 2,
        'повторный init: слушателей по-прежнему по одному на панель');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/vk_keynav)`);
if (failures > 0) process.exit(1);
