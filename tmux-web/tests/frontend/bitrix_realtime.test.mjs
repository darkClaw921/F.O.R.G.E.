// Регресс-тест REALTIME панели «Битрикс24» (Ф7) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в bitrix_dom_stub.mjs.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ:
//
//   1. БЕЙДЖ. `recomputeUnread` — ЕДИНСТВЕННЫЙ путь его обновления. Второй
//      писатель разъезжается с первым неизбежно, и снять расхождение можно
//      только перезагрузкой страницы. Плюс он обязан уважать БЕЛЫЙ СПИСОК
//      (Ф11): при действующем фильтре серверный `total` непригоден целиком.
//   2. ТОСТ. Пять гейтов сразу (своё / заглушённое / скрытое белым списком /
//      открытая лента / окно тишины). Каждый из них — отдельная причина
//      «панель шумит», и каждая ловится только тестом.
//   3. УДАЛЕНИЕ. Портал удаление НЕ СТИРАЕТ: строка остаётся в истории с
//      подменённым текстом. Убери её фронт — она вернулась бы при первой же
//      догрузке, и это выглядело бы как воскресшее сообщение.
//   4. РЕКОННЕКТ. Пока сокет был мёртв, события шли мимо; «догрузить хвост»
//      не приносит ни удалений, ни правок. Поэтому ПЕРЕоткрытие сокета
//      обязано вести к полному перечитыванию состояния, а не к продолжению.
//   5. БИТЫЙ КАДР не имеет права уронить панель: сервер новее фронта из кэша
//      PWA — штатная ситуация.
//
// Запуск: node tmux-web/tests/frontend/bitrix_realtime.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $dialogList, $log, $toasts, $unread, flush, jsonRes, netCalls, setNet,
} from './bitrix_dom_stub.mjs';

const M = await import(new URL('../../static/js/bitrix/main.js', import.meta.url).href);
const S = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);
const D = await import(new URL('../../static/js/bitrix/dialogs.js', import.meta.url).href);
const C = await import(new URL('../../static/js/bitrix/chat.js', import.meta.url).href);

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

/** Все узлы поддерева в порядке обхода. */
function walk(node, out = []) {
    out.push(node);
    for (const c of (node.childNodes || [])) walk(c, out);
    return out;
}

/** Строки ленты (без распорки и разделителей дней). */
function lines() {
    return $log.childNodes.filter((n) => String(n.className || '').startsWith('b24-line'));
}

/** Тосты в общем стеке. */
function toasts() {
    return $toasts.children.filter((n) => String(n.className || '').startsWith('echo-toast'));
}

function clearToasts() {
    $toasts.textContent = '';
}

/** MessageDto, как его отдаёт бэкенд. */
function msg(id, over = {}) {
    const text = over.text == null ? `строка ${id}` : over.text;
    return {
        id,
        dialog_id: 'chat27',
        out: false,
        date: '2026-08-05T20:20:35+03:00',
        sender: { id: '15', name: 'Максим Крылов', color: null, bot: false },
        system: false,
        media: [],
        cards: [],
        reply_markup: null,
        edited: false,
        likes: [],
        deleted: false,
        action: null,
        unread: false,
        reply_to: null,
        fwd: null,
        ...over,
        text,
        segments: over.segments || [{ kind: 'text', text, value: null, children: [] }],
    };
}

/** DialogDto строки колонки. */
function dialog(id, over = {}) {
    return {
        id: String(id),
        chat_id: null,
        kind: String(id).startsWith('chat') ? 'chat' : 'user',
        title: `Диалог ${id}`,
        avatar_url: null,
        color: null,
        unread_count: 0,
        last_message: null,
        date_update: '2026-08-05T20:20:00+03:00',
        pinned: false,
        muted: false,
        can_write: true,
        members: null,
        entity_type: null,
        entity_id: null,
        task_id: null,
        entity_link_url: null,
        role: null,
        online: false,
        was_online: null,
        archived: false,
        folder_id: null,
        ...over,
    };
}

const H = M.buildHandlers();

S.b24State.selfUser = { id: '1', name: 'Игорь', last_name: 'Герасимов' };
S.b24State.loginState = 'authorized';
S.b24State.enabled = true;
D.initDialogs({});
C.initChat({});

// Сеть по умолчанию: история открытого диалога и пустой список.
setNet(async (url) => {
    if (url.includes('/messages')) return jsonRes(200, { messages: [msg(101)], has_more: false });
    if (url.includes('/filters')) return jsonRes(200, { filters: [], counters: {} });
    return jsonRes(200, { dialogs: [], has_more: false });
});

