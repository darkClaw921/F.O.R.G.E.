// Регресс-тест ЛЕНТЫ панели «Битрикс24» (Ф5) на фейковом DOM.
//
// Грузит РЕАЛЬНЫЕ модули `js/bitrix/{chat,botkb,attachments}.js` поверх
// bitrix_dom_stub.mjs. Стаб обязан импортироваться ПЕРВЫМ: `core/dom.js`
// резолвит все `$b24*` один раз на верхнем уровне модуля, и импортируйся он
// раньше стаба — каждый узел оказался бы `null`, а тест зеленел бы по
// случайности.
//
// Что покрыто:
//   • STICK-TO-BOTTOM ЗАЩЁЛКОЙ: новое сообщение прокручивает ленту, только
//     если пользователь стоял внизу; отлистанная вверх лента не дёргается;
//   • ДОГРУЗКА СВЕРХУ компенсирует высоту — позиция чтения не прыгает;
//   • системная строка (`system: true`) рисуется ОТДЕЛЬНЫМ рядом: без ника,
//     без времени, со своим классом;
//   • разделители дней не дублируются на стыке догруженного куска;
//   • смена диалога полностью очищает ленту и не смешивает истории;
//   • кнопки клавиатуры: PUT подставляет текст в поле ввода, SEND без
//     обработчика и CALL/DIALOG приезжают выключенными С ОБЪЯСНЕНИЕМ;
//   • XSS: подпись кнопки и текст сообщения попадают в документ ТЕКСТОМ.
//
// Запуск: node tmux-web/tests/frontend/bitrix_chat.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { $input, $log, $sessionHead, flush, jsonRes, netCalls, setNet } from './bitrix_dom_stub.mjs';

const chat = await import(new URL('../../static/js/bitrix/chat.js', import.meta.url).href);
const botkb = await import(new URL('../../static/js/bitrix/botkb.js', import.meta.url).href);
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

/** Классы потомков ленты по порядку — так видно раскладку целиком. */
function layout() {
    return $log.childNodes.map((n) => String(n.className || '').split(' ')[0]);
}

/** Сообщение в форме MessageDto, как его отдаёт бэкенд. */
function msg(id, over = {}) {
    return {
        id,
        dialog_id: 'chat27',
        out: false,
        date: '2026-08-05T20:20:35+03:00',
        sender: { id: '15', name: 'Максим Крылов', color: '#3e99ce', bot: false },
        system: false,
        text: `строка ${id}`,
        segments: [{ kind: 'text', text: `строка ${id}`, value: null, children: [] }],
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
    };
}

/** Ответ роута истории. */
function page(messages, hasMore = false) {
    return jsonRes(200, { messages, has_more: hasMore });
}

/** Ставит геометрию ленты: она у фейкового DOM своя. */
function setGeometry({ height, top, view = 300 }) {
    $log.scrollHeight = height;
    $log.scrollTop = top;
    $log.clientHeight = view;
}

// Лента инициализируется один раз — как в панели.
chat.initChat({});

// ---------------------------------------------------------------------------
// 1. Открытие диалога: страница рисуется, порядок сохраняется
// ---------------------------------------------------------------------------

{
    setNet(async () => page([msg(67, { system: true, text: 'Игорь создал чат', action: 'CHAT_JOIN', sender: null }), msg(69), msg(71)]));
    await chat.openDialog('chat27', { id: 'chat27', title: 'FORGE probe chat', kind: 'chat', can_write: true });
    await flush(8);

    eq(netCalls.length, 1, 'открытие диалога стоит одного запроса');
    ok(netCalls[0].url.includes('/api/bitrix/dialogs/chat27/messages'), 'адрес истории собран по контракту');
    ok(netCalls[0].url.includes('limit=50'), 'страница просит потолок платформы');

    eq(lines().length, 3, 'все три строки нарисованы');
    eq($log.childNodes[0].className, 'b24-log-spacer', 'первый потомок ленты — распорка');
    ok(String($sessionHead.textContent).includes('FORGE probe chat'), 'шапка сессии подписана');

    const ids = lines().map((n) => n.dataset.msgId);
    eq(ids, ['67', '69', '71'], 'порядок строк — по возрастанию id');

    const debug = chat._debugChatState();
    eq(debug.dialogId, 'chat27', 'лента знает открытый диалог');
    eq(debug.messages, 3, 'состояние совпадает с DOM');
    eq(debug.oldest, 67, 'самая старая строка запомнена — по ней идёт догрузка');
}

