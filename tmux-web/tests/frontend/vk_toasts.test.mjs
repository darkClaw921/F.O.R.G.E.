// tmux-web — гейты тостов вкладки VK (js/vk/main.js::maybeToast).
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ.
//
//   1. ПРЕФИКС «VK · ». При двух включённых панелях (Telegram и VK) тосты
//      падают в ОДИН стек `#echo-toasts`. Заголовок «Пётр Петров» без
//      префикса не отвечает на единственный вопрос, который у пользователя в
//      этот момент есть: куда идти отвечать. Это не косметика, а смысл;
//   2. ГЕЙТ MUTE держится на полях УРОВНЯ КАДРА (`muted`/`archived`), которые
//      посчитал сервер, а НЕ на локальном Set: тот собран из ЗАГРУЖЕННОЙ
//      страницы `/dialogs`, то есть знает верхушку списка, а WS вещает по
//      всему аккаунту. Именно поэтому «ненужный» чат вне страницы раньше
//      считался бы незаглушённым и исправно всплывал;
//   3. СВОЁ ИСХОДЯЩЕЕ никогда не всплывает;
//   4. ТРОТТЛИНГ режет серию сообщений из одной беседы;
//   5. БЕЛЫЙ СПИСОК: скрытая беседа не звенит — открыть её кликом по тосту
//      всё равно было бы некуда, в колонке её нет.
//
// Вторая половина гейта («пользователь и так на это смотрит») живёт в
// vk_toast_focus.test.mjs — своим процессом, см. пояснение там.
//
// Запуск: node tmux-web/tests/frontend/vk_toasts.test.mjs
//
// main.js тянет DOM-модули, поэтому document/window заглушаются ДО импорта.
// Все ref'ы core/dom.js становятся null, и рисовальщики честно уходят в ранний
// выход — проверяется ровно логика решений, без браузера. Заодно `$vk` тоже
// null, то есть ветка «вкладка открыта на этой беседе» здесь не исполняется
// НИ РАЗУ, и остальные гейты видно в чистом виде.

// ---------------- заглушки окружения ----------------

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

globalThis.document = {
    getElementById: () => null,
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
    // Сеть в тесте недоступна намеренно: обработчики обязаны переживать отказ.
    fetch: () => Promise.reject(new Error('offline')),
    setTimeout, clearTimeout,
    confirm: () => true,
    // Публичный контракт: отсюда state.js читает белый список видимости.
    ForgeApp: { state: { userSettings: {} } },
};
/** Настройки пользователя — правятся сценарием белого списка в конце файла. */
const userSettings = globalThis.window.ForgeApp.state.userSettings;
globalThis.fetch = globalThis.window.fetch;
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

const M = await import(new URL('../../static/js/vk/main.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);

// ---------------- мини-фреймворк ----------------

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

// Тост без контейнера #echo-toasts уходит в console.log — по нему и считаем
// показы: возвращаемое значение notify() в этом окружении всегда null.
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
        sender: { id: String(dialogId), first_name: 'Пётр', last_name: 'Петров' },
        attachments: [],
        fwd: [],
    }, extra || {});
}

function frame(msg, extra) {
    return Object.assign({ type: 'new_message', message: msg, muted: false, archived: false }, extra || {});
}

/** Сбрасывает троттлинг и производные карты между сценариями. */
function reset() {
    shown.length = 0;
    S.vkState.dialogs = [];
    S.vkState.activeDialogId = null;
    S.vkState.loginState = 'authorized';
    // Тем же путём, каким это делает logout: троттлинг тостов и флаги бесед.
    H.login_state({ state: 'not_configured' });
    S.vkState.loginState = 'authorized';
    S.invalidateVisibilityFilter();
}

// ---------------------------------------------------------------------------
// 1. Префикс «VK · » — главное отличие от телеграмного близнеца.
// ---------------------------------------------------------------------------

{
    reset();
    const toast = M.maybeToast(message(1, 1234), frame(message(1, 1234)));
    eq(shown.length, 1, 'чужое сообщение всплывает');
    ok(shown[0].title.startsWith('VK · '), `заголовок с префиксом: ${shown[0].title}`);
    ok(shown[0].title.includes('Пётр Петров'), `имя отправителя в заголовке: ${shown[0].title}`);
    eq(shown[0].body, 'привет', 'текст сообщения в теле');
    eq(toast, null, 'без контейнера notify() возвращает null — это ок');
    eq(M.TOAST_PREFIX, 'VK · ', 'префикс объявлен константой и экспортирован');
}