// Открытый диалог + две строки в колонке.
D.applyDialogsPatch([dialog('chat27'), dialog('19')], []);
await C.openDialog('chat27', dialog('chat27'));
await settle();

// ---------------------------------------------------------------------------
// 1. Новое сообщение в ОТКРЫТУЮ ленту: строка появляется, тоста нет
// ---------------------------------------------------------------------------

{
    clearToasts();
    const before = lines().length;
    H.message_new({ type: 'message_new', dialog_id: 'chat27', message: msg(103), muted: false });
    await settle();

    eq(lines().length, before + 1, 'строка realtime вставлена в открытую ленту');
    ok(lines().some((n) => n.dataset.msgId === '103'), 'у строки правильный идентификатор');
    // Лента открыта и активна — шуметь незачем: пользователь смотрит на неё.
    eq(toasts().length, 0, 'по открытой ленте тоста нет');

    const listed = S.b24State.dialogs.find((d) => d.id === 'chat27');
    eq(listed.last_message.id, 103, 'строка колонки подняла превью локально');
    eq(listed.unread_count, 0, 'открытая лента непрочитанное не копит');
    // Ни одного запроса к порталу: всё нужное лежит в кадре.
    eq(netCalls.filter((c) => c.url.includes('/dialogs?')).length, 0,
        'кадр не порождает перезапрос списка');

    // forge-uo7b: живое сообщение в ОТКРЫТОЙ ленте обязано отправить отметку
    // о прочтении ПОРТАЛУ, а не только нарисовать строку локально. До фикса
    // единственным триггером `scheduleRead` был побочный эффект нативного
    // `scroll`, которого в этой ситуации в реальном браузере часто просто не
    // рождалось (короткая лента не переполнена) — сообщение оставалось
    // непрочитанным на портале, пока пользователь не открывал его в
    // официальном клиенте.
    const readBefore = netCalls.filter((c) => c.url.includes('/dialogs/chat27/read')).length;
    await settle(C.READ_DEBOUNCE_MS + 50);
    const readCalls = netCalls.filter((c) => c.url.includes('/dialogs/chat27/read'));
    eq(readCalls.length, readBefore + 1, 'отметка о прочтении ушла порталу без единой прокрутки');
    eq(readCalls[readCalls.length - 1].body, { message_id: 103 },
        'отмечено ИМЕННО новым сообщением, а не старым состоянием ленты');
}

// ---------------------------------------------------------------------------
// 2. Сообщение в ДРУГОЙ диалог: тост, счётчик строки и бейдж
// ---------------------------------------------------------------------------

{
    clearToasts();
    S.b24State.counters = { total: 0, chat: 0, dialog: 0, tasks: 0, notify: 0 };
    H.message_new({
        type: 'message_new',
        dialog_id: '19',
        message: msg(105, { dialog_id: '19', text: 'входящее' }),
        muted: false,
    });
    await settle();

    eq(toasts().length, 1, 'по неоткрытому диалогу тост показан');
    const body = walk(toasts()[0]).find((n) => n.className === 'echo-toast-body');
    eq(body.textContent, 'входящее', 'в теле тоста текст сообщения');
    const listed = S.b24State.dialogs.find((d) => d.id === '19');
    eq(listed.unread_count, 1, 'счётчик строки вырос');
    eq($unread.textContent, '1', 'бейдж двинулся СРАЗУ, не дожидаясь синхронизации');
    ok(!$unread.hidden, 'бейдж показан');

    // Второе сообщение в том же диалоге внутри окна тишины — без тоста.
    H.message_new({
        type: 'message_new',
        dialog_id: '19',
        message: msg(107, { dialog_id: '19', text: 'ещё одно' }),
        muted: false,
    });
    await settle();
    eq(toasts().length, 1, 'окно тишины подавило второй тост подряд');
    eq(S.b24State.dialogs.find((d) => d.id === '19').unread_count, 2,
        'счётчик при этом растёт всё равно');
}

// ---------------------------------------------------------------------------
// 3. Гейты тоста: своё, заглушённое, системное
// ---------------------------------------------------------------------------

{
    clearToasts();
    eq(M.maybeToast({
        dialog_id: '19',
        message: msg(109, { dialog_id: '19', out: true }),
        muted: false,
    }), null, 'своё исходящее не всплывает никогда');

    // Заглушённость решает СЕРВЕР и кладёт в кадр: во фронте загружена только
    // страница списка, и заглушённый чат может быть за её пределами.
    eq(M.maybeToast({
        dialog_id: 'chat33',
        message: msg(111, { dialog_id: 'chat33' }),
        muted: true,
    }), null, 'заглушённый диалог молчит по признаку КАДРА');

    eq(M.maybeToast({
        dialog_id: '19',
        message: msg(113, { dialog_id: '19', system: true }),
        muted: false,
    }), null, 'системная строка («создал чат») не тост');
    eq(toasts().length, 0, 'ни один из трёх гейтов тоста не пропустил');
}

