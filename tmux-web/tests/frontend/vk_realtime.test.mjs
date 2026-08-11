// Регресс-тест realtime панели VK (js/vk/main.js: buildHandlers,
// recomputeUnread, typing, read) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ:
//
//   1. БЕЙДЖ. `recomputeUnread` — ЕДИНСТВЕННЫЙ путь его обновления. Второй
//      писатель неизбежно разъезжается с первым, и снять расхождение можно
//      будет только перезагрузкой страницы;
//   2. СЕРВЕРНЫЙ СЧЁТЧИК. У VK общий unread приходит готовым (событие
//      LongPoll 80). Он обязан иметь приоритет над локальной суммой по
//      ЗАГРУЖЕННОЙ СТРАНИЦЕ списка — иначе бейдж считает верхушку и молчит
//      про остальной аккаунт;
//   3. БЕСЕДЫ ВНЕ СТРАНИЦЫ. WS вещает по всему аккаунту, а `GET /dialogs`
//      отдаёт верхушку. Непрочитанное по беседе, которой на странице нет,
//      обязано попадать в бейдж (и уходить из него при прочтении);
//   4. ЗАГЛУШЁННОЕ. Решение о mute принимает СЕРВЕР и кладёт его в кадр;
//      локальный Set знает только загруженную страницу и один гейтом быть
//      не может;
//   5. «ПЕЧАТАЕТ…». VK не присылает «перестал печатать» — он просто
//      перестаёт слать событие. Без TTL строка залипает навсегда;
//   6. НОВЫЕ КАДРЫ VK: `history_cleared` (границa, а не список id),
//      `dialog_changed` (перечитывание строки с дебаунсом), `unread_total`;
//   7. БИТЫЙ КАДР не имеет права уронить панель.
//
// Запуск: node tmux-web/tests/frontend/vk_realtime.test.mjs

import {
    $log, $unread, $typing, $panel,
    flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

const M = await import(new URL('../../static/js/vk/main.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const D = await import(new URL('../../static/js/vk/dialogs.js', import.meta.url).href);
const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit); здесь он только
// заставлял бы каждый запрос ждать настоящие 380 мс.
R.vkLimits.minIntervalMs = 1;
R.vkLimits.burst = 100;

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
    await flush();
};

const H = M.buildHandlers();

function dialog(id, extra) {
    return Object.assign({
        id: String(id),
        kind: 'user',
        title: `Пётр ${id}`,
        unread_count: 0,
        muted: false,
        pinned: false,
        archived: false,
        bot: false,
        contact: false,
        important: false,
        can_write: true,
    }, extra || {});
}

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

/** Кадр `new_message` целиком: флаги лежат НА УРОВНЕ КАДРА. */
function frame(msg, extra) {
    return Object.assign({ type: 'new_message', message: msg, muted: false, archived: false }, extra || {});
}

/** Ставит загруженную страницу списка и пересчитывает бейдж. */
function loadPage(list) {
    S.vkState.dialogs = list;
    D.renderAll();
    M.recomputeUnread();
}

/** Полный сброс между секциями. */
function reset() {
    S.vkState.messages.clear();
    S.vkState.activeDialogId = null;
    S.vkState.dialogs = [];
    D.resetDialogs();
    C.resetChat();
    // Обнуляет serverUnread, pendingUnread, flagsExtra, троттлинг тостов и
    // индикаторы набора — тем же путём, каким это делает logout.
    H.login_state({ state: 'not_configured' });
    S.vkState.loginState = 'authorized';
}

setNet(() => Promise.resolve(jsonRes(200, { dialogs: [], has_more: false })));

// ---------------------------------------------------------------------------
// 1. Бейдж по загруженной странице.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1, { unread_count: 3 }), dialog(2, { unread_count: 2 })]);
    eq($unread.textContent, '5', 'бейдж — сумма по странице');
    ok(!$unread.hidden, 'бейдж виден при непрочитанном');

    // Заглушённая беседа из суммы выпадает целиком.
    loadPage([dialog(1, { unread_count: 3 }), dialog(2, { unread_count: 2, muted: true })]);
    eq($unread.textContent, '3', 'заглушённая беседа в бейдж не входит');

    loadPage([dialog(1, { unread_count: 0 })]);
    eq($unread.textContent, '0', 'нет непрочитанного — ноль');
    ok($unread.hidden, 'пустой бейдж прячется');

    loadPage([dialog(1, { unread_count: 500 })]);
    eq($unread.textContent, '99+', 'потолок отображения');
}

