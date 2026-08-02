// Краевые сценарии автопрокрутки ленты Telegram («следование» за кнопкой бота).
//
// Соседний telegram_scroll_ui.test.mjs проверяет ОСНОВНОЙ путь фичи; здесь
// собраны углы, в которых она обязана либо сработать, либо честно уступить
// прежнему поведению:
//   * рост сообщения без всякого нажатия — только прилипание к низу;
//   * нажатие из середины истории — уезжаем за результатом СВОЕГО нажатия;
//   * дребезг прокрутки против настоящего ухода вверх;
//   * ответ бота через 20 секунд — окно давно истекло, поведение прежнее;
//   * две кнопки в разных сообщениях подряд;
//   * правка в НЕактивном диалоге, в том числе с совпавшим message_id;
//   * сообщение стало КОРОЧЕ (кнопки исчезли) — ленту не выкидывает;
//   * подгрузка истории вверх — якорь чтения не сбивается доскроллом;
//   * окружение без ResizeObserver и prefers-reduced-motion;
//   * жизненный цикл наблюдателя: смена диалога, выпадение строки, teardown.
//
// Почему СВОЙ процесс и свой фейковый DOM (а не дописка в соседние файлы):
// `core/dom.js` резолвит все ref'ы ОДИН раз на верхнем уровне модуля, поэтому
// стабы обязаны стоять до первого импорта, а второй набор стабов в тот же
// процесс не вписать (`?bust=` создал бы второй экземпляр state.js — состояние
// на одном модуле, ассерты на другом, и тест позеленел бы по случайности).
//
// Запуск: node tmux-web/tests/frontend/telegram_scroll_edge.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM
// ---------------------------------------------------------------------------

