// Регресс-тест клавиатурной навигации по панели Telegram (keynav.js).
//
// Чистая логика комбинаций покрыта соседним telegram_keys.test.mjs (там keys.js
// грузится вообще без DOM). Здесь проверяется вторая половина фичи, которую
// чистыми функциями не поймать: курсор по чатам и сообщениям, выполнение
// действий, шпаргалка, гейты (поле ввода, лайтбокс, модалка, выключенная
// настройка) и снятие слушателя при teardown.
//
// Почему свой процесс и свой фейковый DOM — причина та же, что у
// telegram_reply_ui.test.mjs: `core/dom.js` резолвит ref'ы ОДИН раз на верхнем
// уровне модуля, поэтому стаб `document` обязан стоять раньше первого импорта.
// Стенд здесь богаче соседнего: keynav ходит по дереву составными селекторами
// (`#tg-dialog-list .tg-dialog`), спрашивает `document.activeElement` и вешает
// слушатель в capture-фазе — всё это пришлось реализовать.
//
// Запуск: node tmux-web/tests/frontend/telegram_keynav.test.mjs
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

// ── дерево панели (упрощённая копия index.html) ──
const $body = makeEl('body');
const $panel = el('telegram');
const $contacts = el('tg-contacts', 'aside');
const $dirList = el('tg-dir-list', 'ul');
const $dialogList = el('tg-dialog-list', 'ul');
const $channelList = el('tg-channel-list', 'ul');
const $log = el('tg-log');
const $composer = makeEl('div');
const $prompt = el('tg-prompt', 'form');
const $input = el('tg-input', 'input');
const $send = el('tg-send', 'button');
const $attach = el('tg-attach', 'button');
const $theme = el('tg-theme-toggle', 'button');
const $lightbox = el('tg-lightbox');
const $login = el('tg-login');
el('tg-session-head');
el('tg-file-input', 'input');
el('tg-dropzone');
el('tg-lightbox-img', 'img');
el('tg-lightbox-close', 'button');
el('tg-lightbox-caption');
el('tg-typing');

$lightbox.hidden = true;
$login.hidden = true;
$panel.appendChild($contacts);
$contacts.appendChild($dirList);
$contacts.appendChild($dialogList);
$contacts.appendChild($channelList);
$panel.appendChild($log);
$panel.appendChild($composer);
$panel.appendChild($login);
$panel.appendChild($lightbox);
$composer.appendChild($prompt);
$prompt.appendChild($input);
$prompt.appendChild($send);
$prompt.appendChild($attach);
$panel.appendChild($theme);
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

const base = new URL('../../static/js/telegram/', import.meta.url).href;
const keynav = await import(`${base}keynav.js`);
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

/** Строка чата в колонке (как её строит dialogs.js::buildRow). */
function addDialog(list, id, cls, unreadText) {
    const li = makeEl('li');
    li.className = cls;
    li.dataset.dialogId = String(id);
    li.tabIndex = 0;
    const unread = makeEl('span');
    unread.className = 'tg-dialog-unread';
    unread.textContent = unreadText || '';
    li.appendChild(unread);
    li._tg = { unread };
    li.addEventListener('click', () => { opened.push(String(id)); });
    list.appendChild(li);
    return li;
}

