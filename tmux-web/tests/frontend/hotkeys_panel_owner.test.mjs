// Регресс-тест гейта владения клавиатурой в static/hotkeys.js.
//
// hotkeys.js — глобальная vim-навигация по сессиям (j/k/h/l/g/1/2/3/?). Она
// занимает те же клавиши, что и панели-мессенджеры, поэтому уступает им по
// контракту `window.ForgeApp.<panel>Keys.owns()`. С появлением второй панели
// (VK) одно захардкоженное имя стало СПИСКОМ `PANEL_KEY_CONTRACTS`, и этот
// тест — единственное автоматическое покрытие той правки.
//
// Что проверяется: vim уступает клавиши, когда владеет ЛЮБАЯ панель; забирает
// их обратно, когда не владеет ни одна; не падает, если контракта ещё нет
// (модули грузятся позже этого файла) или если панель бросила исключение.
//
// hotkeys.js — классический скрипт, а не ES-модуль: он грузится через
// `import()` как CommonJS и исполняет свою IIFE, вешая слушатель на
// поддельный `document`. Наблюдаемое следствие «vim сработал» —
// `preventDefault()` на событии (см. onKeyDown: `if (vimAction(e)) e.preventDefault()`).
//
// Запуск: node tmux-web/tests/frontend/hotkeys_panel_owner.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM
// ---------------------------------------------------------------------------

let activeElement = null;

function makeEl(tag) {
    const node = {
        nodeType: 1,
        tagName: String(tag || 'div').toLowerCase(),
        childNodes: [],
        parentNode: null,
        attributes: {},
        listeners: new Map(),
        className: '',
        style: {},
        tabIndex: -1,
        focusCount: 0,
        clickCount: 0,
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
        contains: (c) => node.className.split(/\s+/).filter(Boolean).includes(c),
        toggle(c, on) {
            const has = node.classList.contains(c);
            const want = on === undefined ? !has : !!on;
            if (want) node.classList.add(c);
            else node.classList.remove(c);
            return want;
        },
    };
    // Разметку шпаргалки vim (ensureHelp) храним строкой, не парся: тест
    // смотрит только на класс `show`, а свой парсер HTML в стенде — отдельная
    // библиотека с собственными багами.
    node.innerHTML = '';
    node.appendChild = (child) => { child.parentNode = node; node.childNodes.push(child); return child; };
    node.contains = (other) => {
        let cur = other;
        while (cur) { if (cur === node) return true; cur = cur.parentNode; }
        return false;
    };
    node.matches = (sel) => {
        const s = String(sel || '');
        if (s.charAt(0) === '.') return s.slice(1).split('.').every((c) => node.classList.contains(c));
        return node.tagName === s.toLowerCase();
    };
    node.querySelectorAll = (sel) => {
        const out = [];
        const walk = (n) => n.childNodes.forEach((c) => {
            if (c.nodeType !== 1) return;
            if (c.matches(sel)) out.push(c);
            walk(c);
        });
        walk(node);
        return out;
    };
    node.querySelector = (sel) => node.querySelectorAll(sel)[0] || null;
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.removeEventListener = () => {};
    node.focus = () => { node.focusCount += 1; activeElement = node; };
    node.click = () => { node.clickCount += 1; };
    node.scrollIntoView = () => {};
    node.getBoundingClientRect = () => ({ top: 0, left: 0, width: 10, height: 10, bottom: 10, right: 10 });
    node.hasAttribute = (k) => k in node.attributes;
    node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
    return node;
}

