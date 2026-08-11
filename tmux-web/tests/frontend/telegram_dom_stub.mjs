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
//
// Второй потребитель — попап «Анализ чата» (telegram_analysis*.test.mjs). Ради
// него срез расширен ровно тем, чего требует общее ядро
// js/messenger/analysis-core.js: `value`/`disabled` (селект глубины, textarea,
// кнопки), геометрия прокрутки (`scrollTop`/`scrollHeight`/`clientHeight`) для
// автоскролла вывода, `createDocumentFragment` + растворение фрагмента в
// `appendChild` (рендерер Markdown отдаёт именно фрагмент), кадр анимации
// (`requestAnimationFrame` — им ядро сливает буфер дельт) и журналируемый
// `fetch`. Всё это ДОБАВЛЕНО, а не заменено: старые тесты колонки видят тот же
// DOM, что и раньше.

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
        // Поля попапа анализа. `value` у <select>/<textarea> здесь обычное
        // свойство (а не вычисляемое из <option>): ядро только читает его и
        // пишет в него. `disabled` — то, чем ядро гасит кнопки на время run'а;
        // без него проверки «START заблокирован» зеленели бы на undefined.
        value: '',
        disabled: false,
        type: '',
        focusCount: 0,
        // Геометрия ленты вывода: браузерной раскладки в стабе нет, тест
        // автоскролла выставляет её руками.
        scrollTop: 0,
        scrollHeight: 0,
        clientHeight: 0,
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
        // DocumentFragment (nodeType 11) РАСТВОРЯЕТСЯ при вставке: в дерево
        // уходят его дети, а не он сам. Без этого правила отрисованный Markdown
        // (js/messenger/markdown.js отдаёт именно фрагмент) оказался бы завёрнут
        // в лишний узел, и тест проверял бы структуру, которой в браузере нет.
        if (child && child.nodeType === 11) {
            child.childNodes.slice().forEach((grand) => {
                detach(grand);
                grand.parentNode = node;
                node.childNodes.push(grand);
            });
            return child;
        }
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
    node.matches = (sel) => matchesSel(node, sel);
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
    node.removeAttribute = (k) => { delete node.attributes[k]; };
    node.hasAttribute = (k) => k in node.attributes;
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    // Снятие слушателя настоящее, а не заглушка: ядро попапа снимает свой
    // обработчик Esc в teardown, и «удаление», которое ничего не удаляет,
    // превратило бы проверку teardown'а в проверку пустоты.
    node.removeEventListener = (type, fn) => {
        const arr = node.listeners.get(type) || [];
        const i = arr.indexOf(fn);
        if (i >= 0) arr.splice(i, 1);
    };
    node.focus = () => { node.focusCount += 1; };
    node.blur = () => {};
    node.click = () => dispatch(node, 'click', { target: node });
    node.scrollIntoView = () => {};
    node.getBoundingClientRect = () => ({
        top: 0, bottom: 0, left: 0, right: 0, width: 0, height: 0,
    });
    return node;
}

