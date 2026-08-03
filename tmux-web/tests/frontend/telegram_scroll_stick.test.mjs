// Регресс-тест «лента Telegram остаётся у последнего сообщения, когда
// содержимое дорастает уже ПОСЛЕ прокрутки» (js/telegram/chat.js +
// js/telegram/media.js).
//
// ЧТО ЗА ДЕФЕКТ. Пользовательская приёмка: «при открытии чата он не
// прокручивается автоматически до самого последнего сообщения». Причина —
// та же, что у уже починенной панели VK, и воспроизведена в браузере на моке:
//
//   * `openDialog` рисует историю и синхронно зовёт `scrollToBottom()` —
//     в этот момент лента ДЕЙСТВИТЕЛЬНО внизу (отступ 0px);
//   * превью вложений грузятся ЛЕНИВО (IntersectionObserver в
//     js/telegram/media.js), и каждое показанное превью растит
//     `.tg-media-body` с min-height 92px до max-height 184px (THUMB_HEIGHT) —
//     то есть лента дорастает уже ПОСЛЕ прокрутки;
//   * `scrollTop` при этом не меняется, и низ уезжает на 92px за карточку
//     (замер живого стенда: отступ 276px при трёх догруженных превью);
//   * дальше дефект самоусиливается: `isAtBottom` теперь ложно (порог 40px
//     против 92px прироста), поэтому ветка `stick` в `onNewMessage` не
//     срабатывает больше НИКОГДА.
//
// Отсюда модель «превью догрузилось» = прирост `scrollHeight` + событие `load`
// на `<img>` карточки. Ровно так это и выглядит в браузере: до декодирования
// картинки `<img>` занимает по вертикали ноль, тело карточки держится на
// min-height, и высота меняется в тот момент, когда браузер узнал размеры, —
// то есть к событию `load` раскладка уже новая.
//
// ОТЛИЧИЕ ОТ VK-ШНОГО ДВОЙНИКА (vk_scroll_stick.test.mjs): у Telegram своя
// механика «следования» за ответом бота (js/telegram/follow.js + ResizeObserver
// в chat.js), у которой СВОЯ цель прокрутки — конец строки бота, а не низ
// ленты. Поэтому здесь добавлены сценарии 7 и 8: защёлка обязана уступать
// живому следованию и не спорить с ним за позицию ленты.
//
// Почему СВОЙ процесс и свой фейковый DOM: `core/dom.js` резолвит все ref'ы
// ОДИН раз на верхнем уровне модуля, поэтому стабы обязаны стоять до первого
// импорта, а второй набор стабов в тот же процесс не вписать.
//
// Проверяется:
//   1. ОТКРЫТИЕ ДИАЛОГА: догрузка превью не оставляет ленту выше низа;
//   1a. отложенное событие прокрутки не снимает защёлку;
//   1b. открытие диалога объявляет НАМЕРЕНИЕ, а не только двигает scrollTop;
//   2. НОВОЕ СООБЩЕНИЕ: та же догрузка после `stick`-ветки;
//   3. САМОУСИЛЕНИЕ: после догрузки следующее сообщение по-прежнему прилипает;
//   4. РЕГРЕСС-ЩИТ: читающего историю рост НЕ дёргает вниз;
//   5. ВОЗВРАТ к нижней кромке снова включает прижатие;
//   6a. ПОДГРУЗКА ВВЕРХ при взведённой защёлке: якорь `loadOlder` важнее;
//   6b. подгрузка вверх у верхней кромки не швыряет ленту в самый низ;
//   7. СЛЕДОВАНИЕ: пока живо follow.js, прирост отдаётся ему, а не защёлке;
//   8. ЧТЕНИЕ ИСТОРИИ + новое сообщение внизу: лента не дёргается ни от
//      сообщения, ни от догрузившегося в нём превью.
//
// Запуск: node tmux-web/tests/frontend/telegram_scroll_stick.test.mjs
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

const RECT_SMALL = { top: 0, left: 0, width: 100, height: 20, bottom: 20, right: 100 };

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
        src: '',
        alt: '',
        tabIndex: -1,
        disabled: false,
        scrollTop: 0,
        scrollHeight: 1000,
        clientHeight: 400,
        focusCount: 0,
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
    node.scrollTo = (opts) => {
        node.scrollCalls.push(opts);
        if (opts && Number.isFinite(opts.top)) node.scrollTop = opts.top;
    };
    node.getBoundingClientRect = () => RECT_SMALL;

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