// ---------------------------------------------------------------------------
// 2. Серверный счётчик (кадр unread_total) имеет приоритет.
// ---------------------------------------------------------------------------

{
    reset();
    // Загруженная страница знает про 3 непрочитанных; VK говорит, что во всём
    // аккаунте их 42. Прав VK — он видит весь аккаунт, а мы верхушку списка.
    loadPage([dialog(1, { unread_count: 3 })]);
    eq($unread.textContent, '3', 'до серверного кадра считаем сами');

    H.unread_total({ type: 'unread_total', count: 42 });
    eq($unread.textContent, '42', 'серверный счётчик перебивает локальный');
    eq(M._debugState().serverUnread, 42, 'значение запомнено');

    // Пересчёт по другой причине (пришла страница списка) серверное значение
    // не роняет — иначе бейдж «мигал» бы туда-обратно на каждой перезагрузке.
    loadPage([dialog(1, { unread_count: 3 })]);
    eq($unread.textContent, '42', 'перерисовка списка не сбрасывает серверный счётчик');

    H.unread_total({ type: 'unread_total', count: 0 });
    eq($unread.textContent, '0', 'ноль от сервера гасит бейдж');
    ok($unread.hidden, 'нулевой бейдж спрятан');

    // Мусор в кадре не имеет права затереть счётчик числом NaN.
    H.unread_total({ type: 'unread_total', count: 'много' });
    eq(M._debugState().serverUnread, null, 'нечисловой счётчик отбрасывается');
    eq($unread.textContent, '3', 'бейдж вернулся к локальному расчёту по странице');
}

// ---------------------------------------------------------------------------
// 3. Поправка «пришло уже после серверного кадра».
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1)]);
    H.unread_total({ type: 'unread_total', count: 10 });
    eq($unread.textContent, '10', 'база — серверный счётчик');

    // Сообщение в беседу ВНЕ страницы: событие 80 придёт следом, отдельным
    // кадром, и до него бейдж обязан двигаться сам — иначе «бейдж не работает».
    H.new_message(frame(message(1, 777)));
    eq($unread.textContent, '11', 'поправка до прихода нового счётчика');

    // Второе сообщение из ТОЙ ЖЕ беседы поправку не удваивает: что именно
    // нумерует счётчик VK (беседы или сообщения), из протокола не следует, и
    // «+1 за беседу» — оценка, которая не может ошибиться в разы.
    H.new_message(frame(message(2, 777)));
    eq($unread.textContent, '11', 'вторая реплика той же беседы поправку не растит');

    H.new_message(frame(message(3, 888)));
    eq($unread.textContent, '12', 'вторая беседа — вторая поправка');

    // Пришёл свежий серверный счётчик — поправка обнуляется.
    H.unread_total({ type: 'unread_total', count: 12 });
    eq($unread.textContent, '12', 'серверное значение поглотило поправку');
    eq(M._debugState().unreadSinceServer, 0, 'поправка снята');
}

