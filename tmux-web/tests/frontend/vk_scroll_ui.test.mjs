// Регресс-тест «следования» ленты VK — UI (js/vk/chat.js + messenger/follow-core.js).
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs. Поверх него
// доставляются три вещи, которых у фейкового DOM нет: журнал `scrollTo`,
// переключаемый `matchMedia` и шпион `ResizeObserver`.
//
// ЧИСТАЯ логика решений (`scrollDecision`, `scrollTargetForRow`, TTL) живёт в
// messenger_follow_factory.test.mjs; здесь проверяется склейка с лентой:
//
//   1. ПРЕЖНЕЕ ПОВЕДЕНИЕ БЕЗ НАЖАТИЯ — читающего историю не дёргаем, у нижней
//      кромки прилипание работает как работало;
//   2. НАЖАТИЕ включает следование и берёт строку под наблюдение;
//   3. ДОСКОЛЛ после правки (обе ветки onEdited) и после нового сообщения;
//   4. ОТЛОЖЕННЫЙ РОСТ добирается через ResizeObserver;
//   5. smooth/auto по prefers-reduced-motion и фолбэк без `scrollTo`;
//   6. ОТМЕНА прокруткой вверх и истечением окна;
//   7. ЖИЗНЕННЫЙ ЦИКЛ наблюдателя: один на панель, снимается при смене
//      беседы, перерисовке истории, удалении сообщения и resetChat;
//   8. РЕГРЕСС-ЩИТ: переход по цитате по-прежнему ЦЕНТРИРУЕТ строку, а не
//      улетает вниз (следование обязано ему уступать).
//
// Запуск: node tmux-web/tests/frontend/vk_scroll_ui.test.mjs

import {
    $log, $prompt, $input, $document,
    dispatch, flush, jsonRes, setNet,
} from './vk_dom_stub.mjs';

// ---------------------------------------------------------------------------
// Доснащение стенда (ДО первого вызова chat.js — он читает всё это лениво,
// но пусть будет на месте с самого начала).
// ---------------------------------------------------------------------------

/** Журнал доскроллов: [{top, behavior}]. */
const scrolls = [];
$log.scrollTo = (opts) => {
    scrolls.push({ top: opts.top, behavior: opts.behavior });
    $log.scrollTop = opts.top;
};

let reduceMotion = false;
globalThis.window.matchMedia = () => ({
    matches: reduceMotion, addEventListener() {}, removeEventListener() {},
});

/**
 * Модель высоты строк: cmid → координаты ОТНОСИТЕЛЬНО верха ленты.
 *
 * Модель, а не патч конкретного узла: строку пересобирает и `onEdited`, и
 * перерисовка истории, и патч, поставленный на старый узел, молча перестал бы
 * действовать ровно в тот момент, который тест и проверяет.
 */
const rowRects = new Map();
const setRow = (cmid, top, height) => rowRects.set(Number(cmid), { top, height });
{
    const create = $document.createElement;
    $document.createElement = (tag) => {
        const node = create(tag);
        node.getBoundingClientRect = () => {
            const r = rowRects.get(Number(node.dataset && node.dataset.cmid))
                || { top: 0, height: 0 };
            return {
                top: r.top, bottom: r.top + r.height, left: 0, right: 100,
                width: 100, height: r.height,
            };
        };
        return node;
    };
}

/** Шпион ResizeObserver: сколько создано и что наблюдается. */
const ro = { created: 0, ops: [], cb: null };
globalThis.ResizeObserver = class {
    constructor(cb) { ro.created += 1; ro.cb = cb; }
    observe(node) { ro.ops.push(['observe', node]); }
    unobserve(node) { ro.ops.push(['unobserve', node]); }
    disconnect() { ro.ops.push(['disconnect', null]); }
};

const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const RL = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

RL.vkLimits.minIntervalMs = 1;
RL.vkLimits.burst = 100;

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

const lines = () => $log.children.filter((n) => n.className.includes('vk-line')
    && !n.className.includes('vk-line-system')
    && !n.className.includes('vk-line-error'));

const rowByCmid = (cmid) => lines().find((n) => n.dataset.cmid === String(cmid)) || null;

