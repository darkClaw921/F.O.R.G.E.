// Регресс-тест UI ответов (reply) вкладки Telegram — краевые случаи.
//
// Чистая логика ответов покрыта соседним telegram_reply.test.mjs (там reply.js
// грузится вообще без DOM). Здесь проверяется ВТОРАЯ половина фичи, которую
// чистыми функциями не поймать: полоса над полем ввода, Esc, двойной клик по
// строке, переход по цитате, реально уходящий `reply_to_id` — и у текста
// (chat.js::send), и у файлов (upload.js::sendFiles).
//
// Почему свой процесс и свой фейковый DOM. `core/dom.js` резолвит все ref'ы
// ОДИН раз на верхнем уровне модуля, поэтому стаб `document` обязан стоять
// раньше первого импорта, а переимпортировать chat.js с другим стабом внутри
// одного процесса нельзя (`?bust=` создаст второй экземпляр state.js — цель
// ответа окажется на одном reply.js, а ассерты на другом, и тест позеленеет по
// случайности). Стаб не претендует на браузер: он реализует ровно тот срез
// DOM, которым пользуются chat.js и upload.js, — дерево, classList/dataset,
// closest/querySelector и делегирование событий.
//
// Сеть заглушена на уровне globalThis.fetch (api.js ходит именно туда, мимо
// подменённого window.fetch из core/auth.js) и XMLHttpRequest (первая нога
// заливки файла). XHR-стаб намеренно НИКОГДА не завершается: очередь upload.js
// последовательная, и «зависший» файл — это ровно то состояние, в котором
// удобно смотреть, с каким `reply_to_id` он встал в очередь.
//
// Запуск: node tmux-web/tests/frontend/telegram_reply_ui.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM
// ---------------------------------------------------------------------------

/**
 * Разбор простого селектора: `tag`, `.class`, `.a.b`, `tag[attr]`.
 *
 * Больше не нужно: chat.js/reply.js ищут по `.tg-line`, `.tg-reply-quote`,
 * `.tg-kb`, `button`, `a[href]` — комбинаторов среди них нет.
 */
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
        scrollTop: 0,
        scrollHeight: 1000,
        clientHeight: 400,
        // Счётчик focus() — «фокус ушёл в поле ввода» иначе не проверить.
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

    // textContent как в браузере: присваивание СОЗДАЁТ текстовый узел, а не
    // прячет строку рядом с детьми. Разница не косметическая — errorLine()
    // сначала пишет текст, а потом дописывает кнопку [retry] соседним узлом,
    // и «строковый» textContent потерял бы текст ошибки при первом appendChild.
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
    node.getBoundingClientRect = () => ({ top: 0, left: 0, width: 100, height: 20, bottom: 20, right: 100 });

    return node;
}

/**
 * Доставка события ОДНОМУ узлу-слушателю.
 *
 * Всплытие не эмулируется намеренно: делегированные обработчики chat.js висят
 * на `#tg-log` и на документе и решают всё по `e.target.closest(...)`, поэтому
 * достаточно позвать слушателя нужного узла с нужным target'ом. Так тест
 * проверяет ровно ту логику, что написана в chat.js, а не мою реализацию
 * capture/bubble.
 */
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
const $send = el('tg-send', 'button');
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
$prompt.appendChild($send);
$body.appendChild($panel);
// Спейсер объявлен в index.html и обязан пережить clearLog().
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
// Сеть
// ---------------------------------------------------------------------------

/** Ответ в том виде, в каком его читает api.js::call. */
function jsonRes(status, obj) {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: { get: (h) => (String(h).toLowerCase() === 'content-type' ? 'application/json' : null) },
        json: async () => obj,
        text: async () => JSON.stringify(obj),
    };
}