/**
 * Обрезка `scrollTop` потолком прокрутки — как в браузере.
 *
 * Фейковый DOM хранит присвоенное значение как есть, и `scrollTop =
 * scrollHeight` оставляло бы ленту «за низом»: любой последующий прирост
 * высоты она бы «переваривала» бесплатно, и отличить прижатую ленту от
 * уехавшей стало бы нечем. Ровно на этой разнице и держится дефект — модель
 * без потолка проверяла бы не то.
 */
{
    let rawTop = 0;
    Object.defineProperty($log, 'scrollTop', {
        get: () => rawTop,
        set(v) {
            const max = Math.max(0, $log.scrollHeight - $log.clientHeight);
            rawTop = Math.max(0, Math.min(Number(v) || 0, max));
        },
    });
}

// ---------------------------------------------------------------------------
// ResizeObserver следования (chat.js создаёт его лениво)
// ---------------------------------------------------------------------------

class SpyResizeObserver {
    constructor(cb) { this.cb = cb; this.observed = []; }
    observe(node) { if (!this.observed.includes(node)) this.observed.push(node); }
    unobserve(node) { this.observed = this.observed.filter((x) => x !== node); }
    disconnect() { this.observed = []; }
}
globalThis.ResizeObserver = SpyResizeObserver;

// ---------------------------------------------------------------------------
// Ленивые превью: наблюдатель ничего не показывает сам
// ---------------------------------------------------------------------------

/**
 * IntersectionObserver, который НИЧЕГО не показывает сам.
 *
 * Ленивость превью — часть контракта: тест обязан уметь проверить, что до
 * появления карточки во вьюпорте запроса НЕ БЫЛО. Поэтому наблюдатель копит
 * цели, а тест «показывает» их вручную через `showObserved()`.
 */
const observed = [];
globalThis.IntersectionObserver = class IntersectionObserver {
    constructor(cb) { this.cb = cb; this.targets = new Set(); observed.push(this); }
    observe(node) { this.targets.add(node); }
    unobserve(node) { this.targets.delete(node); }
    disconnect() { this.targets.clear(); }
};

/** Изображает попадание всех наблюдаемых карточек во вьюпорт. */
function showObserved() {
    for (const io of observed) {
        const entries = Array.from(io.targets).map((target) => ({ target, isIntersecting: true }));
        if (entries.length) io.cb(entries);
    }
}

let urlSeq = 0;
globalThis.URL.createObjectURL = (blob) => {
    urlSeq += 1;
    void blob;
    return `blob:tg/${urlSeq}`;
};
globalThis.URL.revokeObjectURL = () => {};

/** Минимальный Blob: media.js читает у него только `size` и `type`. */
globalThis.Blob = class Blob {
    constructor(parts, opts) {
        this.type = (opts && opts.type) || '';
        this.size = (parts || []).reduce((acc, p) => acc + (p.byteLength || p.length || 0), 0);
    }
};

// ---------------------------------------------------------------------------
// Сеть
// ---------------------------------------------------------------------------

function jsonRes(status, obj) {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: {
            get: (h) => (String(h).toLowerCase() === 'content-type' ? 'application/json' : null),
        },
        json: async () => obj,
        text: async () => JSON.stringify(obj),
    };
}