function matchesSel(node, sel) {
    if (!node || !node.tagName) return false;
    const m = String(sel).trim().match(/^([a-zA-Z]*)((?:\.[-\w]+)*)(?:\[([-\w]+)\])?$/);
    if (!m) return false;
    const [, tag, classes, attr] = m;
    if (tag && node.tagName !== tag.toLowerCase()) return false;
    for (const c of (classes || '').split('.').filter(Boolean)) {
        if (!node.classList.contains(c)) return false;
    }
    if (attr && !(attr in node.attributes)) return false;
    return true;
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

/**
 * Какие строки «не помещаются в экран».
 *
 * Раскладки в Node нет: `scrollHeight` — обычное поле и от appendChild не
 * растёт. Поэтому рост моделируется ПОСТРОЧНО, отчётом `getBoundingClientRect`
 * — ровно тем, что читает chat.js::rowOffset. Множество, а не общий флаг:
 * краевые сценарии требуют, чтобы одно сообщение выросло, а соседнее осталось
 * коротким (и чтобы выросшее потом СЖАЛОСЬ обратно).
 */
const tallRows = new Set();

const RECT_SMALL = { top: 0, left: 0, width: 100, height: 20, bottom: 20, right: 100 };
const RECT_TALL = { top: 0, left: 0, width: 100, height: 5000, bottom: 5000, right: 100 };

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
        placeholder: '',
        tabIndex: -1,
        disabled: false,
        scrollTop: 0,
        scrollHeight: 1000,
        clientHeight: 400,
        focusCount: 0,
        /** Журнал вызовов scrollTo: [{top, behavior}]. */
        scrollCalls: [],
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
    node.replaceWith = (next) => {
        const p = node.parentNode;
        if (!p) return;
        const i = p.childNodes.indexOf(node);
        detach(next);
        next.parentNode = p;
        p.childNodes.splice(i, 1, next);
        node.parentNode = null;
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
    node.focus = () => { node.focusCount += 1; };
    node.blur = () => {};
    node.click = () => dispatch(node, 'click', { target: node });
    node.scrollIntoView = () => {};
    // Настоящий scrollTo: пишет в журнал И двигает ленту (в браузере smooth
    // доезжает не сразу, но проверяем мы адрес и режим, а не анимацию).
    node.scrollTo = (opts) => {
        node.scrollCalls.push(opts);
        if (opts && Number.isFinite(opts.top)) node.scrollTop = opts.top;
    };
    node.getBoundingClientRect = () => {
        const id = node.dataset ? node.dataset.msgId : null;
        return (id && tallRows.has(String(id))) ? RECT_TALL : RECT_SMALL;
    };

    return node;
}

function dispatch(node, type, ev) {
    const e = Object.assign({
        type,
        target: node,
        defaultPrevented: false,
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

const $body = makeEl('body');
const $panel = el('telegram');
const $log = el('tg-log');
const $composer = makeEl('div');
const $prompt = el('tg-prompt', 'form');
const $input = el('tg-input', 'input');
el('tg-send', 'button');
el('tg-session-head');
el('tg-attach', 'button');
el('tg-file-input', 'input');
el('tg-dropzone');
const $lightbox = el('tg-lightbox');
el('tg-lightbox-img', 'img');
el('tg-lightbox-close', 'button');
el('tg-lightbox-caption');

$lightbox.hidden = true;
$panel.appendChild($log);
$panel.appendChild($composer);
$composer.appendChild($prompt);
$prompt.appendChild($input);
$body.appendChild($panel);
const $spacer = makeEl('div');
$spacer.className = 'tg-log-spacer';
$log.appendChild($spacer);

const $document = makeEl('#document');
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

/** Системная настройка «меньше движения» — переключается прямо между сценариями. */
let reducedMotion = false;

/** Все созданные наблюдатели: по длине видно, не плодим ли мы их. */
const ros = [];
class SpyResizeObserver {
    constructor(cb) {
        this.cb = cb;
        this.observed = [];
        this.disconnected = false;
        ros.push(this);
    }
    observe(node) { if (!this.observed.includes(node)) this.observed.push(node); }
    unobserve(node) { this.observed = this.observed.filter((x) => x !== node); }
    disconnect() { this.disconnected = true; this.observed = []; }
    /** Смоделировать «строка досчиталась в высоте». */
    fire(node) { this.cb([{ target: node }], this); }
}
globalThis.ResizeObserver = SpyResizeObserver;

// ---------------------------------------------------------------------------
// Сеть
// ---------------------------------------------------------------------------

function jsonRes(status, obj) {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: { get: (h) => (String(h).toLowerCase() === 'content-type' ? 'application/json' : null) },
        json: async () => obj,
        text: async () => JSON.stringify(obj),
    };
}

const netCalls = [];
let netHandler = () => Promise.resolve(jsonRes(200, {}));

globalThis.fetch = (url, init) => {
    let body = null;
    try { body = init && init.body ? JSON.parse(init.body) : null; } catch (_) { body = init.body; }
    netCalls.push({ url: String(url), method: (init && init.method) || 'GET', body });
    return netHandler(String(url), init);
};
/** Журнал внешних переходов: по нему видно, что url-кнопка реально сработала. */
const opened = [];
globalThis.window = {
    fetch: globalThis.fetch,
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    // Без заглушки клик по url-кнопке роняет весь файл на TypeError, и сценарий
    // «ссылка следование не включает» проверить нечем.
    open(url) { opened.push(String(url)); return null; },
    matchMedia: (q) => ({
        matches: /prefers-reduced-motion/.test(String(q)) && reducedMotion,
        addEventListener() {}, removeEventListener() {},
    }),
    setTimeout, clearTimeout,
};
globalThis.Headers = class Headers {
    constructor(init) { this.map = new Map(Object.entries(init || {})); }
    has(k) { return this.map.has(k); }
    set(k, v) { this.map.set(k, v); }
    get(k) { return this.map.get(k) || null; }
};
globalThis.XMLHttpRequest = class XMLHttpRequest {
    constructor() { this.upload = {}; this.status = 0; this.responseText = ''; }
    open() {} setRequestHeader() {} send() {} abort() {}
};

// ---------------------------------------------------------------------------
// Модули
// ---------------------------------------------------------------------------

const base = new URL('../../static/js/telegram/', import.meta.url).href;
const chat = await import(`${base}chat.js`);
const follow = await import(`${base}follow.js`);
const { tgState } = await import(`${base}state.js`);

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

const tick = () => new Promise((r) => setImmediate(r));

/**
 * Потолок прокрутки ленты: `scrollHeight - clientHeight`.
 *
 * Именно сюда приезжает доскролл следования — в отличие от старого
 * `scrollToBottom()`, который пишет в scrollTop сам `scrollHeight` (в браузере
 * это одно и то же место, потому что значение клампится; в стенде — два разных
 * числа, и по ним видно, КАКОЙ веткой поехала лента).
 */
const MAX_TOP = 1000 - 400;
/** Значение scrollTop, при котором `isAtBottom` ещё ложно (порог 40 в format.js). */
const READING = 120;
/** Значение scrollTop, при котором `isAtBottom` истинно. */
const AT_BOTTOM = 600;

const dialog = { id: '777000', title: 'ShopBot', kind: 'user' };
const other = { id: '42', title: 'Sofia', kind: 'user' };

const plain = {
    id: 3, dialog_id: '777000', out: false, date: 1700000000,
    text: 'привет', sender: { id: '5001', username: 'shopbot' }, media: null,
};

/** Сообщение бота с инлайн-клавиатурой — источник всей фичи. */
function botMsg(id, over) {
    return Object.assign({
        id, dialog_id: '777000', out: false, date: 1700000000 + id,
        text: 'выберите раздел', sender: { id: '5001', username: 'shopbot' }, media: null,
        reply_markup: { kind: 'inline', rows: [[{ text: 'каталог', kind: 'callback', data: `c${id}` }]] },
    }, over || {});
}

const rowOf = (id) => $log.children.find((c) => c.dataset.msgId === String(id)) || null;
const kbBtnOf = (id) => {
    const row = rowOf(id);
    return row ? row.querySelector('.tg-kb-btn') : null;
};

/** Нажать инлайн-кнопку и дать callback'у долететь до сети. */
async function pressButton(id) {
    kbBtnOf(id).click();
    await tick();
    await tick();
}

/**
 * Поставить ленту в заданную позицию ТАК ЖЕ, как это делает браузер.
 *
 * Присваивание scrollTop в браузере порождает событие `scroll`, и от него
 * зависит `local.lastScrollTop` — база, по которой считается «пользователь ушёл
 * вверх». Без события тест мерил бы направление от устаревшего значения.
 */
function scrollTo(top) {
    $log.scrollTop = top;
    dispatch($log, 'scroll', {});
}

/**
 * Отлистать в середину истории и НАЖАТЬ кнопку — свежий интент пользователя.
 *
 * Порядок именно такой: доскролл следования уезжает вниз, и любое последующее
 * `scrollTo(READING)` — это уже уход вверх сверх порога, то есть ОТМЕНА
 * следования (её проверяет секция 3). Сценарии, которым нужно живое окно,
 * обязаны нажимать кнопку после того, как встали в позицию чтения.
 */
async function pressFromReading(id) {
    scrollTo(READING);
    await pressButton(id);
    resetScrollLog();
}

/** Забыть журнал прокруток перед очередным замером. */
function resetScrollLog() {
    $log.scrollCalls.length = 0;
}

/**
 * Последний вызов scrollTo (или null).
 *
 * Через хелпер, а не `scrollCalls[0].behavior` напрямую: когда доскролла не
 * случилось, обращение к полю несуществующего элемента роняет весь файл на
 * TypeError — и вместо внятного `[FAIL]` с меткой мы получаем стек, по которому
 * непонятно, какой из двадцати сценариев сломался.
 */
const lastCall = () => $log.scrollCalls[$log.scrollCalls.length - 1] || null;
const lastBehavior = () => (lastCall() ? lastCall().behavior : null);

/**
 * Живой наблюдатель размера.
 *
 * Наблюдатель у вкладки один, но `teardownChat` распускает его насовсем, и
 * следующее нажатие заводит СЛЕДУЮЩИЙ. Ссылаться на `ros[0]` после teardown —
 * значит проверять распущенный объект и не заметить, что фича замолчала.
 */
const liveRo = () => ros[ros.length - 1] || { observed: [], disconnected: false, fire() {} };

const state = () => chat._debugChatState();

netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });

tgState.dialogs = [dialog, other];
tgState.activeDialogId = '777000';
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.initChat({});
chat.renderHistory([plain, botMsg(4)], dialog);

eq(state().rendered, 2, 'стенд: две строки нарисованы');
ok(!!kbBtnOf(4), 'стенд: инлайн-кнопка бота на месте');

// ---------------------------------------------------------------------------
// 1. Рост БЕЗ нажатия: работает только прежнее прилипание к низу
// ---------------------------------------------------------------------------

// Сообщение выросло, пользователь был у нижней кромки ⇒ доскролл (прежний путь).
tallRows.add('4');
scrollTo(AT_BOTTOM);
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'бот дописал длинный ответ', edit_date: 1700000200 }));
eq($log.scrollTop, 1000, 'внизу без нажатия: выросшая правка тянет ленту прежним прилипанием');
eq($log.scrollCalls.length, 0,
    'внизу без нажатия: прилипание едет присваиванием scrollTop, как и раньше — scrollTo не зовём');
