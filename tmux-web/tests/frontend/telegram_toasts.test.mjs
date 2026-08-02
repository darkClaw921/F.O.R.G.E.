// tmux-web — регресс-тест гейта уведомлений вкладки Telegram (js/telegram/main.js).
//
// Закрывает дефект «уведомления продолжают приходить от ненужных чатов»:
// источник истины про mute и архив — поля УРОВНЯ КАДРА `new_message`
// (`muted`/`archived`), которые считает сервер, а не локальный Set, собранный
// из ЗАГРУЖЕННОЙ страницы диалогов (она знает только верхушку списка).
//
// Запуск: node tmux-web/tests/frontend/telegram_toasts.test.mjs
//
// main.js тянет DOM-модули, поэтому document/window заглушаются ДО импорта
// (по образцу telegram_media.test.mjs). Все ref'ы core/dom.js становятся null,
// и рисовальщики честно уходят в ранний выход — проверяется ровно логика
// решений, без браузера.

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
/** Настройки пользователя — правятся сценариями видимости в конце файла. */
const userSettings = globalThis.window.ForgeApp.state.userSettings;
globalThis.fetch = globalThis.window.fetch;
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

const M = await import(new URL('../../static/js/telegram/main.js', import.meta.url).href);
const { tgState } = await import(new URL('../../static/js/telegram/state.js', import.meta.url).href);

// ---------------- мини-фреймворк (как в telegram_format.test.mjs) ----------------

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

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

// Тост без контейнера #echo-toasts уходит в console.log — по нему и считаем
// показы: возвращаемое значение notify() в этом окружении всегда null.
let toasts = 0;
const origLog = console.log;
console.log = (...args) => {
    if (typeof args[0] === 'string' && args[0].startsWith('[toast/')) toasts += 1;
};

// ---------------- frameFlag ----------------

eq(M.frameFlag(true, false), true, 'frameFlag: true кадра перебивает локальное false');
eq(M.frameFlag(false, true), false, 'frameFlag: false кадра перебивает локальное true');
eq(M.frameFlag(undefined, true), true, 'frameFlag: поля нет — берётся локальное true');
eq(M.frameFlag(undefined, false), false, 'frameFlag: поля нет — берётся локальное false');
eq(M.frameFlag(null, true), true, 'frameFlag: null — это «не знаю», а не «нет»');
eq(M.frameFlag('true', false), false, 'frameFlag: строка не boolean — берётся локальное');

// ---------------- dialogRings ----------------

eq(M.dialogRings({ muted: false, archived: false }, false, false), true,
    'rings: сервер сказал «не заглушён и не архив» — звенит');
eq(M.dialogRings({ muted: true, archived: false }, false, false), false,
    'rings: muted кадра гасит тост даже для диалога, которого нет в списке');
eq(M.dialogRings({ muted: false, archived: true }, false, false), false,
    'rings: архивный диалог не звенит');
eq(M.dialogRings({ muted: false, archived: false }, true, true), true,
    'rings: кадр сильнее локального знания — mute сняли на другом устройстве');
eq(M.dialogRings({}, true, false), false,
    'rings: старый сервер без полей — фолбэк на локальный mute');
eq(M.dialogRings({}, false, true), false,
    'rings: старый сервер без полей — фолбэк на локальный архив');
eq(M.dialogRings({}, false, false), true,
    'rings: старый сервер, локально ничего не знаем — звенит (прежнее поведение)');
eq(M.dialogRings(null, false, false), true, 'rings: кадра нет — работаем по локальному знанию');

// ---------------- badgeUnread ----------------

const sample = [
    { id: '1', kind: 'user', title: 'a', unread_count: 2, muted: false, archived: false },
    { id: '2', kind: 'user', title: 'b', unread_count: 3, muted: true, archived: false },
    { id: '3', kind: 'user', title: 'c', unread_count: 5, muted: false, archived: true },
];

