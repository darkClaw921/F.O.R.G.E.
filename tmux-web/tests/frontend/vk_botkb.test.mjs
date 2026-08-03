// Регресс-тест клавиатур сообществ панели VK (js/vk/botkb.js + стыковка в
// js/vk/chat.js).
//
// Стаб DOM импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ПРОВЕРЯЕТСЯ, — ЧЕСТНАЯ ДЕГРАДАЦИЯ. ВКонтакте не даёт
// нажать под пользовательским токеном ничего, кроме `text` и `open_link`:
// `messages.sendMessageEventAnswer` это сторона СООБЩЕСТВА, а клиентской
// половины `message_event` в публичном API нет вовсе. Значит `callback`,
// `vkpay`, `open_app` и `location` обязаны быть ВИДНЫ, но отключены и
// объяснены — «пропавшая» кнопка читалась бы как поломка бота.
//
// Разделы:
//   1. чистая логика — cleanButtonText, normalizeButton/Markup, buttonAction;
//   2. таблица видов кнопок и белый список схем URL;
//   3. инлайн-клавиатура в строке ленты: что нажимается, что отключено;
//   4. нажатие `text` — POST .../button с КООРДИНАТАМИ и «следование» ДО
//      запроса;
//   5. `open_link` — окно открывается фронтом, сеть не трогается;
//   6. нижняя клавиатура полосой над #vk-prompt: показ, one_time, смена
//      беседы, пересчёт по истории.
//
// МУТАЦИОННАЯ ПРОВЕРКА: пометить `callback` как supported ⇒ краснеет раздел 3
// (кнопка перестаёт быть disabled); слать `payload` вместо координат ⇒
// краснеет раздел 4; снять белый список схем в safeUrl ⇒ краснеет раздел 2.
//
// Запуск: node tmux-web/tests/frontend/vk_botkb.test.mjs

import {
    $log, $prompt, dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

const B = await import(new URL('../../static/js/vk/botkb.js', import.meta.url).href);
const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const RL = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit): здесь он только
// заставлял бы каждый запрос ждать настоящие 380 мс.
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

/** Невидимые символы тестовых данных — только через коды: литеральные байты
 * сделали бы файл «бинарным» для grep и сломали бы любые скриптовые правки. */
const BIDI = String.fromCharCode(0x202E);
const NUL = String.fromCharCode(0);

/** Кнопка DTO: вид, подпись, координаты, поддержка. */
function btn(over) {
    return Object.assign({
        row: 0, col: 0, text: 'ok', kind: 'text', color: null, url: null, supported: true,
    }, over || {});
}

/** Разметка DTO. */
function markup(rows, over) {
    return Object.assign({ kind: 'inline', rows, single_use: false }, over || {});
}

const DIALOG_ID = '-70';
const DIALOG = { id: DIALOG_ID, kind: 'group', title: 'Магазин', bot: true };

function message(id, extra) {
    return Object.assign({
        id,
        dialog_id: DIALOG_ID,
        out: false,
        date: 1_700_000_000 + id,
        sender: { id: '-70', first_name: 'Магазин' },
        text: `сообщение ${id}`,
        media: null,
        attachments: [],
        reply_to_id: null,
        reply_to: null,
        edit_date: null,
        reply_markup: null,
        cmid: id - 7000,
        fwd: [],
        action: null,
        payload: null,
    }, extra || {});
}

// ---------------------------------------------------------------------------
// 1. Чистая логика: подписи, координаты, нормализация матрицы
// ---------------------------------------------------------------------------

eq(B.cleanButtonText('Показать заказы', 48), 'Показать заказы', 'подпись как есть');
eq(B.cleanButtonText('  a   b  ', 48), 'a b', 'пробелы схлопнуты');
eq(B.cleanButtonText('a\nb\tc', 48), 'a b c', 'переводы строк не ломают ряд');
eq(B.cleanButtonText('', 48), '', 'пустая подпись — кнопки нет');
eq(B.cleanButtonText(null, 48), '', 'не строка — подписи нет');
eq(B.cleanButtonText(BIDI + 'удалить', 48), 'удалить', 'bidi-override вычищен');
eq(B.cleanButtonText('абвгд', 3), 'аб…', 'обрезка по символам с многоточием');
eq(Array.from(B.cleanButtonText('😀😀😀😀', 3)).length, 3, 'обрезка не режет эмодзи пополам');

