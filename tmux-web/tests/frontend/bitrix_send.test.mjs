// Регресс-тест ЦИКЛА ПЕРЕПИСКИ панели «Битрикс24» (Ф6) на фейковом DOM.
//
// Грузит РЕАЛЬНЫЕ модули `js/bitrix/{chat,api,botkb}.js` поверх
// bitrix_dom_stub.mjs. Стаб обязан импортироваться ПЕРВЫМ: `core/dom.js`
// резолвит все `$b24*` один раз на верхнем уровне модуля.
//
// Что покрыто (в порядке значимости):
//
//   • ОПТИМИСТИЧНАЯ ОТПРАВКА: строка появляется до ответа портала и
//     заменяется настоящей — по её собственному идентификатору, а не по
//     совпадению текста (портал переписывает ссылки при URL_PREVIEW, и сырые
//     строки не сойдутся никогда);
//   • ОТСУТСТВИЕ АВТОПОВТОРА (Р9): ошибка оставляет строку с кнопкой «⟳», а
//     повтор уходит в сеть ТОЛЬКО от нажатия — ровно один раз;
//   • СЧЁТЧИК СИМВОЛОВ (Р12): 20 001 символ не порождает запроса вовсе;
//   • ПРАВКА и УДАЛЕНИЕ: метод, путь, тело, замена строки в ленте;
//   • ОТВЕТ (Р10): reply_to + автор и текст цитаты уходят в теле;
//   • ЛАЙК: новое состояние берётся из перечитанной сервером строки;
//   • «ПЕЧАТАЕТ…»: не чаще одного запроса в окно;
//   • ОТМЕТКА О ПРОЧТЕНИИ: дебаунс — один запрос на серию.
//
// Запуск: node tmux-web/tests/frontend/bitrix_send.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $charCount, $input, $log, $promptNote, $send, dispatch, flush, jsonRes, netCalls, setNet,
} from './bitrix_dom_stub.mjs';

const chat = await import(new URL('../../static/js/bitrix/chat.js', import.meta.url).href);
const state = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);

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

/** Кнопка строки по значению `data-action`. */
function lineButton(msgId, action) {
    const row = lines().find((n) => n.dataset.msgId === String(msgId));
    if (!row) return null;
    return walk(row).find((n) => n.dataset && n.dataset.action === action) || null;
}

/** Сообщение в форме MessageDto, как его отдаёт бэкенд. */
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
        action: null,
        unread: false,
        reply_to: null,
        fwd: null,
        ...over,
        text,
        segments: over.segments || [{ kind: 'text', text, value: null, children: [] }],
    };
}

/** Журнал сети с момента отметки. */
function netSince(mark) {
    return netCalls.slice(mark);
}

state.b24State.selfUser = { id: '1', name: 'Игорь', last_name: 'Герасимов' };
chat.initChat({});

// ---------------------------------------------------------------------------
// 1. Оптимистичная отправка: строка появляется сразу и заменяется настоящей
// ---------------------------------------------------------------------------

{
    setNet(async (url, init) => {
        const method = (init && init.method) || 'GET';
        if (url.includes('/messages') && method === 'GET') {
            return jsonRes(200, { messages: [msg(101)], has_more: false });
        }
        if (url.includes('/messages') && method === 'POST') {
            // Портал переписал текст: голая ссылка обёрнута в [URL].
            return jsonRes(200, {
                id: 114,
                echoed: true,
                message: msg(114, { out: true, text: 'смотри https://example.com' }),
            });
        }
        return jsonRes(200, { ok: true, sent: true });
    });

    await chat.openDialog('chat27', { id: 'chat27', kind: 'chat', title: 'FORGE probe chat' });
    await flush(8);

    $input.value = 'смотри https://example.com';
    const mark = netCalls.length;
    const sending = chat.submitPrompt();

    // ДО ответа портала строка уже в ленте и помечена «отправляется».
    const pendingIds = lines()
        .filter((n) => String(n.className).includes('b24-line-pending'))
        .map((n) => Number(n.dataset.msgId));
    eq(pendingIds.length, 1, 'оптимистичная строка появилась мгновенно');
    ok(pendingIds[0] > chat.TEMP_ID_BASE, 'временный id больше любого портального');
    eq($input.value, '', 'поле ввода очищено сразу — набор следующего не ждёт сети');

    await sending;
    await flush(8);

    const ids = lines().map((n) => Number(n.dataset.msgId));
    eq(ids, [101, 114], 'оптимистичная строка ЗАМЕНЕНА настоящей, дубля нет');
    eq(
        lines().filter((n) => String(n.className).includes('b24-line-pending')).length,
        0,
        'пометка «отправляется» снята',
    );

    const post = netSince(mark).find((c) => c.method === 'POST');
    eq(post.url, '/api/bitrix/dialogs/chat27/messages', 'путь отправки');
    eq(post.body, { text: 'смотри https://example.com' }, 'тело отправки — только текст');
}