eq(state().followActive, false, 'внизу без нажатия: следование не включалось');
eq(ros.length, 0, 'внизу без нажатия: наблюдатель размера не создан');

// Тот же рост, но пользователь читает историю выше и кнопку не жал.
scrollTo(READING);
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'бот дописал ещё абзац сверх того', edit_date: 1700000300 }));
eq($log.scrollTop, READING, 'читает историю: чужая правка ленту не дёргает');
eq($log.scrollCalls.length, 0, 'читает историю: прокрутки не было вовсе');
ok(rowOf(4).textContent.includes('ещё абзац сверх того'),
    'читает историю: сама правка при этом применена — мы гасим прокрутку, а не апдейт');
eq(rowOf(4).querySelector('.tg-line-suffix').textContent, ' (edited)',
    'читает историю: пометка «(edited)» на месте');
eq(ros.length, 0, 'читает историю: наблюдателя по-прежнему нет');

// ---------------------------------------------------------------------------
// 2. Отлистал вверх, но САМ нажал кнопку ⇒ уезжаем за результатом
// ---------------------------------------------------------------------------

scrollTo(READING);
await pressButton(4);
eq(state().followActive, true, 'нажатие из середины истории: следование включилось');
eq(state().followMessageId, 4, 'нажатие: цель — нажатое сообщение');
eq($log.scrollTop, READING, 'нажатие: само по себе ленту не двигает — ждём ответ бота');

resetScrollLog();
chat.onEdited(botMsg(4, {
    text: 'каталог:\n1. молоко\n2. хлеб\n3. соль',
    edit_date: 1700000400,
    reply_markup: { kind: 'inline', rows: [[{ text: 'назад', kind: 'callback', data: 'b' }]] },
}));
eq($log.scrollTop, MAX_TOP, 'нажатие: результат своего нажатия догнал пользователя');
eq($log.scrollCalls.length, 1, 'нажатие: доскролл ровно один');
eq(lastBehavior(), 'smooth', 'нажатие: по умолчанию плавно');
eq(lastCall() && lastCall().top, MAX_TOP, 'нажатие: адрес — конец блока, клампится потолком прокрутки');
eq(ros.length, 1, 'нажатие: наблюдатель создан ровно один');
ok(ros[0].observed[0] === rowOf(4), 'нажатие: наблюдается пересобранная строка');

// ---------------------------------------------------------------------------
// 3. Дребезг против настоящего ухода вверх
// ---------------------------------------------------------------------------

// Отскок на единицы пикселей (инерция, подпрыгнувшая полоса клавиатуры) —
// не повод гасить следование.
scrollTo(MAX_TOP - (follow.FOLLOW_CANCEL_UP_PX - 4));
eq(state().followActive, true, 'дребезг: отскок под порогом следование не гасит');
scrollTo(MAX_TOP - follow.FOLLOW_CANCEL_UP_PX - 4);
eq(state().followActive, true, 'дребезг: и второй мелкий отскок от НОВОЙ базы — тоже не гасит');

// А вот честный уход вверх — гасит сразу.
scrollTo(READING);
eq(state().followActive, false, 'уход вверх: пользователь передумал ждать — следование снято');
eq(state().followObserving, false, 'уход вверх: наблюдение снято вместе со следованием');
eq(ros[0].observed.length, 0, 'уход вверх: наблюдатель отпустил строку');

resetScrollLog();
chat.onEdited(botMsg(4, { text: 'запоздалый ответ бота', edit_date: 1700000500 }));
eq($log.scrollTop, READING, 'уход вверх: пришедшая после отмены правка ленту не двигает');
eq($log.scrollCalls.length, 0, 'уход вверх: scrollTo не звался');

// Прежнее поведение при этом цело: вернулся вниз — прилипание работает.
scrollTo(AT_BOTTOM);
chat.onEdited(botMsg(4, { text: 'и ещё правка', edit_date: 1700000600 }));
eq($log.scrollTop, 1000, 'после отмены: прилипание к низу продолжает работать');

// ---------------------------------------------------------------------------
// 4. Бот ответил НОВЫМ сообщением вместо правки
// ---------------------------------------------------------------------------