eq(M.badgeUnread(sample, new Set(['2'])), 2, 'badge: без заглушённых и без архива');
eq(M.badgeUnread(sample, []), 5, 'badge: архив не считается даже с пустым Set заглушённых');
eq(M.badgeUnread([], new Set()), 0, 'badge: пустой список — 0');

// ---------------- бейдж по списку диалогов ----------------

tgState.dialogs = sample.map((d) => ({ ...d }));
tgState.activeDialogId = null;
eq(M.recomputeUnread(), 2, 'recomputeUnread: сумма ~/inbox без заглушённых');

// ---------------- кадры new_message ----------------

const H = M.buildHandlers();
const frame = (dialogId, id, muted, archived) => ({
    type: 'new_message',
    message: {
        id, dialog_id: dialogId, out: false, date: 1700000000, text: 'привет',
        sender: null, media: null,
    },
    muted,
    archived,
});

// Диалога 777 нет в загруженной странице — раньше он считался незаглушённым
// и исправно всплывал тостом. Кадр говорит обратное.
toasts = 0;
H.new_message(frame('777', 101, true, false));
eq(M._debugState().pendingUnread, 0, 'кадр muted: непрочитанное неизвестного диалога не копится');
eq(M._debugState().unreadTotal, 2, 'кадр muted: бейдж не вырос');
eq(toasts, 0, 'кадр muted: тоста нет');

H.new_message(frame('778', 102, false, true));
eq(M._debugState().unreadTotal, 2, 'кадр archived: бейдж не вырос');
eq(toasts, 0, 'кадр archived: тоста нет');

H.new_message(frame('888', 103, false, false));
eq(M._debugState().pendingUnread, 1, 'звенящий неизвестный диалог: непрочитанное посчитано');
eq(M._debugState().unreadTotal, 3, 'звенящий неизвестный диалог: бейдж вырос');
eq(toasts, 1, 'звенящий неизвестный диалог: тост показан');

H.new_message(frame('888', 104, false, false));
eq(M._debugState().unreadTotal, 4, 'второе сообщение того же диалога: бейдж вырос ещё раз');
eq(toasts, 1, 'тост по тому же диалогу троттлится 5с');

// Кадр авторитетнее списка: чат заглушили с телефона, страницу /dialogs с тех
// пор не перечитывали.
H.new_message(frame('1', 105, true, false));
ok(tgState.dialogs[0].muted === true, 'кадр muted патчит DialogDto загруженного диалога');
eq(M._debugState().unreadTotal, 2, 'заглушённый кадром диалог выпадает из бейджа');

// ---------------- notify_settings ----------------

H.notify_settings({ type: 'notify_settings', dialog_id: '888', muted: true });
eq(M._debugState().pendingUnread, 0, 'notify_settings: непрочитанное заглушённого чата снято');
eq(M._debugState().unreadTotal, 0, 'notify_settings: бейдж пересчитан');
// Mute сняли обратно — кадр снова считает диалог звенящим, и запись в бейдже
// оживает. Тоста при этом нет: по 888 он уже был секунду назад (троттлинг
// живёт отдельно от гейта и переживает смену настроек).
toasts = 0;
H.new_message(frame('888', 106, false, false));
eq(M._debugState().pendingUnread, 1, 'notify_settings не вечен: кадр снова сказал «звенит»');
eq(M._debugState().unreadTotal, 1, 'снятый mute вернул неизвестный диалог в бейдж');
eq(toasts, 0, 'троттлинг тостов не сбрасывается сменой настроек');

H.notify_settings({ type: 'notify_settings', dialog_id: '2', muted: false });
ok(tgState.dialogs[1].muted === false, 'notify_settings патчит DialogDto из списка');
eq(M._debugState().unreadTotal, 4, 'notify_settings: снятый mute вернул диалог в бейдж');

// ---------------- notify_defaults_changed ----------------

H.notify_defaults_changed({
    type: 'notify_defaults_changed', users: true, chats: false, broadcasts: true,
});
eq(tgState.notifyDefaults, { users: true, chats: false, broadcasts: true },
    'notify_defaults_changed: дефолты категорий запомнены');
