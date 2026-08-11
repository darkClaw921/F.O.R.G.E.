// Регресс-тест ПЕРЕХОДА ПО ЦИТАТЕ панели «Битрикс24» на фейковом DOM
// (forge-92x6).
//
// Грузит РЕАЛЬНЫЙ `js/bitrix/chat.js` поверх bitrix_dom_stub.mjs. Стаб обязан
// импортироваться ПЕРВЫМ: `core/dom.js` резолвит все `$b24*` один раз на
// верхнем уровне модуля.
//
// Дефект, ради которого тест существует: при клике на сообщение-ответ панель
// НИКАК не показывала, на что именно отвечали, — цитата была обычным текстом
// «> Автор: …» в начале строки. У VK клик по цитате уносит к оригиналу и
// подсвечивает его (`vk/chat.js::jumpToMessage`); здесь то же самое, но
// оригинал приходится ИСКАТЬ: `REPLY_ID` при чтении портал не отражает (Р10).
//
// Что покрыто:
//   • ответ рисует ОТДЕЛЬНЫЙ узел цитаты, а её текст уходит из тела строки;
//   • клик по цитате подсвечивает оригинал классом `b24-line-flash`;
//   • подсветка переживает перерисовку ленты и снимается по таймеру;
//   • оригинал выше загруженного догружается страницей истории;
//   • не нашли — панель говорит об этом строкой над полем ввода, а не молчит;
//   • двойной клик по цитате НЕ начинает ответ на ответ.
//
// Запуск: node tmux-web/tests/frontend/bitrix_reply_ui.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { $log, $promptNote, flush, jsonRes, setNet } from './bitrix_dom_stub.mjs';

const chat = await import(new URL('../../static/js/bitrix/chat.js', import.meta.url).href);

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

function msg(id, text, name = 'Максим Крылов') {
    return {
        id,
        dialog_id: 'chat27',
        out: false,
        date: '2026-08-05T20:20:35+03:00',
        sender: { id: '15', name, color: '#3e99ce', bot: false },
        system: false,
        text,
        segments: [{ kind: 'text', text, value: null, children: [] }],
        media: [],
        cards: [],
        reply_markup: null,
        edited: false,
        likes: [],
        action: null,
        unread: false,
        reply_to: null,
        fwd: null,
    };
}

/**
 * Ждёт РЕАЛЬНОГО времени: `flush` гоняет только микрозадачи, а подсветку
 * снимает `setTimeout`, и без настоящей паузы таймер не сработает.
 */
function wait(ms) {
    return new Promise((resolve) => { setTimeout(resolve, ms); });
}

function page(messages, hasMore = false) {
    return jsonRes(200, { messages, has_more: hasMore });
}

function lines() {
    return $log.childNodes.filter((n) => String(n.className || '').startsWith('b24-line'));
}

function rowById(id) {
    return lines().find((n) => n.dataset.msgId === String(id)) || null;
}

/** Узел цитаты внутри строки (его строит chat.js::buildQuoteNode). */
function quoteNode(id) {
    const row = rowById(id);
    return row ? row.querySelector('.b24-line-quote') : null;
}

chat.initChat({});

// ---------------------------------------------------------------------------
// 1. Цитата рисуется отдельным узлом и уходит из тела строки
// ---------------------------------------------------------------------------

{
    setNet(async () => page([
        msg(100, 'где отчёт по спринту'),
        msg(102, 'не по теме'),
        msg(104, '> Максим Крылов: где отчёт по спринту\nсдал вчера'),
    ]));
    await chat.openDialog('chat27', { id: 'chat27', title: 'probe', kind: 'chat', can_write: true });
    await flush(8);

    eq(lines().length, 3, 'три строки нарисованы');
    const quote = quoteNode(104);
    ok(quote !== null, 'у ответа появился отдельный узел цитаты');
    eq(quote && quote.tagName, 'button', 'цитата — кнопка: фокус и Enter достаются бесплатно');
    eq(quote && quote.dataset.quoteFor, '104', 'цитата помнит, в какой строке она нарисована');
    ok(String(quote.textContent).includes('где отчёт'), 'в цитате виден текст оригинала');
    ok(String(quote.textContent).includes('Максим Крылов'), 'в цитате виден автор оригинала');

    const bodyText = rowById(104).querySelector('.b24-line-text');
    eq(String(bodyText.textContent).trim(), 'сдал вчера', 'текст ответа остался без строки цитаты');
    ok(!String(bodyText.textContent).includes('>'), 'стрелочка цитаты в тело не протекла');

    // Обычная строка узла цитаты не получает вовсе.
    eq(quoteNode(102), null, 'у обычной строки цитаты нет');
}