scrollTo(READING);
await pressButton(4);
tallRows.add('30');
resetScrollLog();
chat.onNewMessage({
    id: 30, dialog_id: '777000', out: false, date: 1700000700,
    text: 'вот ваш заказ', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, MAX_TOP, 'новое сообщение: ответ бота на нажатие доезжает сам');
ok(ros[0].observed[0] === rowOf(30), 'новое сообщение: наблюдение переехало на него');

// Окно ещё живо — второе сообщение подряд (бот прислал текст и следом файл).
tallRows.add('31');
scrollTo(READING);
chat.onNewMessage({
    id: 31, dialog_id: '777000', out: false, date: 1700000800,
    text: 'и накладная', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, MAX_TOP, 'новое сообщение: второе подряд тоже доезжает — окно ещё открыто');

// Соседний чат ленту дёргать не имеет права даже при живом следовании.
scrollTo(READING);
chat.onNewMessage({ id: 32, dialog_id: '42', out: false, date: 1700000900, text: 'привет', media: null });
eq($log.scrollTop, READING, 'адресность: сообщение соседнего чата ленту не двигает');

// ---------------------------------------------------------------------------
// 5. Бот ответил через 20 секунд — окно давно истекло
// ---------------------------------------------------------------------------

// Отматываем регистрацию на 20с назад: ждать реальные 12с в тесте нечем и незачем.
follow.noteFollow('777000', 4, Date.now() - 20000);
eq(state().followActive, false, 'через 20с: окно следования истекло');

scrollTo(READING);
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'бот проснулся и ответил', edit_date: 1700001000 }));
eq($log.scrollTop, READING, 'через 20с: правка при чтении истории ленту не двигает');
eq($log.scrollCalls.length, 0, 'через 20с: scrollTo не звался');

scrollTo(READING);
chat.onNewMessage({
    id: 33, dialog_id: '777000', out: false, date: 1700001100,
    text: 'поздний ответ', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, READING, 'через 20с: новое сообщение тоже не дёргает ленту');

// Прежнее поведение вернулось ЦЕЛИКОМ, а не «выключилось».
scrollTo(AT_BOTTOM);
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'а вот и ответ', edit_date: 1700001200 }));
eq($log.scrollTop, 1000, 'через 20с: у нижней кромки прилипание работает как до фичи');
eq($log.scrollCalls.length, 0, 'через 20с: и едет прежним присваиванием scrollTop');

// Наблюдатель мог пережить истечение на кадр — он обязан сняться сам.
ros[0].fire(rowOf(4));
eq(ros[0].observed.length, 0, 'через 20с: сработавший после истечения наблюдатель снимает себя');
eq(state().followObserving, false, 'через 20с: строка отпущена');

// ---------------------------------------------------------------------------
// 6. Две кнопки в разных сообщениях нажаты подряд
// ---------------------------------------------------------------------------

tallRows.clear();
const pair = [plain, botMsg(4), botMsg(8)];
tgState.messages.set('777000', pair.slice());
chat.renderHistory(pair, dialog);

// Позицию чтения выставляем ДО нажатий: прокрутка вверх после нажатия — это
// уже отмена следования (её проверяет секция 3).
scrollTo(READING);
await pressButton(4);
eq(state().followMessageId, 4, 'две кнопки: первое нажатие целится в своё сообщение');
await pressButton(8);
eq(state().followMessageId, 8, 'две кнопки: второе нажатие перецелило следование');
eq(ros.length, 1, 'две кнопки: наблюдатель по-прежнему один на вкладку');
eq(ros[0].observed.length, 1, 'две кнопки: наблюдается ровно одна строка');
ok(ros[0].observed[0] === rowOf(8), 'две кнопки: наблюдается последняя нажатая');

// Правка ПЕРВОГО сообщения ленту уже не тянет: актуален последний интент.
tallRows.add('4');
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'старое сообщение дописалось', edit_date: 1700001300 }));
eq($log.scrollTop, READING, 'две кнопки: правка прошлой цели ленту не двигает');
eq($log.scrollCalls.length, 0, 'две кнопки: прокрутки не было');

// А правка второго — тянет.
tallRows.add('8');
chat.onEdited(botMsg(8, { text: 'вот результат второго нажатия', edit_date: 1700001400 }));
eq($log.scrollTop, MAX_TOP, 'две кнопки: правка актуальной цели доезжает');

// Собственный доскролл в браузере тоже порождает событие `scroll` — и не имеет
// права сойти за «пользователь ушёл читать историю».
dispatch($log, 'scroll', {});
eq(state().followActive, true, 'две кнопки: свой же доскролл вниз следование не гасит');