/** Строка сообщения в ленте (как её строит chat.js::buildMessageRow). */
function addMessage(id, text, opts) {
    const li = makeEl('li');
    li.className = 'tg-line';
    const textEl = makeEl('span');
    textEl.className = 'tg-line-text';
    textEl.textContent = text;
    li.appendChild(textEl);
    li._tg = {
        text: textEl,
        suffix: makeEl('span'),
        msg: {
            id,
            dialog_id: String(tgState.activeDialogId || '100'),
            text,
            out: false,
            date: 1700000000,
            sender: 'Вася',
        },
    };
    if (opts && opts.media) {
        const card = makeEl('div');
        card.className = 'tg-media-card';
        const body = makeEl('div');
        body.className = 'tg-media-body';
        card.appendChild(body);
        li.appendChild(card);
        li._tg.card = card;
        li._tg.msg.media = { kind: 'photo', size: 100 };
    }
    if (opts && opts.button) {
        const kb = makeEl('div');
        kb.className = 'tg-kb';
        const btn = makeEl('button');
        btn.className = 'tg-kb-btn';
        btn.textContent = 'кнопка';
        kb.appendChild(btn);
        li.appendChild(kb);
        li._tg.kb = kb;
    }
    if (opts && opts.quote) {
        const q = makeEl('div');
        q.className = 'tg-reply-quote';
        q.dataset.replyTo = String(opts.quote);
        q.addEventListener('click', () => { jumped.push(Number(opts.quote)); });
        li.appendChild(q);
    }
    $log.appendChild(li);
    return li;
}

function messageRows() {
    return $log.querySelectorAll('.tg-line');
}

function cursorNode() {
    return $panel.querySelector('.' + keynav.KB_CURSOR_CLASS);
}

/** id сообщения под курсором (или null). */
function cursorMsgId() {
    const node = cursorNode();
    return node && node._tg && node._tg.msg ? node._tg.msg.id : null;
}

/** id чата под курсором (или null). */
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

tgState.activeDialogId = '100';
tgState.dialogs = [
    { id: '100', title: 'Первый', kind: 'user' },
    { id: '200', title: 'Второй', kind: 'user' },
];

const $d100 = addDialog($dialogList, '100', 'tg-dialog', '');
const $d200 = addDialog($dialogList, '200', 'tg-dialog', '3');
const $d300 = addDialog($channelList, '300', 'tg-channel', '');

const $dirInbox = makeEl('li');
$dirInbox.className = 'tg-dir active';
$dirInbox.dataset.dir = 'inbox';
$dirInbox.addEventListener('click', () => { dirClicks.push('inbox'); });
const $dirArchive = makeEl('li');
$dirArchive.className = 'tg-dir';
$dirArchive.dataset.dir = 'archive';
$dirArchive.addEventListener('click', () => { dirClicks.push('archive'); });
$dirList.appendChild($dirInbox);
$dirList.appendChild($dirArchive);
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
userSettings.telegram_hotkeys_enabled = false;
eq(keynav.hotkeysEnabled(), false, 'гейт: тумблер выключает');
userSettings.telegram_hotkeys_enabled = true;
eq(keynav.hotkeysEnabled(), true, 'гейт: тумблер включает обратно');
delete userSettings.telegram_hotkeys_enabled;
eq(keynav.hotkeysEnabled(), true, 'гейт: отсутствие поля = включено (дефолт backend)');

eq(keynav.panelVisible(), true, 'гейт: панель видна');
$panel.hidden = true;
eq(keynav.panelVisible(), false, 'гейт: скрытая панель');
eq(keynav.telegramOwnsKeyboard(), false, 'гейт: скрытая панель клавиатурой не владеет');
$panel.hidden = false;

// Оверлей логина закрывает панель целиком — навигация по чатам там бессмысленна.
$login.hidden = false;
eq(keynav.panelVisible(), false, 'гейт: открытый логин = панель не готова');
$login.hidden = true;

activeElement = null;
eq(keynav.telegramOwnsKeyboard(), true, 'гейт: фокус нигде — панель владеет');
$log.focus();
eq(keynav.telegramOwnsKeyboard(), true, 'гейт: фокус внутри панели — владеет');
// Ключевое: уйдя фокусом в сайдбар сессий, пользователь возвращает себе vim.
const $outside = makeEl('div');
$body.appendChild($outside);
$outside.focus();
eq(keynav.telegramOwnsKeyboard(), false, 'гейт: фокус вне панели — НЕ владеет');
activeElement = null;
userSettings.telegram_hotkeys_enabled = false;
eq(keynav.telegramOwnsKeyboard(), false, 'гейт: выключённые хоткеи не владеют клавиатурой');
delete userSettings.telegram_hotkeys_enabled;