/** Журнал запросов: [{url, method, body}]. */
const netCalls = [];
/** Текущий обработчик запроса (ставится сценарием). */
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
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    setTimeout, clearTimeout,
};
globalThis.Headers = class Headers {
    constructor(init) { this.map = new Map(Object.entries(init || {})); }
    has(k) { return this.map.has(k); }
    set(k, v) { this.map.set(k, v); }
    get(k) { return this.map.get(k) || null; }
};

/** Заливки, начатые upload.js: XHR намеренно «висит» (см. шапку файла). */
const xhrs = [];
globalThis.XMLHttpRequest = class XMLHttpRequest {
    constructor() {
        this.upload = {};
        this.status = 0;
        this.responseText = '';
        this.aborted = false;
        xhrs.push(this);
    }
    open(method, path) { this.method = method; this.path = path; }
    setRequestHeader() {}
    send(body) { this.body = body; }
    abort() { this.aborted = true; if (typeof this.onabort === 'function') this.onabort(); }
};

// ---------------------------------------------------------------------------
// Модули
// ---------------------------------------------------------------------------

const base = new URL('../../static/js/telegram/', import.meta.url).href;
const chat = await import(`${base}chat.js`);
const upload = await import(`${base}upload.js`);
const reply = await import(`${base}reply.js`);
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

/** Даёт микрозадачам (await внутри send/openDialog) доработать. */
const tick = () => new Promise((r) => setImmediate(r));

const dialog = { id: '777000', title: 'Sofia', kind: 'user' };
const m7 = {
    id: 7, dialog_id: '777000', out: false, date: 1700000000,
    text: 'старое сообщение', sender: { id: '4242', username: 'sofia' }, media: null,
};
const m8 = { id: 8, dialog_id: '777000', out: true, date: 1700000100, text: 'моё', media: null };

/** Строка ленты по message_id (без обращения к приватным полям chat.js). */
const rowOf = (id) => $log.children.find((c) => c.dataset.msgId === String(id)) || null;
/** Последняя строка лога — по ней читаются системные строки и ошибки. */
const lastLine = () => $log.children[$log.children.length - 1] || null;
/** Полоса ответа: она всегда непосредственно перед формой. */
const bar = () => $composer.children.find((c) => c.classList.contains('tg-reply-bar')) || null;

tgState.dialogs = [dialog];
chat.initChat({});

// ---------------------------------------------------------------------------
// 1. Полоса ответа: выбор цели
// ---------------------------------------------------------------------------

eq(chat.startReply(m7, dialog), false, 'полоса: без активного диалога цель не ставится');
ok(bar() === null, 'полоса: до первого ответа узла полосы вообще нет');

tgState.activeDialogId = '777000';
tgState.messages.set('777000', [m7, m8]);
chat.renderHistory([m7, m8], dialog);
eq(chat._debugChatState().rendered, 2, 'лента: две строки нарисованы');

const focusBefore = $input.focusCount;
eq(chat.startReply(m7, dialog), true, 'полоса: цель выбрана');
eq(chat._debugChatState().replyToId, 7, 'полоса: reply_to_id взят из выбранного сообщения');
eq(chat._debugChatState().replyBarVisible, true, 'полоса: показана');
ok($input.focusCount > focusBefore, 'полоса: фокус ушёл в поле ввода — можно сразу печатать');
eq(bar().querySelector('.tg-reply-bar-name').textContent, 'sofia', 'полоса: имя автора оригинала');
eq(bar().querySelector('.tg-reply-bar-text').textContent, 'старое сообщение', 'полоса: превью оригинала');
eq(bar().querySelector('.tg-reply-cancel').textContent, '✕', 'полоса: кнопка отмены на месте');
eq(bar().querySelector('.tg-reply-cancel').getAttribute('aria-label'), 'Отменить ответ',
    'полоса: кнопка отмены подписана для скринридера');
ok($prompt.previousElementSibling === bar(), 'полоса: стоит вплотную над формой ввода');
eq($input.value, '', 'полоса: текст в поле не трогаем — placeholder и value принадлежат botkb.js');