// ---------------------------------------------------------------------------
// 2. Р9: ошибка НЕ повторяется сама, строка не теряет текст
// ---------------------------------------------------------------------------

{
    let attempts = 0;
    setNet(async (url, init) => {
        const method = (init && init.method) || 'GET';
        if (url.includes('/messages') && method === 'POST') {
            attempts += 1;
            if (attempts === 1) {
                return jsonRes(503, { error: { code: 'not_connected', message: 'нет связи' } });
            }
            return jsonRes(200, { id: 116, echoed: true, message: msg(116, { out: true, text: 'вторая попытка' }) });
        }
        return jsonRes(200, { ok: true, sent: true });
    });

    $input.value = 'вторая попытка';
    await chat.submitPrompt();
    await flush(8);

    eq(attempts, 1, 'АВТОПОВТОРА НЕТ: одна попытка на одно нажатие (Р9)');
    const failed = lines().filter((n) => String(n.className).includes('b24-line-failed'));
    eq(failed.length, 1, 'неотправленная строка осталась в ленте');
    ok(failed[0].textContent.includes('вторая попытка'), 'текст не потерян');

    const retry = lineButton(failed[0].dataset.msgId, 'retry');
    ok(retry !== null, 'на строке есть кнопка «повторить»');
    ok(lineButton(failed[0].dataset.msgId, 'discard') !== null, 'и кнопка «убрать»');

    // Повтор уходит в сеть ТОЛЬКО от нажатия.
    dispatch(retry, 'click', {});
    await flush(10);
    eq(attempts, 2, 'повтор состоялся ровно один раз — по решению пользователя');
    const ids = lines().map((n) => Number(n.dataset.msgId));
    eq(ids, [101, 114, 116], 'после повтора строка встала настоящей');
    eq(lines().filter((n) => String(n.className).includes('b24-line-failed')).length, 0,
        'пометка отказа снята');
}

// ---------------------------------------------------------------------------
// 3. Р12: счётчик символов и блокировка отправки
// ---------------------------------------------------------------------------

{
    eq(chat.charCountState('привет').len, 6, 'длина в символах');
    // Эмодзи — ОДИН символ, хотя String.length считает два: у портала лимит
    // символьный, и разъезд счётов означал бы отказ сервера там, где фронт
    // разрешил.
    eq(chat.charCountState('👍').len, 1, 'эмодзи считается одним символом');
    eq('👍'.length, 2, 'проверка потеряла бы смысл, если бы это было не так');

    $input.value = 'коротко';
    let state1 = chat.updateCharCount();
    eq(state1.over, false, 'короткий текст — не перебор');
    eq($charCount.hidden, true, 'счётчик прячется, пока до лимита далеко');
    eq($send.disabled, false, 'кнопка отправки доступна');

    $input.value = 'я'.repeat(chat.CHAR_WARN_AT);
    chat.updateCharCount();
    eq($charCount.hidden, false, 'на подходе к лимиту счётчик появляется');
    ok($charCount.textContent.includes(String(chat.MAX_MESSAGE_CHARS)), 'в счётчике виден потолок');

    $input.value = 'я'.repeat(chat.MAX_MESSAGE_CHARS + 1);
    const over = chat.updateCharCount();
    eq(over.over, true, 'перебор опознан');
    eq($send.disabled, true, 'кнопка отправки заблокирована ДО запроса');
    ok(String($charCount.className).includes('b24-charcount-over'), 'счётчик покраснел');

    const mark = netCalls.length;
    await chat.submitPrompt();
    await flush(4);
    eq(netSince(mark).length, 0, 'сверхдлинный текст не порождает запроса вовсе');
    ok(!$promptNote.hidden, 'причина отказа показана рядом с полем ввода');
    ok($promptNote.textContent.includes(String(chat.MAX_MESSAGE_CHARS)), 'в объяснении есть лимит');

    $input.value = '';
    chat.updateCharCount();
    eq($send.disabled, false, 'после сокращения отправка снова доступна');
}