eq(M._debugState().pendingUnread, 0,
    'notify_defaults_changed: производные записи сброшены (считались по старым дефолтам)');

// ===================================================================
// КРАЕВЫЕ СЛУЧАИ
//
// Всё ниже — про «не boolean», «не в списке» и «сервер передумал». Именно на
// этих стыках и жил дефект: локальный Set знает верхнюю страницу /dialogs, а
// WS вещает по всему аккаунту, и любое «не знаю», принятое за «нет», выливается
// в тост от чата, который пользователь заглушил месяц назад.
// ===================================================================

// ---------------- frameFlag: только строгий boolean ----------------
// JSON-число 1/0 в поле кадра — это не «да/нет», а сломанный контракт. Считать
// 0 за «не заглушён» опаснее всего: ровно так дефект и выглядел бы снова.
eq(M.frameFlag(0, true), true, 'frameFlag: число 0 — не boolean, берётся локальное true');
eq(M.frameFlag(1, false), false, 'frameFlag: число 1 не считается «заглушён»');
eq(M.frameFlag(NaN, true), true, 'frameFlag: NaN — «не знаю»');
eq(M.frameFlag('', true), true, 'frameFlag: пустая строка — «не знаю», а не false');
eq(M.frameFlag(undefined, 'муть'), true, 'frameFlag: фолбэк приводится к boolean');
eq(M.frameFlag(undefined, 0), false, 'frameFlag: falsy-фолбэк — это false');

// ---------------- dialogRings: мусор вместо кадра ----------------
eq(M.dialogRings([], false, false), true,
    'rings: массив вместо кадра — полей нет, работаем по локальному знанию');
eq(M.dialogRings('muted', true, false), false,
    'rings: строка вместо кадра не отменяет локальный mute');
eq(M.dialogRings({ muted: 1 }, false, false), true,
    'rings: muted числом контрактом не считается — гейт остаётся на локальном знании');
eq(M.dialogRings({ archived: 'true' }, false, true), false,
    'rings: archived строкой — «не знаю», локальный архив гасит');
eq(M.dialogRings(undefined, false, false), true, 'rings: undefined-кадр не роняет гейт');
eq(M.dialogRings({ muted: false, archived: false }, true, true), true,
    'rings: оба поля кадра сильнее обоих локальных значений');

// ---------------- badgeUnread: битые DialogDto ----------------
eq(M.badgeUnread([{ id: '1', kind: 'user', unread_count: '4' }], new Set()), 4,
    'badge: строковый unread_count приводится к числу');
eq(M.badgeUnread([{ id: '1', kind: 'user', unread_count: -3 }], new Set()), 0,
    'badge: отрицательный счётчик не уменьшает сумму');
eq(M.badgeUnread([{ id: '1', kind: 'user', unread_count: 'много' }], new Set()), 0,
    'badge: нечисловой счётчик не даёт NaN на кнопке');
eq(M.badgeUnread([null, { id: '1', kind: 'user', unread_count: 2 }], new Set()), 2,
    'badge: дырка в списке не роняет пересчёт');
eq(M.badgeUnread(null, new Set()), 0, 'badge: не массив — 0');
eq(M.badgeUnread([{ id: 2, kind: 'user', unread_count: 3 }], new Set(['2'])), 0,
    'badge: id сравниваются как строки (mutedIds отдаёт строки — инвариант)');
eq(M.badgeUnread([{ id: '1', kind: 'user', unread_count: 3 }], ['1']), 0,
    'badge: массив заглушённых работает наравне с Set');
eq(M.badgeUnread([{ id: '1', kind: 'user', unread_count: 3, archived: undefined }], new Set()), 3,
    'badge: отсутствие archived — это «не в архиве»');

