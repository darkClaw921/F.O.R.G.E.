// Регресс-тест автопрокрутки ленты Telegram — UI-половина «следования».
//
// Жалоба, из-за которой всё это существует: «нажал кнопку бота → бот
// отредактировал своё сообщение, оно стало больше и уехало под нижнюю кромку,
// приходится листать руками». Чистая логика решения живёт в follow.js и покрыта
// соседним telegram_follow.test.mjs; здесь проверяется ровно то, что чистыми
// функциями не поймать: реальный доскролл в chat.js::onEdited/onNewMessage,
// жизненный цикл ResizeObserver и выбор режима прокрутки.
//
// Почему СВОЙ процесс и свой фейковый DOM (а не дописка в telegram_reply_ui):
// `core/dom.js` резолвит все ref'ы ОДИН раз на верхнем уровне модуля, поэтому
// стабы обязаны стоять до первого импорта, а второй набор стабов в тот же
// процесс не вписать (`?bust=` создал бы второй экземпляр state.js — состояние
// на одном модуле, ассерты на другом, и тест позеленел бы по случайности).
//
// Стенд — тот же срез DOM, что в telegram_reply_ui.test.mjs, плюс четыре вещи,
// без которых эту фичу не измерить:
//   * `scrollTo` + журнал `scrollCalls` — иначе не видно, каким режимом ехали;
//   * управляемый `window.matchMedia` — переключаемый prefers-reduced-motion;
//   * шпион `ResizeObserver` — observe/unobserve/disconnect под протокол;
//   * флаг `rowsOverflow` — реальной раскладки в Node нет, «строка выросла и её
//     конец уехал под кромку» моделируется отчётом getBoundingClientRect.
//
// Запуск: node tmux-web/tests/frontend/telegram_scroll_ui.test.mjs
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
 * «Строки ленты стали выше экрана».
 *
 * Раскладки в Node нет: `scrollHeight` — обычное поле и от appendChild не
 * растёт. Поэтому рост моделируется отчётом `getBoundingClientRect` строк —
 * ровно тем, что читает chat.js::rowOffset.
 */
let rowsOverflow = false;

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
    node.getBoundingClientRect = () => (
        rowsOverflow && node.classList.contains('tg-line') ? RECT_TALL : RECT_SMALL
    );

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

// ---------------------------------------------------------------------------
// prefers-reduced-motion — переключаемый на ходу
// ---------------------------------------------------------------------------

/** Системная настройка «меньше движения» — меняется прямо между сценариями. */
let reducedMotion = false;