// Полоса reply-клавиатуры бота вставляется тем же insertBefore($tgPrompt) и
// ЛЕНИВО — она может вклиниться между полосой ответа и формой уже после
// выбора цели. Позиция обязана перепроверяться на каждый показ.
const kbBar = makeEl('div');
kbBar.className = 'tg-kb-bar';
$composer.insertBefore(kbBar, $prompt);
ok($prompt.previousElementSibling === kbBar, 'полоса: чужой узел вклинился (эмуляция клавиатуры бота)');
chat.startReply(m8, dialog);
ok($prompt.previousElementSibling === bar(), 'полоса: показ возвращает её вплотную к форме');
eq(chat._debugChatState().replyToId, 8, 'полоса: повторный выбор перецеливает, а не добавляет вторую');
kbBar.remove();

// Optimistic-строка ещё не имеет подтверждённого id — Telegram отверг бы ответ.
chat.startReply(m7, dialog);
eq(chat.startReply({ id: null, out: true, text: 'в пути' }, dialog), false,
    'полоса: на неподтверждённое сообщение ответить нельзя');
eq(chat._debugChatState().replyToId, 7, 'полоса: неудачный выбор не сбивает уже выбранную цель');

// ---------------------------------------------------------------------------
// 2. Отмена: Esc, кнопка ✕, гард лайтбокса
// ---------------------------------------------------------------------------

const esc = () => dispatch($document, 'keydown', { key: 'Escape' });

let e = esc();
eq(chat._debugChatState().replyToId, null, 'Esc: цель снята');
eq(chat._debugChatState().replyBarVisible, false, 'Esc: полоса спрятана');
eq(e.defaultPrevented, true, 'Esc: событие погашено — оно израсходовано на отмену');

// Без цели Esc обязан пройти дальше: его ждут лайтбокс, модалки и браузер.
e = esc();
eq(e.defaultPrevented, false, 'Esc: без выбранного ответа чужой Esc не глотаем');

chat.startReply(m7, dialog);
e = dispatch($document, 'keydown', { key: 'Enter' });
eq(chat._debugChatState().replyToId, 7, 'Esc: другая клавиша ответ не снимает');

// Гард на лайтбокс: обработчик ответа зарегистрирован РАНЬШЕ медийного
// (initChat идёт до initMedia), и без проверки один Esc закрывал бы полосу
// вместо картинки.
$lightbox.hidden = false;
e = esc();
eq(chat._debugChatState().replyToId, 7, 'Esc: при открытом лайтбоксе ответ остаётся выбранным');
eq(chat._debugChatState().replyBarVisible, true, 'Esc: полоса ответа при этом никуда не делась');
// Esc израсходован лайтбоксом — он и закрылся. Это и есть доказательство, что
// приоритет отдан ему, а не полосе: оба слушателя висят на одном документе.
eq($lightbox.hidden, true, 'Esc: закрылся лайтбокс — обработчики ответа его пропустили');

const focusBeforeCancel = $input.focusCount;
dispatch(bar().querySelector('.tg-reply-cancel'), 'click', {});
eq(chat._debugChatState().replyToId, null, '✕: цель снята кнопкой');
eq(chat._debugChatState().replyBarVisible, false, '✕: полоса спрятана');
ok($input.focusCount > focusBeforeCancel, '✕: фокус возвращён в поле ввода');

eq(chat.cancelReply(), false, 'отмена: повторная отмена идемпотентна и сообщает «нечего снимать»');

// ---------------------------------------------------------------------------
// 3. Двойной клик по строке
// ---------------------------------------------------------------------------

const row7 = rowOf(7);
ok(!!row7, 'строка: сообщение #7 в ленте найдено');
const text7 = row7.querySelector('.tg-line-text');

dispatch($log, 'dblclick', { target: text7 });
eq(chat._debugChatState().replyToId, 7, 'двойной клик: по тексту строки — ответ на неё');
chat.cancelReply();