// ---------------------------------------------------------------------------
// 2. Имя: title беседы важнее имени отправителя (в группе они разные).
// ---------------------------------------------------------------------------

{
    reset();
    S.vkState.dialogs = [{ id: '2000000001', kind: 'chat', title: 'Рабочая беседа', unread_count: 0 }];
    M.recomputeUnread();
    M.maybeToast(message(2, 2000000001), frame(message(2, 2000000001)));
    eq(shown.length, 1, 'тост показан');
    ok(shown[0].title.includes('Рабочая беседа'), `title беседы: ${shown[0].title}`);
    ok(shown[0].title.startsWith('VK · '), 'префикс на месте и здесь');

    // Беседы в списке нет, имени отправителя тоже — остаётся id, но тост
    // обязан быть: молчание хуже неподписанного уведомления.
    reset();
    const anon = message(3, 555, { sender: null });
    M.maybeToast(anon, frame(anon));
    eq(shown.length, 1, 'без имени тост всё равно показывается');
    eq(shown[0].title, 'VK · 555', 'в заголовке остаётся id беседы');
}

// ---------------------------------------------------------------------------
// 3. Своё исходящее не всплывает никогда.
// ---------------------------------------------------------------------------

{
    reset();
    const own = message(4, 1234, { out: true });
    eq(M.maybeToast(own, frame(own)), null, 'исходящее не тостит');
    eq(shown.length, 0, 'ни одного показа');
}

// ---------------------------------------------------------------------------
// 4. Гейт mute держится на полях КАДРА.
// ---------------------------------------------------------------------------

{
    reset();
    // Беседы в загруженном списке НЕТ — локальный Set о ней не знает ничего.
    // Решение сервера приезжает полем кадра, и его достаточно.
    const msg = message(5, 9999);
    eq(M.maybeToast(msg, frame(msg, { muted: true })), null, 'заглушённая по кадру не тостит');
    eq(shown.length, 0, 'ни одного показа');

    // Тот же кадр без флага — тост идёт.
    M.maybeToast(msg, frame(msg));
    eq(shown.length, 1, 'незаглушённая всплывает');

    reset();
    // Кадр СТАРОГО сервера (полей нет вовсе) не имеет права превратить
    // «не знаю» в «не заглушена»: локальное знание из списка обязано сработать.
    S.vkState.dialogs = [{ id: '1234', kind: 'user', title: 'Пётр', unread_count: 0, muted: true }];
    M.recomputeUnread();
    eq(M.maybeToast(message(6, 1234), { type: 'new_message' }), null,
        'кадр без полей — гейт по локальному знанию');
    eq(shown.length, 0, 'ни одного показа');

    // …и наоборот: заглушённая локально, но незаглушённая по свежему кадру —
    // прав кадр, он новее.
    M.maybeToast(message(7, 1234), frame(message(7, 1234), { muted: false }));
    eq(shown.length, 1, 'свежее решение сервера перебивает старый список');
}

// ---------------------------------------------------------------------------
// 5. `frameFlag` — та самая развилка «не знаю» / «нет».
// ---------------------------------------------------------------------------

{
    eq(M.frameFlag(true, false), true, 'булево из кадра выигрывает');
    eq(M.frameFlag(false, true), false, 'явное false тоже выигрывает');
    eq(M.frameFlag(undefined, true), true, 'нет поля — берём локальное');
    eq(M.frameFlag(null, false), false, 'null — это тоже «не знаю»');
    eq(M.frameFlag('true', false), false, 'строка булевым не считается');
}

// ---------------------------------------------------------------------------
// 6. Троттлинг: серия из одной беседы даёт один тост.
// ---------------------------------------------------------------------------