// ---------------------------------------------------------------------------
// 2. Системная строка — отдельный ряд, а не реплика
// ---------------------------------------------------------------------------

{
    const system = lines()[0];
    ok(system.className.includes('b24-line-system'), 'системное сообщение получило свой класс');
    eq(system.dataset.action, 'CHAT_JOIN', 'код действия ленты сохранён в dataset');
    const parts = walk(system).map((n) => String(n.className || ''));
    ok(!parts.some((c) => c.includes('b24-line-nick')), 'у системной строки нет ника');
    ok(!parts.some((c) => c.includes('b24-line-time')), 'у системной строки нет времени');

    const normal = lines()[1];
    ok(!normal.className.includes('b24-line-system'), 'обычная строка системной не считается');
    const normalParts = walk(normal).map((n) => String(n.className || ''));
    ok(normalParts.some((c) => c.includes('b24-line-nick')), 'у обычной строки ник есть');
    ok(normalParts.some((c) => c.includes('b24-line-time')), 'у обычной строки время есть');
}

// ---------------------------------------------------------------------------
// 3. STICK-TO-BOTTOM — ЗАЩЁЛКА
// ---------------------------------------------------------------------------

{
    // Пользователь ВНИЗУ: событие прокрутки взводит защёлку.
    setGeometry({ height: 500, top: 200, view: 300 });
    $log.listeners.get('scroll').forEach((fn) => fn({ type: 'scroll' }));
    ok(chat._debugChatState().stickBottom, 'у нижнего края защёлка взведена');

    $log.scrollHeight = 560;
    chat.appendMessage(msg(73));
    eq($log.scrollTop, 560, 'новое сообщение прокрутило ленту вниз');
    eq(lines().length, 4, 'строка добавлена в конец');

    // Пользователь ОТЛИСТАЛ ВВЕРХ: защёлка снимается, и лента больше не
    // дёргается от чужих сообщений.
    setGeometry({ height: 560, top: 40, view: 300 });
    $log.listeners.get('scroll').forEach((fn) => fn({ type: 'scroll' }));
    ok(!chat._debugChatState().stickBottom, 'вверху защёлка снята');

    $log.scrollHeight = 620;
    chat.appendMessage(msg(75));
    eq($log.scrollTop, 40, 'лента НЕ уехала вниз: пользователь читает историю');
    eq(lines().length, 5, 'строка всё равно добавлена');

    // Повторная вставка того же сообщения (правка) не даёт дубля.
    chat.appendMessage(msg(75, { text: 'исправлено', edited: true, segments: [{ kind: 'text', text: 'исправлено', children: [] }] }));
    eq(lines().length, 5, 'правка не создала вторую строку');
    ok($log.textContent.includes('исправлено'), 'правка видна в ленте');
    ok($log.textContent.includes('(изм.)'), 'пометка о правке нарисована');
}

// ---------------------------------------------------------------------------
// 4. Догрузка сверху компенсирует высоту
// ---------------------------------------------------------------------------

{
    const before = layout().length;
    // Высота ленты в браузере меняется САМА при вставке узлов — здесь она
    // моделируется геттером «двадцать пикселей на потомка». Без модели тест
    // проверял бы арифметику по константе, а не саму компенсацию.
    const ROW_PX = 20;
    Object.defineProperty($log, 'scrollHeight', {
        configurable: true,
        get: () => 100 + $log.childNodes.length * ROW_PX,
    });
    $log.scrollTop = 40;
    $log.clientHeight = 300;

    const older = [msg(61, { date: '2026-08-04T10:00:00+03:00' }), msg(63, { date: '2026-08-04T10:05:00+03:00' })];
    chat.renderOlder(older);

    const grew = (layout().length - before) * ROW_PX;
    ok(grew > 0, 'вставка действительно вырастила ленту');
    eq($log.scrollTop, 40 + grew, 'позиция чтения сдвинута ровно на прирост высоты');
    ok(layout().length > before, 'узлы вставлены');
    eq(layout()[0], 'b24-log-spacer', 'распорка осталась первым потомком');
    const ids = lines().map((n) => n.dataset.msgId);
    eq(ids.slice(0, 3), ['61', '63', '67'], 'догруженные строки встали ПЕРЕД старыми');
    eq(chat._debugChatState().oldest, 61, 'самая старая строка обновилась');

    // Разделители дней: их ровно два (4 и 5 августа), без дублей на стыке.
    const seps = $log.childNodes.filter((n) => n.className === 'b24-date-sep');
    eq(seps.length, 2, 'на стыке кусков не появилось второго разделителя того же дня');

    // Возвращаем обычное поле: дальше высота задаётся вручную.
    Object.defineProperty($log, 'scrollHeight', {
        configurable: true,
        writable: true,
        value: 100 + $log.childNodes.length * ROW_PX,
    });
}