// Новое сообщение в этом же диалоге доезжает при любой цели: следование за
// сообщением всегда подразумевает следование за его чатом.
scrollTo(READING);
await pressButton(4);
tallRows.add('34');
resetScrollLog();
chat.onNewMessage({
    id: 34, dialog_id: '777000', out: false, date: 1700001500,
    text: 'ответ на вторую кнопку', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, MAX_TOP, 'две кнопки: новое сообщение диалога доезжает независимо от цели');

// ---------------------------------------------------------------------------
// 7. Правка в НЕактивном диалоге
// ---------------------------------------------------------------------------

// Сообщения соседнего чата в ленте нет вовсе — трогать нечего.
scrollTo(READING);
resetScrollLog();
chat.onEdited({
    id: 99, dialog_id: '42', out: false, date: 1700001600, media: null, reply_markup: null,
    text: 'правка в соседнем чате', edit_date: 1700001700,
});
eq($log.scrollTop, READING, 'чужой диалог: правка неизвестного сообщения ленту не двигает');
eq($log.scrollCalls.length, 0, 'чужой диалог: scrollTo не звался');

// Совпадение message_id между чатами — обычное дело: в каналах нумерация своя
// и начинается с единицы. Строка ищется по одному лишь id, поэтому чужая
// правка не имеет права ни двигать ленту, ни подменять её текст.
const textBefore = rowOf(8).textContent;
tallRows.add('8');
scrollTo(READING);
resetScrollLog();
chat.onEdited({
    id: 8, dialog_id: '42', out: false, date: 1700001800, media: null, reply_markup: null,
    text: 'ЧУЖОЙ ТЕКСТ ИЗ СОСЕДНЕГО ЧАТА', edit_date: 1700001900,
});
eq($log.scrollTop, READING, 'совпадение id: правка соседнего чата ленту не двигает');
eq($log.scrollCalls.length, 0, 'совпадение id: прокрутки не было');
eq(rowOf(8).textContent, textBefore,
    'совпадение id: строка открытого чата не подменена текстом из соседнего');

// Даже когда пользователь у нижней кромки: прилипание — про СВОЙ диалог.
scrollTo(AT_BOTTOM);
chat.onEdited({
    id: 8, dialog_id: '42', out: false, date: 1700001800, media: null, reply_markup: null,
    text: 'и ещё раз чужой текст', edit_date: 1700002000,
});
eq($log.scrollTop, AT_BOTTOM, 'совпадение id: у нижней кромки чужая правка тоже ничего не делает');

// ---------------------------------------------------------------------------
// 8. Сообщение стало КОРОЧЕ (кнопки исчезли)
// ---------------------------------------------------------------------------

scrollTo(READING);
await pressButton(8);
// Бот убрал клавиатуру и укоротил текст — строка помещается целиком.
tallRows.delete('8');
resetScrollLog();
chat.onEdited({
    id: 8, dialog_id: '777000', out: false, date: 1700001008, media: null,
    sender: { id: '5001', username: 'shopbot' },
    text: 'готово', edit_date: 1700002100, reply_markup: null,
});
eq($log.scrollTop, READING, 'укоротилось: ленту не выкидывает — догонять нечего');
eq($log.scrollCalls.length, 0, 'укоротилось: прокрутки не было');
ok(!kbBtnOf(8), 'укоротилось: клавиатура действительно исчезла (правка применена)');

// И у нижней кромки укорачивание тоже не швыряет ленту вверх.
scrollTo(MAX_TOP);
resetScrollLog();
chat.onEdited({
    id: 8, dialog_id: '777000', out: false, date: 1700001008, media: null,
    sender: { id: '5001', username: 'shopbot' },
    text: 'ок', edit_date: 1700002200, reply_markup: null,
});
eq($log.scrollTop, MAX_TOP, 'укоротилось у кромки: лента осталась на месте');
eq($log.scrollCalls.length, 0, 'укоротилось у кромки: следование вверх не тянет');

// Наблюдатель на сжавшейся строке тоже молчит.
resetScrollLog();
ros[0].fire(rowOf(8));
eq($log.scrollTop, MAX_TOP, 'укоротилось: наблюдатель размера ленту не трогает');
eq($log.scrollCalls.length, 0, 'укоротилось: лишних scrollTo нет');

chat.teardownChat();

// ---------------------------------------------------------------------------
// 9. Подгрузка истории вверх: якорь чтения не сбивается доскроллом
// ---------------------------------------------------------------------------

const page = [plain, botMsg(4)];
netHandler = async () => jsonRes(200, { messages: page, has_more: true });
await chat.openDialog('777000', dialog);
eq(state().hasMore, true, 'история: сервер обещал ещё страницу');

netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });
tallRows.add('4');
// Позиция ОБЯЗАНА быть ниже потолка прокрутки, иначе ассерт «лента не
// сдвинулась» вырождается: у самого низа доскроллу и так двигаться некуда, и
// пропавший гард `local.loading` тест бы не заметил. И одновременно ниже порога
// подгрузки (200 в format.js::shouldLoadOlder) — иначе событие scroll само
// запустило бы вторую loadOlder поверх нашей.
const READING_MID = 250;
ok(READING_MID > 200 && READING_MID < MAX_TOP,
    'история: позиция чтения одновременно вне порога подгрузки и вне потолка прокрутки');
scrollTo(READING_MID);
await pressButton(4);
eq(state().loading, false, 'история: одно лишь вставание в позицию чтения подгрузку не запускает');

// Страница истории «зависает» в полёте — ровно окно, внутри которого loadOlder
// держит якорь между замером scrollHeight и восстановлением scrollTop.
let releaseOlder = null;
const olderPage = new Promise((r) => { releaseOlder = r; });
netHandler = (url) => (
    url.includes('before_id')
        ? olderPage
        : Promise.resolve(jsonRes(200, { message: null, alert: false, url: null }))
);
const loading = chat.loadOlder(dialog);
eq(state().loading, true, 'история: страница вверх в полёте');

const topBeforeLoad = $log.scrollTop;
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'бот ответил ровно во время подгрузки', edit_date: 1700002300 }));
eq($log.scrollTop, topBeforeLoad, 'история: доскролл следования не вмешивается в подгрузку');
eq($log.scrollCalls.length, 0, 'история: scrollTo во время подгрузки не звался');
liveRo().fire(rowOf(4));
eq($log.scrollTop, topBeforeLoad, 'история: наблюдатель размера тоже молчит, пока держится якорь');
eq($log.scrollCalls.length, 0, 'история: и наблюдатель scrollTo не звал');

releaseOlder(jsonRes(200, {
    messages: [{
        id: 1, dialog_id: '777000', out: false, date: 1699990000, text: 'самое старое', media: null,
        sender: { id: '5001', username: 'shopbot' },
    }],
    has_more: false,
}));
await loading;
eq($log.scrollTop, topBeforeLoad, 'история: якорь чтения восстановлен ровно там, где был');
ok(!!rowOf(1), 'история: старая страница дорисована');
ok(liveRo().observed[0] === rowOf(4),
    'история: наблюдение переехало на пересозданную строку, мёртвый узел не держим');