// Кнопки бота отправляют callback на КАЖДЫЙ клик: двойной по ним — два
// callback'а, и ещё ответ третьим действием был бы перебор.
const kb = makeEl('div');
kb.className = 'tg-kb';
const kbBtn = makeEl('button');
kb.appendChild(kbBtn);
row7.appendChild(kb);
dispatch($log, 'dblclick', { target: kbBtn });
eq(chat._debugChatState().replyToId, null, 'двойной клик: кнопка бота ответ не начинает');

// Ссылка открывается, а не отвечается.
const link = makeEl('a');
link.setAttribute('href', 'https://example.com');
row7.appendChild(link);
dispatch($log, 'dblclick', { target: link });
eq(chat._debugChatState().replyToId, null, 'двойной клик: ссылка ответ не начинает');

// Нативные controls <video> ретаргетят события на сам элемент плеера.
const player = makeEl('video');
player.className = 'tg-media-player';
row7.appendChild(player);
dispatch($log, 'dblclick', { target: player });
eq(chat._debugChatState().replyToId, null, 'двойной клик: перемотка в плеере ответ не начинает');
kb.remove(); link.remove(); player.remove();

// Мимо строк (спейсер, разделитель дат) и по системной строке — ничего:
// у системных строк нет `_tg`, и отвечать там не на что.
dispatch($log, 'dblclick', { target: $spacer });
eq(chat._debugChatState().replyToId, null, 'двойной клик: мимо строк — ничего');
chat.systemLine('reconnected');
dispatch($log, 'dblclick', { target: lastLine() });
eq(chat._debugChatState().replyToId, null, 'двойной клик: системная строка не отвечается');

// ---------------------------------------------------------------------------
// 4. Кнопка «ответить» на строке
// ---------------------------------------------------------------------------

const replyBtn = row7.querySelector('.tg-reply-btn');
ok(!!replyBtn, 'кнопка: у подтверждённой строки кнопка «ответить» есть');
eq(replyBtn.tagName, 'button', 'кнопка: это button — значит доступна с клавиатуры и табом');
eq(replyBtn.getAttribute('aria-label'), 'Ответить: sofia', 'кнопка: подписана для скринридера');
ok(row7.children[row7.children.length - 1] === replyBtn,
    'кнопка: последним потомком — она позиционируется абсолютно и в потоке рвала бы текст');
const btnEvent = dispatch(replyBtn, 'click', {});
eq(chat._debugChatState().replyToId, 7, 'кнопка: клик выбирает цель');
eq(btnEvent.propagationStopped, true, 'кнопка: клик не всплывает в делегированные обработчики ленты');
chat.cancelReply();

// ---------------------------------------------------------------------------
// 5. Цитата в строке и переход к оригиналу
// ---------------------------------------------------------------------------

const m9 = {
    id: 9, dialog_id: '777000', out: false, date: 1700000200, text: 'ответ', media: null,
    sender: { id: '4242', username: 'sofia' },
    reply_to: {
        message_id: 7, sender: 'sofia', sender_id: '4242', text: 'старое сообщение',
        media_kind: null, quote: null, out: false, unavailable: false,
    },
    reply_to_id: 7,
};
// Ответ на сторис: оригинала не существует в природе, «перейти» некуда.
const m10 = {
    id: 10, dialog_id: '777000', out: false, date: 1700000300, text: 'про сторис', media: null,
    reply_to: { message_id: null, unavailable: true },
};
tgState.messages.set('777000', [m7, m8, m9, m10]);
chat.renderHistory([m7, m8, m9, m10], dialog);