// ---------------- битые кадры new_message ----------------
tgState.dialogs = [];
tgState.activeDialogId = null;
M.recomputeUnread();
toasts = 0;
H.new_message({ type: 'new_message' });
H.new_message(null);
H.new_message({ type: 'new_message', message: null });
eq(toasts, 0, 'кадр без MessageDto: тоста нет и обработчик не падает');
eq(M._debugState().pendingUnread, 0, 'кадр без MessageDto: бейдж не тронут');

// Своё сообщение, отправленное с телефона, приходит тем же кадром. Оно не
// звенит и в бейдж не идёт — даже по чату, которого нет в списке.
H.new_message({
    type: 'new_message',
    message: { id: 200, dialog_id: '900', out: true, text: 'мой ответ' },
    muted: false, archived: false,
});
eq(toasts, 0, 'исходящее с другого устройства не всплывает тостом');
eq(M._debugState().pendingUnread, 0, 'исходящее не копит непрочитанное');

// ---------------- кадр сильнее списка в ОБЕ стороны ----------------
tgState.dialogs = [
    { id: '701', kind: 'user', title: 'заглушённый', unread_count: 0, muted: true, archived: false },
    { id: '702', kind: 'user', title: 'архивный', unread_count: 0, muted: false, archived: true },
];
M.recomputeUnread();
eq(M._debugState().unreadTotal, 0, 'старт: заглушённый и архивный в бейдж не идут');

toasts = 0;
H.new_message(frame('701', 210, false, false));
ok(tgState.dialogs[0].muted === false, 'кадр muted:false снимает mute с DialogDto (сняли с телефона)');
eq(M._debugState().unreadTotal, 1, 'разглушённый диалог вернулся в бейдж');
eq(toasts, 1, 'разглушённый диалог снова всплывает тостом');

H.new_message(frame('702', 211, false, false));
ok(tgState.dialogs[1].archived === false, 'кадр archived:false достаёт диалог из архива');
eq(M._debugState().unreadTotal, 2, 'диалог из архива вернулся в бейдж');
eq(toasts, 2, 'диалог из архива снова всплывает тостом');

// ---------------- чат, которого нет в списке, замолчал ----------------
// Ровно жалоба пользователя: чат вне загруженной страницы наговорил в бейдж,
// потом его заглушили с телефона. О mute вкладка узнаёт из СЛЕДУЮЩЕГО кадра
// (updateNotifySettings до неё может и не дойти — при архивации, например), и
// накопленное обязано уйти вместе с чатом, а не висеть до перезагрузки.
tgState.dialogs = [];
M.recomputeUnread();
toasts = 0;
H.new_message(frame('801', 220, false, false));
H.new_message(frame('801', 221, false, false));
H.new_message(frame('801', 222, false, false));
eq(M._debugState().unreadTotal, 3, 'неизвестный чат накопил три непрочитанных');
eq(toasts, 1, 'три сообщения подряд — один тост (троттлинг)');

H.new_message(frame('801', 223, true, false));
eq(M._debugState().pendingUnread, 0, 'кадр muted:true снял накопленное по неизвестному чату');
eq(M._debugState().unreadTotal, 0, 'бейдж не считает чат, который сервер объявил заглушённым');

H.new_message(frame('802', 230, false, false));
eq(M._debugState().unreadTotal, 1, 'второй неизвестный чат звенит и считается');
H.new_message(frame('802', 231, false, true));
eq(M._debugState().unreadTotal, 0, 'кадр archived:true так же снимает накопленное');

// ---------------- диалог доехал до списка: без двойного счёта ----------------
H.new_message(frame('803', 240, false, false));
H.new_message(frame('803', 241, false, false));
eq(M._debugState().unreadTotal, 2, 'неизвестный чат: два сообщения — двойка на кнопке');
const flagsBefore = M._debugState().flagsExtra;
tgState.dialogs = [
    { id: '803', kind: 'user', title: 'догрузился', unread_count: 2, muted: false, archived: false },
];
eq(M.recomputeUnread(), 2, 'появившийся в списке диалог считается ОДИН раз');
eq(M._debugState().pendingUnread, 0, 'запись вне страницы снята — список авторитетнее');
// Чужие записи (по чатам, которых в списке по-прежнему нет) при этом остаются:
// чистится ровно тот диалог, что доехал до страницы.
eq(M._debugState().flagsExtra, flagsBefore - 1,
    'флаги вне страницы сняты ровно по доехавшему диалогу');

