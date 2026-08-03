// Регресс-тест «следования» ленты VK — КРАЕВЫЕ УГЛЫ.
//
// Свой процесс и свой стенд: здесь ResizeObserver СНАЧАЛА ОТСУТСТВУЕТ (старый
// WebView, node без DOM), и часть сценариев проверяет именно деградацию — а
// подключается он в середине файла, потому что `chat.js::ensureFollowObserver`
// спрашивает про него в момент вызова, а не на импорте.
//
// Что проверяется (типовые сценарии — в vk_scroll_ui.test.mjs):
//
//   1. ПУСТАЯ ЛЕНТА и ЛЕНТА КОРОЧЕ ЭКРАНА — прокрутки нет, падений тоже;
//   2. ГРАНИЦА isAtBottom — ровно на пороге прилипание ещё работает;
//   3. СООБЩЕНИЕ СТАЛО КОРОЧЕ — ленту вверх не тянет никогда;
//   4. АДРЕСНОСТЬ — чужая беседа и СОВПАДАЮЩИЙ cmid в соседней беседе
//      (у VK cmid нумеруется в каждой беседе с единицы — это не экзотика,
//      а обычное дело);
//   5. ПОДГРУЗКА ИСТОРИИ ВВЕРХ — якорь чтения не сбивается ни доскроллом, ни
//      наблюдателем;
//   6. ЖИЗНЬ ПОСЛЕ ОТМЕНЫ — запоздалое срабатывание наблюдателя ленту не
//      двигает;
//   7. ДЕГРАДАЦИЯ без ResizeObserver: доскролл в момент правки продолжает
//      работать, теряется только добор отложенного роста.
//
// Запуск: node tmux-web/tests/frontend/vk_scroll_edge.test.mjs

import {
    $log, $document,
    dispatch, flush, jsonRes, setNet,
} from './vk_dom_stub.mjs';

const scrolls = [];
$log.scrollTo = (opts) => {
    scrolls.push({ top: opts.top, behavior: opts.behavior });
    $log.scrollTop = opts.top;
};

globalThis.window.matchMedia = () => ({
    matches: false, addEventListener() {}, removeEventListener() {},
});

/** Модель высоты строк: cmid → координаты относительно верха ленты. */
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

// ResizeObserver ПОКА НЕТ — это часть сценария (см. шапку).
const ro = { created: 0, ops: [], cb: null };

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