// ---------------------------------------------------------------------------
// 4. Беседы вне загруженной страницы: pendingUnread.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1)]);
    // Серверного счётчика ещё не было — работает локальный путь.
    H.new_message(frame(message(1, 555)));
    H.new_message(frame(message(2, 555)));
    eq($unread.textContent, '2', 'беседа вне страницы всё равно считается');
    eq(M._debugState().pendingUnread, 1, 'одна беседа вне страницы');

    // Заглушённая беседа вне страницы не звенит и не копит: решение сервера
    // приезжает полем кадра, локальный Set про такую беседу не знает ничего.
    H.new_message(frame(message(3, 666), { muted: true }));
    eq($unread.textContent, '2', 'заглушённая беседа вне страницы не копит');

    // Прочитали её с телефона — накопленное уходит.
    H.read({ type: 'read', dialog_id: '555', max_id: 2, direction: 'in' });
    eq($unread.textContent, '0', 'read/in снимает накопленное');
    eq(M._debugState().pendingUnread, 0, 'карта очищена');

    // Беседа появилась в списке — производные карты по ней вычищаются, иначе
    // одно и то же сообщение посчиталось бы дважды.
    H.new_message(frame(message(4, 555)));
    eq(M._debugState().pendingUnread, 1, 'снова накопили');
    loadPage([dialog(1), dialog(555, { unread_count: 1 })]);
    eq(M._debugState().pendingUnread, 0, 'список авторитетнее кадров');
    eq($unread.textContent, '1', 'без двойного счёта');
}

// ---------------------------------------------------------------------------
// 5. Активная беседа: непрочитанное не растёт, отметка о прочтении уходит.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1, { unread_count: 0 })]);
    S.vkState.activeDialogId = '1';
    S.vkState.messages.set('1', []);

    H.new_message(frame(message(10, 1)));
    eq($unread.textContent, '0', 'в открытую беседу непрочитанное не копится');

    // Своё исходящее не копит непрочитанное нигде.
    H.new_message(frame(message(11, 2), { message: message(11, 2, { out: true }) }));
    eq($unread.textContent, '0', 'исходящее не считается');
}

// ---------------------------------------------------------------------------
// 6. notify_settings: глушение с другого устройства.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1, { unread_count: 4 })]);
    eq($unread.textContent, '4', 'исходное состояние');

    H.notify_settings({ type: 'notify_settings', dialog_id: '1', muted: true });
    eq($unread.textContent, '0', 'заглушённая беседа выпадает из бейджа');

    H.notify_settings({ type: 'notify_settings', dialog_id: '1', muted: false });
    eq($unread.textContent, '4', 'и возвращается при снятии глушения');

    // Беседа ВНЕ страницы: её mute запоминается отдельно, иначе кадры по ней
    // продолжали бы копить непрочитанное.
    H.new_message(frame(message(1, 999)));
    eq($unread.textContent, '5', 'беседа вне страницы посчитана');
    H.notify_settings({ type: 'notify_settings', dialog_id: '999', muted: true });
    eq($unread.textContent, '4', 'глушение снимает и накопленное');
    eq(M._debugState().flagsExtra, 1, 'решение сервера запомнено');
}