{
    const norm = B.normalizeButton(btn({ row: 2, col: 3, text: ' Да ' }));
    eq(norm.text, 'Да', 'подпись вычищена');
    eq([norm.row, norm.col], [2, 3], 'координаты пришли из DTO');
    eq(B.normalizeButton(btn({ text: '   ' })), null, 'кнопка без подписи не рисуется');
    eq(B.normalizeButton(btn({ kind: 'intent_subscribe' })).kind, 'other',
        'незнакомый вид сводится к other');
    eq(B.normalizeButton(btn({ row: -1 })).row, null, 'отрицательная координата — не координата');
    eq(B.normalizeButton(btn({ col: 99 })).col, null, 'координата за потолком отбрасывается');
    eq(B.normalizeButton(btn({ row: '4' })).row, 4, 'строковая координата разбирается');
    eq(B.normalizeButton(null), null, 'мусор вместо кнопки');
}

{
    // Матрица: ряды и кнопки в ряду, каждая со своими координатами.
    const m = B.normalizeMarkup(markup([
        [btn({ row: 0, col: 0, text: 'A' }), btn({ row: 0, col: 1, text: 'B' })],
        [btn({ row: 1, col: 0, text: 'C' })],
    ]));
    eq(m.rows.length, 2, 'два ряда');
    eq(m.rows[0].map((b) => b.text), ['A', 'B'], 'первый ряд');
    eq(m.rows[1][0].row, 1, 'координата второго ряда');
    eq(m.kind, 'inline', 'вид разметки');
    eq(m.singleUse, false, 'one_time по умолчанию выключен');
    eq(B.normalizeMarkup(markup([], { single_use: true })), null,
        'разметка без кнопок — это отсутствие разметки');
    eq(B.normalizeMarkup(markup([[btn()]], { kind: 'hide' })), null,
        'вида hide у VK не существует — разметка отбрасывается');
    eq(B.normalizeMarkup(null), null, 'пустая разметка');
    eq(B.normalizeMarkup(markup([[btn({ text: '' })]])), null,
        'ряд из одних безымянных кнопок не рисуется');
    eq(B.normalizeMarkup(markup([[btn()]], { single_use: true })).singleUse, true,
        'one_time доезжает');
}

{
    // Потолки: клавиатура 40×20 обрезается по обеим осям — ровно как на
    // сервере (dto.rs::MAX_KB_ROWS / MAX_KB_ROW_BUTTONS).
    const rows = [];
    for (let r = 0; r < 40; r += 1) {
        const row = [];
        for (let c = 0; c < 20; c += 1) row.push(btn({ row: r, col: c, text: `${r}:${c}` }));
        rows.push(row);
    }
    const m = B.normalizeMarkup(markup(rows));
    eq(m.rows.length, B.MAX_ROWS, 'рядов не больше потолка');
    ok(m.rows.every((r) => r.length === B.MAX_ROW_BUTTONS), 'кнопок в ряду не больше потолка');
    eq(m.rows[0][0].text, '0:0', 'обрезка берёт ПРЕФИКС, а не хвост');
}

// ---------------------------------------------------------------------------
// 2. Таблица видов и белый список схем
// ---------------------------------------------------------------------------

{
    const a = B.buttonAction(btn({ kind: 'text', row: 1, col: 2, text: 'Заказы' }));
    eq(a.action, 'press', 'text нажимается');
    eq([a.row, a.col], [1, 2], 'координаты уезжают в действие');
    eq(a.text, 'Заказы', 'подпись уезжает в действие');
}

{
    const a = B.buttonAction(btn({ kind: 'open_link', url: 'https://example.com/shop' }));
    eq(a.action, 'open-url', 'open_link открывается');
    eq(a.url, 'https://example.com/shop', 'адрес прошёл белый список');
}

// Неподдерживаемые виды: сервер помечает их supported:false.
for (const kind of ['callback', 'vkpay', 'open_app', 'location', 'other']) {
    const a = B.buttonAction(btn({ kind, supported: false }));
    eq(a.action, 'unsupported', `${kind}: нажать нельзя`);
    ok(a.reason && a.reason.length > 10, `${kind}: причина объяснена словами`);
}
ok(
    B.buttonAction(btn({ kind: 'callback', supported: false })).reason.includes('message_event'),
    'причина у callback называет механику, которой нет у клиента',
);