function layout(scrollTop, scrollHeight, clientHeight) {
    $log.scrollTop = scrollTop;
    $log.scrollHeight = scrollHeight;
    $log.clientHeight = clientHeight;
    $log.getBoundingClientRect = () => ({
        top: 0, bottom: clientHeight, left: 0, right: 100, width: 100, height: clientHeight,
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
const OTHER = { id: '1234', kind: 'user', title: 'Павел' };
S.vkState.dialogs = [DIALOG, OTHER];
S.vkState.selfUser = { id: '999' };
C.initChat({ onRead: () => {}, presenceOf: () => null });

const history = (msgs, hasMore) => async (url, init) => {
    if (init && init.method === 'POST') return jsonRes(200, { ok: true });
    if (url.includes('/messages')) return jsonRes(200, { messages: msgs, has_more: !!hasMore });
    return jsonRes(200, {});
};

async function open(msgs, hasMore, dialog) {
    setNet(history(msgs, hasMore));
    const d = dialog || DIALOG;
    await C.openDialog(String(d.id), d);
    await settle();
    scrolls.length = 0;
    ro.ops.length = 0;
}

// ---------------------------------------------------------------------------
// 1. Пустая лента и лента короче экрана
// ---------------------------------------------------------------------------

{
    await open([], false);
    eq(lines().length, 0, 'беседа пуста');
    layout(0, 100, 300); // содержимое короче окна — прокрутки не существует
    C.noteButtonPress('2000000007', 1001);
    ok(!C._debugChatState().observingRow, 'наблюдать нечего — строки такой нет');

    setRow(1005, 0, 40);
    C.onNewMessage(message(7005));
    eq(scrolls.length, 0, 'на ленте короче экрана доскролл не нужен и не делается');
    eq($log.scrollTop, 0, 'и прокручивать нечего — потолок прокрутки нулевой');
    eq(lines().length, 1, 'сообщение при этом нарисовано');
}

// ---------------------------------------------------------------------------
// 2. Граница isAtBottom
// ---------------------------------------------------------------------------

{
    // Следование из прошлого раздела снимаем явно: повторное открытие ТОЙ ЖЕ
    // беседы его сохраняет намеренно (кнопка [retry]), а здесь проверяется
    // поведение БЕЗ него.
    C.resetChat();
    await open([message(7001)], false);
    // Порог прилипания — 40px (format.js::isAtBottom).
    layout(760, 1000, 200); // gap = 40 — ровно на пороге
    C.onNewMessage(message(7002));
    eq($log.scrollTop, $log.scrollHeight, 'ровно на пороге лента ещё прилипает');

    layout(759, 1000, 200); // gap = 41 — на пиксель дальше
    C.onNewMessage(message(7003));
    eq($log.scrollTop, 759, 'на пиксель дальше порога — уже чтение истории, лента стоит');
}

// ---------------------------------------------------------------------------
// 3. Деградация без ResizeObserver (он ещё не определён)
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    setRow(1002, 50, 300);
    ok(C._debugChatState().observingRow,
        'узел запомнен даже без ResizeObserver — иначе теряется и доскролл при правке');

    C.onEdited(message(7002, { text: 'дописал', edit_date: 1_700_000_900 }));
    eq(scrolls.length, 1, 'без ResizeObserver доскролл в момент правки работает');
    eq(scrolls[0].top, 258, 'и уводит к концу блока');
}

// Дальше наблюдатель нужен: подключаем его так же, как это сделал бы браузер.
globalThis.ResizeObserver = class {
    constructor(cb) { ro.created += 1; ro.cb = cb; }
    observe(node) { ro.ops.push(['observe', node]); }
    unobserve(node) { ro.ops.push(['unobserve', node]); }
    disconnect() { ro.ops.push(['disconnect', null]); }
};

// ---------------------------------------------------------------------------
// 4. Сообщение стало КОРОЧЕ — вверх не тянем
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(400, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    // Строка целиком выше видимой области: догонять нечего, а «центрировать»
    // следование не обязано — это работа перехода по цитате.
    setRow(1002, -400, 60);
    scrolls.length = 0;
    C.onEdited(message(7002, { text: 'короче', edit_date: 1_700_000_901 }));
    eq(scrolls.length, 0, 'вверх следование не тянет никогда');
    eq($log.scrollTop, 400, 'лента осталась на месте');
}

// ---------------------------------------------------------------------------
// 5. Адресность: чужая беседа и совпадающий cmid
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1002);
    setRow(1002, 50, 300);
    scrolls.length = 0;

    // Правка в ДРУГОЙ беседе с ТЕМ ЖЕ cmid: у VK нумерация внутри беседы, и
    // 1002 существует в каждой. Без сверки беседы лента дёрнулась бы.
    S.vkState.messages.set('1234', [message(9002, { dialog_id: '1234', cmid: 1002 })]);
    C.onEdited(message(9002, { dialog_id: '1234', cmid: 1002, text: 'чужое', edit_date: 1 }));
    eq(scrolls.length, 0, 'совпадающий cmid в СОСЕДНЕЙ беседе ленту не двигает');
    eq($log.scrollTop, 100, 'лента не шелохнулась');

    // Новое сообщение в чужой беседе — тоже.
    C.onNewMessage(message(9003, { dialog_id: '1234', cmid: 1003 }));
    eq(scrolls.length, 0, 'сообщение чужой беседы следование не дёргает');

    // А своё — двигает.
    setRow(1006, 60, 280);
    C.onNewMessage(message(7006));
    eq(scrolls.length, 1, 'в своей беседе следование работает');
}