const quote9 = rowOf(9).querySelector('.tg-reply-quote');
ok(!!quote9, 'цитата: у ответа нарисована');
ok(rowOf(9).children[0] === quote9, 'цитата: первым потомком — «на что отвечают» читается раньше ответа');
eq(quote9.querySelector('.tg-reply-quote-name').textContent, 'sofia', 'цитата: имя автора оригинала');
eq(quote9.querySelector('.tg-reply-quote-text').textContent, 'старое сообщение', 'цитата: превью');
eq(quote9.dataset.replyTo, '7', 'цитата: id оригинала — хук перехода');
eq(quote9.getAttribute('role'), 'button', 'цитата: объявлена кнопкой');
eq(quote9.tabIndex, 0, 'цитата: в таб-порядке — переход доступен с клавиатуры');

const quote10 = rowOf(10).querySelector('.tg-reply-quote');
ok(!!quote10, 'цитата: ответ на сторис тоже рисует цитату, а не пустоту');
ok(!('replyTo' in quote10.dataset), 'цитата: у сторис id нет — курсор не обещает переход');
eq(quote10.getAttribute('role'), null, 'цитата: у сторис нет роли кнопки — нажимать нечего');

// Оригинал в ленте: переход = прокрутка + подсветка.
dispatch($log, 'click', { target: quote9.querySelector('.tg-reply-quote-text') });
eq(chat._debugChatState().flashId, 7, 'переход: оригинал найден в ленте и подсвечен');
ok(rowOf(7).classList.contains('tg-msg-highlight'), 'переход: класс подсветки на строке оригинала');

// Подсветка живёт по id: перерисовка ленты (подгрузка истории вверх) не должна
// её терять — иначе класс исчезает ровно в тот момент, когда его ждут.
chat.renderHistory([m7, m8, m9, m10], dialog);
ok(rowOf(7).classList.contains('tg-msg-highlight'),
    'переход: подсветка переживает перерисовку ленты (хранится по id, а не по узлу)');

// Enter на цитате — то же, что клик (иначе переход недоступен с клавиатуры).
dispatch($log, 'click', { target: quote10 });
eq(chat._debugChatState().flashId, 7, 'переход: клик по цитате без id ничего не делает');
const keyEv = dispatch($log, 'keydown', { key: 'Enter', target: quote9 });
eq(keyEv.defaultPrevented, true, 'переход: Enter на цитате обработан');
const spaceEv = dispatch($log, 'keydown', { key: ' ', target: quote9 });
eq(spaceEv.defaultPrevented, true, 'переход: пробел на цитате не прокручивает ленту, а переходит');
eq(dispatch($log, 'keydown', { key: 'a', target: quote9 }).defaultPrevented, false,
    'переход: обычная клавиша на цитате не перехватывается');

// Оригинал ВНЕ ленты: сервер отдаёт его одним запросом, показываем строкой.
netCalls.length = 0;
netHandler = async () => jsonRes(200, {
    id: 3, dialog_id: '777000', out: false, date: 1699999000, text: 'очень старое',
    sender: { id: '4242', username: 'sofia' }, media: null,
});
const farQuote = makeEl('div');
farQuote.className = 'tg-reply-quote';
farQuote.dataset.replyTo = '3';
rowOf(9).appendChild(farQuote);
dispatch($log, 'click', { target: farQuote });
await tick(); await tick();
ok(netCalls.some((c) => c.url.includes('/messages/3')),
    'переход: оригинал вне ленты запрашивается точечно, а не страницами истории');
eq(lastLine().textContent, '>> ↳ #3 sofia: очень старое',
    'переход: недостижимый оригинал показан строкой лога');

// Удалённый оригинал: 404 unknown_message — конечное состояние, а не сбой сети.
netHandler = async () => jsonRes(404, { error: { code: 'unknown_message', message: 'gone' } });
dispatch($log, 'click', { target: farQuote });
await tick(); await tick();
eq(lastLine().textContent, '>> сообщение недоступно: #3 (удалено)',
    'переход: удалённый оригинал объясняется человеческим текстом');