// Даже если сервер СОЛГАЛ про поддержку, фронт не нажимает вид, который не
// умеет исполнять: «нажатие в никуда» хуже честного пояснения.
for (const kind of ['callback', 'vkpay', 'open_app', 'location', 'other', 'intent_subscribe']) {
    eq(B.buttonAction(btn({ kind, supported: true })).action, 'unsupported',
        `${kind}: supported:true не делает вид исполнимым`);
}

// Белый список схем (js/vk/format.js::safeUrl).
for (const url of [
    'javascript:alert(1)', 'JaVaScRiPt:alert(1)', 'java' + NUL + 'script:alert(1)',
    'data:text/html,<script>x</script>', 'blob:https://evil/x', 'file:///etc/passwd',
    '//evil.com/x', '/relative', 'https:evil', '', null,
]) {
    eq(B.buttonAction(btn({ kind: 'open_link', url })).action, 'unsupported',
        `недопустимая схема отбрасывается: ${url}`);
}
eq(B.buttonAction(btn({ kind: 'open_link', url: 'vk.com/shop' })).action, 'unsupported',
    'адрес без схемы отбрасывается');
eq(B.buttonAction(btn({ kind: 'open_link', url: 'vk://vk.com/shop' })).url, 'vk://vk.com/shop',
    'vk-deeplink разрешён');

// text без координат нажимать нечем — кнопка честно отключается.
eq(B.buttonAction(btn({ kind: 'text', row: null })).action, 'unsupported',
    'кнопка без координат не нажимается');
eq(B.buttonAction(null).action, 'unsupported', 'мусор вместо кнопки');

// ---------------------------------------------------------------------------
// 3. Инлайн-клавиатура в строке ленты
// ---------------------------------------------------------------------------

S.vkState.dialogs = [DIALOG];
S.vkState.selfUser = { id: '999', first_name: 'Я' };
C.initChat({ onRead: () => {}, presenceOf: () => null });

const KB_MESSAGE = message(7100, {
    text: 'Выберите действие',
    reply_markup: markup([
        [
            btn({ row: 0, col: 0, kind: 'text', text: 'Показать заказы', color: 'primary' }),
            btn({ row: 0, col: 1, kind: 'open_link', text: 'Сайт', url: 'https://example.com/shop' }),
        ],
        [btn({ row: 1, col: 0, kind: 'callback', text: 'Отменить', supported: false })],
    ]),
});

const history = (msgs) => async (url, init) => {
    if (init && init.method === 'POST') {
        return jsonRes(200, {
            message: message(7101, { out: true, text: 'Показать заказы', cmid: 101 }),
        });
    }
    if (url.includes('/messages')) return jsonRes(200, { messages: msgs, has_more: false });
    return jsonRes(200, {});
};

setNet(history([KB_MESSAGE]));
await C.openDialog(DIALOG_ID, DIALOG);
await settle();

const kbNode = () => $log.querySelector('.vk-kb');
const kbButtons = () => (kbNode() ? kbNode().querySelectorAll('.vk-kb-btn') : []);

{
    ok(kbNode(), 'клавиатура нарисована под строкой сообщения');
    const buttons = kbButtons();
    eq(buttons.length, 3, 'три кнопки: text, open_link, callback');
    eq(buttons.map((b) => b.textContent), ['Показать заказы', 'Сайт', 'Отменить'],
        'подписи — только textContent');
    eq(buttons.map((b) => b.dataset.kbKind), ['text', 'open_link', 'callback'], 'виды в разметке');
    eq(buttons[0].dataset.kbColor, 'primary', 'цвет VK доезжает атрибутом');

    ok(!buttons[0].disabled, 'text активна');
    ok(!buttons[1].disabled, 'open_link активна');
    ok(buttons[2].disabled, 'КЛЮЧЕВОЕ: callback отключена');
    ok(buttons[2].className.includes('vk-kb-btn-unsupported'), 'и помечена классом');
    ok(buttons[2].title.includes('ВКонтакте не позволяет'),
        `и объясняет причину тултипом: ${buttons[2].title}`);
    eq(buttons[2].dataset.kbAction, 'unsupported', 'действие объявлено в разметке');

    // Клик по отключённой кнопке невозможен: обработчик к ней не привязан
    // вовсе — «нажатие», которое ничего не делает, выглядело бы поломкой.
    const before = netCalls.length;
    dispatch(buttons[2], 'click');
    eq(netCalls.length, before, 'клик по отключённой кнопке не порождает запросов');
}