// ---------------------------------------------------------------------------
// 7. «Печатает…» и его TTL.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1), dialog(2000000001, { kind: 'chat', title: 'Беседа' })]);
    S.vkState.activeDialogId = '1';

    H.typing({ type: 'typing', dialog_id: '1', user_id: '1', action: 'typing' });
    ok($typing.textContent.includes('Пётр 1'), `имя собеседника в строке: ${$typing.textContent}`);
    ok($typing.textContent.includes('is typing'), 'один печатающий — «is»');

    // Событие из ДРУГОЙ беседы в открытую ленту не пишется.
    H.typing({ type: 'typing', dialog_id: '2000000001', user_id: '42', action: 'typing' });
    ok(!$typing.textContent.includes('42'), 'чужая беседа строку не трогает');

    // Переключились в беседу — там печатают двое.
    S.vkState.activeDialogId = '2000000001';
    H.typing({ type: 'typing', dialog_id: '2000000001', user_id: '43', action: 'typing' });
    ok($typing.textContent.includes('are typing'), `двое — «are»: ${$typing.textContent}`);

    // Голосовое важнее: «печатает» означало бы, что вот-вот придёт текст.
    H.typing({ type: 'typing', dialog_id: '2000000001', user_id: '44', action: 'record_voice' });
    ok($typing.textContent.includes('recording voice'), `голосовое: ${$typing.textContent}`);

    // Один печатающий в беседе 1 плюс трое в беседе 2000000001.
    eq(M._debugState().typing, 4, 'четыре печатающих под TTL');

    // Битые кадры не роняют панель и не добавляют записей.
    H.typing(null);
    H.typing({ type: 'typing' });
    H.typing({ type: 'typing', dialog_id: '1' });
    eq(M._debugState().typing, 4, 'битые кадры игнорируются');

    // Уход из беседы гасит индикаторы ИМЕННО ПОКИНУТОЙ: иначе при быстром
    // переборе бесед они копятся и мигают в чужой ленте. Индикатор беседы, в
    // которую мы пришли, наоборот, обязан остаться — там правда печатают.
    M.openDialogById('1');
    await settle();
    eq(M._debugState().typing, 1, 'снята ровно тройка покинутой беседы');
    ok($typing.textContent.includes('Пётр 1'), `остался индикатор открытой беседы: ${$typing.textContent}`);

    // А вот полный сброс (logout) снимает всё.
    H.login_state({ state: 'not_configured' });
    eq(M._debugState().typing, 0, 'сброс аккаунта гасит все индикаторы');
    eq($typing.textContent, '', 'строка пуста');
}

// ---------------------------------------------------------------------------
// 8. history_cleared — новый кадр VK.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1)]);
    S.vkState.activeDialogId = '1';
    S.vkState.messages.set('1', [message(1, 1), message(2, 1), message(3, 1)]);

    const removed = C.onHistoryCleared({ type: 'history_cleared', dialog_id: '1', up_to_id: 2 });
    eq(removed, 2, 'убраны сообщения до границы включительно');
    eq(S.vkState.messages.get('1').map((m) => m.id), [3], 'остались только более свежие');

    // Граница 0 — «удалено всё».
    C.onHistoryCleared({ type: 'history_cleared', dialog_id: '1', up_to_id: 0 });
    eq(S.vkState.messages.get('1').length, 0, 'up_to_id=0 очищает беседу целиком');

    // Повтор и мусор ничего не ломают.
    eq(C.onHistoryCleared({ type: 'history_cleared', dialog_id: '1', up_to_id: 5 }), 0, 'повтор — no-op');
    eq(C.onHistoryCleared(null), 0, 'битый кадр — no-op');
    eq(C.onHistoryCleared({ dialog_id: 'неизвестная', up_to_id: 5 }), 0, 'чужая беседа — no-op');
}

// ---------------------------------------------------------------------------
// 9. dialog_changed — перечитывание с дебаунсом и склейкой.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1)]);
    netCalls.length = 0;
    setNet(() => Promise.resolve(jsonRes(200, { dialogs: [dialog(1, { title: 'Новое имя' })], has_more: false })));

    // Беседы нет ни в списке, ни в открытой ленте — читать нечего.
    eq(D.onDialogChanged({ type: 'dialog_changed', dialog_id: '404' }), false, 'неизвестная беседа не стоит запроса');
    eq(netCalls.length, 0, 'запроса не было');

    // Пачка событий по видимой беседе склеивается в ОДНО чтение.
    ok(D.onDialogChanged({ type: 'dialog_changed', dialog_id: '1' }), 'запланировано');
    D.onDialogChanged({ type: 'dialog_changed', dialog_id: '1' });
    D.onDialogChanged({ type: 'dialog_changed', dialog_id: '1' });
    eq(netCalls.length, 0, 'до истечения дебаунса запросов нет');
    await settle(D.DIR_DEBOUNCE_MS + 60);
    eq(netCalls.filter((c) => c.url.includes('/api/vk/dialogs?')).length, 1,
        'пачка событий стоит одного запроса');

    D.onDialogChanged(null);
    eq(D.onDialogChanged({ type: 'dialog_changed' }), false, 'кадр без dialog_id — no-op');
}