// Нетерпеливый двойной клик по цитате не должен множить запросы: Telegram
// отвечает на такое FLOOD_WAIT'ом, а пользователь всё равно ждёт один переход.
let releaseJump;
netHandler = () => new Promise((r) => {
    releaseJump = () => r(jsonRes(404, { error: { code: 'unknown_message', message: 'gone' } }));
});
netCalls.length = 0;
dispatch($log, 'click', { target: farQuote });
dispatch($log, 'click', { target: farQuote });
await tick();
eq(netCalls.filter((c) => c.url.includes('/messages/3')).length, 1,
    'переход: второй клик поверх незавершённого перехода запроса не плодит');
// Отпускаем запрос — иначе гард `jumping` остался бы поднятым навсегда и
// следующий сценарий проверял бы не то, что думает.
releaseJump();
await tick(); await tick();

// Любая другая ошибка обязана отличаться от «удалено»: повтор тут осмыслен.
netHandler = async () => jsonRes(503, { error: { code: 'not_connected', message: 'no session' } });
dispatch($log, 'click', { target: farQuote });
await tick(); await tick();
eq(lastLine().textContent, '>> не удалось открыть #3: no session',
    'переход: обрыв связи не выдаётся за удаление сообщения');
farQuote.remove();

// ---------------------------------------------------------------------------
// 6. Отправка текста с ответом
// ---------------------------------------------------------------------------

const sentDto = (id, replyToId) => ({
    message: {
        id, dialog_id: '777000', out: true, date: 1700000400, text: 'ответ мой',
        media: null, reply_to_id: replyToId, reply_to: null, edit_date: null,
    },
});

chat.startReply(m7, dialog);
netCalls.length = 0;
// Отложенный ответ сервера: между отправкой и подтверждением есть окно, в
// котором пользователь уже должен видеть свою цитату.
let releaseSend;
netHandler = () => new Promise((r) => { releaseSend = () => r(jsonRes(200, sentDto(11, 7))); });
const sending = chat.send('ответ мой');
const optimistic = lastLine();
ok(optimistic.classList.contains('tg-msg-pending'), 'отправка: optimistic-строка появилась сразу');
eq(optimistic.querySelector('.tg-reply-quote').querySelector('.tg-reply-quote-text').textContent,
    'старое сообщение', 'отправка: цитата видна ДО подтверждения сервером');
ok(optimistic.querySelector('.tg-reply-btn') === null,
    'отправка: у неподтверждённой строки кнопки «ответить» нет — id ещё неизвестен');
eq(chat._debugChatState().replyBarVisible, true,
    'отправка: полоса держится, пока сервер не подтвердил — иначе [retry] отвечал бы в пустоту');
releaseSend();
await sending;
eq(netCalls[0].body.reply_to_id, 7, 'отправка: reply_to_id ушёл в теле запроса');
eq(netCalls[0].body.text, 'ответ мой', 'отправка: текст ушёл вместе с ответом');
eq(chat._debugChatState().replyToId, null, 'отправка: подтверждение снимает полосу');
eq(chat._debugChatState().replyBarVisible, false, 'отправка: полоса спрятана после успеха');

// Ошибка сети: цель ОБЯЗАНА пережить провал — по [retry] она нужна целой.
chat.startReply(m7, dialog);
netCalls.length = 0;
netHandler = async () => { throw new Error('network down'); };
await chat.send('не дойдёт');
eq(chat._debugChatState().replyToId, 7, 'ошибка: выбранный ответ пережил провал отправки');
eq(chat._debugChatState().replyBarVisible, true, 'ошибка: полоса осталась на экране');
ok(lastLine().textContent.startsWith('>> ERR: не отправлено'), 'ошибка: красная строка в ленте');
ok(!!lastLine().querySelector('.tg-retry-btn'), 'ошибка: кнопка [retry] на месте');