// ---------------------------------------------------------------------------
// 4. Белый список (Ф11): скрытый диалог не шумит и не считается
// ---------------------------------------------------------------------------

{
    clearToasts();
    // Белый список читается ЛЕНИВО через публичный контракт window.ForgeApp —
    // тем же приёмом, что и в бою: модули грузятся раньше ответа
    // /api/user-settings, и снимок, взятый на старте, застыл бы навсегда.
    globalThis.window.ForgeApp = {
        state: {
            userSettings: {
                bitrix_filter_enabled: true,
                bitrix_visible_kinds: ['chat'],
                bitrix_visible_peers: [],
            },
        },
    };
    S.invalidateVisibilityFilter();

    eq(M.maybeToast({
        dialog_id: '19',
        message: msg(115, { dialog_id: '19' }),
        muted: false,
    }), null, 'скрытый белым списком диалог не всплывает');

    // Бейдж при действующем фильтре считается ПО ВИДИМЫМ строкам, а серверный
    // total игнорируется целиком: расщепить готовое число нечем.
    S.b24State.counters = { total: 99, chat: 0, dialog: 0, tasks: 0, notify: 0 };
    D.setUnread('19', 5);
    D.setUnread('chat27', 2);
    M.recomputeUnread();
    eq($unread.textContent, '2', 'бейдж считает только видимые диалоги');

    globalThis.window.ForgeApp = { state: { userSettings: {} } };
    S.invalidateVisibilityFilter();
    M.recomputeUnread();
    eq($unread.textContent, '99', 'без фильтра источник — серверный счётчик');
}

// ---------------------------------------------------------------------------
// 5. Счётчики и патч списка
// ---------------------------------------------------------------------------

{
    H.counters({
        type: 'counters', total: 7, chat: 3, dialog: 4, tasks: 0, notify: 0,
    });
    await settle();
    eq($unread.textContent, '7', 'кадр counters — источник истины для бейджа');

    const before = S.b24State.dialogs.length;
    H.dialogs_patch({
        type: 'dialogs_patch',
        dialogs: [dialog('chat9', { title: 'Новый чат', unread_count: 1 })],
        removed: [],
    });
    await settle();
    eq(S.b24State.dialogs.length, before + 1, 'патч добавил строку');
    ok($dialogList.children.some((n) => n.dataset.dialogId === 'chat9'),
        'новая строка нарисована в колонке');

    // Патч ОБНОВЛЯЕТ уже известную строку, а не задваивает её.
    H.dialogs_patch({
        type: 'dialogs_patch',
        dialogs: [dialog('chat9', { title: 'Переименованный', unread_count: 0 })],
        removed: [],
    });
    await settle();
    eq(S.b24State.dialogs.filter((d) => d.id === 'chat9').length, 1,
        'повторный патч не задваивает строку');
    eq(S.b24State.dialogs.find((d) => d.id === 'chat9').title, 'Переименованный',
        'патч перетирает поля строки');
}

// ---------------------------------------------------------------------------
// 6. Правка чужого сообщения (её приносит только медленный воркер)
// ---------------------------------------------------------------------------

{
    H.message_update({
        type: 'message_update',
        dialog_id: 'chat27',
        message: msg(103, { text: 'исправлено', edited: true }),
    });
    await settle();

    const row = lines().find((n) => n.dataset.msgId === '103');
    const text = walk(row).find((n) => n.className === 'b24-line-text');
    eq(text.textContent, 'исправлено', 'текст строки заменён на месте');
    const edited = walk(row).find((n) => n.className === 'b24-line-edited');
    ok(!!edited, 'метка «(изм.)» появилась');
    eq(lines().filter((n) => n.dataset.msgId === '103').length, 1,
        'правка не породила вторую строку');
}

// ---------------------------------------------------------------------------
// 7. Удаление: строка ОСТАЁТСЯ, но помечена
// ---------------------------------------------------------------------------

{
    const before = lines().length;
    H.message_delete({ type: 'message_delete', dialog_id: 'chat27', message_ids: [103] });
    await settle();

    eq(lines().length, before, 'удалённая строка не исчезает из ленты');
    const row = lines().find((n) => n.dataset.msgId === '103');
    ok(String(row.className).includes('b24-line-deleted'), 'строка помечена классом');
    const text = walk(row).find((n) => n.className === 'b24-line-text');
    eq(text.textContent, C.DELETED_TEXT, 'текст подменён пометкой');
    const actions = walk(row).find((n) => n.className === 'b24-line-actions');
    eq(actions, undefined, 'у удалённой строки нет кнопок действий');

    // Кадр ЧУЖОГО диалога ленту не трогает: сервер вещает без фильтрации.
    H.message_delete({ type: 'message_delete', dialog_id: '19', message_ids: [101] });
    await settle();
    ok(lines().some((n) => n.dataset.msgId === '101'), 'кадр чужого диалога проигнорирован');
}

