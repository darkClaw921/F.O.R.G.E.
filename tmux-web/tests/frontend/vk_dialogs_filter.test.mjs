// Регресс-тест ПРИМЕНЕНИЯ белого списка бесед VK — все ЧЕТЫРЕ точки (Ф10).
//
// Стаб DOM импортируется ПЕРВЫМ: core/dom.js резолвит ref'ы один раз на
// верхнем уровне, и модуль, загруженный раньше стаба, увидел бы одни null.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Сам предикат видимости покрыт чистым тестом
// vk_visibility. Здесь проверяется другое — что предикат ПРИМЕНЁН везде, где
// скрытая беседа может высунуться наружу. Точек ровно четыре, и пропуск любой
// из них = протечка:
//
//   1. колонка бесед                 — dialogs.js::renderAll (через visibleSource);
//   2. счётчики серверных фильтров   — dialogs.js::renderDirs;
//   3. бейдж на кнопке Ⓥ            — main.js::recomputeUnread;
//   4. тосты и накопление непрочитанного — main.js::maybeToast/noteUnknownUnread.
//
// Скрыть беседу только в колонке — половина работы: она продолжит всплывать
// тостом и накручивать цифру на Ⓥ, которая ведёт в беседу, которой в списке
// нет. Поэтому каждая точка закрыта СВОИМ ассертом, и мутация «убрать фильтр
// из точки N» краснит именно его.
//
// Плюс контракт `window.ForgeApp.vkFilter.refresh` — три обязательных
// действия: сброс кэша разбора, перерисовка колонки, пересчёт бейджа.
//
// Запуск: node tmux-web/tests/frontend/vk_dialogs_filter.test.mjs

import {
    $dirList, $dialogList, $communityList, $unread,
    flush, jsonRes, setNet,
} from './vk_dom_stub.mjs';

// Публичный контракт: отсюда state.js читает настройки белого списка. Ставится
// ДО импорта модулей панели — они читают его лениво, но так честнее.
const userSettings = {};
globalThis.window.ForgeApp = { state: { userSettings } };