// ---------------------------------------------------------------------------
// 5. Вложения, карточки и клавиатура внутри строки
// ---------------------------------------------------------------------------

{
    chat.appendMessage(msg(77, {
        media: [{ index: 0, file_id: 95, kind: 'file', name: 'forge_probe.txt', size: 500, has_full: true, has_thumb: false, waveform: [] }],
        cards: [{
            id: '1', color: '#8d51eb', description: '',
            blocks: [{ kind: 'grid', text: null, segments: [], links: [], rows: [{ name: 'Поле', value: 'Значение', display: 'line', color: null }] }],
        }],
    }));
    const row = lines().find((n) => n.dataset.msgId === '77');
    const classes = walk(row).map((n) => String(n.className || ''));
    ok(classes.some((c) => c.includes('b24-media-card')), 'карточка вложения внутри строки');
    ok(classes.some((c) => c.includes('b24-card-grid')), 'карточка ATTACH внутри строки');
    ok(row.textContent.includes('forge_probe.txt'), 'имя файла видно');
    ok(row.textContent.includes('Значение'), 'значение поля GRID видно');
}

// ---------------------------------------------------------------------------
// 6. Клавиатура: PUT и SEND работают, CALL выключен С ОБЪЯСНЕНИЕМ
//
// SEND ожил в Ф6: `initChat` передаёт `onSendText`, и кнопка отправляет текст
// тем же путём, что Enter. CALL и DIALOG остаются выключенными навсегда — они
// требуют scope `imbot`, которого у ВХОДЯЩЕГО вебхука нет и быть не может.
// ---------------------------------------------------------------------------

{
    const xss = '<img src=x onerror=alert(1)>';
    chat.appendMessage(msg(79, {
        reply_markup: {
            kind: 'inline',
            rows: [[
                { text: 'Кнопка', action: 'put', value: 'hello', enabled: true, disabled_reason: null, bg_color: '#29619b', text_color: '#fff' },
                { text: 'Отправить', action: 'send', value: 'да', enabled: true, disabled_reason: null, bg_color: null, text_color: null },
                { text: xss, action: 'call', value: '+70000000000', enabled: false, disabled_reason: 'Кнопка требует прав чат-бота (scope imbot), которых у входящего вебхука нет', bg_color: null, text_color: null },
            ]],
        },
    }));

    const row = lines().find((n) => n.dataset.msgId === '79');
    const buttons = walk(row).filter((n) => String(n.className || '').includes('b24-kb-btn'));
    eq(buttons.length, 3, 'нарисованы все три кнопки');

    const [put, send, call] = buttons;
    eq(put.disabled, false, 'PUT нажимается');
    eq(put.textContent, 'Кнопка', 'подпись кнопки — ровно текст бота');

    ok(send.disabled === false, 'SEND нажимается: отправка появилась в Ф6');
    eq(send.title, 'Отправить сообщением', 'у SEND честный title');

    ok(call.disabled === true, 'CALL выключен: нет scope imbot');
    ok(String(call.title).includes('imbot'), 'причина отказа CALL названа');
    eq(call.textContent, xss, 'XSS-подпись попала в документ ТЕКСТОМ');
    ok(!walk(row).some((n) => n.tagName === 'img'), 'элемент img не создан');

    // Нажатие PUT подставляет текст в поле ввода и НЕ ходит в сеть.
    const netBefore = netCalls.length;
    put.listeners.get('click').forEach((fn) => fn({ type: 'click' }));
    eq($input.value, 'hello', 'PUT подставил ACTION_VALUE в поле ввода');
    eq(netCalls.length, netBefore, 'нажатие кнопки не стоит ни одного запроса');

    // Выключенная кнопка ничего не делает даже при прямом вызове.
    eq(botkb.pressButton({ text: 'x', action: 'call', value: '1', enabled: false }, null), false,
        'выключенная кнопка не выполняется');
    eq(botkb._debugBotKbState(), { canPut: true, canSend: true }, 'снимок botkb честен');

    // …а без обработчика отправки SEND честно выключается обратно: ровно так
    // кнопка выглядела до Ф6, и ровно так она обязана выглядеть, если лента
    // инициализирована без отправки.
    botkb.initBotKeyboards({ onPutText: () => {}, onSendText: null });
    eq(botkb.buttonState({ text: 'x', action: 'send', value: 'да', enabled: true }),
        { enabled: false, reason: botkb.SEND_UNAVAILABLE }, 'без обработчика SEND выключен');
    eq(botkb.pressButton({ text: 'x', action: 'send', value: 'да', enabled: true }, null), false,
        'SEND без обработчика не выполняется');
    // Возвращаем обработчики ленты: следующие блоки теста рассчитывают на них.
    chat.initChat({});
}