// ---------------------------------------------------------------------------
// 8. Реакция: приезжает ИТОГОВЫЙ список, а не дельта
// ---------------------------------------------------------------------------

{
    H.reaction({
        type: 'reaction', dialog_id: 'chat27', message_id: 101, likes: ['1', '15'],
    });
    await settle();
    let row = lines().find((n) => n.dataset.msgId === '101');
    let likes = walk(row).find((n) => n.className === 'b24-line-likes');
    eq(likes.textContent, '♥ 2', 'счётчик лайков нарисован');

    // Повторный кадр с тем же составом (дубли у портала реальны) ничего не
    // ломает — присваивание идемпотентно.
    H.reaction({
        type: 'reaction', dialog_id: 'chat27', message_id: 101, likes: ['1', '15'],
    });
    await settle();
    row = lines().find((n) => n.dataset.msgId === '101');
    likes = walk(row).find((n) => n.className === 'b24-line-likes');
    eq(likes.textContent, '♥ 2', 'повторный кадр не удвоил счётчик');

    // Снятие приезжает ПУСТЫМ списком, а не отрицательной дельтой.
    H.reaction({ type: 'reaction', dialog_id: 'chat27', message_id: 101, likes: [] });
    await settle();
    row = lines().find((n) => n.dataset.msgId === '101');
    eq(walk(row).find((n) => n.className === 'b24-line-likes'), undefined,
        'пустой список снял лайк');
}

// ---------------------------------------------------------------------------
// 9. Присутствие
// ---------------------------------------------------------------------------

{
    H.presence({ type: 'presence', user_id: '19', online: true, was_online: null });
    await settle();
    eq(S.b24State.dialogs.find((d) => d.id === '19').online, true,
        'присутствие живёт в строке списка: у портала это свойство собеседника');
}

// ---------------------------------------------------------------------------
// 10. resync и реконнект: ПОЛНОЕ перечитывание состояния
// ---------------------------------------------------------------------------

{
    const mark = netCalls.length;
    H.resync({ type: 'resync' });
    await settle(20);

    const calls = netCalls.slice(mark).map((c) => c.url);
    ok(calls.some((u) => u.includes('/api/bitrix/dialogs?')), 'resync перечитал список');
    ok(calls.some((u) => u.includes('/api/bitrix/filters')), 'resync перечитал счётчики');
    ok(calls.some((u) => u.includes('/messages')), 'resync перечитал открытую ленту');
}

{
    // Первое открытие сокета данными не занимается — их грузит гейт.
    const first = netCalls.length;
    H.open();
    await settle(20);
    eq(netCalls.length, first, 'первое открытие сокета не перезагружает панель');

    // ПЕРЕоткрытие означает разрыв: пока сокет был мёртв, события шли мимо.
    H.open();
    await settle(20);
    ok(netCalls.length > first, 'реконнект ведёт к полному resync');
}

// ---------------------------------------------------------------------------
// 11. Битые кадры не роняют панель
// ---------------------------------------------------------------------------

{
    for (const [name, frame] of [
        ['message_new', {}],
        ['message_new', { dialog_id: 'chat27' }],
        ['message_update', { dialog_id: 'chat27' }],
        ['message_delete', { dialog_id: 'chat27' }],
        ['reaction', { dialog_id: 'chat27' }],
        ['dialogs_patch', {}],
        ['presence', {}],
        ['counters', {}],
        ['error', {}],
    ]) {
        try {
            H[name](frame);
            checks += 1;
        } catch (e) {
            failures += 1;
            console.error(`[FAIL] битый кадр ${name} уронил обработчик: ${e && e.message}`);
        }
    }
    await settle();
}

// ---------------------------------------------------------------------------
// 12. Каждый кадр протокола имеет обработчик
// ---------------------------------------------------------------------------

{
    const W = await import(new URL('../../static/js/bitrix/ws.js', import.meta.url).href);
    const missing = W.SERVER_FRAMES.filter((t) => typeof H[t] !== 'function');
    eq(missing, [], 'у каждого кадра SERVER_FRAMES есть обработчик');
}

console.log(`\n[bitrix_realtime] проверок: ${checks}, провалов: ${failures}`);
process.exit(failures === 0 ? 0 : 1);