{
    reset();
    for (let i = 0; i < 10; i += 1) {
        M.maybeToast(message(100 + i, 1234), frame(message(100 + i, 1234)));
    }
    eq(shown.length, 1, 'десять сообщений подряд — один тост');

    // Другая беседа троттлингом первой не связана.
    M.maybeToast(message(200, 4321), frame(message(200, 4321)));
    eq(shown.length, 2, 'соседняя беседа звенит своим тостом');

    eq(M.TOAST_THROTTLE_MS, 5000, 'окно троттлинга — 5 секунд');
}

// ---------------------------------------------------------------------------
// 7. Белый список бесед (полностью включается в Ф10).
// ---------------------------------------------------------------------------

{
    reset();
    // Инвариант «включён ≠ действует»: поднятый тумблер с пустыми списками
    // ничего не прячет — иначе выглядело бы как пропажа всех уведомлений.
    userSettings.vk_filter_enabled = true;
    userSettings.vk_visible_kinds = [];
    userSettings.vk_visible_peers = [];
    S.invalidateVisibilityFilter();
    M.maybeToast(message(300, 1234), frame(message(300, 1234)));
    eq(shown.length, 1, 'пустой фильтр не прячет ничего');

    // Фильтр по ТИПУ: у VK он точен даже для беседы вне загруженного списка —
    // тип выводится из самого peer_id, а не ищется в колонке.
    reset();
    userSettings.vk_filter_enabled = true;
    userSettings.vk_visible_kinds = ['chat'];
    userSettings.vk_visible_peers = [];
    S.invalidateVisibilityFilter();
    M.maybeToast(message(301, 1234), frame(message(301, 1234)));
    eq(shown.length, 0, 'личная беседа скрыта фильтром по типу «chat»');
    M.maybeToast(message(302, 2000000001), frame(message(302, 2000000001)));
    eq(shown.length, 1, 'а беседа проходит');

    // Фильтр поимённо.
    reset();
    userSettings.vk_filter_enabled = true;
    userSettings.vk_visible_kinds = [];
    userSettings.vk_visible_peers = ['1234'];
    S.invalidateVisibilityFilter();
    M.maybeToast(message(303, 4321), frame(message(303, 4321)));
    eq(shown.length, 0, 'беседа не из белого списка молчит');
    M.maybeToast(message(304, 1234), frame(message(304, 1234)));
    eq(shown.length, 1, 'названная поимённо звенит');

    // Скрытая беседа и непрочитанного копить не должна: цифра на Ⓥ вела бы
    // туда, чего в колонке нет.
    reset();
    userSettings.vk_filter_enabled = true;
    userSettings.vk_visible_kinds = ['chat'];
    userSettings.vk_visible_peers = [];
    S.invalidateVisibilityFilter();
    H.new_message(frame(message(305, 4321)));
    eq(M._debugState().pendingUnread, 0, 'скрытая беседа не копит непрочитанное');
    eq(M._debugState().unreadSinceServer, 0, 'и не двигает поправку');

    // Настройки выключили — всё снова видно, БЕЗ перезагрузки страницы.
    userSettings.vk_filter_enabled = false;
    S.invalidateVisibilityFilter();
    reset();
    M.maybeToast(message(306, 4321), frame(message(306, 4321)));
    eq(shown.length, 1, 'выключенный фильтр действует сразу');
    delete userSettings.vk_filter_enabled;
    delete userSettings.vk_visible_kinds;
    delete userSettings.vk_visible_peers;
    S.invalidateVisibilityFilter();
}

// ---------------------------------------------------------------------------
// 8. Битые входы.
// ---------------------------------------------------------------------------

{
    reset();
    eq(M.maybeToast(null, null), null, 'нет сообщения — нет тоста');
    eq(M.maybeToast({}, null), null, 'сообщение без dialog_id — нет тоста');
    eq(M.maybeToast(message(400, ''), null), null, 'пустой dialog_id — нет тоста');
    eq(shown.length, 0, 'ни одного показа на мусоре');

    // Сообщение без текста (одно вложение) подписывается заглушкой, а не
    // пустотой: пустое тело выглядит как сломанный тост.
    const media = message(401, 1234, { text: '' });
    M.maybeToast(media, frame(media));
    eq(shown.length, 1, 'медиа-сообщение всплывает');
    eq(shown[0].body, '[media]', 'тело подписано заглушкой');
}

console.log = origLog;
console.log(`vk_toasts: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