// Контракт для hotkeys.js обязан быть опубликован — иначе vim не узнает,
// что должен молчать.
ok(globalThis.window.ForgeApp.telegramKeys, 'контракт: ForgeApp.telegramKeys опубликован');
eq(typeof globalThis.window.ForgeApp.telegramKeys.owns, 'function', 'контракт: owns() есть');
eq(typeof globalThis.window.ForgeApp.telegramKeys.invalidate, 'function', 'контракт: invalidate() есть');

// ---------------------------------------------------------------------------
// 2. Курсор по чатам
// ---------------------------------------------------------------------------

press('KeyD', 'd');
eq(cursorDialogId(), '100', 'чаты: d ставит курсор на активный чат');
eq(keynav._debugKeynavState().activeZone, 'dialogs', 'чаты: зона — список чатов');

press('KeyJ', 'j');
eq(cursorDialogId(), '200', 'чаты: j — следующий');
press('KeyJ', 'j');
eq(cursorDialogId(), '300', 'чаты: j доходит до списка каналов (обе колонки — один список)');
press('KeyJ', 'j');
eq(cursorDialogId(), '300', 'чаты: на последнем j никуда не уводит');
press('KeyK', 'k');
eq(cursorDialogId(), '200', 'чаты: k — предыдущий');

// Курсор хранится идентификатором, а не ссылкой: после перерисовки колонки
// (dialogs.js пересоздаёт <li> целиком) он обязан найтись заново.
const stale = $d200;
detach($d200);
const $d200b = addDialog($dialogList, '200', 'tg-dialog', '3');
// Порядок восстанавливаем: новый узел ушёл в конец списка.
detach($d200b);
$dialogList.insertBefore($d200b, $d300);
press('KeyJ', 'j');
eq(cursorDialogId(), '300', 'чаты: после перерисовки курсор нашёлся по id и шагнул дальше');
eq($panel.querySelectorAll('.' + keynav.KB_CURSOR_CLASS).length, 1,
    'чаты: подсвечена ровно одна строка, дублей после перерисовки нет');
ok(stale.parentNode === null, 'чаты: старая строка действительно выброшена из дерева');

press('KeyK', 'k');
eq(cursorDialogId(), '200', 'чаты: вернулись на пересозданную строку');
const beforeOpen = opened.length;
press('Enter', 'Enter');
eq(opened.length, beforeOpen + 1, 'чаты: Enter кликает по строке');
eq(opened[opened.length - 1], '200', 'чаты: открылся именно чат под курсором');

// Непрочитанные.
press('KeyD', 'd');
press('KeyU', 'u');
eq(cursorDialogId(), '200', 'чаты: u ведёт к непрочитанному');
// Единственный непрочитанный: повторное нажатие оставляет курсор на нём
// (обход по кругу), а не сбрасывает в никуда.
press('KeyU', 'u');
eq(cursorDialogId(), '200', 'чаты: u по кругу возвращается к тому же');

// Папки.
const dirBefore = dirClicks.length;
press('BracketRight', ']');
eq(dirClicks.length, dirBefore + 1, 'папки: ] переключает');
eq(dirClicks[dirClicks.length - 1], 'archive', 'папки: ] идёт к следующей');
$dirArchive.classList.add('active');
$dirInbox.classList.remove('active');
press('BracketLeft', '[');
eq(dirClicks[dirClicks.length - 1], 'inbox', 'папки: [ идёт к предыдущей');
$dirInbox.classList.add('active');
$dirArchive.classList.remove('active');

// ---------------------------------------------------------------------------
// 3. Курсор по сообщениям
// ---------------------------------------------------------------------------

press('KeyM', 'm');
eq(cursorMsgId(), 3, 'лента: m ставит курсор на последнее сообщение');
eq(keynav._debugKeynavState().activeZone, 'log', 'лента: зона — лента');
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
press('KeyG', 'G', { shift: true });
eq($log.scrollTop, $log.scrollHeight, 'лента: ⇧G прокручивает в самый низ');
press('KeyJ', 'j');
eq(cursorMsgId(), 3, 'лента: после ⇧G курсор тоже внизу — j не возвращает наверх');