// ---------------------------------------------------------------------------
// 4. Нажатие text: координаты, отсутствие payload, «следование» ДО запроса
// ---------------------------------------------------------------------------

{
    netCalls.length = 0;
    const followBefore = C._debugChatState().following.messageId;
    eq(followBefore, null, 'до нажатия ленте некуда следовать');

    kbButtons()[0].click();
    // Проверка ДО await: нажатие обязано отметиться раньше, чем уйдёт запрос —
    // ответ сообщества приходит отдельным кадром LongPoll и вполне может
    // опередить ответ роута.
    eq(C._debugChatState().following.messageId, KB_MESSAGE.cmid,
        '«следование» включено ДО запроса и адресовано cmid');
    eq(C._debugChatState().following.dialogId, DIALOG_ID, 'и той самой беседе');

    await settle(20);
    const press = netCalls.find((c) => c.url.includes('/button'));
    ok(press, `запрос нажатия ушёл: ${netCalls.map((c) => c.url).join(', ')}`);
    eq(press.method, 'POST', 'нажатие — POST');
    eq(press.url, `/api/vk/dialogs/-70/messages/7100/button`, 'адресуется сообщением с клавиатурой');
    eq(press.body, { row: 0, col: 0 }, 'в теле ТОЛЬКО координаты — payload подставит сервер');
    ok(!('payload' in press.body), 'payload с фронта не уходит никогда');
    ok(!netCalls.some((c) => c.url.endsWith('/messages') && c.method === 'POST'),
        'обычная отправка при этом не дублируется');

    // Ответ роута — обычный MessageDto: строка встаёт в ленту, как у отправки.
    const own = $log.children.filter((n) => n.dataset && n.dataset.msgId === '7101');
    eq(own.length, 1, 'отправленное нажатием сообщение нарисовано в ленте');
}

// ---------------------------------------------------------------------------
// 5. open_link: окно открывает фронт, сеть не трогается
// ---------------------------------------------------------------------------

{
    const opened = [];
    globalThis.window.open = (url, target, features) => {
        opened.push({ url, target, features });
        return null;
    };
    netCalls.length = 0;
    kbButtons()[1].click();
    await settle();
    eq(opened.length, 1, 'ссылка открыта браузером');
    eq(opened[0].url, 'https://example.com/shop', 'ровно тот адрес, что в кнопке');
    ok(String(opened[0].features).includes('noopener'), 'без noopener открывать нельзя');
    eq(netCalls.length, 0, 'бэкенд в открытии ссылки не участвует');
}

// ---------------------------------------------------------------------------
// 6. Нижняя клавиатура: полоса над полем ввода
// ---------------------------------------------------------------------------

const bar = () => $prompt.parentNode.children.find((n) => n.className.includes('vk-kb-bar')) || null;
const barButtons = () => (bar() ? bar().querySelectorAll('.vk-kb-btn') : []);

