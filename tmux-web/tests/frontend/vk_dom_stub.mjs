// ВСПОМОГАТЕЛЬНЫЙ МОДУЛЬ (не тест): фейковый DOM для тестов панели VK.
//
// Запускать его напрямую незачем — он импортируется из vk_login.test.mjs и
// vk_captcha.test.mjs ДО первого импорта модулей js/vk/*.
//
// Почему стаб обязан стоять раньше: `core/dom.js` резолвит все ref'ы ОДИН раз
// на верхнем уровне модуля. Импортируй он позже — каждый `$vk*` оказался бы
// null, и тест проверял бы пустоту, зеленея по случайности.
//
// Стаб не претендует на браузер: он реализует ровно тот срез DOM, которым
// пользуются login.js и captcha.js, — id-поиск, hidden/value/src, textContent,
// addEventListener на узлах и на документе, focus(). Всплытие событий не
// эмулируется: оба модуля вешают слушателей на конкретные узлы и на document,
// и решают всё по `e.target`, поэтому достаточно доставки одному слушателю.

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
        type: '',
        focusCount: 0,
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

    // textContent как в браузере: присваивание создаёт текстовый узел.
    Object.defineProperty(node, 'textContent', {
        get: () => node.childNodes.map((c) => c.textContent).join(''),
        set(v) {
            node.childNodes.slice().forEach(detach);
            const t = String(v == null ? '' : v);
            if (t !== '') node.appendChild(makeText(t));
        },
    });

    // `src` у <img> — обычный атрибут: тест смотрит и на свойство, и на
    // getAttribute('src'), потому что removeAttribute обязан убирать оба.
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
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
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
    node.focus = () => { node.focusCount += 1; };
    node.blur = () => {};
    node.click = () => dispatch(node, 'click', { target: node });

    // ------------------------------------------------------------------
    // Срез API, которым пользуются dialogs.js и chat.js.
    //
    // Селекторы поддержаны ровно в трёх формах, которые встречаются в коде
    // панели: `.class`, `#id` и имя тега. Полноценный парсер CSS здесь был бы
    // отдельной библиотекой с собственными багами.
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
    node.insertBefore = (child, ref) => {
        detach(child);
        const i = ref ? node.childNodes.indexOf(ref) : -1;
        child.parentNode = node;
        if (i >= 0) node.childNodes.splice(i, 0, child);
        else node.childNodes.push(child);
        return child;
    };
    // `after` нужен кнопке повтора отправки (js/vk/upload.js::showRetry): она
    // встаёт СРАЗУ ЗА статусом карточки, а не в конец подвала, — иначе
    // «RETRY» оказывался бы правее кнопки отмены.
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
        if (i >= 0) p.childNodes.splice(i, 1, next);
        else p.childNodes.push(next);
        node.parentNode = null;
    };
    Object.defineProperty(node, 'previousElementSibling', {
        get() {
            const p = node.parentNode;
            if (!p) return null;
            const i = p.childNodes.indexOf(node);
            for (let k = i - 1; k >= 0; k -= 1) {
                if (p.childNodes[k].nodeType === 1) return p.childNodes[k];
            }
            return null;
        },
    });
    Object.defineProperty(node, 'children', {
        get: () => node.childNodes.filter((c) => c.nodeType === 1),
    });
    // Геометрия ленты: тест выставляет её вручную, браузерной раскладки нет.
    node.scrollTop = 0;
    node.scrollHeight = 0;
    node.clientHeight = 0;
    node.getBoundingClientRect = () => ({ top: 0, bottom: 0, left: 0, right: 0, width: 0, height: 0 });
    return node;
}

/**
 * Доставка события ОДНОМУ узлу-слушателю.
 *
 * @param {object} node
 * @param {string} type
 * @param {object} [ev]
 * @returns {object} само событие (можно посмотреть defaultPrevented)
 */
