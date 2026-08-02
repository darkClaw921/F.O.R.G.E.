// tmux-web — вторая половина гейта тостов вкладки Telegram: «пользователь и так
// на это смотрит» (js/telegram/main.js::maybeToast).
//
// Почему ОТДЕЛЬНЫЙ файл, а не секция в telegram_toasts.test.mjs. Ветка держится
// на ref'е `$telegram` из core/dom.js, а он резолвится ОДИН раз, на верхнем
// уровне модуля (`const $telegram = document.getElementById('telegram')`). В
// соседнем тесте стаб отдаёт null на любой id — значит `onTelegramTab` там
// всегда false, и ветка «вкладка открыта на этом же диалоге» просто не
// исполняется ни разу. Переимпортировать main.js с другим стабом внутри того же
// процесса нельзя: ESM-кэш отдаст ту же самую dom.js (а `?bust=` создаст ВТОРОЙ
// экземпляр state.js — обработчики окажутся на одном tgState, а ассерты на
// другом, и тест будет зелёным по случайности). Поэтому — свой процесс со своим
// document.
//
// Стаб отдаёт элемент ТОЛЬКО для id 'telegram': контейнер тостов #echo-toasts
// обязан остаться null, иначе notify() нарисует тост в DOM вместо console.log,
// и считать показы будет нечем.
//
// Запуск: node tmux-web/tests/frontend/telegram_toast_focus.test.mjs

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

/** Панель вкладки Telegram: её `hidden` и есть «пользователь смотрит сюда». */
const panel = stubEl();

globalThis.document = {
    getElementById: (id) => (id === 'telegram' ? panel : null),
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
};
globalThis.fetch = globalThis.window.fetch;
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

const M = await import(new URL('../../static/js/telegram/main.js', import.meta.url).href);
const { tgState } = await import(new URL('../../static/js/telegram/state.js', import.meta.url).href);

// ---------------- мини-фреймворк ----------------

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n  ожидалось: ${e}\n  получено:  ${a}`);
    }
}

// Тост без контейнера #echo-toasts уходит в console.log — по нему и считаем.
let toasts = 0;
const origLog = console.log;
console.log = (...args) => {
    if (typeof args[0] === 'string' && args[0].startsWith('[toast/')) toasts += 1;
};

const H = M.buildHandlers();
/** Звенящий кадр: гейт mute/архива заведомо пропускает — проверяем фокус. */
const frame = (dialogId, id) => ({
    type: 'new_message',
    message: { id, dialog_id: dialogId, out: false, date: 1700000000, text: 'привет' },
    muted: false,
    archived: false,
});

tgState.dialogs = [];

// ---------------- вкладка открыта на ЭТОМ диалоге ----------------
// Сообщение прилетело в чат, который прямо сейчас на экране: тост поверх
// собственной ленты — это шум, а не уведомление.
tgState.activeDialogId = '500';
panel.hidden = false;
toasts = 0;
H.new_message(frame('500', 1));
eq(toasts, 0, 'фокус: открытый диалог на видимой вкладке не даёт тост');
eq(M._debugState().pendingUnread, 0,
    'фокус: непрочитанное по открытому диалогу не копится (его сейчас читают)');
eq(M._debugState().unreadTotal, 0, 'фокус: бейдж по открытому диалогу не растёт');

// ---------------- вкладка открыта, но на ДРУГОМ диалоге ----------------
H.new_message(frame('501', 2));
eq(toasts, 1, 'фокус: сообщение из другого чата всплывает даже на открытой вкладке');
eq(M._debugState().unreadTotal, 1, 'фокус: чужой чат учтён в бейдже');

// ---------------- вкладка скрыта, диалог тот же ----------------
// Пользователь ушёл в терминал: чат «открыт», но на экране его нет — тост
// обязан быть, иначе сообщение проходит мимо.
panel.hidden = true;
H.new_message(frame('500', 3));
eq(toasts, 2, 'фокус: скрытая вкладка возвращает тосты и по открытому диалогу');
// ЗАФИКСИРОВАННАЯ АСИММЕТРИЯ, а не описка. Тост решает про ВИДИМОСТЬ панели, а
// счётчики — про то, открыт ли диалог: `isActive` в обработчике new_message (и
// в dialogs.onNewMessage) считается без учёта `$telegram.hidden`. То есть по
// открытому, но невидимому чату тост есть, а бейдж не растёт — сообщение
// «уже доставлено в ленту», и цифра появится, только если пользователь уйдёт
// из чата, не вернувшись на вкладку. Менять это в рамках гейта уведомлений
// нельзя: тем же `isActive` управляется и отметка о прочтении.
eq(M._debugState().unreadTotal, 1,
    'фокус: по ОТКРЫТОМУ диалогу бейдж не растёт даже при скрытой вкладке');

// ---------------- гейт mute сильнее фокуса ----------------
// Порядок проверок в maybeToast значим: сначала «звенит ли чат», потом
// «смотрит ли пользователь». Заглушённый чат не всплывает НИ В КАКОМ фокусе.
panel.hidden = false;
tgState.activeDialogId = null;
toasts = 0;
H.new_message({
    type: 'new_message',
    message: { id: 4, dialog_id: '502', out: false, date: 1700000000, text: 'x' },
    muted: true, archived: false,
});
eq(toasts, 0, 'фокус: заглушённый чат молчит и при видимой вкладке без активного диалога');
H.new_message({
    type: 'new_message',
    message: { id: 5, dialog_id: '503', out: false, date: 1700000000, text: 'x' },
    muted: false, archived: true,
});
eq(toasts, 0, 'фокус: архивный чат молчит и при видимой вкладке');

// ---------------- троттлинг поверх фокуса ----------------
toasts = 0;
H.new_message(frame('504', 6));
H.new_message(frame('504', 7));
eq(toasts, 1, 'фокус: два сообщения подряд из одного чата — один тост');
eq(M._debugState().unreadTotal, 3,
    'фокус: троттлинг тостов не мешает бейджу считать оба сообщения');

// ---------------- итог ----------------

console.log = origLog;
console.log(`\n${checks - failures}/${checks} ассертов прошло (frontend/telegram_toast_focus)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