// Повтор уходит в ТО ЖЕ сообщение, даже если полосу успели перецелить.
netCalls.length = 0;
netHandler = async () => jsonRes(200, sentDto(12, 7));
chat.startReply(m8, dialog);
eq(chat._debugChatState().replyToId, 8, 'повтор: полоса перецелена на другое сообщение');
const retryBtn = lastLine().querySelector('.tg-retry-btn');
dispatch(retryBtn, 'click', {});
await tick(); await tick();
eq(netCalls[0].body.reply_to_id, 7, 'повтор: [retry] отвечает туда же, куда собирался пользователь');
eq(chat._debugChatState().replyToId, 8,
    'повтор: чужую цель повтор не снимает — она относится к другому сообщению');

// Явный null — «отправить БЕЗ ответа»: так повтор не подхватывает чужую цель.
netCalls.length = 0;
netHandler = async () => jsonRes(200, sentDto(13, null));
await chat.send('без ответа', null);
eq('reply_to_id' in netCalls[0].body, false, 'отправка: явный null не кладёт reply_to_id в тело');
eq(chat._debugChatState().replyToId, 8, 'отправка: сообщение без ответа не гасит активную полосу');

// ---------------------------------------------------------------------------
// 7. Автосброс цели: удаление оригинала, смена диалога, logout
// ---------------------------------------------------------------------------

chat.startReply(m7, dialog);
chat.onDeleted({ dialog_id: '777000', message_ids: [8] });
eq(chat._debugChatState().replyToId, 7, 'удаление: снос ЧУЖОГО сообщения цель не трогает');
chat.onDeleted({ dialog_id: '777000', message_ids: [7] });
eq(chat._debugChatState().replyToId, null, 'удаление: снос цели снимает полосу — отвечать нечем');
eq(chat._debugChatState().replyBarVisible, false, 'удаление: полоса спрятана');

// Смена диалога: цель принадлежит покинутому чату.
tgState.messages.set('777000', [m7]);
chat.renderHistory([m7], dialog);
chat.startReply(m7, dialog);
eq(reply.replyTargetId('777000'), 7, 'смена диалога: цель есть до перехода');
netHandler = async () => jsonRes(200, { messages: [], has_more: false });
await chat.openDialog('999', { id: '999', title: 'Other', kind: 'user' });
eq(chat._debugChatState().replyToId, null, 'смена диалога: цель сброшена');
eq(chat._debugChatState().replyBarVisible, false, 'смена диалога: полоса спрятана');
eq(reply.replyTargetId('777000'), null, 'смена диалога: старый чат тоже больше не отдаёт цель');
eq(chat._debugChatState().flashId, null, 'смена диалога: подсветка от чужого перехода снята');

// Logout.
tgState.activeDialogId = '777000';
tgState.messages.set('777000', [m7]);
chat.renderHistory([m7], dialog);
chat.startReply(m7, dialog);
chat.resetChat();
eq(chat._debugChatState().replyToId, null, 'logout: цель ответа не переживает смену аккаунта');
eq(chat._debugChatState().replyBarVisible, false, 'logout: полоса спрятана');

// ---------------------------------------------------------------------------
// 8. Отправка МЕДИА с активной цитатой
// ---------------------------------------------------------------------------

const file = (name, size) => ({ name, size, type: '' });
const queued = () => upload._debugUploadState();

tgState.activeDialogId = '777000';
tgState.messages.set('777000', [m7, m8]);
chat.renderHistory([m7, m8], dialog);
$input.value = '';

chat.startReply(m7, dialog);
eq(upload.sendFiles([file('a.jpg', 1024)]), 1, 'медиа: файл принят в очередь');
eq(queued().replies, [7], 'медиа: файл уходит с reply_to_id из полосы');
eq(chat._debugChatState().replyToId, null, 'медиа: принятый файл гасит полосу');
eq(chat._debugChatState().replyBarVisible, false, 'медиа: полоса спрятана после отправки файла');
const uploadRow = $log.children.find((c) => c.dataset.replyToId === '7');
ok(!!uploadRow, 'медиа: карточка отправки помечена целью ответа (хук для цитаты)');
upload.teardownUpload();