// ---------------------------------------------------------------------------
// 10. resync и битые кадры.
// ---------------------------------------------------------------------------

{
    reset();
    loadPage([dialog(1)]);
    H.new_message(frame(message(1, 555)));
    H.unread_total({ type: 'unread_total', count: 7 });
    ok(M._debugState().serverUnread === 7, 'счётчик до resync');

    netCalls.length = 0;
    setNet((url) => {
        if (url.includes('/api/vk/status')) {
            return Promise.resolve(jsonRes(200, { enabled: true, login: { state: 'authorized' } }));
        }
        return Promise.resolve(jsonRes(200, { dialogs: [dialog(1, { unread_count: 1 })], has_more: false }));
    });
    H.resync({ type: 'resync' });
    await settle(30);
    eq(M._debugState().pendingUnread, 0, 'производные карты сброшены');
    eq(M._debugState().unreadSinceServer, 0, 'поправка сброшена');
    ok(netCalls.some((c) => c.url.includes('/api/vk/dialogs')), 'список перечитан');

    // Список после resync перечитан один раз и без дублей строк.
    eq(S.vkState.dialogs.length, 1, 'дублей в списке нет');

    // Битые кадры любого типа не роняют панель.
    for (const [type, payload] of [
        ['new_message', {}],
        ['new_message', { message: null }],
        ['message_edited', {}],
        ['message_deleted', {}],
        ['read', null],
        ['online', null],
        ['unread_total', null],
        ['notify_settings', null],
        ['history_cleared', null],
        ['dialog_changed', null],
        // Четвёрка кадров анализа чата: они приходят в ТУ ЖЕ карту, и мусор в
        // них обязан гаснуть так же тихо. Попап при этом закрыт — обработчик,
        // который делает работу без открытого окна, рисовал бы в никуда.
        ['analysis_started', null],
        ['analysis_started', {}],
        ['analysis_delta', null],
        ['analysis_delta', { text: null }],
        ['analysis_done', null],
        ['analysis_done', {}],
        ['analysis_error', null],
        ['analysis_error', { message: null }],
        ['typing', null],
        ['upload_progress', null],
        ['download_progress', null],
        ['ping', null],
    ]) {
        try {
            H[type](payload);
            checks += 1;
        } catch (e) {
            failures += 1;
            checks += 1;
            console.error(`[FAIL] кадр ${type} с мусором уронил обработчик: ${e && e.message}`);
        }
    }
    ok(true, 'панель пережила все битые кадры');
}

// ---------------------------------------------------------------------------
// 11. Полнота карты кадров против протокола сервера.
// ---------------------------------------------------------------------------

{
    const W = await import(new URL('../../static/js/vk/ws.js', import.meta.url).href);
    const missing = W.SERVER_FRAMES.filter((t) => typeof H[t] !== 'function');
    eq(missing, [], 'на каждый кадр протокола есть обработчик');

    // Кадры Telegram, которых у VK нет, не должны воскреснуть по недосмотру:
    // обработчик, который никогда не сработает, вводит в заблуждение.
    for (const gone of ['notify_defaults_changed', 'folders_changed']) {
        ok(typeof H[gone] !== 'function', `у VK нет кадра ${gone}`);
    }
}

// ---------------------------------------------------------------------------
// 12. Открытие беседы гасит её непрочитанное.
// ---------------------------------------------------------------------------

{
    reset();
    setNet(() => Promise.resolve(jsonRes(200, { messages: [], has_more: false })));
    loadPage([dialog(1, { unread_count: 4 }), dialog(2, { unread_count: 1 })]);
    eq($unread.textContent, '5', 'исходное состояние');

    M.openDialogById('1');
    await settle(20);
    eq($unread.textContent, '1', 'открытая беседа считается прочитанной');
    eq(S.vkState.activeDialogId, '1', 'беседа стала активной');
}

