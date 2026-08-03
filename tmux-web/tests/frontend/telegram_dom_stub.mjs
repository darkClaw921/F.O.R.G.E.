// ВСПОМОГАТЕЛЬНЫЙ МОДУЛЬ (не тест): фейковый DOM для тестов колонки Telegram.
//
// Запускать напрямую незачем — он импортируется из telegram_dialogs_filter
// и telegram_dialog_name ДО первого импорта модулей js/telegram/*.
//
// Почему стаб обязан стоять раньше: `core/dom.js` резолвит все ref'ы ОДИН раз
// на верхнем уровне модуля. Импортируй он позже — каждый `$tg*` оказался бы
// null, и тест проверял бы пустоту, зеленея по случайности.
//
// Стаб не претендует на браузер: он реализует ровно тот срез DOM, которым
// пользуется dialogs.js, — id-поиск, className/classList, textContent как
// текстовые узлы, dataset/title/tabIndex, appendChild и селекторы вида
// `tag.class`. Раньше эта же копия жила прямо в telegram_dialogs_filter;
// вынесена, когда понадобилась второму тесту — двух копий стаба на одну
// панель быть не должно (у VK ровно по этой причине есть vk_dom_stub.mjs).

// ---------------------------------------------------------------------------
// Фейковый DOM (срез, которым пользуется dialogs.js)
// ---------------------------------------------------------------------------

function matchesSel(node, sel) {
    if (!node || node.nodeType !== 1) return false;
    const m = String(sel).trim().match(/^([a-zA-Z]*)((?:\.[-\w]+)*)$/);
    if (!m) return false;
    const [, tag, classes] = m;
    if (tag && node.tagName !== tag.toLowerCase()) return false;
    for (const c of (classes || '').split('.').filter(Boolean)) {
        if (!node.classList.contains(c)) return false;
    }
    return true;
}

export function makeText(data) {
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
        title: '',
        tabIndex: -1,
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
    node.appendChild = (child) => {
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
    node.closest = (sel) => {
        let cur = node;
        while (cur) {
            if (matchesSel(cur, sel)) return cur;
            cur = cur.parentNode;
        }
        return null;
    };
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
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.removeEventListener = () => {};
    node.focus = () => {};
    node.scrollIntoView = () => {};
    return node;
}

export const els = new Map();
function el(id, tag) {
    const node = makeEl(tag || 'div');
    node.attributes.id = id;
    els.set(id, node);
    return node;
}

export const $body = makeEl('body');
export const $panel = el('telegram');
export const $dirList = el('tg-dir-list', 'ul');
export const $dialogList = el('tg-dialog-list', 'ul');
export const $channelList = el('tg-channel-list', 'ul');
export const $nodesTitle = el('tg-nodes-title');
// Шапка сессии: её рисует chat.js::updateHead. Колонке она не нужна, но стаб
// один на панель, а тест названия чата проверяет обе точки — строку списка и
// шапку — одним прогоном.
export const $sessionHead = el('tg-session-head');
el('tg-status-right');
$panel.appendChild($dirList);
$panel.appendChild($dialogList);
$panel.appendChild($channelList);
$panel.appendChild($nodesTitle);
$panel.appendChild($sessionHead);
$body.appendChild($panel);

export const $document = makeEl('#document');
$document.getElementById = (id) => els.get(id) || null;
$document.createElement = (tag) => makeEl(tag);
$document.createTextNode = (t) => makeText(t);
$document.body = $body;
$document.documentElement = makeEl('html');
$document.querySelector = (sel) => $body.querySelector(sel);
$document.querySelectorAll = (sel) => $body.querySelectorAll(sel);
Object.defineProperty($document, 'activeElement', { get: () => null });
globalThis.document = $document;

globalThis.location = {
    protocol: 'http:', host: 'localhost', hash: '', href: 'http://localhost/',
    origin: 'http://localhost', pathname: '/', search: '',
};
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

/** Настройки, которые видит панель через публичный контракт. */
export const userSettings = {};
globalThis.window = {
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    fetch: () => Promise.reject(new Error('offline')),
    setTimeout, clearTimeout,
    ForgeApp: { state: { userSettings } },
};
globalThis.fetch = globalThis.window.fetch;