// ---------------- СТАРЫЙ сервер: гейт держится на прошлых кадрах ----------------
// У кадра нет ни muted, ни archived. Единственное, что знает вкладка про чат
// вне страницы, — прошлый notify_settings. Без flagsExtra тост был бы показан.
tgState.dialogs = [];
M.recomputeUnread();
H.notify_settings({ type: 'notify_settings', dialog_id: '901', muted: true });
toasts = 0;
H.new_message({ type: 'new_message', message: { id: 250, dialog_id: '901', out: false, text: 'x' } });
eq(toasts, 0, 'старый кадр по заглушённому чату вне страницы: тоста нет');
eq(M._debugState().unreadTotal, 0, 'старый кадр по заглушённому чату вне страницы: бейдж не вырос');

H.notify_settings({ type: 'notify_settings', dialog_id: '901', muted: false });
toasts = 0;
H.new_message({ type: 'new_message', message: { id: 251, dialog_id: '901', out: false, text: 'x' } });
eq(toasts, 1, 'снятый mute вернул тосты и на старом сервере');
eq(M._debugState().unreadTotal, 1, 'снятый mute вернул чат в бейдж');

// Про чат, о котором не известно вообще ничего, старый сервер оставляет
// прежнее поведение: показать лишний тост лучше, чем потерять сообщение.
toasts = 0;
H.new_message({ type: 'new_message', message: { id: 252, dialog_id: '902', out: false, text: 'x' } });
eq(toasts, 1, 'старый сервер, чат неизвестен — прежнее поведение сохранено');

// ---------------- битые кадры notify_settings ----------------
const beforeBadge = M._debugState().unreadTotal;
H.notify_settings({ type: 'notify_settings' });
H.notify_settings(null);
H.notify_settings({ type: 'notify_settings', dialog_id: '', muted: true });
eq(M._debugState().unreadTotal, beforeBadge, 'notify_settings без dialog_id ничего не ломает');

// ---------------- белый список видимости (forge-c2dt) ----------------
//
// Фильтр обязан гасить не только колонку, но и тосты с бейджем: спрятанный чат,
// продолжающий всплывать уведомлением, — это ровно то, от чего пользователь
// просил избавиться.
//
// Каждый сценарий начинается с clearAccountData(): она снимает и накопленное
// предыдущими секциями непрочитанное, и отметки троттлинга тостов (5 с на чат),
// из-за которых второе сообщение того же чата не всплыло бы независимо от
// фильтра.

const { invalidateVisibilityFilter } = await import(
    new URL('../../static/js/telegram/state.js', import.meta.url).href);

/** Ставит белый список и сбрасывает кэш разбора. */
function setFilter(enabled, folders, dialogs) {
    userSettings.telegram_filter_enabled = enabled;
    userSettings.telegram_visible_folders = folders || [];
    userSettings.telegram_visible_dialogs = dialogs || [];
    invalidateVisibilityFilter();
}

/** Чистый лист: пустой бейдж, снятый троттлинг, два чата в списке. */
function resetForFilter(enabled, folders, dialogs) {
    M.clearAccountData();
    setFilter(enabled, folders, dialogs);
    tgState.folders = [{ id: 7, title: 'Работа', include_ids: ['10'] }];
    tgState.dialogs = [
        { id: '10', kind: 'user', title: 'нужный', unread_count: 0, muted: false, archived: false },
        { id: '20', kind: 'user', title: 'лишний', unread_count: 0, muted: false, archived: false },
    ];
    tgState.activeDialogId = null;
    M.recomputeUnread();
}