/** Ответ прокси медиа: байты + заголовки, как их читает media.js. */
function mediaRes(bytes, type) {
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

let netHandler = async () => jsonRes(200, {});
const setNet = (fn) => { netHandler = fn; };
globalThis.fetch = (url, init) => netHandler(String(url), init);

globalThis.window = {
    fetch: globalThis.fetch,
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
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
const C = await import(`${base}chat.js`);
const FOLLOW = await import(`${base}follow.js`);
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

const settle = async (ms) => {
    await new Promise((r) => setTimeout(r, ms === undefined ? 5 : ms));
};

/**
 * Насколько тело карточки вырастает, когда превью догрузилось.
 *
 * `.tg-media-body`: min-height 92px (иконка вида вложения) → max-height 184px
 * (`THUMB_HEIGHT`). Числа зашиты в css/telegram.css и здесь только
 * моделируются.
 */
const THUMB_GROWTH_PX = 92;

/**
 * Геометрия ленты: браузерной раскладки нет, тест выставляет её руками.
 *
 * Размеры ставятся ДО `scrollTop`: потолок прокрутки считается по ним, и
 * обратный порядок обрезал бы позицию по геометрии прошлого сценария.
 */
function layout(scrollTop, scrollHeight, clientHeight) {
    $log.scrollHeight = scrollHeight === undefined ? 1000 : scrollHeight;
    $log.clientHeight = clientHeight === undefined ? 200 : clientHeight;
    $log.scrollTop = scrollTop;
}

/** Отступ последнего сообщения от нижней кромки (0 ⇒ лента внизу). */
const gap = () => $log.scrollHeight - $log.clientHeight - $log.scrollTop;

/** Все превью, которым уже подставлен blob (то есть ждущие декодирования). */
const pendingThumbs = () => $log.querySelectorAll('.tg-media-thumb')
    .filter((t) => !t.hidden && t.src);

/**
 * Изображает декодирование превью: лента дорастает, затем прилетает `load`.
 *
 * Порядок именно такой, как в браузере: к моменту события раскладка уже
 * пересчитана, и обработчик, читающий `scrollHeight`, видит НОВУЮ высоту.
 *
 * @returns {number} сколько превью «догрузилось»
 */
function decodeThumbs() {
    const thumbs = pendingThumbs();
    for (const img of thumbs) {
        $log.scrollHeight += THUMB_GROWTH_PX;
        img.decoded = true;
        dispatch(img, 'load', { target: img });
    }
    return thumbs.length;
}

/** Вложение-картинка в форме MediaDto бэкенда. */
function photo() {
    return {
        kind: 'photo',
        mime: 'image/jpeg',
        size: 120000,
        file_name: 'photo.jpg',
        width: 1280,
        height: 960,
        duration_secs: null,
        has_thumb: true,
        has_full: true,
        waveform: [],
    };
}

function message(id, withPhoto, extra) {
    return Object.assign({
        id,
        dialog_id: '777000',
        out: false,
        date: 1_700_000_000 + id * 60,
        sender: { id: '5001', username: 'shopbot', first_name: 'ShopBot' },
        text: `сообщение ${id}`,
        media: withPhoto ? photo() : null,
        reply_to_id: null,
        reply_to: null,
        edit_date: null,
        reply_markup: null,
    }, extra || {});
}

const DIALOG = { id: '777000', kind: 'user', title: 'ShopBot' };
tgState.dialogs = [DIALOG];
tgState.selfUser = { id: '999' };
C.initChat({ onRead: () => {}, presenceOf: () => null });

/** История + байты медиа-прокси; всё остальное — пустой JSON. */
const net = (msgs, hasMore) => async (url, init) => {
    if (url.includes('/api/telegram/media/')) return mediaRes([1, 2, 3, 4], 'image/jpeg');
    if (init && init.method === 'POST') return jsonRes(200, { ok: true });
    if (url.includes('/messages')) return jsonRes(200, { messages: msgs, has_more: !!hasMore });
    return jsonRes(200, {});
};

async function open(msgs, hasMore) {
    setNet(net(msgs, hasMore));
    await C.openDialog('777000', DIALOG);
    await settle();
    // Браузер шлёт `scroll` и на ПРОГРАММНУЮ прокрутку — фейковый DOM сам не
    // умеет. Без этого события лента запомнила бы прошлое положение, и
    // «пользователь ушёл вверх» ниже сравнивалось бы с чужим числом.
    dispatch($log, 'scroll');
}

// История на несколько экранов; каждое четвёртое сообщение — с вложением.
const HISTORY = Array.from({ length: 40 }, (_, k) => {
    const id = 7001 + k;
    return message(id, id % 4 === 0);
});

// ---------------------------------------------------------------------------
// 1. Открытие диалога: догрузка превью не оставляет ленту выше низа
// ---------------------------------------------------------------------------

{
    layout(0, 4000, 700);
    await open(HISTORY, false);
    // Сама прокрутка при открытии работала и до починки — фиксируем это, чтобы
    // отличать «не прокрутилось вовсе» от «уехало потом».
    eq(gap() <= 0, true, 'сразу после renderHistory лента у нижней кромки');

    // Карточки подъехали к вьюпорту — IntersectionObserver отдал их загрузке.
    showObserved();
    await settle();
    const grown = decodeThumbs();
    ok(grown > 0, `превью действительно догрузились (${grown} шт.)`);

    eq(gap() <= 0, true,
        `догрузка превью не должна отрывать ленту от низа (отступ ${gap()}px)`);
    eq($log.scrollTop, $log.scrollHeight - $log.clientHeight, 'лента прижата к самому низу');
}

// ---------------------------------------------------------------------------
// 1a. Событие прокрутки приходит ПОЗЖЕ роста — защёлка обязана уцелеть
// ---------------------------------------------------------------------------

{
    // Самая тонкая часть починки. `scroll` доставляется асинхронно, и к моменту
    // доставки лента успевает дорасти ещё раз — например превью соседней
    // карточки или подменённым шрифтом, о котором ленте никто не сообщает.
    // Наивное правило «прижата ⇔ isAtBottom прямо сейчас» на этом событии
    // прочитало бы «пользователь ушёл вверх», хотя руки его ленты не касались,
    // — и следующий прирост уже не догнали бы. Именно так дефект и выглядел в
    // браузере: отступ залипал на 92px (высота одного превью).
    ok(C._debugChatState().stickBottom, 'исходно лента прижата');
    const before = $log.scrollTop;
    $log.scrollHeight += 200;
    dispatch($log, 'scroll');
    ok(C._debugChatState().stickBottom,
        'отложенное событие прокрутки не снимает прижатие: рука пользователя ленты не касалась');

    // И следующий прирост по-прежнему догоняется.
    $log.scrollHeight += THUMB_GROWTH_PX;
    C.onMediaGrew();
    eq(gap() <= 0, true, 'лента догнала прирост, случившийся после события');
    ok($log.scrollTop > before, 'и действительно уехала вниз');
}

// ---------------------------------------------------------------------------
// 1b. Открытие диалога объявляет НАМЕРЕНИЕ, а не только двигает scrollTop
// ---------------------------------------------------------------------------

{
    // Пользователь ушёл читать историю — прижатие снято.
    layout(400, 4000, 700);
    dispatch($log, 'scroll');
    eq(C._debugChatState().stickBottom, false, 'чтение истории прижатие снимает');

    // Открытие диалога — намерение «показать конец переписки». Событие
    // прокрутки после этого НАМЕРЕННО не отправляется: защёлка обязана
    // держаться на самом факте доскролла, а не на том, успел ли браузер
    // прислать `scroll` раньше, чем догрузилось первое превью.
    setNet(net(HISTORY, false));
    await C.openDialog('777000', DIALOG);
    await settle();
    eq(C._debugChatState().stickBottom, true, 'открытие диалога прижимает ленту к низу');
    dispatch($log, 'scroll');
}

// ---------------------------------------------------------------------------
// 2. Новое сообщение: та же догрузка после ветки stick
// ---------------------------------------------------------------------------

{
    layout(3300, 4000, 700);
    C.onNewMessage(message(7500, true));
    eq(gap() <= 0, true, 'новое сообщение у нижней кромки прилипает (прежнее поведение)');

    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью нового сообщения догрузилось');
    eq(gap() <= 0, true,
        `догрузка превью нового сообщения тоже не отрывает ленту (отступ ${gap()}px)`);
}

// ---------------------------------------------------------------------------
// 3. Самоусиление: после догрузки следующее сообщение по-прежнему прилипает
// ---------------------------------------------------------------------------

{
    // Ровно тот сценарий, в котором дефект становился необратимым: первая
    // догрузка уводила ленту от низа, `isAtBottom` начинал врать, и ветка
    // `stick` не срабатывала уже никогда.
    eq(gap() <= 0, true,
        'после первой догрузки лента всё ещё у кромки — значит stick вообще сработает');
    const before = $log.scrollTop;
    C.onNewMessage(message(7501, true));
    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью следующего сообщения тоже догрузилось');
    ok($log.scrollTop > before, 'лента уехала вниз за новым сообщением');
    eq(gap() <= 0, true, 'stick продолжает работать после догрузки превью');
}

// ---------------------------------------------------------------------------
// 4. Регресс-щит: читающего историю рост НЕ дёргает вниз
// ---------------------------------------------------------------------------

{
    await open(HISTORY, false);
    layout(1200, 4000, 700);
    // Пользователь ушёл читать историю — событие прокрутки об этом сообщает.
    dispatch($log, 'scroll');

    showObserved();
    await settle();
    const grown = decodeThumbs();
    ok(grown > 0, 'превью догрузились и в истории тоже');
    eq($log.scrollTop, 1200,
        'лента читающего историю остаётся на месте, сколько бы ни доросла');
}

// ---------------------------------------------------------------------------
// 5. Возврат к нижней кромке снова включает прижатие
// ---------------------------------------------------------------------------

{
    // Вернулись вниз руками — прижатие обязано восстановиться, иначе после
    // одного взгляда в историю лента переставала бы следить за перепиской.
    $log.scrollTop = $log.scrollHeight - $log.clientHeight;
    dispatch($log, 'scroll');
    // Замер ИМЕННО ЗДЕСЬ, до всякого нового сообщения: `scrollToBottom` внутри
    // ветки `stick` защёлку взводит сам, и проверка «после сообщения» прошла бы
    // и с обработчиком прокрутки, который возврат к кромке проспал.
    eq(C._debugChatState().stickBottom, true, 'возврат к нижней кромке снова взводит защёлку');

    // И одного лишь роста содержимого (без нового сообщения) теперь достаточно,
    // чтобы лента доехала: ровно этого пользователь и ждёт, вернувшись вниз.
    $log.scrollHeight += THUMB_GROWTH_PX;
    eq(C.onMediaGrew(), true, 'вернувшемуся вниз рост ленту догоняет');
    eq(gap() <= 0, true, 'и приводит её к самой кромке');

    C.onNewMessage(message(7502, true));
    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью догрузилось после возврата вниз');
    eq(gap() <= 0, true, 'вернувшемуся вниз лента снова прижата');
}

// ---------------------------------------------------------------------------
// 6. Подгрузка вверх: рост во время неё не сбивает якорь чтения
// ---------------------------------------------------------------------------

// 6a. Гард `loading`: взведённая защёлка обязана молчать, пока едет страница.
{
    // `loadOlder` держит СВОЙ якорь между замером `scrollHeight` и
    // восстановлением `scrollTop`; посторонний доскролл внутри этого окна
    // необратимо теряет позицию чтения. Проверяется случай, когда защёлка при
    // этом ВЗВЕДЕНА (лента у низа, страницу тянет переход к оригиналу цитаты):
    // без гарда по `local.loading` защёлка перебила бы якорь.
    await open(HISTORY, true);
    layout($log.scrollHeight - $log.clientHeight, 4000, 700);
    dispatch($log, 'scroll');
    ok(C._debugChatState().stickBottom, 'перед подгрузкой вверх лента прижата');

    const olderPage = Array.from({ length: 10 }, (_, k) => message(6800 + k, true));
    /** Что ответила защёлка ВНУТРИ окна подгрузки. */
    let inside = null;
    const topBeforePage = $log.scrollTop;
    setNet(async (url, init) => {
        if (url.includes('/api/telegram/media/')) return mediaRes([1, 2, 3, 4], 'image/jpeg');
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (url.includes('before_id=') || url.includes('beforeId=')) {
            // Пока летит страница, догрузилось превью уже нарисованной карточки.
            $log.scrollHeight += THUMB_GROWTH_PX;
            inside = { grew: C.onMediaGrew(), top: $log.scrollTop };
            return jsonRes(200, { messages: olderPage, has_more: false });
        }
        return jsonRes(200, { messages: HISTORY, has_more: true });
    });
    await C.loadOlder(DIALOG);
    await settle();

    eq(inside && inside.grew, false, 'во время подгрузки вверх защёлка ленту не трогает');
    eq(inside && inside.top, topBeforePage, 'якорь loadOlder остался нетронутым');
}

// 6b. Читатель у верхней кромки: рост во время подгрузки не швыряет его вниз.
{
    await open(HISTORY, true);
    // Пользователь у верхней кромки — сейчас поедет страница истории.
    layout(0, 4000, 700);
    dispatch($log, 'scroll');

    const older = Array.from({ length: 10 }, (_, k) => message(6900 + k, true));
    setNet(async (url, init) => {
        if (url.includes('/api/telegram/media/')) return mediaRes([1, 2, 3, 4], 'image/jpeg');
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (url.includes('before_id=') || url.includes('beforeId=')) {
            // Пока летит страница, догружается превью уже нарисованной карточки.
            $log.scrollHeight += THUMB_GROWTH_PX;
            for (const img of pendingThumbs()) dispatch(img, 'load', { target: img });
            return jsonRes(200, { messages: older, has_more: false });
        }
        return jsonRes(200, { messages: HISTORY, has_more: true });
    });
    // Модель прироста высоты от вставленной страницы.
    const prevHeight = $log.scrollHeight;
    await C.loadOlder(DIALOG);
    $log.scrollHeight = prevHeight + 1500;
    await settle(20);

    ok($log.scrollTop < $log.scrollHeight - $log.clientHeight,
        `подгрузка вверх не швыряет ленту в самый низ (scrollTop ${$log.scrollTop})`);
}

// ---------------------------------------------------------------------------
// 7. Следование за ответом бота: прирост отдаётся ему, а не защёлке
// ---------------------------------------------------------------------------

{
    // ТО, ЧЕГО НЕТ У VK. Нажатие кнопки бота включает «следование» (follow.js):
    // у него СВОЯ цель прокрутки — конец растущей строки бота, а не низ ленты,
    // и свой наблюдатель размера. Если бы защёлка тоже дёргала ленту вниз на
    // каждом догруженном превью, два доскролла спорили бы за позицию, и
    // пользователь видел бы рывок вверх-вниз ровно в тот момент, когда бот
    // дописывает ответ.
    await open(HISTORY, false);
    ok(C._debugChatState().stickBottom, 'перед нажатием кнопки лента прижата');

    // Включаем следование за строкой бота напрямую: сама механика нажатия
    // покрыта telegram_scroll_ui, здесь важен только факт «следование живо».
    FOLLOW.noteFollow('777000', 7040, Date.now());
    ok(C._debugChatState().followActive, 'следование действительно живо');

    layout($log.scrollHeight - $log.clientHeight, 4000, 700);
    const before = $log.scrollTop;
    $log.scrollHeight += THUMB_GROWTH_PX;
    eq(C.onMediaGrew(), false, 'пока живо следование, защёлка ленту не трогает');
    eq($log.scrollTop, before, 'позиция ленты осталась за следованием');

    // Следование кончилось — защёлка снова работает (и та же карточка,
    // выросшая уже после, ленту догоняет).
    FOLLOW.clearFollow();
    eq(C._debugChatState().followActive, false, 'следование снято');
    eq(C.onMediaGrew(), true, 'после следования защёлка снова ведёт ленту вниз');
    eq(gap() <= 0, true, 'и лента опять у нижней кромки');
}

// ---------------------------------------------------------------------------
// 8. Чтение истории + новое сообщение внизу: ленту не дёргает ничто
// ---------------------------------------------------------------------------

{
    // Вторая половина проверки «защёлка и follow.js разведены»: следования нет,
    // пользователь читает середину ленты, и внизу приходит сообщение с
    // вложением. Ни само сообщение, ни догрузившееся в нём превью не имеют
    // права сдвинуть ленту — это ровно та жалоба, ради которой ветка `stick`
    // когда-то и стала условной.
    await open(HISTORY, false);
    layout(1500, 4000, 700);
    dispatch($log, 'scroll');
    eq(C._debugChatState().stickBottom, false, 'читатель истории не прижат');

    C.onNewMessage(message(7600, true));
    eq($log.scrollTop, 1500, 'новое сообщение внизу не дёргает читающего историю');

    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью нового сообщения догрузилось');
    eq($log.scrollTop, 1500, 'и его догрузка тоже ленту не двигает');
}

console.log(`telegram_scroll_stick: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