// ---------------------------------------------------------------------------
// 7. Смена диалога полностью очищает ленту
// ---------------------------------------------------------------------------

{
    setNet(async () => page([msg(11, { dialog_id: '19', text: 'личное' })]));
    await chat.openDialog('19', { id: '19', title: 'Алексей Пруцков', kind: 'user', can_write: true });
    await flush(8);

    const ids = lines().map((n) => n.dataset.msgId);
    eq(ids, ['11'], 'в ленте только строки нового диалога');
    eq(chat._debugChatState().dialogId, '19', 'открытый диалог сменился');
    ok(String($sessionHead.textContent).includes('Алексей Пруцков'), 'шапка обновилась');
    ok(chat._debugChatState().stickBottom, 'новый диалог открывается на конце истории');

    // История прошлого диалога не потерялась — она в кэше состояния.
    ok(state.b24State.messages.get('chat27').length >= 8, 'кэш прошлого диалога цел');

    // Возврат показывает её мгновенно, ещё до ответа сети.
    let resolveLater;
    setNet(() => new Promise((r) => { resolveLater = r; }));
    chat.openDialog('chat27', { id: 'chat27', title: 'FORGE probe chat' });
    ok(lines().length >= 8, 'возврат в диалог рисует кэш немедленно');
    resolveLater(page([]));
    await flush(8);
}

// ---------------------------------------------------------------------------
// 8. Ошибка сети видна В ЛЕНТЕ, а не в пустом экране
// ---------------------------------------------------------------------------

{
    state.b24State.messages = new Map();
    setNet(async () => jsonRes(409, { error: { code: 'not_authorized', message: 'нет вебхука' } }));
    await chat.openDialog('chat9', { id: 'chat9', title: 'Пустой' });
    await flush(8);

    ok($log.textContent.includes('Портал не подключён'), 'причина отказа написана в ленте');
    eq(lines().length, 1, 'вместо истории — одна служебная строка');

    // Пустая история — тоже сообщение, а не молчание.
    setNet(async () => page([]));
    await chat.openDialog('chat10', { id: 'chat10', title: 'Пустой' });
    await flush(8);
    ok($log.textContent.includes('история пуста'), 'пустой диалог объясняет свою пустоту');
}

// ---------------------------------------------------------------------------
// 9. Клавиатура: LINK открывает ссылку, COMMAND ходит в сеть и БЛОКИРУЕТСЯ
//    НАВСЕГДА успехом (forge-zw3d).
//
// До форge-zw3d обе кнопки приезжали ВЫКЛЮЧЕННЫМИ с ложным «незнакомое
// действие»: разбирался только `ACTION`, а РЕАЛЬНЫЕ боты портала («Команда
// заботы Битрикс24») шлют кнопки полями `LINK` и `COMMAND`/`COMMAND_PARAMS`.
// ---------------------------------------------------------------------------