/** Геометрия ленты: окно 200px, содержимое 1000px. */
function layout(scrollTop, scrollHeight, clientHeight) {
    $log.scrollTop = scrollTop;
    $log.scrollHeight = scrollHeight === undefined ? 1000 : scrollHeight;
    $log.clientHeight = clientHeight === undefined ? 200 : clientHeight;
    $log.getBoundingClientRect = () => ({
        top: 0, bottom: $log.clientHeight, left: 0, right: 100,
        width: 100, height: $log.clientHeight,
    });
}

function message(id, extra) {
    return Object.assign({
        id,
        dialog_id: '2000000007',
        out: false,
        date: 1_700_000_000 + id,
        sender: { id: '1234', screen_name: 'durov' },
        text: `сообщение ${id}`,
        media: null,
        attachments: [],
        reply_to_id: null,
        reply_to: null,
        edit_date: null,
        reply_markup: null,
        cmid: id - 6000,
        fwd: [],
        action: null,
        payload: null,
    }, extra || {});
}

const DIALOG = { id: '2000000007', kind: 'chat', title: 'Проектная' };
S.vkState.dialogs = [DIALOG];
S.vkState.selfUser = { id: '999' };
C.initChat({ onRead: () => {}, presenceOf: () => null });

const history = (msgs, hasMore) => async (url, init) => {
    if (init && init.method === 'POST') return jsonRes(200, { ok: true });
    if (url.includes('/messages')) return jsonRes(200, { messages: msgs, has_more: !!hasMore });
    return jsonRes(200, {});
};

async function open(msgs, hasMore) {
    setNet(history(msgs, hasMore));
    await C.openDialog('2000000007', DIALOG);
    await settle();
    scrolls.length = 0;
    ro.ops.length = 0;
}

// ---------------------------------------------------------------------------
// 1. Прежнее поведение БЕЗ нажатия
// ---------------------------------------------------------------------------

await open([message(7001), message(7002)], false);

{
    // Пользователь читает историю: лента далеко от низа — не трогаем.
    layout(100, 1000, 200);
    C.onNewMessage(message(7003));
    eq($log.scrollTop, 100, 'читающего историю новое сообщение НЕ дёргает');
    eq(scrolls.length, 0, 'и плавных доскроллов не порождает');

    // Пользователь у нижней кромки: прежнее прилипание.
    layout(760, 1000, 200);
    C.onNewMessage(message(7004));
    eq($log.scrollTop, $log.scrollHeight, 'у нижней кромки лента прилипает к низу');
    eq(scrolls.length, 0, 'прилипание — мгновенное присваивание, а не плавный доскролл');
    eq(C._debugChatState().following.dialogId, null, 'следование при этом не включалось');
}

// ---------------------------------------------------------------------------
// 2. Нажатие включает следование и берёт строку под наблюдение
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    const row = rowByCmid(1002);
    C.noteButtonPress('2000000007', 1002);

    eq(C._debugChatState().following.dialogId, '2000000007', 'следование включено');
    eq(C._debugChatState().following.messageId, 1002, 'цель — cmid сообщения с кнопкой');
    ok(C._debugChatState().observingRow, 'строка взята под наблюдение');
    eq(ro.created, 1, 'наблюдатель создан ровно один');
    ok(ro.ops.some(([op, n]) => op === 'observe' && n === row), 'наблюдается именно эта строка');

    // Пачка нажатий не плодит наблюдателей.
    C.noteButtonPress('2000000007', 1001);
    eq(ro.created, 1, 'второе нажатие переиспользует наблюдателя, а не создаёт новый');
    ok(ro.ops.some(([op]) => op === 'unobserve'), 'наблюдение переставлено, а не продублировано');
    eq(C._debugChatState().following.messageId, 1001, 'цель перецелена на последнее нажатие');
}