// g — подгрузка истории выше (прокрутка вверх).
$log.scrollTop = 800;
press('KeyG', 'g');
ok($log.scrollTop < 800, 'лента: g прокручивает вверх');

// ---------------------------------------------------------------------------
// 4. Действия над сообщением
// ---------------------------------------------------------------------------

press('KeyM', 'm');
eq(cursorMsgId(), 3, 'действия: курсор на последнем');
press('KeyR', 'r');
{
    const target = reply.replyTarget('100');
    ok(target !== null, 'ответ: r выбрал цель ответа');
    eq(target && target.messageId, 3, 'ответ: цель — сообщение под курсором');
}
// Фокус после r обязан уйти в поле ввода: ответ — это начало набора.
ok(activeElement === $input, 'ответ: фокус ушёл в поле ввода');
reply.clearReplyTarget();

// Копирование.
$log.focus();
press('KeyM', 'm');
const clipBefore = clipboard.length;
press('KeyY', 'y');
eq(clipboard.length, clipBefore + 1, 'копирование: y положил текст в буфер');
eq(clipboard[clipboard.length - 1], 'третье', 'копирование: скопирован текст сообщения под курсором');

// Цитата.
resetLog();
addMessage(10, 'без цитаты');
const $withQuote = addMessage(11, 'с цитатой', { quote: 10 });
press('KeyM', 'm');
eq(cursorMsgId(), 11, 'цитата: курсор на сообщении с цитатой');
const jumpBefore = jumped.length;
press('KeyO', 'o');
eq(jumped.length, jumpBefore + 1, 'цитата: o кликает по цитате');
eq(jumped[jumped.length - 1], 10, 'цитата: переход к нужному оригиналу');
ok($withQuote !== null, 'цитата: строка на месте');

// Медиа: Enter открывает вложение.
resetLog();
const $media = addMessage(20, 'фото', { media: true });
press('KeyM', 'm');
const $mediaBody = $media.querySelector('.tg-media-body');
const mediaBefore = $mediaBody.clickCount;
press('Enter', 'Enter');
eq($mediaBody.clickCount, mediaBefore + 1, 'медиа: Enter кликает по телу карточки');

// Кнопки бота: b фокусирует, Enter нажимает.
resetLog();
const $withBtn = addMessage(30, 'меню', { button: true });
press('KeyM', 'm');
press('KeyB', 'b');
const $btn = $withBtn.querySelector('.tg-kb-btn');
ok(activeElement === $btn, 'кнопки: b ставит фокус на кнопку сообщения');
// Enter по сфокусированной кнопке мы НЕ перехватываем: <button> браузер
// активирует сам, а свой click рядом с нативным мог бы отправить боту два
// callback'а на одно нажатие.
{
    const btnBefore = $btn.clickCount;
    const e = press('Enter', 'Enter', {}, $btn);
    eq(e.defaultPrevented, false, 'кнопки: Enter на кнопке отдан браузеру');
    eq($btn.clickCount, btnBefore, 'кнопки: своего клика не добавляем — дубля callback не будет');
}
// А вот когда фокус на строке, а не на кнопке, нажать её должны мы.
$log.focus();
press('KeyM', 'm');
{
    const btnBefore = $btn.clickCount;
    const e = press('Enter', 'Enter');
    eq(e.defaultPrevented, true, 'кнопки: Enter со строки перехвачен');
    eq($btn.clickCount, btnBefore + 1, 'кнопки: Enter со строки нажимает первую кнопку сообщения');
}

// У сообщения без кнопок — честное сообщение, а не тишина.
resetLog();
addMessage(40, 'просто текст');
press('KeyM', 'm');
const noticesBefore = notices.length;
press('KeyB', 'b');
eq(notices.length, noticesBefore + 1, 'кнопки: без кнопок показываем подсказку');