export function dispatch(node, type, ev) {
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

/** Создаёт узел с id и кладёт его в реестр. */
export function el(id, tag) {
    const node = makeEl(tag || 'div');
    node.attributes.id = id;
    els.set(id, node);
    return node;
}

// ---------------------------------------------------------------------------
// Разметка панели: ровно те id, что перечислены в core/dom.js для секции VK.
// ---------------------------------------------------------------------------

const $body = makeEl('body');
export const $panel = el('vk');
export const $toggle = el('vk-toggle', 'button');
export const $unread = el('vk-unread-badge', 'span');
el('vk-titlebar');
export const $selfUser = el('vk-self-user', 'span');
export const $logoutBtn = el('vk-logout', 'button');
export const $mode = el('vk-mode', 'span');
export const $lat = el('vk-lat', 'span');

export const $login = el('vk-login');
export const $loginStep = el('vk-login-step-token');
export const $hint = el('vk-login-hint', 'p');
export const $token = el('vk-token', 'input');
export const $submit = el('vk-login-token-submit', 'button');
export const $status = el('vk-login-status');
export const $error = el('vk-login-error');
export const $countdown = el('vk-login-countdown');

export const $captcha = el('vk-captcha');
export const $captchaImg = el('vk-captcha-img', 'img');
export const $captchaKey = el('vk-captcha-key', 'input');
export const $captchaSubmit = el('vk-captcha-submit', 'button');
export const $captchaCancel = el('vk-captcha-cancel', 'button');
export const $captchaError = el('vk-captcha-error');

// Колонка бесед (js/vk/dialogs.js).
export const $dirList = el('vk-dir-list', 'ul');
export const $dialogList = el('vk-dialog-list', 'ul');
export const $communityList = el('vk-community-list', 'ul');
export const $nodesTitle = el('vk-nodes-title');
export const $statusRight = el('vk-status-right', 'span');
export const $contacts = el('vk-contacts');
export const $contactsToggle = el('vk-contacts-toggle', 'button');
export const $contactsScrim = el('vk-contacts-scrim');

// Лента и поле ввода (js/vk/chat.js).
export const $sessionHead = el('vk-session-head');
export const $log = el('vk-log');
export const $typing = el('vk-typing');
export const $prompt = el('vk-prompt', 'form');
export const $input = el('vk-input', 'input');
export const $send = el('vk-send', 'button');
// #vk-session собран НАСТОЯЩИМ деревом (как в index.html), а не набором
// одиночных узлов: полоса ответа встаёт `insertBefore($vkPrompt)` в его
// родителя, и без него chat.js::ensureReplyBar молча возвращал бы null —
// тест зеленел бы на отсутствии полосы, которую и проверяет.
export const $session = el('vk-session', 'section');
$session.appendChild($sessionHead);
$session.appendChild($log);
$session.appendChild($typing);
$session.appendChild($prompt);
$panel.appendChild($session);
export const $attach = el('vk-attach', 'button');
export const $fileInput = el('vk-file-input', 'input');
export const $dropzone = el('vk-dropzone');
export const $lightbox = el('vk-lightbox');
$lightbox.hidden = true;
// Лайтбокс-ГАЛЕРЕЯ (Ф7): у VK вложений до десяти, поэтому кроме картинки и
// подписи в разметке есть стрелки. Все пять узлов обязаны существовать здесь —
// core/dom.js резолвит ref'ы ОДИН раз при импорте, и отсутствующий узел
// означал бы тест, зеленеющий на пустоте.
export const $lightboxImg = el('vk-lightbox-img', 'img');
export const $lightboxPrev = el('vk-lightbox-prev', 'button');
export const $lightboxNext = el('vk-lightbox-next', 'button');
export const $lightboxClose = el('vk-lightbox-close', 'button');
export const $lightboxCaption = el('vk-lightbox-caption');
$lightbox.appendChild($lightboxClose);
$lightbox.appendChild($lightboxPrev);
$lightbox.appendChild($lightboxImg);
$lightbox.appendChild($lightboxNext);
$lightbox.appendChild($lightboxCaption);
$lightboxPrev.hidden = true;
$lightboxNext.hidden = true;
// Спейсер объявлен в index.html и обязан пережить очистку ленты.
const $spacer = makeEl('div');
$spacer.className = 'vk-log-spacer';
$log.appendChild($spacer);

$login.hidden = true;
$captcha.hidden = true;
$status.hidden = true;
$error.hidden = true;
$countdown.hidden = true;
$body.appendChild($panel);

export const $document = makeEl('#document');
$document.getElementById = (id) => els.get(id) || null;
$document.createElement = (tag) => makeEl(tag);
$document.createTextNode = (t) => makeText(t);
$document.body = $body;
$document.documentElement = makeEl('html');
$document.visibilityState = 'visible';
globalThis.document = $document;

globalThis.location = {
    protocol: 'http:', host: 'localhost', hash: '', href: 'http://localhost/',
    origin: 'http://localhost', pathname: '/', search: '',
};
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
globalThis.history = { replaceState() {} };
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

/** Текущий обработчик запроса; сценарий подменяет его через setNet(). */
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

// ---------------------------------------------------------------------------
// Блобы и ленивые превью (Ф7)
// ---------------------------------------------------------------------------

/**
 * Учёт objectURL: сколько создано, сколько отозвано и что живо сейчас.
 *
 * Ради него стаб и заводит собственный `URL`: инвариант «каждый созданный
 * objectURL отозван» проверяется СЧЁТЧИКОМ, а не «вроде бы вызвали revoke».
 */
export const objectUrls = { created: 0, revoked: 0, live: new Set() };

// ВАЖНО: `URL` НЕ подменяется целиком — тесты соседних модулей строят через
// него пути (`new URL('../../static/...', import.meta.url)`). Дописываются
// только две статические функции, которых в Node нет применительно к нашим
// фейковым блобам.
let urlSeq = 0;
globalThis.URL.createObjectURL = (blob) => {
    urlSeq += 1;
    const url = `blob:vk/${urlSeq}`;
    objectUrls.created += 1;
    objectUrls.live.add(url);
    void blob;
    return url;
};
globalThis.URL.revokeObjectURL = (url) => {
    if (objectUrls.live.delete(url)) objectUrls.revoked += 1;
};

/** Минимальный Blob: media.js читает у него только `size` и `type`. */
globalThis.Blob = class Blob {
    constructor(parts, opts) {
        this.type = (opts && opts.type) || '';
        this.size = (parts || []).reduce((acc, p) => acc + (p.byteLength || p.length || 0), 0);
    }
};

/**
 * IntersectionObserver, который НИЧЕГО не показывает сам.
 *
 * Ленивость превью — часть контракта: тест обязан уметь проверить, что до
 * появления карточки в вьюпорте запроса НЕ БЫЛО. Поэтому наблюдатель копит
 * цели, а тест «показывает» их вручную через `showObserved()`.
 */
export const observed = [];
globalThis.IntersectionObserver = class IntersectionObserver {
    constructor(cb) {
        this.cb = cb;
        observed.push(this);
        this.targets = new Set();
    }

    observe(el) { this.targets.add(el); }

    unobserve(el) { this.targets.delete(el); }

    disconnect() { this.targets.clear(); }
};

/** Изображает попадание всех наблюдаемых карточек в вьюпорт. */
export function showObserved() {
    for (const io of observed) {
        const entries = Array.from(io.targets).map((target) => ({ target, isIntersecting: true }));
        if (entries.length) io.cb(entries);
    }
}

/** Ответ прокси медиа: байты + заголовки, как их читает media.js. */
export function mediaRes(bytes, type) {
    const body = new Uint8Array(bytes);
    return {
        ok: true,
        status: 200,
        headers: {
            get: (h) => {
                const k = String(h).toLowerCase();
                if (k === 'content-type') return type || 'image/jpeg';
                if (k === 'content-length') return String(body.length);
                return null;
            },
        },
        // Поток намеренно есть: именно по нему считается «сетевая нога»
        // прогресса, и подмена его на blob() спрятала бы половину кода.
        body: {
            getReader() {
                let sent = false;
                return {
                    read: async () => {
                        if (sent) return { done: true, value: undefined };
                        sent = true;
                        return { done: false, value: body };
                    },
                };
            },
        },
        blob: async () => new globalThis.Blob([body], { type: type || 'image/jpeg' }),
    };
}

/** Даёт микрозадачам (промисам модулей) доехать. */
export const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