// Без фильтра звенят оба чата — это поведение до фичи.
resetForFilter(false, [], []);
eq(M._debugState().unreadTotal, 0, 'фильтр: старт с нулевым бейджем');
toasts = 0;
H.new_message(frame('10', 900, false, false));
H.new_message(frame('20', 901, false, false));
eq(toasts, 2, 'фильтр выключен: тосты от обоих чатов');
eq(M._debugState().unreadTotal, 2, 'фильтр выключен: бейдж посчитал оба');

// Фильтр на папку, в которой только чат 10.
resetForFilter(true, ['folder:7'], []);
toasts = 0;
H.new_message(frame('10', 902, false, false));
eq(toasts, 1, 'фильтр: чат из выбранной папки звенит');
eq(M._debugState().unreadTotal, 1, 'фильтр: он же посчитан в бейдже');
H.new_message(frame('20', 903, false, false));
eq(toasts, 1, 'фильтр: чат вне выбора НЕ звенит');
eq(M._debugState().unreadTotal, 1, 'фильтр: он же не увеличил бейдж');

// Чат вне загруженной страницы: WS вещает по всему аккаунту, и про такой чат
// не известно ничего, кроме id. При действующем фильтре он обязан молчать —
// иначе фильтр протекает ровно теми уведомлениями, ради которых он включён.
resetForFilter(true, ['folder:7'], []);
toasts = 0;
H.new_message(frame('55555', 904, false, false));
eq(toasts, 0, 'фильтр: неизвестный чат не звенит');
eq(M._debugState().pendingUnread, 0, 'фильтр: неизвестный чат не копит непрочитанное');

// Поимённый выбор работает и без загруженного DTO — он не требует полей чата.
resetForFilter(true, ['folder:7'], ['55555']);
toasts = 0;
H.new_message(frame('55555', 905, false, false));
eq(toasts, 1, 'фильтр: поимённо выбранный неизвестный чат звенит');
eq(M._debugState().pendingUnread, 1, 'фильтр: он же копит непрочитанное');

// Накопленное по чату, который затем спрятали, обязано уйти из бейджа: иначе
// цифра на кнопке ведёт в чат, которого в колонке нет.
setFilter(true, ['folder:7'], []);
H.new_message(frame('55555', 906, false, false));
eq(M._debugState().pendingUnread, 0, 'фильтр: накопленное по спрятанному чату снято');

// Включённый тумблер с пустым выбором фильтром не считается: показываем всё.
resetForFilter(true, [], []);
toasts = 0;
H.new_message(frame('20', 907, false, false));
eq(toasts, 1, 'фильтр: включён без выбора — ведёт себя как выключенный');
eq(M._debugState().unreadTotal, 1, 'фильтр: включён без выбора — бейдж считает всех');

// Выключение возвращает всё, выбор при этом остаётся в настройках.
resetForFilter(false, ['folder:7'], []);
toasts = 0;
H.new_message(frame('20', 908, false, false));
eq(toasts, 1, 'фильтр: выключенный тумблер не мешает чату вне выбора');

// Заглушённость сильнее видимости: выбранный чат, заглушённый кадром, молчит.
resetForFilter(true, [], ['20']);
toasts = 0;
H.new_message(frame('20', 909, true, false));
eq(toasts, 0, 'фильтр: выбранный, но заглушённый чат не звенит');
eq(M._debugState().unreadTotal, 0, 'фильтр: он же не идёт в бейдж');

// Архивный чат, выбранный поимённо, — видимость не отменяет правил архива.
resetForFilter(true, [], ['20']);
toasts = 0;
H.new_message(frame('20', 910, false, true));
eq(toasts, 0, 'фильтр: выбранный, но архивный чат не звенит');

M.clearAccountData();
setFilter(false, [], []);

// ---------------- итог ----------------

console.log = origLog;
console.log(`\n${checks - failures}/${checks} ассертов прошло (frontend/telegram_toasts)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