eq(state().followActive, true, 'история: подгрузка вверх само следование не отменяет');

// ---------------------------------------------------------------------------
// 10. prefers-reduced-motion
// ---------------------------------------------------------------------------

// Переключение МЕЖДУ вызовами ловит «прочитали медиазапрос один раз на импорте»:
// если бы медиазапрос читался при загрузке модуля, второй замер повторил бы
// первый и тест этого не заметил.
reducedMotion = true;
await pressFromReading(4);
chat.onEdited(botMsg(4, { text: 'ответ при включённом reduce', edit_date: 1700002400 }));
eq($log.scrollCalls.length, 1, 'reduce: доскролл ровно один');
eq(lastBehavior(), 'auto', 'reduce: прокрутка мгновенная');
eq($log.scrollTop, MAX_TOP, 'reduce: сам доскролл при этом не отменяется');

await pressFromReading(4);
liveRo().fire(rowOf(4));
eq(lastBehavior(), 'auto', 'reduce: наблюдатель размера тоже едет мгновенно');
eq($log.scrollTop, MAX_TOP, 'reduce: адрес доскролла от режима движения не зависит');

reducedMotion = false;
await pressFromReading(4);
liveRo().fire(rowOf(4));
eq(lastBehavior(), 'smooth', 'reduce: снятие настройки возвращает плавность');

// ---------------------------------------------------------------------------
// 11. Кнопка reply-клавиатуры (полоса над полем ввода)
// ---------------------------------------------------------------------------

chat.teardownChat();
tallRows.clear();
const kbMsg = {
    id: 50, dialog_id: '777000', out: false, date: 1700003000, media: null,
    sender: { id: '5001', username: 'shopbot' }, text: 'чем помочь?',
    reply_markup: {
        kind: 'keyboard',
        rows: [[{ text: 'меню', kind: 'text' }]],
        placeholder: '',
        single_use: false,
    },
};
tgState.messages.set('777000', [kbMsg]);
chat.renderHistory([kbMsg], dialog);
// Именно из контейнера рядов: первый `.tg-kb-btn` полосы — это её крестик
// «свернуть», и клик по нему проверял бы совсем другую механику.
const barRows = $composer.querySelector('.tg-kb-bar-rows');
const barBtn = barRows ? barRows.querySelector('.tg-kb-btn') : null;
ok(!!barBtn, 'reply-клавиатура: полоса с кнопкой нарисована над полем ввода');
eq(barBtn && barBtn.textContent, 'меню', 'reply-клавиатура: это кнопка бота, а не крестик полосы');

netHandler = async (url) => (
    url.includes('/messages')
        ? jsonRes(200, {
            message: {
                id: 51, dialog_id: '777000', out: true, date: 1700003100,
                text: 'меню', media: null, sender: null,
            },
        })
        : jsonRes(200, {})
);
scrollTo(READING);
barBtn.click();
await tick();
await tick();
eq(state().followActive, true, 'reply-клавиатура: нажатие тоже включает следование');
eq(state().followDialogId, '777000', 'reply-клавиатура: следование адресовано открытому чату');
eq(state().followMessageId, 50,
    'reply-клавиатура: цель — сообщение, принёсшее клавиатуру (ответ придёт новым сообщением)');

tallRows.add('52');
scrollTo(READING);
chat.onNewMessage({
    id: 52, dialog_id: '777000', out: false, date: 1700003200,
    text: 'вот меню:\nа\nб\nв', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, MAX_TOP, 'reply-клавиатура: ответ бота на кнопку полосы доезжает сам');

// ---------------------------------------------------------------------------
// 12. Жизненный цикл наблюдателя: утечек нет
// ---------------------------------------------------------------------------

// Следуемое сообщение выпало из ленты (пользователь ушёл по цитате далеко
// вверх, и страница с ним уехала) — держать detached-узел нельзя.
tallRows.clear();
const withoutTarget = [plain, botMsg(4)];
tgState.messages.set('777000', withoutTarget.slice());
chat.renderHistory(withoutTarget, dialog);
ok(!rowOf(50), 'перерисовка: цели следования в ленте действительно больше нет');
eq(state().followActive, true,
    'перерисовка: само следование живо — окно не истекло, а цель лишь уехала из ленты');
eq(state().followObserving, false, 'перерисовка: выпавшую из ленты цель наблюдать нечего');
eq(liveRo().observed.length, 0, 'перерисовка: наблюдатель отпустил мёртвый узел');

// Отдельный угол: окно истекло ПО ЧАСАМ, а таймер снятия ещё не выстрелил.
// Тогда renderHistory наблюдение уже не перевешивает (цели-то нет), и
// единственный, кто отпускает мёртвый узел, — clearLog. Без него detached-строка
// осталась бы под наблюдением до следующего нажатия, то есть неопределённо долго.
netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);
await pressButton(4);
eq(state().followObserving, true, 'истёкшее окно: до истечения наблюдение стоит');
follow.noteFollow('777000', 4, Date.now() - 20000);
eq(state().followActive, false, 'истёкшее окно: по часам следования уже нет');
chat.renderHistory([plain, botMsg(4)], dialog);
eq(state().followObserving, false, 'истёкшее окно: перерисовка отпустила мёртвый узел');
eq(liveRo().observed.length, 0, 'истёкшее окно: наблюдатель не держит выброшенную строку');

// Смена диалога снимает и следование, и наблюдение.
await pressButton(4);
eq(liveRo().observed.length, 1, 'смена диалога: перед уходом наблюдение стоит');
const roBeforeSwitch = ros.length;
netHandler = async () => jsonRes(200, { messages: [], has_more: false });
await chat.openDialog('42', other);
eq(state().followActive, false, 'смена диалога: следование покинутого чата снято');
eq(state().followObserving, false, 'смена диалога: наблюдение снято');
eq(liveRo().observed.length, 0, 'смена диалога: наблюдатель отпустил строку');
eq(liveRo().disconnected, false, 'смена диалога: наблюдатель не распущен — он переживает переходы');
eq(ros.length, roBeforeSwitch, 'смена диалога: новых наблюдателей не наплодили');