// ---------------------------------------------------------------------------
// 2. Клик по цитате подсвечивает оригинал
// ---------------------------------------------------------------------------

{
    quoteNode(104).click();
    await flush(2);

    const origin = rowById(100);
    ok(origin.className.includes('b24-line-flash'), 'оригинал подсвечен');
    eq(chat._debugChatState().flashId, 100, 'подсвеченная строка запомнена по id');
    ok(!rowById(102).className.includes('b24-line-flash'), 'соседняя строка не подсвечена');
    ok(!chat._debugChatState().stickBottom, 'переход вверх снял защёлку следования за низом');

    // Перерисовка ленты подсветку не теряет: класс возвращается по id.
    chat.appendMessage(msg(106, 'ещё строка'));
    ok(rowById(100).className.includes('b24-line-flash'), 'подсветка пережила перерисовку');

    // Таймер снимает её один раз в конце.
    await wait(chat.REPLY_FLASH_MS + 60);
    eq(chat._debugChatState().flashId, null, 'подсветка снята таймером');
    ok(!rowById(100).className.includes('b24-line-flash'), 'класс со строки убран');
}

// ---------------------------------------------------------------------------
// 3. Оригинал выше загруженного — догружается страницей истории
// ---------------------------------------------------------------------------

{
    setNet(async () => page([
        msg(200, 'середина'),
        msg(202, '> Максим Крылов: самое начало\nответ на давнее'),
    ], true));
    await chat.openDialog('chat27', { id: 'chat27', title: 'probe', kind: 'chat', can_write: true });
    await flush(8);
    eq(quoteNode(202) !== null, true, 'цитата нарисована и на давний оригинал');

    // Следующий запрос отдаёт страницу ВЫШЕ загруженной — с оригиналом.
    setNet(async () => page([msg(198, 'самое начало')], false));
    quoteNode(202).click();
    await flush(20);

    ok(rowById(198) !== null, 'страница истории догружена');
    ok(rowById(198).className.includes('b24-line-flash'), 'оригинал из догруженной страницы подсвечен');
    eq(chat._debugChatState().jumping, false, 'флаг перехода снят');
    await wait(chat.REPLY_FLASH_MS + 60);
}

// ---------------------------------------------------------------------------
// 4. Оригинала нет — панель говорит об этом, а не молчит
// ---------------------------------------------------------------------------

{
    setNet(async () => page([msg(300, '> Кто-то: чего в истории нет\nответ')], false));
    await chat.openDialog('chat27', { id: 'chat27', title: 'probe', kind: 'chat', can_write: true });
    await flush(8);

    quoteNode(300).click();
    await flush(20);

    eq(chat._debugChatState().flashId, null, 'ничего не подсвечено');
    ok(!$promptNote.hidden, 'строка над полем ввода показана');
    ok(String($promptNote.textContent).includes('оригинал не найден'), 'причина названа словами');
}

// ---------------------------------------------------------------------------
// 5. Двойной клик по цитате не превращается в «ответить на ответ»
// ---------------------------------------------------------------------------

{
    eq(chat.classifyLineDblClick(quoteNode(300)), 'ignore', 'двойной клик по цитате игнорируется');
    ok(
        chat.REPLY_DBLCLICK_IGNORE.includes('.b24-line-quote'),
        'цитата перечислена в списке исключений явно',
    );
}

console.log(`bitrix_reply_ui: проверок ${checks}, провалов ${failures}`);
process.exit(failures === 0 ? 0 : 1);