// ---------------------------------------------------------------------------
// 4. Р10: ответ уходит с reply_to И текстовой цитатой
// ---------------------------------------------------------------------------

{
    let body = null;
    setNet(async (url, init) => {
        const method = (init && init.method) || 'GET';
        if (url.includes('/messages') && method === 'POST') {
            body = JSON.parse(init.body);
            return jsonRes(200, { id: 118, echoed: true, message: msg(118, { out: true, text: 'мой ответ' }) });
        }
        return jsonRes(200, { ok: true, sent: true });
    });

    chat.startReply(101);
    ok(!$promptNote.hidden, 'режим ответа виден над полем ввода');
    ok($promptNote.textContent.includes('ответ'), 'подпись режима');

    $input.value = 'мой ответ';
    await chat.submitPrompt();
    await flush(8);

    eq(body.reply_to, 101, 'reply_to уехал');
    eq(body.reply_author, 'Максим Крылов', 'автор цитаты уехал');
    ok(typeof body.reply_text === 'string' && body.reply_text.length > 0, 'текст цитаты уехал');
    eq($promptNote.hidden, true, 'после отправки режим ответа снят');

    // Esc снимает режим, не трогая набранное.
    chat.startReply(114);
    eq(chat._debugChatState().mode, 'reply', 'режим ответа включён');
    $input.value = 'черновик';
    dispatch($input, 'keydown', { key: 'Escape' });
    eq(chat._debugChatState().mode, null, 'Esc снял режим ответа');
    eq($input.value, 'черновик', 'набранный ответ не стёрт');
    $input.value = '';
    chat.updateCharCount();
}

// ---------------------------------------------------------------------------
// 5. Правка и удаление своих сообщений
// ---------------------------------------------------------------------------

{
    const seen = [];
    setNet(async (url, init) => {
        const method = (init && init.method) || 'GET';
        seen.push({ url, method, body: init && init.body ? JSON.parse(init.body) : null });
        if (method === 'PATCH') {
            return jsonRes(200, {
                id: 114,
                echoed: true,
                message: msg(114, { out: true, text: 'исправленный текст', edited: true }),
            });
        }
        if (method === 'DELETE') return jsonRes(200, { ok: true, sent: true });
        return jsonRes(200, { ok: true, sent: true });
    });

    // Кнопки правки и удаления есть только у СВОЕЙ строки.
    ok(lineButton(114, 'edit') !== null, 'у своей строки есть «изменить»');
    ok(lineButton(101, 'edit') === null, 'у чужой строки правки нет');
    ok(lineButton(101, 'like') !== null, 'а лайк есть у любой');

    chat.startEdit(114);
    eq(chat._debugChatState().mode, 'edit', 'режим правки включён');
    ok($input.value.length > 0, 'текст сообщения переехал в поле ввода');

    $input.value = 'исправленный текст';
    await chat.submitPrompt();
    await flush(8);

    const patch = seen.find((c) => c.method === 'PATCH');
    eq(patch.url, '/api/bitrix/messages/114', 'путь правки');
    eq(patch.body, { text: 'исправленный текст', dialog_id: 'chat27' }, 'тело правки');
    eq(chat._debugChatState().mode, null, 'после правки режим снят');
    eq($input.value, '', 'поле ввода очищено');

    const row = lines().find((n) => n.dataset.msgId === '114');
    ok(row.textContent.includes('исправленный текст'), 'строка обновилась');
    ok(row.textContent.includes('(изм.)'), 'метка правки появилась');

    // Удаление: подтверждение спрашивается, отказ ничего не делает.
    globalThis.confirm = () => false;
    await chat.deleteLine(114);
    await flush(4);
    ok(lines().some((n) => n.dataset.msgId === '114'), 'отказ от подтверждения оставляет строку');
    ok(!seen.some((c) => c.method === 'DELETE'), 'и не порождает запроса');

    globalThis.confirm = () => true;
    await chat.deleteLine(114);
    await flush(6);
    const del = seen.find((c) => c.method === 'DELETE');
    eq(del.url, '/api/bitrix/messages/114', 'путь удаления');
    ok(!lines().some((n) => n.dataset.msgId === '114'), 'строка исчезла из ленты');
}