// Удаление следуемого сообщения — тот же класс утечки, только повод другой.
netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });
tgState.activeDialogId = '777000';
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);
await pressButton(4);
chat.onDeleted({ dialog_id: '777000', message_ids: [4] });
eq(state().followActive, false, 'удаление: следовать больше не за чем');
eq(state().followObserving, false, 'удаление: наблюдение снято с заглушки');
eq(liveRo().observed.length, 0, 'удаление: наблюдатель отпустил переиспользованный узел');
eq(ros.length, roBeforeSwitch, 'удаление: наблюдатель по-прежнему один на вкладку');

// То же удаление, но окно уже истекло по часам: `followsMessage` не сработает,
// и единственный, кто отпустит узел, переиспользованный под заглушку
// «<< message purged >>», — точечная сверка `followRow === row` в onDeleted.
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);
await pressButton(4);
follow.noteFollow('777000', 4, Date.now() - 20000);
eq(state().followActive, false, 'удаление после истечения: следования уже нет');
eq(state().followObserving, true, 'удаление после истечения: узел всё ещё под наблюдением');
chat.onDeleted({ dialog_id: '777000', message_ids: [4] });
eq(state().followObserving, false, 'удаление после истечения: заглушку наблюдать нечего');
eq(liveRo().observed.length, 0, 'удаление после истечения: наблюдатель отпустил узел');

// Возврат в чат бота и teardown вкладки.
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);
await pressButton(4);
const roBeforeTeardown = liveRo();
const roCountBeforeTeardown = ros.length;
chat.teardownChat();
eq(roBeforeTeardown.disconnected, true, 'teardown: наблюдатель распущен целиком');
eq(roBeforeTeardown.observed.length, 0, 'teardown: все узлы отпущены');
eq(state().followActive, false, 'teardown: следование снято');
eq(state().followObserving, false, 'teardown: строка отпущена');

// После teardown вкладка ещё может ожить (SPA-возврат) — фича обязана
// завести СВЕЖИЙ наблюдатель, а не молча замолчать на распущенном.
await pressButton(4);
eq(ros.length, roCountBeforeTeardown + 1, 'после teardown: заведён свежий наблюдатель');
eq(roBeforeTeardown.disconnected, true, 'после teardown: старый так и остался распущенным');
ok(liveRo() !== roBeforeTeardown, 'после teardown: работаем уже на новом наблюдателе');
eq(liveRo().observed.length, 1, 'после teardown: наблюдение работает на новом');
tallRows.add('4');
scrollTo(READING);
chat.onEdited(botMsg(4, { text: 'жизнь после teardown', edit_date: 1700002500 }));
eq($log.scrollTop, MAX_TOP, 'после teardown: доскролл снова работает');

// ---------------------------------------------------------------------------
// 13. Окружение без ResizeObserver
// ---------------------------------------------------------------------------

chat.teardownChat();
const savedRO = globalThis.ResizeObserver;
const roCountNoRo = ros.length;
delete globalThis.ResizeObserver;

tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);
scrollTo(READING);
await pressButton(4);
eq(state().followActive, true, 'без ResizeObserver: следование всё равно включается');
eq(state().followObserving, true,
    'без ResizeObserver: цель всё равно запомнена — теряется только добор отложенного роста');
eq(ros.length, roCountNoRo, 'без ResizeObserver: новых наблюдателей не появилось');

resetScrollLog();
chat.onEdited(botMsg(4, { text: 'ответ бота без наблюдателя', edit_date: 1700002600 }));
eq($log.scrollTop, MAX_TOP, 'без ResizeObserver: доскролл в момент правки на месте');
eq(lastBehavior(), 'smooth', 'без ResizeObserver: режим прокрутки прежний');

// Доскролл уехал вниз, и вернуться в позицию чтения = отменить следование.
// Значит для второго сценария нужен свежий интент — новое нажатие.
tallRows.add('60');
await pressFromReading(4);
chat.onNewMessage({
    id: 60, dialog_id: '777000', out: false, date: 1700002700,
    text: 'и новым сообщением тоже', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, MAX_TOP, 'без ResizeObserver: новое сообщение тоже доезжает');

// Смена диалога и teardown в таком окружении обязаны отработать без исключений.
netHandler = async () => jsonRes(200, { messages: [], has_more: false });
await chat.openDialog('42', other);
eq(state().followActive, false, 'без ResizeObserver: смена диалога снимает следование');
chat.teardownChat();
eq(state().followObserving, false, 'без ResizeObserver: teardown отработал без наблюдателя');
globalThis.ResizeObserver = savedRO;

// Окружение без scrollTo (старый WebView) — фолбэк на присваивание scrollTop.
netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });
tgState.activeDialogId = '777000';
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);
scrollTo(READING);
await pressButton(4);
const savedScrollTo = $log.scrollTo;
delete $log.scrollTo;
chat.onEdited(botMsg(4, { text: 'ответ в древнем вебвью', edit_date: 1700002800 }));
eq($log.scrollTop, MAX_TOP, 'без scrollTo: доскролл идёт присваиванием scrollTop');
$log.scrollTo = savedScrollTo;

// ---------------------------------------------------------------------------
// 14. Кнопки, которые ленту трогать не должны
// ---------------------------------------------------------------------------