// Пачка файлов: подпись достаётся первому, а ОТВЕТ — всем. Три файла в ответ
// на одно сообщение и должны выглядеть как три ответа.
chat.startReply(m7, dialog);
eq(upload.sendFiles([file('a.jpg', 10), file('b.jpg', 10), file('c.jpg', 10)]), 3,
    'медиа: принята вся пачка');
eq(queued().replies, [7, 7, 7], 'медиа: reply_to_id достаётся каждому файлу пачки');
upload.teardownUpload();

// Цель фиксируется в момент постановки в очередь, а не перед отправкой:
// очередь строго последовательная, и пока до второго файла дойдёт ход, полоса
// давно показывает другое сообщение. Здесь это видно буквально — первый файл
// ещё заливается, а второй встал в очередь уже с другой целью.
chat.startReply(m7, dialog);
upload.sendFiles([file('first.jpg', 10)]);
chat.startReply(m8, dialog);
upload.sendFiles([file('second.jpg', 10)]);
eq(queued().replies, [7, 8], 'медиа: каждый файл помнит СВОЮ цель, а не текущую полосу');
upload.teardownUpload();

// На карточку ещё не отправленного файла ответить нельзя: id у неё появится
// только после подтверждения Telegram.
chat.cancelReply();
upload.sendFiles([file('pending.jpg', 10)]);
const cardRow = $log.children[$log.children.length - 1];
ok(cardRow.classList.contains('tg-line') && !!cardRow.querySelector('.tg-line-text'),
    'медиа: карточка отправки — обычная строка ленты, двойной клик по ней доходит до обработчика');
dispatch($log, 'dblclick', { target: cardRow.querySelector('.tg-line-text') });
eq(chat._debugChatState().replyToId, null, 'медиа: двойной клик по карточке отправки ответ не начинает');
upload.teardownUpload();

// Всё отвергнуто до сети — терять выбранный ответ пользователю не за что.
chat.startReply(m7, dialog);
eq(upload.sendFiles([file('empty.bin', 0)]), 0, 'медиа: пустой файл отсекается до сети');
eq(chat._debugChatState().replyToId, 7, 'медиа: отвергнутый файл цель ответа не снимает');
eq(queued().queued, 0, 'медиа: очередь пуста');

// Явный opts.replyToId — это вызов из кода, а не полоса: гасить чужую полосу
// по нему нельзя.
eq(upload.sendFiles([file('d.jpg', 10)], { replyToId: 99, caption: 'x' }), 1,
    'медиа: явная цель принимает файл');
eq(queued().replies, [99], 'медиа: явный replyToId сильнее полосы');
eq(chat._debugChatState().replyToId, 7, 'медиа: явная цель полосу не гасит');
upload.teardownUpload();

// Цель из ДРУГОГО диалога не имеет права уехать вместе с файлом.
chat.startReply(m7, dialog);
tgState.activeDialogId = '999';
tgState.messages.set('999', []);
eq(upload.sendFiles([file('e.jpg', 10)]), 1, 'медиа: файл в другом чате принят');
eq(queued().replies, [null], 'медиа: цель чужого диалога с файлом не уходит');
upload.teardownUpload();
tgState.activeDialogId = '777000';

// Подпись из поля ввода уходит вместе с ответом — одно сообщение, а не два.
$input.value = 'подпись';
chat.startReply(m7, dialog);
eq(upload.sendFiles([file('f.jpg', 10)]), 1, 'медиа: файл с подписью принят');
eq(queued().replies, [7], 'медиа: подпись и ответ уживаются в одном сообщении');
eq($input.value, '', 'медиа: поле ввода очищено — текст ушёл подписью');
upload.teardownUpload();

// ---------------------------------------------------------------------------
// Итог
// ---------------------------------------------------------------------------

chat.resetChat();
upload.teardownUpload();

console.log(`\n${checks - failures}/${checks} ассертов прошло (frontend/telegram_reply_ui)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