{
    ok(bar() === null || bar().hidden, 'у беседы с inline-клавиатурой полосы нет');

    const lower = message(7200, {
        text: 'Меню',
        reply_markup: markup(
            [[
                btn({ row: 0, col: 0, kind: 'text', text: 'Каталог' }),
                btn({ row: 0, col: 1, kind: 'location', text: 'Где я', supported: false }),
            ]],
            { kind: 'keyboard', single_use: true },
        ),
    });

    setNet(history([KB_MESSAGE, lower]));
    await C.openDialog(DIALOG_ID, DIALOG);
    await settle();

    const b = bar();
    ok(b && !b.hidden, 'полоса нижней клавиатуры показана');
    ok($prompt.parentNode.children.indexOf(b) < $prompt.parentNode.children.indexOf($prompt),
        'полоса стоит НАД формой ввода');
    eq(b.querySelector('.vk-kb-bar-title').textContent, '// KEYBOARD · ONE-TIME',
        'заголовок называет one_time');
    eq(B._debugBotKbState().kind, 'keyboard', 'состояние беседы — нижняя клавиатура');
    eq(B._debugBotKbState().messageId, 7200, 'клавиатура взята из САМОГО НИЖНЕГО сообщения');

    const buttons = barButtons().filter((n) => !n.className.includes('vk-kb-bar-close'));
    eq(buttons.length, 2, 'обе кнопки нарисованы');
    ok(!buttons[0].disabled, 'text активна');
    ok(buttons[1].disabled, 'location отключена');
    ok(buttons[1].title.includes('официальном приложении'), 'и объясняет причину');

    // one_time: клавиатура сворачивается ПОСЛЕ отправки.
    netCalls.length = 0;
    buttons[0].click();
    await settle(20);
    const press = netCalls.find((c) => c.url.includes('/button'));
    ok(press, 'нажатие из полосы уходит тем же роутом');
    eq(press.url, '/api/vk/dialogs/-70/messages/7200/button', 'адресуется сообщением клавиатуры');
    eq(press.body, { row: 0, col: 0 }, 'координаты кнопки полосы');
    ok(B._debugBotKbState().collapsed, 'one_time свернул клавиатуру после нажатия');
    const show = barButtons().find((n) => n.className.includes('vk-kb-bar-show'));
    ok(show, 'свёрнутая полоса оставляет способ вернуть клавиатуру');
    show.click();
    ok(!B._debugBotKbState().collapsed, 'клавиатура возвращается');
}

{
    // Клик по неподдерживаемой кнопке полосы не сворачивает соседние рабочие.
    const buttons = barButtons().filter((n) => !n.className.includes('vk-kb-bar-close'));
    netCalls.length = 0;
    dispatch(buttons[1], 'click');
    await settle();
    eq(netCalls.length, 0, 'отключённая кнопка полосы молчит');
    ok(!B._debugBotKbState().collapsed, 'и не прячет клавиатуру');
}

{
    // Смена беседы: полоса уходит с экрана, состояние беседы сохраняется.
    setNet(history([message(7300)]));
    await C.openDialog('1234', { id: '1234', kind: 'user', title: 'Пётр' });
    await settle();
    ok(!bar() || bar().hidden, 'в другой беседе чужой клавиатуры нет');
    eq(B._debugBotKbState().kind, null, 'у новой беседы клавиатуры нет');

    setNet(history([message(7200, {
        reply_markup: markup([[btn({ row: 0, col: 0, text: 'Каталог' })]], { kind: 'keyboard' }),
    })]));
    await C.openDialog(DIALOG_ID, DIALOG);
    await settle();
    ok(bar() && !bar().hidden, 'возврат в беседу восстановил клавиатуру из истории');
    eq(bar().querySelector('.vk-kb-bar-title').textContent, '// KEYBOARD',
        'заголовок без one_time');
}

{
    // Пересчёт по истории: клавиатура берётся из НИЖНЕГО сообщения, свои
    // сообщения не в счёт, inline не поднимается в полосу.
    const withKb = (id, kind) => message(id, {
        reply_markup: markup([[btn({ row: 0, col: 0, text: `меню ${id}` })]], { kind }),
    });
    eq(B.latestReplyMarkup([withKb(1, 'keyboard'), withKb(2, 'keyboard')]).messageId, 2,
        'берётся самое нижнее сообщение');
    eq(B.latestReplyMarkup([withKb(1, 'keyboard'), withKb(2, 'inline')]).messageId, 1,
        'inline-разметка состоянием беседы не является');
    eq(B.latestReplyMarkup([
        withKb(1, 'keyboard'),
        Object.assign(withKb(2, 'keyboard'), { out: true }),
    ]).messageId, 1, 'свои сообщения игнорируются');
    eq(B.latestReplyMarkup([message(1), message(2)]), null, 'сообщений с клавиатурой нет');
    eq(B.latestReplyMarkup('мусор'), null, 'не массив');
    eq(B.barTitle(null), '', 'подписи у пустой разметки нет');
}

{
    // resetChat (logout) уносит клавиатуры вместе с лентой.
    C.resetChat();
    eq(B._debugBotKbState().dialogs, 0, 'состояния бесед стёрты');
    ok(!bar() || bar().hidden, 'полоса убрана с экрана');
}

const label = 'vk_botkb';
if (failures) {
    console.error(`\n${label}: ${checks} проверок, ${failures} провалов`);
    process.exit(1);
}
console.log(`${label}: ${checks} проверок, 0 провалов`);