// ---------------------------------------------------------------------------
// 6. Лайк: состояние берётся из ПЕРЕЧИТАННОЙ сервером строки
// ---------------------------------------------------------------------------

{
    let liked = false;
    setNet(async (url, init) => {
        const method = (init && init.method) || 'GET';
        if (url.includes('/like') && method === 'POST') {
            liked = !liked;
            return jsonRes(200, {
                id: 101,
                echoed: true,
                message: msg(101, { likes: liked ? ['1'] : [] }),
            });
        }
        return jsonRes(200, { ok: true, sent: true });
    });

    const mark = netCalls.length;
    dispatch(lineButton(101, 'like'), 'click', {});
    await flush(8);
    let row = lines().find((n) => n.dataset.msgId === '101');
    ok(row.textContent.includes('♥ 1'), 'лайк отразился в строке');

    dispatch(lineButton(101, 'like'), 'click', {});
    await flush(8);
    row = lines().find((n) => n.dataset.msgId === '101');
    ok(!row.textContent.includes('♥ 1'), 'повторный лайк снят — метод переключающий');

    const calls = netSince(mark).filter((c) => c.url.includes('/like'));
    eq(calls.length, 2, 'два нажатия — два запроса');
    eq(calls[0].url, '/api/bitrix/messages/101/like', 'путь лайка');
    eq(calls[0].body, { dialog_id: 'chat27' }, 'диалог нужен, чтобы перечитать строку');
}

// ---------------------------------------------------------------------------
// 7. «Печатает…» и отметка о прочтении троттлятся
// ---------------------------------------------------------------------------

{
    setNet(async () => jsonRes(200, { ok: true, sent: true }));

    const mark = netCalls.length;
    $input.value = 'наб';
    dispatch($input, 'input', {});
    $input.value = 'набор';
    dispatch($input, 'input', {});
    $input.value = 'набор текста';
    dispatch($input, 'input', {});
    await flush(6);

    const typing = netSince(mark).filter((c) => c.url.includes('/typing'));
    eq(typing.length, 1, 'три события набора — один запрос «печатает…»');
    eq(typing[0].url, '/api/bitrix/dialogs/chat27/typing', 'путь индикатора');
    $input.value = '';
    chat.updateCharCount();

    // Отметка о прочтении: серия вызовов сворачивается в один запрос с самым
    // свежим идентификатором.
    const readMark = netCalls.length;
    chat.scheduleRead('chat27', 101);
    chat.scheduleRead('chat27', 114);
    chat.scheduleRead('chat27', 118);
    eq(netSince(readMark).filter((c) => c.url.includes('/read')).length, 0,
        'до истечения окна отметка не уходит вовсе');

    await new Promise((resolve) => setTimeout(resolve, chat.READ_DEBOUNCE_MS + 60));
    await flush(6);
    const read = netSince(readMark).filter((c) => c.url.includes('/read'));
    eq(read.length, 1, 'серия отметок стоит одного запроса');
    eq(read[0].url, '/api/bitrix/dialogs/chat27/read', 'путь отметки');
    eq(read[0].body, { message_id: 118 }, 'уехал САМЫЙ СВЕЖИЙ идентификатор');
}

// ---------------------------------------------------------------------------
// 8. Смена диалога сбрасывает режим ввода
// ---------------------------------------------------------------------------

{
    setNet(async () => jsonRes(200, { messages: [], has_more: false }));
    chat.startReply(101);
    eq(chat._debugChatState().mode, 'reply', 'режим включён');
    await chat.openDialog('19', { id: '19', kind: 'user', title: 'Алексей' });
    await flush(6);
    eq(chat._debugChatState().mode, null, 'открытие другого диалога снимает режим');
    eq($input.value, '', 'и очищает поле ввода');
}

// ---------------------------------------------------------------------------

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_send)`);
if (failures > 0) process.exit(1);