/**
 * Доставка события ОДНОМУ узлу-слушателю.
 *
 * Всплытие не эмулируется намеренно: и колонка, и попап вешают слушателей на
 * конкретные узлы и на документ, а решают всё по `e.target`.
 *
 * @param {object} node
 * @param {string} type
 * @param {object} [ev]
 * @returns {object} само событие (видно defaultPrevented/propagationStopped)
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
// ---------------------------------------------------------------------------
// Попап «Анализ чата» (js/telegram/analysis.js поверх messenger/analysis-core.js)
// ---------------------------------------------------------------------------
//
// Узлы обязаны быть здесь ВСЕ до единого и с теми же id, что в index.html:
// core/dom.js резолвит ref'ы ОДИН раз при импорте, и пропущенный узел дал бы
// тест, зеленеющий на пустоте (ядро молча пропускает отсутствующие ref'ы).
export const $analysisToggle = el('tg-analysis-toggle', 'button');
export const $analysis = el('tg-analysis');
export const $analysisTarget = el('tg-analysis-target', 'span');
export const $analysisClose = el('tg-analysis-close', 'button');
export const $analysisTabRun = el('tg-analysis-tab-run', 'button');
export const $analysisTabCtx = el('tg-analysis-tab-ctx', 'button');
export const $analysisPaneRun = el('tg-analysis-pane-run');
export const $analysisPaneCtx = el('tg-analysis-pane-ctx');
export const $analysisDepth = el('tg-analysis-depth', 'select');
export const $analysisStart = el('tg-analysis-start', 'button');
export const $analysisRefresh = el('tg-analysis-refresh', 'button');
export const $analysisCancel = el('tg-analysis-cancel', 'button');
export const $analysisClear = el('tg-analysis-clear', 'button');
export const $analysisClearThread = el('tg-analysis-clear-thread', 'button');
export const $analysisStatus = el('tg-analysis-status');
export const $analysisUpdated = el('tg-analysis-updated');
export const $analysisOutToggle = el('tg-analysis-out-toggle', 'button');
export const $analysisOut = el('tg-analysis-out');
export const $analysisThread = el('tg-analysis-thread');
export const $analysisAsk = el('tg-analysis-ask', 'form');
export const $analysisQuestion = el('tg-analysis-question', 'textarea');
export const $analysisSend = el('tg-analysis-send', 'button');
export const $analysisError = el('tg-analysis-error');
export const $analysisPrompt = el('tg-analysis-prompt', 'textarea');
export const $analysisPromptCount = el('tg-analysis-prompt-count');
export const $analysisPromptSave = el('tg-analysis-prompt-save', 'button');
export const $analysisPromptClear = el('tg-analysis-prompt-clear', 'button');
export const $analysisPromptStatus = el('tg-analysis-prompt-status');
// Каретка внутри шапки разбора — она есть в разметке, и ядро ищет её
// querySelector'ом по классу с префиксом панели (`tg-`). Без неё проверка
// «свёрнуто/развёрнуто читается без CSS» проверяла бы только aria-атрибут.
export const $analysisOutCaret = makeEl('span');
$analysisOutCaret.className = 'tg-analysis-out-caret';
$analysisOutCaret.textContent = '▾';
$analysisOutToggle.appendChild($analysisOutCaret);
// Начальные состояния — те же, что стоят атрибутами в index.html.
$analysis.hidden = true;
$analysisPaneCtx.hidden = true;
$analysisCancel.hidden = true;
// REFRESH выключен в РАЗМЕТКЕ: сохранённого разбора при первом открытии попапа
// нет, и кнопка включается только кодом после успешного GET.
$analysisRefresh.disabled = true;
$analysisError.hidden = true;
$analysisDepth.value = '200';
$analysis.appendChild($analysisPaneRun);
$analysis.appendChild($analysisPaneCtx);
$panel.appendChild($analysis);

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
// Фрагмент нужен рендереру Markdown (js/messenger/markdown.js): он собирает
// дерево ОТДЕЛЬНО и вставляет одной операцией. `nodeType: 11` — тот признак, по
// которому appendChild выше растворяет фрагмент вместо вставки его самого.
$document.createDocumentFragment = () => {
    const frag = makeEl('#fragment');
    frag.nodeType = 11;
    return frag;
};
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
// Кадр анимации. В Node его нет, а ядро попапа сливает накопленные дельты
// именно им — без стаба модуль ушёл бы в резервную ветку с таймером, и тест
// проверял бы не тот код, что выполняется в браузере. Кадр изображается
// макрозадачей, поэтому его дожидается тот же `flush()`.
globalThis.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 0);
globalThis.cancelAnimationFrame = (id) => clearTimeout(id);

/** Настройки, которые видит панель через публичный контракт. */
export const userSettings = {};
globalThis.window = {
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    fetch: (...args) => globalThis.fetch(...args),
    setTimeout, clearTimeout,
    ForgeApp: { state: { userSettings } },
};

// ---------------------------------------------------------------------------
// Сеть
// ---------------------------------------------------------------------------
//
// Тесты панели в сеть НЕ ходят: `fetch` подменён журналирующей заглушкой, а
// сценарий задаёт ответы через `setNet`. По умолчанию запрос ПАДАЕТ — ровно
// так же, как до появления этой секции, чтобы тесты колонки (они fetch не
// трогают вовсе) не начали молча получать «успешные» ответы.

/** Ответ в том виде, в каком его читает js/telegram/api.js::call. */
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
let netHandler = () => Promise.reject(new Error('offline'));

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

/** Даёт микрозадачам (промисам модулей) доехать. */
export const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