// ---------------------------------------------------------------------------
// 5. Панель: вложение, тема
// ---------------------------------------------------------------------------

{
    const before = $attach.clickCount;
    press('KeyA', 'a');
    eq($attach.clickCount, before + 1, 'панель: a открывает выбор файла');
}
{
    const before = $theme.clickCount;
    press('KeyT', 't');
    eq($theme.clickCount, before + 1, 'панель: t переключает тему');
}

// ---------------------------------------------------------------------------
// 6. Поле ввода
// ---------------------------------------------------------------------------

resetLog();
addMessage(50, 'раз');
addMessage(51, 'два');
press('KeyM', 'm');
eq(cursorMsgId(), 51, 'ввод: исходно курсор на последнем');

press('KeyI', 'i');
ok(activeElement === $input, 'ввод: i ставит фокус в поле');

// Главное правило: в поле ввода одиночные клавиши обязаны доходить до поля.
$input.tagName = 'input';
const evJ = press('KeyJ', 'j', {}, $input);
eq(evJ.defaultPrevented, false, 'ввод: j в поле НЕ перехвачен');
eq(cursorMsgId(), 51, 'ввод: курсор от j в поле не сдвинулся');
const evG = press('KeyG', 'G', { shift: true }, $input);
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
// 7. Шпаргалка
// ---------------------------------------------------------------------------

// Фокус после предыдущей секции остался в поле ввода, а там одиночные
// комбинации не перехватываются — уводим его в ленту.
$log.focus();
eq($panel.querySelectorAll('.tg-keys-help').length, 0, 'шпаргалка: изначально закрыта');
{
    // Заодно фиксируем само правило: из поля ввода «?» шпаргалку не открывает,
    // это просто знак вопроса в тексте сообщения.
    const e = press('Slash', '?', { shift: true }, $input);
    eq(e.defaultPrevented, false, 'шпаргалка: «?» в поле ввода — обычный символ');
    eq($panel.querySelectorAll('.tg-keys-help').length, 0, 'шпаргалка: из поля не открылась');
}
press('Slash', '?', { shift: true });
eq($panel.querySelectorAll('.tg-keys-help').length, 1, 'шпаргалка: ? открывает');
{
    const card = $panel.querySelector('.tg-keys-help-card');
    ok(card.textContent.includes('ГОРЯЧИЕ КЛАВИШИ'), 'шпаргалка: заголовок на месте');
    ok(card.querySelectorAll('.tg-keys-help-row').length >= 10,
        'шпаргалка: перечислены все действия');
    ok(card.textContent.includes('Настройки'),
        'шпаргалка: сказано, где переназначать');
}
press('Slash', '?', { shift: true });
eq($panel.querySelectorAll('.tg-keys-help').length, 0, 'шпаргалка: ? закрывает');
press('Slash', '?', { shift: true });
press('Escape', 'Escape');
eq($panel.querySelectorAll('.tg-keys-help').length, 0, 'шпаргалка: Escape закрывает');

// Переназначенная клавиша обязана попасть в шпаргалку — иначе она врёт.
userSettings.telegram_hotkeys = { 'msg.reply': 'Ctrl+r' };
keynav.invalidateBindings();
press('Slash', '?', { shift: true });
{
    const card = $panel.querySelector('.tg-keys-help-card');
    ok(card.textContent.includes('Ctrl'), 'шпаргалка: показывает переназначенную комбинацию');
}
press('Escape', 'Escape');

// ---------------------------------------------------------------------------
// 8. Переназначение и его применение
// ---------------------------------------------------------------------------