function dispatch(node, type, ev) {
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

const els = new Map();
const $body = makeEl('body');
const $sessionList = makeEl('div');
$sessionList.attributes.id = 'session-list';
els.set('session-list', $sessionList);
$body.appendChild($sessionList);

const $s1 = makeEl('div');
$s1.className = 'session-item active';
const $s2 = makeEl('div');
$s2.className = 'session-item';
$sessionList.appendChild($s1);
$sessionList.appendChild($s2);

const $document = makeEl('#document');
$document.getElementById = (id) => els.get(id) || null;
$document.createElement = (tag) => makeEl(tag);
$document.body = $body;
$document.documentElement = makeEl('html');
$document.querySelector = (sel) => $body.querySelector(sel);
$document.querySelectorAll = (sel) => $body.querySelectorAll(sel);
Object.defineProperty($document, 'activeElement', { get: () => activeElement });
globalThis.document = $document;

const userSettings = {};
globalThis.window = {
    addEventListener() {},
    removeEventListener() {},
    getComputedStyle: () => ({ display: 'block', visibility: 'visible', opacity: '1' }),
    innerWidth: 1400,
    innerHeight: 900,
    setTimeout,
    clearTimeout,
    ForgeApp: { state: { userSettings } },
};
globalThis.getComputedStyle = globalThis.window.getComputedStyle;

// ---------------------------------------------------------------------------
// Загрузка hotkeys.js (классический скрипт, исполняется как CommonJS)
// ---------------------------------------------------------------------------

await import(new URL('../../static/hotkeys.js', import.meta.url).href);

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

/** Нажатие клавиши vim-навигации; вернёт true, если vim её обработал. */
function vimHandled(key) {
    activeElement = null;
    const e = dispatch($document, 'keydown', { key, target: $body });
    return e.defaultPrevented === true;
}

/** Контракт панели с фиксированным ответом owns(). */
function panel(owns) {
    return { owns: () => owns, enabled: () => true, invalidate() {} };
}

// ---------------------------------------------------------------------------
// 1. Ни одной панели — vim работает
// ---------------------------------------------------------------------------

globalThis.window.ForgeApp = { state: { userSettings } };
ok(vimHandled('j'), 'без панелей: j обрабатывается vim-навигацией');
ok(vimHandled('k'), 'без панелей: k обрабатывается vim-навигацией');
ok(vimHandled('?'), 'без панелей: ? открывает шпаргалку vim');
// Закрываем шпаргалку, чтобы не мешала соседним проверкам.
vimHandled('?');

// ---------------------------------------------------------------------------
// 2. Панель владеет клавиатурой — vim молчит
// ---------------------------------------------------------------------------

globalThis.window.ForgeApp.telegramKeys = panel(true);
eq(vimHandled('j'), false, 'Telegram владеет: vim не трогает j');
eq(vimHandled('k'), false, 'Telegram владеет: vim не трогает k');
globalThis.window.ForgeApp.telegramKeys = panel(false);
ok(vimHandled('j'), 'Telegram не владеет: vim снова обрабатывает j');

// VK — вторая панель в списке контрактов. Именно её пропуск (список из одного
// имени) и есть та ошибка, ради которой этот тест написан.
globalThis.window.ForgeApp.vkKeys = panel(true);
eq(vimHandled('j'), false, 'VK владеет: vim не трогает j');
eq(vimHandled('h'), false, 'VK владеет: vim не трогает h');
eq(vimHandled('1'), false, 'VK владеет: vim не переключает вкладки цифрами');
globalThis.window.ForgeApp.vkKeys = panel(false);
ok(vimHandled('j'), 'VK не владеет: vim снова обрабатывает j');

// Обе объявлены, владеет вторая — перебор обязан дойти до конца списка.
globalThis.window.ForgeApp.telegramKeys = panel(false);
globalThis.window.ForgeApp.vkKeys = panel(true);
eq(vimHandled('j'), false, 'обе объявлены, владеет VK: vim молчит');

// Обе объявлены, не владеет никто (открыт терминал) — vim наш.
globalThis.window.ForgeApp.vkKeys = panel(false);
ok(vimHandled('j'), 'обе объявлены, не владеет никто: vim работает');

// ---------------------------------------------------------------------------
// 3. Деградация: контракта нет / он сломан
// ---------------------------------------------------------------------------

// Модули панели ещё не загрузились (этот файл выполняется раньше них) —
// поведение обязано совпадать с «до фичи».
globalThis.window.ForgeApp = { state: { userSettings } };
ok(vimHandled('j'), 'нет контрактов: vim работает как до фичи');

// Контракт есть, но без owns() — мусор игнорируется, а не роняет обработчик.
globalThis.window.ForgeApp.vkKeys = { enabled: () => true };
ok(vimHandled('j'), 'контракт без owns(): пропущен, vim работает');

// owns() бросил — панель сломана, но vim молчать не обязан: неработающая
// навигация хуже, чем навигация поверх сломанной панели.
globalThis.window.ForgeApp.vkKeys = { owns: () => { throw new Error('сломано'); } };
ok(vimHandled('j'), 'owns() бросил: исключение проглочено, vim работает');

// Первая панель бросила, вторая владеет — исключение не должно обрывать перебор.
globalThis.window.ForgeApp.telegramKeys = { owns: () => { throw new Error('сломано'); } };
globalThis.window.ForgeApp.vkKeys = panel(true);
eq(vimHandled('j'), false, 'исключение в первой панели не мешает увидеть владение второй');

// owns() вернул не-true (undefined, 'yes') — владением это не считается:
// сравнение строгое, чтобы случайный truthy не отнял клавиши молча.
globalThis.window.ForgeApp.telegramKeys = panel(false);
globalThis.window.ForgeApp.vkKeys = { owns: () => 'yes' };
ok(vimHandled('j'), 'owns() вернул строку: не владение, vim работает');
globalThis.window.ForgeApp.vkKeys = { owns: () => undefined };
ok(vimHandled('j'), 'owns() вернул undefined: не владение, vim работает');

// ForgeApp отсутствует целиком — самый ранний момент загрузки страницы.
globalThis.window.ForgeApp = undefined;
ok(vimHandled('j'), 'ForgeApp отсутствует: vim работает');

// ---------------------------------------------------------------------------
// 4. РЕЕСТР (введён на третьей панели — Битрикс24)
//
// До Ф11 имена контрактов были захардкожены списком, и комментарий в
// hotkeys.js обещал реестр «когда появится третья панель». Реестр —
// `window.ForgeApp.keyboardOwners`: МАССИВ имён, в который панель дописывает
// себя сама (js/bitrix/keynav.js::registerKeyboardOwner). Два имени,
// появившихся ДО реестра, остаются засеянными в hotkeys.js: их регистрация
// потребовала бы правки работающих js/telegram/keynav.js и js/vk/keynav.js.
// ---------------------------------------------------------------------------

// Контракт есть, а в реестре имени нет — панель невидима для vim. Это ровно
// то состояние, в котором находится ЛЮБАЯ новая панель до регистрации, и
// проверка ловит забытый вызов registerKeyboardOwner().
globalThis.window.ForgeApp = { state: { userSettings } };
globalThis.window.ForgeApp.b24Keys = panel(true);
ok(vimHandled('j'), 'панель вне реестра: vim не спрашивает её и работает сам');

// Зарегистрировалась — vim уступил, файл hotkeys.js при этом не менялся.
globalThis.window.ForgeApp.keyboardOwners = ['b24Keys'];
eq(vimHandled('j'), false, 'Битрикс24 в реестре и владеет: vim молчит');
eq(vimHandled('1'), false, 'Битрикс24 владеет: vim не переключает вкладки цифрами');
globalThis.window.ForgeApp.b24Keys = panel(false);
ok(vimHandled('j'), 'Битрикс24 в реестре, но не владеет: vim работает');

// Реестр ДОПОЛНЯЕТ засев, а не заменяет его: старые панели обязаны продолжать
// работать при непустом реестре.
globalThis.window.ForgeApp.vkKeys = panel(true);
eq(vimHandled('j'), false, 'при непустом реестре засеянный vkKeys всё ещё опрашивается');
globalThis.window.ForgeApp.vkKeys = panel(false);

// Мусор в реестре не роняет опрос: не-строки, пустые строки и повторы.
globalThis.window.ForgeApp.keyboardOwners = ['b24Keys', 'b24Keys', '', '   ', 42, null, { a: 1 }];
globalThis.window.ForgeApp.b24Keys = panel(true);
eq(vimHandled('j'), false, 'мусор в реестре пропущен, зарегистрированная панель найдена');

// Реестр не массив (кто-то положил объект) — деградация до засеянных имён.
globalThis.window.ForgeApp.keyboardOwners = { b24Keys: true };
ok(vimHandled('j'), 'реестр не массив: игнорируется, поведение как до фичи');

// Имя в реестре есть, а контракта под ним нет (панель ещё грузится) —
// пропускается молча.
globalThis.window.ForgeApp = { state: { userSettings }, keyboardOwners: ['b24Keys'] };
ok(vimHandled('j'), 'имя зарегистрировано, контракта ещё нет: vim работает');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/hotkeys_panel_owner)`);
if (failures > 0) process.exit(1);