// ---------------------------------------------------------------------------
// 3. Доскролл после правки и после нового сообщения
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    // Лента отлистана вверх — обычного stick-to-bottom не хватило бы.
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    setRow(1002, 50, 300); // низ строки на 350 — под кромкой 200
    scrolls.length = 0;

    // Точечная ветка onEdited (текст изменился, вложений нет).
    C.onEdited(message(7002, { text: 'бот дописал', edit_date: 1_700_000_900 }));
    eq(scrolls.length, 1, 'правка сообщения с кнопкой утянула ленту к концу блока');
    eq(scrolls[0].top, 100 + (350 - 200) + 8, 'цель доскролла — конец блока плюс зазор');
    eq(scrolls[0].behavior, 'smooth', 'плавно');

    // Ответ НОВЫМ сообщением — тот же результат нажатия.
    layout(100, 1000, 200);
    scrolls.length = 0;
    setRow(1005, 60, 260);
    C.onNewMessage(message(7005, { text: 'ответ бота' }));
    eq(scrolls.length, 1, 'новое сообщение при живом следовании тоже тянет ленту');

    // Правка ЧУЖОГО сообщения в той же беседе ленту не двигает.
    layout(100, 1000, 200);
    scrolls.length = 0;
    C.onEdited(message(7001, { text: 'сосед поправил', edit_date: 1_700_000_901 }));
    eq(scrolls.length, 0, 'правка не той строки следованием не считается');
    eq($log.scrollTop, 100, 'и лента стоит на месте');
}

// ---------------------------------------------------------------------------
// 4. Отложенный рост добирается наблюдателем
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    const row = rowByCmid(1002);
    setRow(1002, 50, 300);
    scrolls.length = 0;

    // Картинка догрузилась через кадр после правки — доскролл в момент замены
    // строки её роста ещё не видел.
    ro.cb([{ target: row }]);
    eq(scrolls.length, 1, 'отложенный рост добран наблюдателем');
    eq(scrolls[0].top, 258, 'цель та же — конец блока плюс зазор');
}

// ---------------------------------------------------------------------------
// 5. prefers-reduced-motion и фолбэк без scrollTo
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    setRow(1002, 50, 300);

    reduceMotion = true;
    scrolls.length = 0;
    ro.cb([]);
    eq(scrolls[0].behavior, 'auto', 'системная настройка «меньше движения» уважается');
    reduceMotion = false;

    // Окружение без scrollTo (старый WebView): доскролл обязан состояться
    // присваиванием, а не уронить ленту.
    const saved = $log.scrollTo;
    $log.scrollTo = undefined;
    layout(100, 1000, 200);
    ro.cb([]);
    eq($log.scrollTop, 258, 'без scrollTo доскролл сделан присваиванием');
    $log.scrollTo = saved;
}

// ---------------------------------------------------------------------------
// 6. Отмена прокруткой вверх и истечением окна
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(500, 1000, 200);
    // Первое событие scroll синхронизирует lastScrollTop.
    dispatch($log, 'scroll');
    C.noteButtonPress('2000000007', 1002);
    ok(C._debugChatState().following.dialogId, 'следование включено');

    // Дребезг в несколько пикселей следование НЕ гасит.
    $log.scrollTop = 495;
    dispatch($log, 'scroll');
    ok(C._debugChatState().following.dialogId, 'отскок ленты на 5px следование не отменяет');

    // Настоящий уход вверх — гасит.
    $log.scrollTop = 300;
    dispatch($log, 'scroll');
    eq(C._debugChatState().following.dialogId, null, 'прокрутка вверх отменяет следование');
    ok(!C._debugChatState().observingRow, 'и снимает наблюдение');

    // Собственный доскролл (вниз) следование не трогает.
    C.noteButtonPress('2000000007', 1002);
    $log.scrollTop = 900;
    dispatch($log, 'scroll');
    ok(C._debugChatState().following.dialogId, 'движение ВНИЗ — это наш доскролл, а не рука');
}

{
    // Окно истекло: наблюдатель снимает сам себя при первом же срабатывании.
    await open([message(7001), message(7002)], false);
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    const row = rowByCmid(1002);
    setRow(1002, 50, 300);

    // Сдвигаем часы вперёд подменой Date.now — иначе тест ждал бы 12 секунд.
    const realNow = Date.now;
    Date.now = () => realNow() + 20_000;
    scrolls.length = 0;
    ro.cb([{ target: row }]);
    eq(scrolls.length, 0, 'после истечения окна лента не двигается');
    ok(!C._debugChatState().observingRow, 'наблюдатель снял себя сам');
    Date.now = realNow;
}

// ---------------------------------------------------------------------------
// 7. Жизненный цикл наблюдения
// ---------------------------------------------------------------------------

