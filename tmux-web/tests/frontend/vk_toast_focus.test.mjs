// tmux-web — вторая половина гейта тостов вкладки VK: «пользователь и так на
// это смотрит» (js/vk/main.js::maybeToast).
//
// ПОЧЕМУ ОТДЕЛЬНЫЙ ФАЙЛ, а не секция в vk_toasts.test.mjs. Ветка держится на
// ref'е `$vk` из core/dom.js, а он резолвится ОДИН раз, на верхнем уровне
// модуля (`const $vk = document.getElementById('vk')`). В соседнем тесте стаб
// отдаёт null на любой id — значит `onVkTab` там всегда false, и ветка «вкладка
// открыта на этой же беседе» не исполняется ни разу. Переимпортировать main.js
// с другим стабом внутри того же процесса нельзя: ESM-кэш отдаст ту же самую
// dom.js, а `?bust=` создаст ВТОРОЙ экземпляр state.js — обработчики окажутся
// на одном vkState, а ассерты на другом, и тест зеленел бы по случайности.
// Поэтому — свой процесс со своим document.
//
// Стаб отдаёт элемент ТОЛЬКО для id 'vk': контейнер тостов #echo-toasts обязан
// остаться null, иначе notify() нарисует тост в DOM вместо console.log, и
// считать показы будет нечем.
//
// Запуск: node tmux-web/tests/frontend/vk_toast_focus.test.mjs

const stubEl = () => ({
    hidden: false,
    textContent: '',
    className: '',
    dataset: {},
    style: {},
    classList: {
        add() {}, remove() {}, toggle() {}, contains() { return false; },
    },
    addEventListener() {}, removeEventListener() {},
    appendChild() {}, remove() {}, closest() { return null; },
    querySelector() { return null; }, querySelectorAll() { return []; },
});

/** Панель вкладки VK: её `hidden` и есть «пользователь смотрит сюда». */
const panel = stubEl();

globalThis.document = {
    getElementById: (id) => (id === 'vk' ? panel : null),
    createElement: () => stubEl(),
    createTextNode: () => ({}),
    addEventListener() {}, removeEventListener() {},
    body: stubEl(),
    documentElement: stubEl(),
    visibilityState: 'visible',
};
globalThis.location = {
    protocol: 'http:', host: 'localhost', hash: '', href: 'http://localhost/', origin: 'http://localhost',
};
globalThis.window = {
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    fetch: () => Promise.reject(new Error('offline')),
    setTimeout, clearTimeout,
    confirm: () => true,
    ForgeApp: { state: { userSettings: {} } },
};
globalThis.fetch = globalThis.window.fetch;
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

const M = await import(new URL('../../static/js/vk/main.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);

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

const shown = [];
const origLog = console.log;
console.log = (...args) => {
    if (typeof args[0] === 'string' && args[0].startsWith('[toast/')) {
        shown.push({ title: String(args[1] || ''), body: String(args[2] || '') });
        return;
    }
    origLog(...args);
};

const H = M.buildHandlers();

function message(id, dialogId, extra) {
    return Object.assign({
        id,
        dialog_id: String(dialogId),
        out: false,
        date: 1800000000,
        text: 'привет',
        sender: { id: String(dialogId), first_name: 'Пётр' },
        attachments: [],
        fwd: [],
    }, extra || {});
}

function frame(msg) {
    return { type: 'new_message', message: msg, muted: false, archived: false };
}

/** Сбрасывает троттлинг тостов между сценариями (logout делает то же самое). */
function reset() {
    shown.length = 0;
    S.vkState.dialogs = [];
    H.login_state({ state: 'not_configured' });
    S.vkState.loginState = 'authorized';
}

// ---------------------------------------------------------------------------
// 1. Вкладка VK открыта на ЭТОЙ беседе — тоста нет.
// ---------------------------------------------------------------------------

{
    reset();
    panel.hidden = false;
    S.vkState.activeDialogId = '1234';
    eq(M.maybeToast(message(1, 1234), frame(message(1, 1234))), null,
        'пользователь смотрит на эту беседу — тост лишний');
    eq(shown.length, 0, 'ни одного показа');
}

// ---------------------------------------------------------------------------
// 2. Вкладка VK открыта, но беседа ДРУГАЯ — тост нужен.
// ---------------------------------------------------------------------------

{
    reset();
    panel.hidden = false;
    S.vkState.activeDialogId = '1234';
    M.maybeToast(message(2, 4321), frame(message(2, 4321)));
    eq(shown.length, 1, 'сообщение из соседней беседы всплывает');
    ok(shown[0].title.startsWith('VK · '), `и тоже с префиксом: ${shown[0].title}`);
}

// ---------------------------------------------------------------------------
// 3. Вкладка ЗАКРЫТА — тост нужен даже по активной беседе.
// ---------------------------------------------------------------------------

{
    reset();
    panel.hidden = true;
    S.vkState.activeDialogId = '1234';
    M.maybeToast(message(3, 1234), frame(message(3, 1234)));
    eq(shown.length, 1, 'панель скрыта — «смотрит» не выполняется');

    // Именно это и есть смысл постоянного WS: пользователь сидит в terminal,
    // а уведомление обязано дойти.
    reset();
    panel.hidden = true;
    S.vkState.activeDialogId = null;
    H.new_message(frame(message(4, 5555)));
    eq(shown.length, 1, 'кадр WS доводит тост при закрытой панели');
}

// ---------------------------------------------------------------------------
// 4. Идентификаторы сравниваются как СТРОКИ.
// ---------------------------------------------------------------------------

{
    // `peer_id` беседы = 2_000_000_000 + chat_id, и контракт проекта — строки.
    // Сравнение числа со строкой молча пропустило бы тост в открытую беседу.
    reset();
    panel.hidden = false;
    S.vkState.activeDialogId = 2000000001;
    eq(M.maybeToast(message(5, '2000000001'), frame(message(5, '2000000001'))), null,
        'число и строка — одна и та же беседа');
    eq(shown.length, 0, 'ни одного показа');
}

console.log = origLog;
console.log(`vk_toast_focus: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