// `open-url` и `inline-query` не порождают ни правки сообщения, ни нового
// сообщения. Включать ради них следование — значит подарить пользователю
// самовольную прокрутку через несколько секунд после возврата из браузера.
chat.teardownChat();
tallRows.clear();
const linkMsg = {
    id: 70, dialog_id: '777000', out: false, date: 1700004000, media: null,
    sender: { id: '5001', username: 'shopbot' }, text: 'наш сайт',
    reply_markup: {
        kind: 'inline',
        rows: [[
            { text: 'открыть', kind: 'url', url: 'https://example.org/' },
            { text: 'искать', kind: 'switch_inline', query: 'коты' },
        ]],
    },
};
tgState.messages.set('777000', [linkMsg]);
chat.renderHistory([linkMsg], dialog);
const linkBtns = rowOf(70).querySelectorAll('.tg-kb-btn');
eq(linkBtns.length, 2, 'ссылки: обе кнопки нарисованы');

scrollTo(READING);
linkBtns[0].click();
eq(opened[opened.length - 1], 'https://example.org/', 'ссылки: переход по URL действительно случился');
eq(state().followActive, false, 'ссылки: переход по URL следование не включает');

linkBtns[1].click();
eq($input.value, 'коты', 'ссылки: inline-запрос подставлен в поле ввода');
eq(state().followActive, false, 'ссылки: подстановка inline-запроса следование не включает');

tallRows.add('70');
resetScrollLog();
chat.onEdited(Object.assign({}, linkMsg, {
    text: 'сайт переехал, вот длинный анонс на несколько строк', edit_date: 1700004100,
}));
eq($log.scrollTop, READING, 'ссылки: пришедшая следом правка ленту не двигает');
eq($log.scrollCalls.length, 0, 'ссылки: scrollTo не звался');
eq(ros.length, roCountNoRo + 1, 'ссылки: наблюдателя ради них не заводили');

// ---------------------------------------------------------------------------
// 15. Гонка «правка обогнала ответ на нажатие» и ошибка нажатия
// ---------------------------------------------------------------------------

tallRows.clear();
tgState.messages.set('777000', [plain, botMsg(4)]);
chat.renderHistory([plain, botMsg(4)], dialog);

// Ответ POST .../callback «зависает» в полёте. Правка бота приходит отдельным
// WS-кадром и вполне может опередить `.then` — и это самый частый порядок
// событий. Если бы следование регистрировалось в `.then`, фича не работала бы
// именно там, где она нужнее всего.
let releaseCallback = null;
netHandler = () => new Promise((r) => { releaseCallback = r; });
scrollTo(READING);
kbBtnOf(4).click();
await tick();
eq(state().followActive, true, 'гонка: следование включено ещё до ответа сервера');

tallRows.add('4');
resetScrollLog();
chat.onEdited(botMsg(4, { text: 'бот успел раньше своего же ответа', edit_date: 1700004200 }));
eq($log.scrollTop, MAX_TOP, 'гонка: правка, обогнавшая ответ, всё равно доезжает');
releaseCallback(jsonRes(200, { message: null, alert: false, url: null }));
await tick();
await tick();
eq(state().followActive, true, 'гонка: пришедший следом ответ сервера следование не гасит');

// Ошибка нажатия (flood_wait, оборванная сеть) — не повод бросать пользователя:
// бот вполне мог отредактировать сообщение до того, как ответ до нас не дошёл.
netHandler = async () => jsonRes(429, { code: 'flood_wait', message: 'слишком часто', retry_after: 3 });
await pressFromReading(4);
eq(state().followActive, true, 'ошибка нажатия: следование пережило неудачный callback');
chat.onEdited(botMsg(4, { text: 'но правку бот всё-таки прислал', edit_date: 1700004300 }));
eq($log.scrollTop, MAX_TOP, 'ошибка нажатия: правка после ошибки всё равно доезжает');

// ---------------------------------------------------------------------------
// 16. Наблюдатель, сработавший уже после отмены прокруткой
// ---------------------------------------------------------------------------

// ResizeObserver отвечает не мгновенно: между уходом пользователя вверх и
// доставкой замера проходит кадр, и запоздалый колбэк не имеет права вернуть
// ленту вниз — это выглядело бы как рывок «сама поехала».
netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });
await pressFromReading(4);
const roLive = liveRo();
eq(roLive.observed.length, 1, 'запоздалый замер: перед уходом строка под наблюдением');

scrollTo(READING - 100);
eq(state().followActive, false, 'запоздалый замер: уход вверх снял следование');
resetScrollLog();
roLive.fire(rowOf(4));
eq($log.scrollTop, READING - 100, 'запоздалый замер: лента осталась там, где её оставил пользователь');
eq($log.scrollCalls.length, 0, 'запоздалый замер: scrollTo не звался');
eq(roLive.observed.length, 0, 'запоздалый замер: наблюдатель отпустил строку сам');

// Регресс-щит: собственная отправка по-прежнему уезжает вниз БЕЗУСЛОВНО —
// следование не имело права подменить это правило.
netHandler = async (url) => (
    url.includes('/messages')
        ? jsonRes(200, {
            message: {
                id: 71, dialog_id: '777000', out: true, date: 1700004400,
                text: 'ок', media: null, sender: null,
            },
        })
        : jsonRes(200, {})
);
scrollTo(READING);
await chat.send('ок');
eq($log.scrollTop, 1000, 'своя отправка: лента по-прежнему безусловно уезжает в самый низ');
eq(state().followActive, false, 'своя отправка: следование она при этом не включает');

// ---------------------------------------------------------------------------
// 17. Полный сброс
// ---------------------------------------------------------------------------

chat.resetChat();
eq(state().followActive, false, 'сброс аккаунта: следование снято');
eq(state().followObserving, false, 'сброс аккаунта: наблюдение снято');
chat.teardownChat();

// Живой таймер следования (12с) подвесил бы этот процесс до срабатывания —
// то, что node завершается сразу, и есть доказательство его зачистки.

console.log(`\n${checks - failures}/${checks} ассертов прошло (frontend/telegram_scroll_edge)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