// ---------------------------------------------------------------------------
// 13. ДОГОН ОТКРЫТОЙ ЛЕНТЫ (forge-oy0c).
//
// Лента открытой беседы живёт кадрами `new_message`, и кадр, которого не было,
// ей не восполняется ничем. Такое окно бывает трижды: страница ушла в фон
// (suspendVk РВЁТ соединение), связь оборвалась, сервер прислал `resync`. До
// forge-oy0c ни одна из трёх веток историю открытой беседы не перечитывала —
// сообщение появлялось только после ухода в другую беседу и возврата.
// ---------------------------------------------------------------------------

{
    reset();
    setNet(() => Promise.resolve(jsonRes(200, { messages: [message(10, 1)], has_more: false })));
    loadPage([dialog(1)]);
    M.openDialogById('1');
    await settle(20);
    eq(S.vkState.messages.get('1').length, 1, 'исходная лента — одна строка');

    // Пока связи не было, в беседе появилось второе сообщение. Кадра о нём не
    // будет: он ушёл в закрытый сокет.
    setNet(() => Promise.resolve(jsonRes(200, {
        messages: [message(10, 1), message(12, 1, { text: 'пропущенное' })],
        has_more: false,
    })));

    const added = await C.refreshActive();
    await settle(10);
    eq(added, 1, 'догон добавил ровно пропущенную строку');
    eq(S.vkState.messages.get('1').map((m) => m.id), [10, 12], 'дубля старой строки нет');
    ok($log.textContent.includes('пропущенное'), 'пропущенное сообщение нарисовано в ленте');

    // Повторный догон без новостей ничего не добавляет и не плодит строк.
    const again = await C.refreshActive();
    await settle(10);
    eq(again, 0, 'догон без новостей ничего не добавляет');
    eq(S.vkState.messages.get('1').length, 2, 'лента не выросла на дублях');
}

{
    // Кадр `resync` перечитывает не только колонку, но и открытую ленту.
    reset();
    setNet(() => Promise.resolve(jsonRes(200, { messages: [message(20, 1)], has_more: false })));
    loadPage([dialog(1)]);
    M.openDialogById('1');
    await settle(20);

    const calls = [];
    setNet((url) => {
        calls.push(String(url));
        if (String(url).includes('/messages')) {
            return Promise.resolve(jsonRes(200, {
                messages: [message(20, 1), message(22, 1, { text: 'после resync' })],
                has_more: false,
            }));
        }
        return Promise.resolve(jsonRes(200, { dialogs: [dialog(1)], has_more: false }));
    });

    H.resync({ type: 'resync' });
    await settle(40);
    ok(calls.some((u) => u.includes('/messages')), 'resync перечитал историю открытой беседы');
    ok($log.textContent.includes('после resync'), 'пропущенная строка появилась в ленте');
}

{
    // ПЕРВОЕ подключение историю не перечитывает — её только что прочитал
    // openDialog. Догон делает лишь РЕКОННЕКТ.
    reset();
    setNet(() => Promise.resolve(jsonRes(200, { messages: [message(30, 1)], has_more: false })));
    loadPage([dialog(1)]);
    M.openDialogById('1');
    await settle(20);

    const first = [];
    setNet((url) => {
        first.push(String(url));
        return Promise.resolve(jsonRes(200, { messages: [message(30, 1)], has_more: false }));
    });
    H.open();
    await settle(20);
    eq(first.length, 0, 'первое подключение не тратит запрос на догон');

    const second = [];
    setNet((url) => {
        second.push(String(url));
        if (String(url).includes('/messages')) {
            return Promise.resolve(jsonRes(200, {
                messages: [message(30, 1), message(32, 1, { text: 'после реконнекта' })],
                has_more: false,
            }));
        }
        return Promise.resolve(jsonRes(200, { dialogs: [dialog(1)], has_more: false }));
    });
    H.open();
    await settle(40);
    ok(second.some((u) => u.includes('/messages')), 'реконнект догоняет открытую ленту');
    ok($log.textContent.includes('после реконнекта'), 'пропущенное за обрыв дорисовано');
}

console.log(`vk_realtime: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