{
    setNet(async () => page([]));
    await chat.openDialog('chat81', { id: 'chat81', title: 'Клавиатура', kind: 'chat', can_write: true });
    await flush(8);

    const openedUrls = [];
    const prevOpen = globalThis.open;
    globalThis.open = (url, target, features) => {
        openedUrls.push({ url, target, features });
        return { opener: null };
    };

    chat.appendMessage(msg(81, {
        dialog_id: 'chat81',
        reply_markup: {
            kind: 'inline',
            rows: [[
                {
                    text: 'Получить консультацию', action: 'link',
                    value: 'https://www.bitrix24.ru/register/cbot.php',
                    enabled: true, disabled_reason: null, bg_color: null, text_color: null,
                    command: null, command_params: null, bot_id: null, lock_after_press: false,
                },
                {
                    text: '5', action: 'command', value: null,
                    enabled: true, disabled_reason: null, bg_color: null, text_color: null,
                    command: 'careButton', command_params: '5', bot_id: 17, lock_after_press: true,
                },
            ]],
        },
    }));

    const row = lines().find((n) => n.dataset.msgId === '81');
    const buttons = walk(row).filter((n) => String(n.className || '').includes('b24-kb-btn'));
    eq(buttons.length, 2, 'обе живые кнопки нарисованы');
    const [link, command] = buttons;
    eq(link.disabled, false, 'LINK нажимается (была бы disabled=true до forge-zw3d)');
    eq(link.title, 'Открыть ссылку', 'у LINK честный title');
    eq(command.disabled, false, 'COMMAND нажимается (была бы disabled=true до forge-zw3d)');
    eq(command.title, 'Выполнить команду бота', 'у COMMAND честный title');

    // LINK — клиентски, сети не касается.
    const netBeforeLink = netCalls.length;
    link.listeners.get('click').forEach((fn) => fn({ type: 'click' }));
    eq(netCalls.length, netBeforeLink, 'LINK не ходит в сеть');
    eq(openedUrls.length, 1, 'ссылка открыта ровно один раз');
    eq(openedUrls[0].url, 'https://www.bitrix24.ru/register/cbot.php', 'открыт ровно тот URL, что прислал бот');
    ok(String(openedUrls[0].features || '').includes('noopener'), 'noopener выставлен — открытая страница не получает window.opener');

    // COMMAND — сеть, и УСПЕХ с BLOCK: Y блокирует кнопку НАВСЕГДА.
    setNet(async (url) => {
        ok(url.includes('/api/bitrix/messages/81/command'), 'адрес команды собран по контракту');
        return jsonRes(200, { ok: true, sent: true });
    });
    const netBeforeCmd = netCalls.length;
    command.listeners.get('click').forEach((fn) => fn({ type: 'click' }));
    eq(command.disabled, true, 'кнопка блокируется СИНХРОННО на время запроса — повтор дал бы второе исполнение');
    ok(command.className.includes('b24-kb-btn-pending'), 'видно, что запрос летит');
    await flush(8);
    eq(netCalls.length, netBeforeCmd + 1, 'ровно один запрос ушёл');
    eq(netCalls[netCalls.length - 1].body, { bot_id: 17, command: 'careButton', command_params: '5' },
        'тело запроса — ровно те поля, что фронт получил в ButtonDto');
    eq(command.disabled, true, 'BLOCK: Y — кнопка осталась выключенной НАВСЕГДА');
    ok(command.className.includes('b24-kb-btn-off'), 'постоянная блокировка — отдельный класс от временного pending');
    ok(!command.className.includes('b24-kb-btn-pending'), 'pending снят после ответа');

    globalThis.open = prevOpen;
}

// ---------------------------------------------------------------------------
// 10. COMMAND без BLOCK: ошибка портала возвращает кнопку в рабочее
//     состояние (а не блокирует навсегда) — решение принимает следующий клик
//     пользователя, а не сервер за него.
// ---------------------------------------------------------------------------

{
    chat.appendMessage(msg(83, {
        dialog_id: 'chat81',
        reply_markup: {
            kind: 'inline',
            rows: [[{
                text: 'Голос', action: 'command', value: null,
                enabled: true, disabled_reason: null, bg_color: null, text_color: null,
                command: 'vote', command_params: 'no', bot_id: 17, lock_after_press: false,
            }]],
        },
    }));
    const row = lines().find((n) => n.dataset.msgId === '83');
    const button = walk(row).find((n) => String(n.className || '').includes('b24-kb-btn'));

    setNet(async () => jsonRes(409, { error: { code: 'invalid_state', message: 'бот не ответил' } }));
    button.listeners.get('click').forEach((fn) => fn({ type: 'click' }));
    eq(button.disabled, true, 'на время запроса выключена, как и в успешном случае');
    await flush(8);
    eq(button.disabled, false, 'ошибка вернула кнопку в рабочее состояние — без BLOCK блокировки навсегда нет');
    ok(!button.className.includes('b24-kb-btn-off'), 'ошибка — это не постоянная блокировка');
    ok(!button.className.includes('b24-kb-btn-pending'), 'pending снят');
}