// ---------------------------------------------------------------------------
// Шпион ResizeObserver
// ---------------------------------------------------------------------------

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
globalThis.window = {
    fetch: globalThis.fetch,
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
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
 * `scrollToBottom()`, который пишет в scrollTop сам `scrollHeight`.
 */
const MAX_TOP = 1000 - 400;

const dialog = { id: '777000', title: 'ShopBot', kind: 'user' };
const other = { id: '42', title: 'Sofia', kind: 'user' };

const plain = {
    id: 3, dialog_id: '777000', out: false, date: 1700000000,
    text: 'привет', sender: { id: '5001', username: 'shopbot' }, media: null,
};
/** Сообщение бота с инлайн-клавиатурой — источник всей фичи. */
function botMsg(over) {
    return Object.assign({
        id: 4, dialog_id: '777000', out: false, date: 1700000100,
        text: 'выберите раздел', sender: { id: '5001', username: 'shopbot' }, media: null,
        reply_markup: { kind: 'inline', rows: [[{ text: 'каталог', kind: 'callback', data: 'cat' }]] },
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

netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });

tgState.dialogs = [dialog, other];
tgState.activeDialogId = '777000';
tgState.messages.set('777000', [plain, botMsg()]);
chat.initChat({});
chat.renderHistory([plain, botMsg()], dialog);

eq(chat._debugChatState().rendered, 2, 'лента: две строки нарисованы');
ok(!!kbBtnOf(4), 'лента: инлайн-кнопка бота на месте');

// ---------------------------------------------------------------------------
// 1. Прежнее поведение без нажатия: читающего историю не дёргаем
// ---------------------------------------------------------------------------

$log.scrollTop = 100;
chat.onEdited(botMsg({ text: 'очень длинный ответ бота', edit_date: 1700000200 }));
eq($log.scrollTop, 100, 'без нажатия: чужая правка не двигает ленту, если читаешь историю');

$log.scrollTop = 100;
chat.onNewMessage({ id: 5, dialog_id: '777000', out: false, date: 1700000300, text: 'ещё', media: null });
eq($log.scrollTop, 100, 'без нажатия: новое чужое сообщение не двигает ленту');

// gap = 1000 - 600 - 400 = 0 ⇒ isAtBottom, прилипание к низу обязано работать.
$log.scrollTop = 600;
chat.onNewMessage({ id: 6, dialog_id: '777000', out: false, date: 1700000400, text: 'и ещё', media: null });
eq($log.scrollTop, 1000, 'прилипание: у нижней кромки новое сообщение тянет ленту вниз');

$log.scrollTop = 600;
chat.onEdited(botMsg({ id: 6, text: 'и ещё, но длиннее', edit_date: 1700000500, reply_markup: null }));
eq($log.scrollTop, 1000, 'прилипание: правка у нижней кромки тоже держит низ (замер ДО пересборки)');

eq(chat._debugChatState().followActive, false, 'следования без нажатия нет и быть не должно');
eq(ros.length, 0, 'наблюдатель размера до первого нажатия не создаётся');

// ---------------------------------------------------------------------------
// 2. Нажатие кнопки включает следование
// ---------------------------------------------------------------------------

$log.scrollTop = 100;
await pressButton(4);

const afterPress = chat._debugChatState();
eq(afterPress.followActive, true, 'нажатие: следование включилось');
eq(afterPress.followDialogId, '777000', 'нажатие: следование адресовано своему диалогу');
eq(afterPress.followMessageId, 4, 'нажатие: следование адресовано нажатому сообщению');
eq(afterPress.followObserving, true, 'нажатие: строка взята под наблюдение сразу');
eq(ros.length, 1, 'нажатие: наблюдатель создан ровно один');
eq(ros[0].observed.length, 1, 'нажатие: наблюдается ровно одна строка');
ok(ros[0].observed[0] === rowOf(4), 'нажатие: наблюдается именно нажатая строка');
ok(netCalls.some((c) => c.url.includes('/messages/4/callback')), 'нажатие: callback ушёл боту');

// ---------------------------------------------------------------------------
// 3. Правка после нажатия доскролливает — обе ветки onEdited
// ---------------------------------------------------------------------------

// Ветка полной пересборки (есть клавиатура ⇒ replaceWith).
rowsOverflow = true;
$log.scrollTop = 100;
$log.scrollCalls.length = 0;
chat.onEdited(botMsg({
    text: 'каталог:\n1. молоко\n2. хлеб\n3. соль',
    edit_date: 1700000600,
    reply_markup: { kind: 'inline', rows: [[{ text: 'назад', kind: 'callback', data: 'b' }]] },
}));
eq($log.scrollTop, MAX_TOP, 'правка: выросшее сообщение с клавиатурой доскроллено к концу блока');
eq($log.scrollCalls.length, 1, 'правка: доскролл сделан ровно один раз');
eq($log.scrollCalls[0].behavior, 'smooth', 'правка: по умолчанию плавно');
eq(ros[0].observed.length, 1, 'правка: наблюдение переехало на пересобранную строку');
ok(ros[0].observed[0] === rowOf(4), 'правка: наблюдается НОВЫЙ узел, старый отпущен');

// Ветка точечного патча (ни медиа, ни клавиатуры ⇒ правится только текст).
tgState.messages.set('777000', [plain, { id: 7, dialog_id: '777000', out: false, date: 1700000700, text: 'ок', media: null }]);
chat.renderHistory([{ id: 7, dialog_id: '777000', out: false, date: 1700000700, text: 'ок', media: null }], dialog);
follow.noteFollow('777000', 7, Date.now());
$log.scrollTop = 100;
$log.scrollCalls.length = 0;
chat.onEdited({
    id: 7, dialog_id: '777000', out: false, date: 1700000700, media: null, reply_markup: null,
    text: 'ок, вот подробный ответ на много строк', edit_date: 1700000800,
});
eq($log.scrollTop, MAX_TOP, 'правка: точечный патч текста тоже доскролливает');
eq(rowOf(7).querySelector('.tg-line-suffix').textContent, ' (edited)',
    'правка: пометка «(edited)» на месте — точечная ветка не подменена');

// ---------------------------------------------------------------------------
// 4. Бот ответил НОВЫМ сообщением вместо правки
// ---------------------------------------------------------------------------

$log.scrollTop = 100;
$log.scrollCalls.length = 0;
chat.onNewMessage({
    id: 11, dialog_id: '777000', out: false, date: 1700000900,
    text: 'вот ваш заказ', media: null, sender: { id: '5001', username: 'shopbot' },
});
eq($log.scrollTop, MAX_TOP, 'новое сообщение: ответ бота на нажатие доезжает сам');
ok(ros[0].observed[0] === rowOf(11), 'новое сообщение: наблюдение переехало на него');

// А вот ЧУЖОЙ диалог ленту дёргать не имеет права даже при живом следовании.
$log.scrollTop = 100;
chat.onNewMessage({ id: 12, dialog_id: '42', out: false, date: 1700001000, text: 'привет', media: null });
eq($log.scrollTop, 100, 'адресность: сообщение соседнего чата ленту не двигает');

// ---------------------------------------------------------------------------
// 5. Отложенный рост добирается наблюдателем
// ---------------------------------------------------------------------------

$log.scrollTop = 100;
$log.scrollCalls.length = 0;
ros[0].fire(rowOf(11));
eq($log.scrollTop, MAX_TOP, 'отложенный рост: догрузившееся медиа добирается наблюдателем');
eq($log.scrollCalls.length, 1, 'отложенный рост: один доскролл на срабатывание');

// Если строка уже видна целиком — наблюдатель ленту не трогает.
rowsOverflow = false;
$log.scrollTop = 100;
$log.scrollCalls.length = 0;
ros[0].fire(rowOf(11));
eq($log.scrollTop, 100, 'отложенный рост: видимую строку догонять не надо');
eq($log.scrollCalls.length, 0, 'отложенный рост: лишних вызовов scrollTo нет');
rowsOverflow = true;

// ---------------------------------------------------------------------------
// 6. prefers-reduced-motion
// ---------------------------------------------------------------------------

// Переключение МЕЖДУ вызовами ловит «прочитали медиазапрос один раз на импорте».
reducedMotion = true;
$log.scrollTop = 100;
$log.scrollCalls.length = 0;
ros[0].fire(rowOf(11));
eq($log.scrollCalls[0].behavior, 'auto', 'движение: reduce ⇒ мгновенная прокрутка');
eq($log.scrollTop, MAX_TOP, 'движение: reduce не отменяет сам доскролл');

reducedMotion = false;
$log.scrollTop = 100;
$log.scrollCalls.length = 0;
ros[0].fire(rowOf(11));
eq($log.scrollCalls[0].behavior, 'smooth', 'движение: снятие reduce возвращает плавность');

// Окружение без scrollTo (старый WebView) обязано ехать присваиванием scrollTop.
const savedScrollTo = $log.scrollTo;
delete $log.scrollTo;
$log.scrollTop = 100;
ros[0].fire(rowOf(11));
eq($log.scrollTop, MAX_TOP, 'фолбэк: без scrollTo доскролл идёт присваиванием scrollTop');
$log.scrollTo = savedScrollTo;

// ---------------------------------------------------------------------------
// 7. Ручная прокрутка вверх гасит следование
// ---------------------------------------------------------------------------

$log.scrollTop = 600;
dispatch($log, 'scroll', {});
eq(chat._debugChatState().followActive, true, 'прокрутка: движение вниз следование не гасит');

$log.scrollTop = 100;
dispatch($log, 'scroll', {});
eq(chat._debugChatState().followActive, false, 'прокрутка: уход вверх отменяет следование');
eq(chat._debugChatState().followObserving, false, 'прокрутка: наблюдение снято вместе со следованием');
eq(ros[0].observed.length, 0, 'прокрутка: наблюдатель отпустил строку');
eq(ros.length, 1, 'наблюдатель: он по-прежнему один на всю вкладку');

$log.scrollTop = 100;
chat.onEdited({
    id: 11, dialog_id: '777000', out: false, date: 1700000900, media: null, reply_markup: null,
    text: 'бот дописал ещё', edit_date: 1700001100,
});
eq($log.scrollTop, 100, 'прокрутка: после отмены правка ленту уже не двигает');

// ---------------------------------------------------------------------------
// 8. Истечение срока следования
// ---------------------------------------------------------------------------

tgState.messages.set('777000', [plain, botMsg()]);
chat.renderHistory([plain, botMsg()], dialog);
await pressButton(4);
eq(chat._debugChatState().followActive, true, 'срок: повторное нажатие включило следование заново');
// Отматываем окно назад — ждать реальные 12с в тесте нечем и незачем.
follow.noteFollow('777000', 4, Date.now() - follow.FOLLOW_TTL_MS - 1000);
eq(chat._debugChatState().followActive, false, 'срок: окно истекло');

$log.scrollTop = 100;
chat.onEdited(botMsg({ text: 'поздняя правка', edit_date: 1700001200 }));
eq($log.scrollTop, 100, 'срок: после истечения правка ленту не двигает');

// Наблюдатель мог пережить истечение на кадр — он обязан сняться сам.
ros[0].fire(rowOf(4));
eq(ros[0].observed.length, 0, 'срок: сработавший после истечения наблюдатель снимает себя');
eq(chat._debugChatState().followObserving, false, 'срок: строка отпущена');

// ---------------------------------------------------------------------------
// 9. Наблюдателей не плодим
// ---------------------------------------------------------------------------

for (let i = 0; i < 5; i += 1) await pressButton(4);
eq(ros.length, 1, 'пачка нажатий: наблюдатель по-прежнему один');
eq(ros[0].observed.length, 1, 'пачка нажатий: наблюдается ровно одна строка');

// ---------------------------------------------------------------------------
// 10. Смена диалога снимает наблюдение
// ---------------------------------------------------------------------------

netHandler = async () => jsonRes(200, { messages: [], has_more: false });
await chat.openDialog('42', other);
eq(chat._debugChatState().followActive, false, 'смена диалога: следование покинутого чата снято');
eq(ros[0].observed.length, 0, 'смена диалога: наблюдение снято');
netHandler = async () => jsonRes(200, { message: null, alert: false, url: null });

// Возврат в чат бота: восстанавливаем ленту и следование.
tgState.activeDialogId = '777000';
tgState.messages.set('777000', [plain, botMsg()]);
chat.renderHistory([plain, botMsg()], dialog);
await pressButton(4);
eq(ros[0].observed.length, 1, 'возврат: нажатие снова ставит наблюдение');

// ---------------------------------------------------------------------------
// 11. Перерисовка ленты переставляет наблюдение (подгрузка истории вверх)
// ---------------------------------------------------------------------------

const rowBefore = rowOf(4);
chat.renderHistory([plain, botMsg()], dialog);
ok(rowOf(4) !== rowBefore, 'перерисовка: строка пересоздана');
eq(ros[0].observed.length, 1, 'перерисовка: наблюдение осталось ровно одно');
ok(ros[0].observed[0] === rowOf(4), 'перерисовка: наблюдается новая строка, а не мёртвый узел');

// ---------------------------------------------------------------------------
// 12. Удаление следуемого сообщения
// ---------------------------------------------------------------------------

chat.onDeleted({ dialog_id: '777000', message_ids: [4] });
eq(chat._debugChatState().followActive, false, 'удаление: следовать больше не за чем');
eq(ros[0].observed.length, 0, 'удаление: наблюдение снято с заглушки');

// ---------------------------------------------------------------------------
// 13. Переход по цитате не перебивается доскроллом
// ---------------------------------------------------------------------------

rowsOverflow = false;
const quoted = {
    id: 20, dialog_id: '777000', out: false, date: 1700002000, text: 'оригинал', media: null,
    sender: { id: '5001', username: 'shopbot' },
};
const answer = {
    id: 21, dialog_id: '777000', out: false, date: 1700002100, text: 'ответ', media: null,
    sender: { id: '5001', username: 'shopbot' },
    reply_to: { message_id: 20, sender: 'shopbot', text: 'оригинал', out: false, unavailable: false },
};
tgState.messages.set('777000', [quoted, answer]);
chat.renderHistory([quoted, answer], dialog);
// Живое следование не имеет права вернуть ленту вниз из-под перехода к оригиналу.
follow.noteFollow('777000', 20, Date.now());
$log.scrollTop = 500;
const quoteEl = rowOf(21).querySelector('.tg-reply-quote');
dispatch($log, 'click', { target: quoteEl });
// Центрирование при константных rect'ах: 0 - (400 - 20) / 2 = -190.
eq($log.scrollTop, 310, 'цитата: переход к оригиналу по-прежнему центрирует строку');
eq(chat._debugChatState().followActive, false, 'цитата: переход к оригиналу гасит следование');
eq(chat._debugChatState().flashId, 20, 'цитата: оригинал подсвечен');

// ---------------------------------------------------------------------------
// 14. Деградация без ResizeObserver
// ---------------------------------------------------------------------------

chat.teardownChat();
eq(ros[0].disconnected, true, 'teardown вкладки: наблюдатель распущен');
eq(chat._debugChatState().followActive, false, 'teardown вкладки: следование снято');

const savedRO = globalThis.ResizeObserver;
delete globalThis.ResizeObserver;
rowsOverflow = true;
tgState.messages.set('777000', [plain, botMsg()]);
chat.renderHistory([plain, botMsg()], dialog);
await pressButton(4);
eq(chat._debugChatState().followActive, true, 'без ResizeObserver: следование всё равно работает');
eq(ros.length, 1, 'без ResizeObserver: новых наблюдателей не появилось');

$log.scrollTop = 100;
chat.onEdited(botMsg({ text: 'ответ без наблюдателя', edit_date: 1700003000 }));
eq($log.scrollTop, MAX_TOP, 'без ResizeObserver: доскролл в момент правки на месте');
globalThis.ResizeObserver = savedRO;

// ---------------------------------------------------------------------------
// 15. Полный сброс гасит таймер следования
// ---------------------------------------------------------------------------

chat.resetChat();
eq(chat._debugChatState().followActive, false, 'сброс: следование снято');
eq(chat._debugChatState().followObserving, false, 'сброс: наблюдение снято');
chat.teardownChat();

// Живой таймер следования (12с) подвесил бы этот процесс до срабатывания —
// то, что node завершается сразу, и есть доказательство его зачистки.

console.log(`\n${checks - failures}/${checks} ассертов прошло (frontend/telegram_scroll_ui)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