{
    // Смена беседы снимает и следование, и наблюдение.
    await open([message(7001), message(7002)], false);
    C.noteButtonPress('2000000007', 1002);
    setNet(history([message(8001, { dialog_id: '1234', cmid: 2001 })], false));
    await C.openDialog('1234', { id: '1234', kind: 'user', title: 'Павел' });
    await settle();
    eq(C._debugChatState().following.dialogId, null, 'следование принадлежало покинутой беседе');
    ok(!C._debugChatState().observingRow, 'наблюдение снято');

    // Перерисовка истории переставляет наблюдение на НОВЫЙ узел: строки
    // пересозданы, и наблюдатель на старом молчал бы навсегда.
    await open([message(7001), message(7002)], true);
    C.noteButtonPress('2000000007', 1002);
    const before = rowByCmid(1002);
    setNet(async (url, init) => {
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (url.includes('before_id=')) {
            return jsonRes(200, { messages: [message(6999, { cmid: 999 })], has_more: false });
        }
        return jsonRes(200, { messages: [message(7001), message(7002)], has_more: true });
    });
    $log.scrollHeight = 1000; $log.clientHeight = 200; $log.scrollTop = 0;
    await C.loadOlder(DIALOG);
    await settle(20);
    const after = rowByCmid(1002);
    ok(after && after !== before, 'строка пересоздана перерисовкой истории');
    ok(C._debugChatState().observingRow, 'наблюдение переехало на живой узел');
    ok(ro.ops.some(([op, n]) => op === 'observe' && n === after), 'наблюдается новый узел');

    // Удаление наблюдаемого сообщения снимает наблюдение.
    C.onDeleted({ dialog_id: '2000000007', message_ids: [7002] });
    ok(!C._debugChatState().observingRow, 'удалённая строка из-под наблюдения убрана');

    // resetChat гасит всё.
    C.noteButtonPress('2000000007', 1001);
    C.resetChat();
    eq(C._debugChatState().following.dialogId, null, 'resetChat снимает следование');
    ok(!C._debugChatState().observingRow, 'и наблюдение');
}

// ---------------------------------------------------------------------------
// 8. Регресс-щит: переход по цитате центрирует строку
// ---------------------------------------------------------------------------

{
    const reply = message(7300, {
        text: 'ответ',
        reply_to: {
            message_id: 7001, cmid: 1001, sender: 'durov', sender_id: '1234',
            text: 'сообщение 7001', media_kind: null, unavailable: false,
        },
    });
    await open([message(7001), message(7002), reply], false);
    layout(500, 1000, 200);
    C.noteButtonPress('2000000007', 1300);
    const original = rowByCmid(1001);
    // Оригинал ВЫШЕ видимой области: центрирование обязано увести ленту ВВЕРХ.
    setRow(1001, -300, 40);
    scrolls.length = 0;

    dispatch($log, 'click', { target: rowByCmid(1300).querySelector('.vk-reply-quote') });
    await settle();
    eq(C._debugChatState().following.dialogId, null,
        'переход по цитате гасит следование — иначе оно вернуло бы ленту вниз');
    ok($log.scrollTop < 500, `лента уехала ВВЕРХ к оригиналу: ${$log.scrollTop}`);
    eq(scrolls.length, 0, 'центрирование делается присваиванием, а не плавным доскроллом');
    ok(original.className.includes('vk-msg-highlight'), 'оригинал подсвечен');
}

// ---------------------------------------------------------------------------
// 9. Своя отправка по-прежнему уезжает в самый низ
// ---------------------------------------------------------------------------

{
    setNet(async (url, init) => {
        if (init && init.method === 'POST' && url.includes('/messages')) {
            return jsonRes(200, { message: message(7400, { out: true, cmid: 1400 }) });
        }
        if (url.includes('/messages')) return jsonRes(200, { messages: [message(7001)], has_more: false });
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();
    layout(0, 1000, 200);
    $input.value = 'своё';
    dispatch($prompt, 'submit');
    eq($log.scrollTop, $log.scrollHeight,
        'своя отправка безусловно уезжает в самый низ, даже когда лента отлистана');
    await settle(20);
}

console.log(`vk_scroll_ui: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