// ---------------------------------------------------------------------------
// 11. Двойной клик по строке — начать ответ (forge-zw3d, по образцу
//     Telegram-плагина: `classifyLineDblClick` + `chat.js::onLogDblClick`).
// ---------------------------------------------------------------------------

{
    setNet(async () => page([]));
    await chat.openDialog('chat85', { id: 'chat85', title: 'Ответы', kind: 'chat', can_write: true });
    await flush(8);

    chat.appendMessage(msg(85, { dialog_id: 'chat85', sender: { id: '15', name: 'Максим Крылов', color: null, bot: false } }));
    chat.appendMessage({
        id: 86, dialog_id: 'chat85', out: false, date: '2026-08-06T10:00:00+03:00',
        sender: null, system: true, action: 'CHAT_JOIN', text: 'Игорь создал чат',
        segments: [{ kind: 'text', text: 'Игорь создал чат', value: null, children: [] }],
        media: [], cards: [], reply_markup: null, edited: false, likes: [], unread: false, reply_to: null, fwd: null,
    });

    ok($log.listeners.has('dblclick'), 'слушатель dblclick навешен на ленту');
    eq($log.listenerCount('dblclick'), 1, 'повторный initChat не удваивает слушатель');

    const row85 = lines().find((n) => n.dataset.msgId === '85');
    const text85 = walk(row85).find((n) => n.className === 'b24-line-text');
    $log.listeners.get('dblclick').forEach((fn) => fn({ type: 'dblclick', target: text85 }));
    eq(chat.replyMode(), { id: 85, author: 'Максим Крылов', text: 'строка 85' }, 'двойной клик по тексту строки начал ответ');

    chat.cancelMode();
    eq(chat.replyMode(), null, 'Esc/cancelMode снимает цель — двойной клик не оставляет вкладку в чужом режиме');

    // Системная строка — не ответ: у неё нет ни автора, ни кнопки «↩».
    const row86 = lines().find((n) => n.dataset.msgId === '86');
    const text86 = walk(row86).find((n) => n.className === 'b24-line-text');
    $log.listeners.get('dblclick').forEach((fn) => fn({ type: 'dblclick', target: text86 }));
    eq(chat.replyMode(), null, 'двойной клик по системной строке не начинает ответ');

    // Двойной клик по кнопке действия строки (например «↩» или «✎») — не
    // новый вызов startReply поверх её собственного обработчика.
    const actionBtn = walk(row85).find((n) => n.tagName === 'button' && n.dataset.action === 'like');
    ok(!!actionBtn, 'у обычной строки есть кнопка действия');
    $log.listeners.get('dblclick').forEach((fn) => fn({ type: 'dblclick', target: actionBtn }));
    eq(chat.replyMode(), null, 'двойной клик по кнопке строки не запускает ответ поверх неё');

    // Мимо строк (распорка, разделитель дня) — тоже не ответ: чистая функция
    // проверяется без диспетчеризации события.
    const spacer = $log.childNodes.find((n) => n.className === 'b24-log-spacer');
    eq(chat.classifyLineDblClick(spacer), 'ignore', 'распорка — не строка');
    eq(chat.classifyLineDblClick(null), 'ignore', 'пустая цель — не ответ');
    eq(chat.classifyLineDblClick({}), 'ignore', 'цель без closest — не ответ');
}

// ---------------------------------------------------------------------------
// 12. Сброс ленты
// ---------------------------------------------------------------------------

{
    chat.resetChat();
    eq($log.childNodes.length, 0, 'сброс очистил ленту целиком');
    eq(chat._debugChatState().dialogId, null, 'открытого диалога больше нет');
    eq(String($sessionHead.textContent), 'SESSION ▸ —', 'шапка вернулась в исходное');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_chat)`);
if (failures > 0) process.exit(1);