resetLog();
addMessage(60, 'раз');
addMessage(61, 'два');
$log.focus();
press('KeyM', 'm');
// 'r' переназначена на Ctrl+r — одиночная r больше не должна отвечать.
{
    const e = press('KeyR', 'r');
    eq(e.defaultPrevented, false, 'переназначение: освободившаяся r ничего не делает');
    eq(reply.replyTarget('100'), null, 'переназначение: ответ не выбран');
}
{
    const e = press('KeyR', 'r', { ctrl: true });
    eq(e.defaultPrevented, true, 'переназначение: Ctrl+r перехвачена');
    const target = reply.replyTarget('100');
    ok(target !== null, 'переназначение: Ctrl+r выбрала цель ответа');
}
reply.clearReplyTarget();

// Конфликт: переопределение забрало клавишу у навигации — та обязана
// замолчать, а не работать «через раз».
userSettings.telegram_hotkeys = { 'msg.copy': 'j' };
keynav.invalidateBindings();
$log.focus();
press('KeyM', 'm');
{
    const before = cursorMsgId();
    const e = press('KeyJ', 'j');
    eq(cursorMsgId(), before, 'конфликт: j больше не двигает курсор');
    eq(e.defaultPrevented, true, 'конфликт: j ушла победителю (копированию)');
}
delete userSettings.telegram_hotkeys;
keynav.invalidateBindings();

// Кэш привязок ключуется по СОДЕРЖИМОМУ карты переопределений, а не по ссылке
// на объект настроек. Отсюда важное свойство: сохранённые настройки действуют
// сразу, без перезагрузки страницы и даже без явного invalidate — тумблер и
// переназначение в модалке обязаны работать «здесь и сейчас».
userSettings.telegram_hotkeys = { 'msg.copy': 'j' };
{
    $log.focus();
    press('KeyM', 'm');
    const before = cursorMsgId();
    press('KeyJ', 'j');
    eq(cursorMsgId(), before, 'кэш: новая привязка действует сразу, без invalidate');
}
// Явный сброс — подстраховка контракта для настроек: он не должен ничего
// ломать и обязан оставить актуальные привязки.
keynav.invalidateBindings();
{
    const before = cursorMsgId();
    press('KeyJ', 'j');
    eq(cursorMsgId(), before, 'кэш: invalidate не воскрешает старую привязку');
}
delete userSettings.telegram_hotkeys;
keynav.invalidateBindings();
{
    $log.focus();
    press('KeyM', 'm');
    const before = cursorMsgId();
    press('KeyK', 'k');
    ok(cursorMsgId() !== before, 'кэш: после снятия переопределения навигация вернулась');
}

// ---------------------------------------------------------------------------
// 9. Гейты во время работы
// ---------------------------------------------------------------------------

// Выключенные хоткеи: ни одно нажатие не должно ничего делать.
userSettings.telegram_hotkeys_enabled = false;
{
    resetLog();
    addMessage(70, 'раз');
    addMessage(71, 'два');
    keynav.clearCursor();
    const e = press('KeyM', 'm');
    eq(e.defaultPrevented, false, 'выключено: нажатие не перехвачено');
    ok(cursorNode() === null, 'выключено: курсор не появился');
}
delete userSettings.telegram_hotkeys_enabled;

// Лайтбокс перекрывает панель и живёт по своим правилам.
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
// 10. Сброс и teardown
// ---------------------------------------------------------------------------

resetLog();
addMessage(80, 'раз');
press('KeyM', 'm');
ok(cursorNode() !== null, 'сброс: курсор поставлен');
keynav.resetKeynav();
ok(cursorNode() === null, 'сброс: resetKeynav снял подсветку');
eq(keynav._debugKeynavState().zone, null, 'сброс: зона очищена');

press('Slash', '?', { shift: true });
eq($panel.querySelectorAll('.tg-keys-help').length, 1, 'teardown: шпаргалка открыта перед снятием');
keynav.teardownKeynav();
eq($panel.querySelectorAll('.tg-keys-help').length, 0, 'teardown: шпаргалка закрыта');
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
eq($document.listeners.get('keydown').length, 1,
    'повторный init: слушатель по-прежнему один');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/telegram_keynav)`);
if (failures > 0) process.exit(1);