// ---------------------------------------------------------------------------
// 6. Подгрузка истории вверх: якорь чтения
// ---------------------------------------------------------------------------

{
    await open([message(7005), message(7006)], true);
    C.noteButtonPress('2000000007', 1006);
    setRow(1006, 50, 300);
    layout(300, 1000, 200);
    scrolls.length = 0;

    setNet(async (url, init) => {
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (url.includes('before_id=')) {
            // Пока идёт загрузка, лента «выросла» — как это делает браузер.
            $log.scrollHeight = 1400;
            return jsonRes(200, { messages: [message(7003), message(7004)], has_more: false });
        }
        return jsonRes(200, { messages: [message(7005), message(7006)], has_more: true });
    });
    await C.loadOlder(DIALOG);
    await settle(20);

    eq(lines().length, 4, 'страницы склеены');
    eq($log.scrollTop, 300 + (1400 - 1000),
        'якорь чтения сохранён: доскролл следования внутрь окна загрузки не влез');
    eq(scrolls.length, 0, 'плавных доскроллов во время подгрузки не было');
}

// ---------------------------------------------------------------------------
// 7. Жизнь после отмены: запоздалое срабатывание наблюдателя
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(500, 1000, 200);
    dispatch($log, 'scroll'); // синхронизируем lastScrollTop
    C.noteButtonPress('2000000007', 1002);
    setRow(1002, 50, 300);
    const row = rowByCmid(1002);

    // Пользователь ушёл вверх — следование снято.
    $log.scrollTop = 200;
    dispatch($log, 'scroll');
    eq(C._debugChatState().following.dialogId, null, 'следование отменено рукой');

    // Наблюдатель мог быть уже в очереди кадра: его запоздалый вызов не имеет
    // права вернуть ленту вниз.
    layout(200, 1000, 200);
    scrolls.length = 0;
    if (ro.cb) ro.cb([{ target: row }]);
    eq(scrolls.length, 0, 'запоздалое срабатывание после отмены ленту не двигает');
    eq($log.scrollTop, 200, 'пользователь остался там, куда ушёл');
}

// ---------------------------------------------------------------------------
// 8. Две кнопки подряд в разных сообщениях
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002)], false);
    layout(100, 1000, 200);
    C.noteButtonPress('2000000007', 1001);
    C.noteButtonPress('2000000007', 1002);
    eq(C._debugChatState().following.messageId, 1002, 'актуально последнее нажатие');

    setRow(1001, 40, 100);
    setRow(1002, 50, 300);
    scrolls.length = 0;
    // Правка ПЕРВОГО сообщения (ответ на устаревшее нажатие) ленту не тянет.
    C.onEdited(message(7001, { text: 'старый ответ', edit_date: 1_700_000_902 }));
    eq(scrolls.length, 0, 'результат устаревшего нажатия ленту не дёргает');
    // А второго — тянет.
    C.onEdited(message(7002, { text: 'свежий ответ', edit_date: 1_700_000_903 }));
    eq(scrolls.length, 1, 'результат актуального нажатия — тянет');
}

// ---------------------------------------------------------------------------
// 9. Нажатие из СЕРЕДИНЫ истории
// ---------------------------------------------------------------------------

{
    await open([message(7001), message(7002), message(7003), message(7004)], false);
    // Пользователь читает середину: до низа далеко.
    layout(50, 2000, 200);
    C.noteButtonPress('2000000007', 1002);
    setRow(1002, 20, 400); // низ строки на 420 — далеко под кромкой
    scrolls.length = 0;
    C.onEdited(message(7002, { text: 'вырос', edit_date: 1_700_000_904 }));
    eq(scrolls.length, 1, 'из середины истории следование тоже работает');
    eq(scrolls[0].top, 50 + (420 - 200) + 8, 'цель — конец блока, а не низ ленты');
    ok(scrolls[0].top < 2000 - 200, 'и это НЕ «в самый низ»: пользователь остался у своего блока');
}

console.log(`vk_scroll_edge: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