const M = await import(new URL('../../static/js/vk/main.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const D = await import(new URL('../../static/js/vk/dialogs.js', import.meta.url).href);

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
// показы (приём тот же, что в vk_toasts).
const shown = [];
const origLog = console.log;
console.log = (...args) => {
    if (typeof args[0] === 'string' && args[0].startsWith('[toast/')) {
        shown.push(String(args[1] || ''));
        return;
    }
    origLog(...args);
};

const H = M.buildHandlers();
setNet(async () => jsonRes(200, { dialogs: [], has_more: false }));

function dialog(id, extra) {
    return Object.assign({
        id: String(id),
        kind: 'user',
        title: `Собеседник ${id}`,
        username: null,
        bot: false,
        unread_count: 0,
        muted: false,
        archived: false,
        last_message: null,
        members: null,
    }, extra || {});
}

/** Список `filter=all`: два человека, беседа и сообщество. */
function baseDialogs() {
    return [
        dialog(1234, { title: 'София', unread_count: 3 }),
        dialog(4321, { title: 'Пётр', unread_count: 1 }),
        dialog(2000000007, { kind: 'chat', title: 'Проектная', members: 4, unread_count: 2 }),
        dialog(-55, { kind: 'group', title: 'Клуб', unread_count: 5 }),
    ];
}

/** Ставит белый список в настройках и сбрасывает кэш разбора. */
function setFilter(enabled, kinds, peers) {
    userSettings.vk_filter_enabled = enabled;
    userSettings.vk_visible_kinds = kinds || [];
    userSettings.vk_visible_peers = peers || [];
    S.invalidateVisibilityFilter();
}

/** Чистое состояние панели: свежий список, сброшенные производные счётчики. */
function reset(enabled, kinds, peers) {
    shown.length = 0;
    setFilter(enabled, kinds, peers);
    // Тем же путём, каким это делает logout: pendingUnread, serverUnread,
    // троттлинг тостов и колонка.
    S.vkState.loginState = 'authorized';
    H.login_state({ state: 'not_configured' });
    S.vkState.loginState = 'authorized';
    S.vkState.dialogs = baseDialogs();
    S.vkState.dirDialogs = [];
    S.vkState.activeDir = 'all';
    S.vkState.activeDialogId = null;
    D.renderAll();
    M.recomputeUnread();
}

/** Имена бесед в колонке узлов и сообществ. */
const nodeIds = () => $dialogList.children.map((li) => li.dataset.dialogId);
const communityIds = () => $communityList.children.map((li) => li.dataset.dialogId);
/** Текст пункта `~/all` секции // FILTERS. */
const dirAll = () => $dirList.children[0].textContent;

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

const frame = (msg, extra) => Object.assign(
    { type: 'new_message', message: msg, muted: false, archived: false }, extra || {},
);

// ---------------------------------------------------------------------------
// ТОЧКА 1. Колонка бесед (renderAll → visibleSource).
// ---------------------------------------------------------------------------

reset(false, [], []);
eq(nodeIds(), ['1234', '4321', '2000000007'], 'фильтр выключен: в колонке все узлы');
eq(communityIds(), ['-55'], 'фильтр выключен: сообщество на месте');

// Инвариант «включён ≠ действует»: тумблер с пустым выбором показывает ВСЁ.
reset(true, [], []);
eq(nodeIds(), ['1234', '4321', '2000000007'], 'включён с пустым выбором: колонка полная');
eq(communityIds(), ['-55'], 'включён с пустым выбором: сообщество на месте');

reset(true, ['chat'], []);
eq(nodeIds(), ['2000000007'], 'тип chat: в колонке только беседа');
eq(communityIds(), [], 'тип chat: сообщество скрыто');

reset(true, ['community'], []);
eq(nodeIds(), [], 'тип community: узлов не осталось');
eq(communityIds(), ['-55'], 'тип community: только сообщество');

reset(true, [], ['1234']);
eq(nodeIds(), ['1234'], 'поимённо: в колонке одна отмеченная беседа');

reset(true, ['chat'], ['1234']);
eq(nodeIds(), ['1234', '2000000007'],
    'тип + поимённый выбор: объединение, порядок сервера сохранён');

// ---------------------------------------------------------------------------
// ТОЧКА 2. Счётчики серверных фильтров (renderDirs).
//
// Считать по всему списку, когда часть бесед скрыта, — значит показать цифру,
// которая ведёт в никуда: пользователь щёлкает по «(11)» и не находит их.
// ---------------------------------------------------------------------------

reset(false, [], []);
eq(dirAll(), '~/all(11)', 'фильтр выключен: счётчик по всему списку (3+1+2+5)');

reset(true, ['chat'], []);
eq(dirAll(), '~/all(2)', 'тип chat: счётчик считает только видимую беседу');
eq($dirList.children[1].textContent, '~/unread(2)', 'у ~/unread та же арифметика');

reset(true, [], ['1234']);
eq(dirAll(), '~/all(3)', 'поимённо: счётчик по одной отмеченной беседе');

reset(true, ['community'], []);
eq(dirAll(), '~/all(5)', 'тип community: счётчик по сообществу');

reset(true, ['user'], ['-55']);
eq(dirAll(), '~/all(9)', 'два людских чата и сообщество поимённо: 3+1+5');

// Заглушённая беседа в счётчик не входит и при действующем фильтре.
reset(true, ['user'], []);
S.vkState.dialogs[1].muted = true;
D.renderAll();
eq(dirAll(), '~/all(3)', 'заглушённый чат выпадает из счётчика поверх фильтра');

// ---------------------------------------------------------------------------
// ТОЧКА 2b. `~/all` против транзиентной выборки серверного фильтра.
//
// У VK «директория» — это ДРУГОЙ ЗАПРОС, и его ответ живёт отдельным списком.
// Белый список обязан действовать и на него: иначе беседа, скрытая в ~/all,
// всплыла бы в «Важных».
// ---------------------------------------------------------------------------

reset(true, ['chat'], []);
S.vkState.activeDir = 'important';
S.vkState.dirDialogs = [dialog(4321, { title: 'Пётр' }), dialog(2000000007, { kind: 'chat' })];
D.renderAll();
eq(nodeIds(), ['2000000007'], 'транзиентная выборка фильтруется тем же белым списком');
eq(dirAll(), '~/all(2)',
    'счётчики по-прежнему считаются по списку filter=all, а не по выборке');

// ---------------------------------------------------------------------------
// ТОЧКА 3. Бейдж на кнопке Ⓥ (recomputeUnread).
// ---------------------------------------------------------------------------

reset(false, [], []);
eq($unread.textContent, '11', 'фильтр выключен: бейдж по всему списку');
ok(!$unread.hidden, 'бейдж виден');

reset(true, ['chat'], []);
eq($unread.textContent, '2', 'тип chat: бейдж считает только видимую беседу');

reset(true, [], ['1234']);
eq($unread.textContent, '3', 'поимённо: бейдж по отмеченной беседе');

reset(true, ['community'], []);
eq($unread.textContent, '5', 'тип community: бейдж по сообществу');

// Серверный счётчик (событие LongPoll 80) посчитан по ВСЕМУ аккаунту, включая
// скрытые беседы, и расщепить его нечем. При действующем фильтре он обязан
// уступить локальному расчёту — иначе цифра снова ведёт в никуда.
reset(true, ['chat'], []);
H.unread_total({ count: 99 });
eq($unread.textContent, '2', 'действующий фильтр: серверный счётчик ИГНОРИРУЕТСЯ');
setFilter(false, [], []);
M.recomputeUnread();
eq($unread.textContent, '99', 'фильтр выключили — серверный счётчик снова главный');

// Ноль прячет бейдж: «(0)» на кнопке выглядит как непрочитанное.
reset(true, ['chat'], []);
S.vkState.dialogs[2].unread_count = 0;
M.recomputeUnread();
eq($unread.textContent, '0', 'видимых непрочитанных нет');
ok($unread.hidden, 'бейдж скрыт при нуле');

// ---------------------------------------------------------------------------
// ТОЧКА 4. Тосты и накопление непрочитанного по беседам вне страницы.
// ---------------------------------------------------------------------------

reset(true, ['chat'], []);
H.new_message(frame(message(500, 4321)));
eq(shown.length, 0, 'скрытая беседа не звенит тостом');
eq(M._debugState().pendingUnread, 0, 'и не копит непрочитанное вне страницы');
eq(M._debugState().unreadSinceServer, 0, 'и не двигает поправку к серверному счётчику');
eq($unread.textContent, '2', 'бейдж от скрытой беседы не вырос');

// Беседа, которой в загруженном списке НЕТ, но она проходит по типу: у VK тип
// выводится из самого peer_id, поэтому решение точное — тост показывается, а
// непрочитанное копится.
reset(true, ['chat'], []);
H.new_message(frame(message(501, 2000000123)));
eq(shown.length, 1, 'неизвестная беседа нужного типа звенит (точное решение по id)');
eq(M._debugState().pendingUnread, 1, 'её непрочитанное учтено');
eq($unread.textContent, '3', 'бейдж = 2 видимых + 1 накопленное');

// Поимённый режим строг к незнакомцам — там тип не помогает.
reset(true, [], ['1234']);
H.new_message(frame(message(502, 2000000123)));
eq(shown.length, 0, 'поимённый режим: неизвестная беседа молчит');
eq(M._debugState().pendingUnread, 0, 'и ничего не копит');

// Выключенный фильтр — поведение до фичи.
reset(false, [], []);
H.new_message(frame(message(503, 4321)));
eq(shown.length, 1, 'фильтр выключен: тост показывается');

// ---------------------------------------------------------------------------
// Контракт window.ForgeApp.vkFilter.refresh — три обязательных действия.
// ---------------------------------------------------------------------------

{
    // initVk идемпотентна и публикует контракт первым же действием. Сеть в
    // стабе отвечает пустым списком, логин не авторизован — панель просто
    // соберётся.
    try {
        M.initVk();
    } catch (e) {
        ok(false, `initVk не должна падать на стабе: ${e && e.message}`);
    }
    await flush();
    const api = globalThis.window.ForgeApp.vkFilter;
    ok(api && typeof api.refresh === 'function', 'контракт vkFilter.refresh опубликован');

    reset(false, [], []);
    eq(nodeIds().length, 3, 'до сохранения настроек видно всё');
    eq($unread.textContent, '11', 'и бейдж полный');

    // Форма настроек сохранила новый выбор и позвала refresh — БЕЗ
    // перезагрузки страницы обязано подействовать всё сразу.
    userSettings.vk_filter_enabled = true;
    userSettings.vk_visible_kinds = ['chat'];
    userSettings.vk_visible_peers = [];
    api.refresh();
    eq(nodeIds(), ['2000000007'], 'refresh: колонка перерисована');
    eq(dirAll(), '~/all(2)', 'refresh: счётчики фильтров пересчитаны');
    eq($unread.textContent, '2', 'refresh: бейдж пересчитан (третье обязательное действие)');

    // И обратно.
    userSettings.vk_filter_enabled = false;
    api.refresh();
    eq(nodeIds().length, 3, 'refresh: выключение возвращает колонку');
    eq($unread.textContent, '11', 'refresh: и бейдж');
}

// ---------------------------------------------------------------------------
// Накопленное по беседе, которую скрыли уже ПОСЛЕ прихода сообщения.
//
// pendingUnread переживает сохранение настроек, и без проверки видимости в
// момент пересчёта бейдж остался бы завышенным до перезагрузки страницы.
// ---------------------------------------------------------------------------

{
    reset(false, [], []);
    H.new_message(frame(message(600, 999999)));
    eq(M._debugState().pendingUnread, 1, 'непрочитанное по беседе вне страницы учтено');
    eq($unread.textContent, '12', 'бейдж вырос на неё');

    globalThis.window.ForgeApp.vkFilter.refresh();
    eq($unread.textContent, '12', 'без фильтра ничего не изменилось');

    userSettings.vk_filter_enabled = true;
    userSettings.vk_visible_kinds = ['chat'];
    globalThis.window.ForgeApp.vkFilter.refresh();
    eq($unread.textContent, '2', 'после скрытия накопленное по ней ушло из бейджа');
}

setFilter(false, [], []);
delete userSettings.vk_filter_enabled;
delete userSettings.vk_visible_kinds;
delete userSettings.vk_visible_peers;
S.invalidateVisibilityFilter();

console.log = origLog;
console.log(`vk_dialogs_filter: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
