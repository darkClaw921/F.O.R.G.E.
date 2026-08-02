// Регресс-тест чистой логики клавиатур ботов вкладки Telegram (дефект D2).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/botkb.js через
// `await import()`. Модуль тянет core/dom.js (там на верхнем уровне
// document.getElementById для каждого ref'а) и echo/notifications.js, поэтому
// перед импортом ставится минимальный стаб `document` — тот же приём, что в
// telegram_media.test.mjs. Ни один ассерт ниже DOM не трогает: проверяются
// только чистые функции (нормализация разметки, классификация кнопки, разбор
// ответа бота, белый список схем URL, выбор актуальной reply-клавиатуры).
//
// Запуск: node tmux-web/tests/frontend/telegram_botkb.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

globalThis.document = {
    getElementById: () => null,
    addEventListener: () => {},
};
globalThis.window = globalThis.window || { addEventListener: () => {} };

const modUrl = new URL('../../static/js/telegram/botkb.js', import.meta.url).href;
const B = await import(modUrl);

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

/** Кнопка-заготовка: чтобы в ассертах был виден только значимый параметр. */
function btn(over) {
    return Object.assign({
        text: 'ok', kind: 'callback', data: null, url: null, query: null,
        requires_password: false,
    }, over || {});
}

/** Разметка-заготовка. */
function markup(over) {
    return Object.assign({
        kind: 'inline', rows: [], resize: false, single_use: false,
        persistent: false, placeholder: null,
    }, over || {});
}

// ---------- safeUrl: белый список схем (XSS) ----------
eq(B.safeUrl('https://example.com/x?a=1'), 'https://example.com/x?a=1', 'url: https проходит');
eq(B.safeUrl('http://example.com'), 'http://example.com', 'url: http проходит');
eq(B.safeUrl('tg://user?id=777000'), 'tg://user?id=777000', 'url: tg-deeplink проходит');
eq(B.safeUrl('javascript:alert(1)'), null, 'url: javascript: отвергается');
eq(B.safeUrl('JaVaScRiPt:alert(1)'), null, 'url: регистр схемы не спасает');
eq(B.safeUrl('  javascript:alert(1)  '), null, 'url: пробелы вокруг не спасают');
eq(B.safeUrl('java\u0000script:alert(1)'), null, 'url: NUL внутри схемы не спасает');
eq(B.safeUrl('java\tscript:alert(1)'), null, 'url: таб внутри схемы не спасает');
eq(B.safeUrl('java\nscript:alert(1)'), null, 'url: перевод строки внутри схемы не спасает');
eq(B.safeUrl('jav\u0009asc\u000Aript:alert(1)'), null, 'url: смесь управляющих символов');
eq(B.safeUrl('\u202Ejavascript:alert(1)'), null, 'url: bidi-override перед схемой');
eq(B.safeUrl('data:text/html,<script>x</script>'), null, 'url: data: запрещён');
eq(B.safeUrl('vbscript:msgbox'), null, 'url: vbscript: запрещён');
eq(B.safeUrl('blob:https://evil/x'), null, 'url: blob: запрещён');
eq(B.safeUrl('file:///etc/passwd'), null, 'url: file: запрещён');
eq(B.safeUrl('//evil.com/x'), null, 'url: protocol-relative без схемы отвергается');
eq(B.safeUrl('/relative/path'), null, 'url: относительный путь отвергается');
eq(B.safeUrl('https:evil'), null, 'url: https без authority отвергается');
eq(B.safeUrl(''), null, 'url: пустая строка');
eq(B.safeUrl(null), null, 'url: null');
eq(B.safeUrl(42), null, 'url: не строка');
eq(B.safeUrl('HTTPS://EXAMPLE.COM/A'), 'HTTPS://EXAMPLE.COM/A', 'url: регистр хоста не портим');

// ---------- cleanButtonText: длина, emoji, управляющие символы ----------
eq(B.cleanButtonText('ok', 48), 'ok', 'text: обычная подпись');
eq(B.cleanButtonText('  a   b  ', 48), 'a b', 'text: пробелы схлопнуты и обрезаны');
eq(B.cleanButtonText('a\nb\tc', 48), 'a b c', 'text: переводы строк не ломают ряд');
eq(B.cleanButtonText('', 48), '', 'text: пустая строка');
eq(B.cleanButtonText('   ', 48), '', 'text: одни пробелы — подписи нет');
eq(B.cleanButtonText(null, 48), '', 'text: null — подписи нет');
eq(B.cleanButtonText(42, 48), '', 'text: число из битого JSON — подписи нет');
eq(B.cleanButtonText('\u0000\u0001\u001F', 48), '', 'text: одни управляющие символы — подписи нет');
ok(!B.cleanButtonText('a\u202Eb', 48).includes('\u202E'), 'text: bidi-override вырезан');
ok(!B.cleanButtonText('a\u200Bb', 48).includes('\u200B'), 'text: zero-width space вырезан');
eq(B.cleanButtonText('a'.repeat(100), 10), `${'a'.repeat(9)}…`, 'text: длинная подпись обрезана с многоточием');
eq(Array.from(B.cleanButtonText('a'.repeat(100), 10)).length, 10, 'text: длина обрезки ровно max');
eq(Array.from(B.cleanButtonText('\u{1F600}'.repeat(30), 5)).length, 5,
    'text: emoji считаются по code points, а не по code units');
ok(!/[\uD800-\uDFFF]/.test(
    B.cleanButtonText('\u{1F600}'.repeat(30), 5).replace(/[\u{10000}-\u{10FFFF}]/gu, '')
), 'text: обрезка не оставляет одиночных суррогатов');
eq(B.cleanButtonText('\u{1F469}\u200D\u{1F4BB} код', 48), '\u{1F469}\u200D\u{1F4BB} код',
    'text: ZWJ-последовательность (составное emoji) сохраняется целиком');
eq(B.cleanButtonText('日本語のボタン', 48), '日本語のボタン', 'text: не-латиница не портится');

// ---------- normalizeButton ----------
eq(B.normalizeButton(btn({ text: 'go', kind: 'callback', data: 'YWJj' })),
    { text: 'go', kind: 'callback', data: 'YWJj', url: null, query: null, requiresPassword: false },
    'button: callback c данными');
eq(B.normalizeButton(btn({ text: '  ' })), null, 'button: без подписи отбрасывается');
eq(B.normalizeButton(null), null, 'button: null');
eq(B.normalizeButton('строка'), null, 'button: не объект');
eq(B.normalizeButton(btn({ kind: 'какой-то_новый_вид' })).kind, 'other',
    'button: неизвестный kind → other (bump layer не ломает вкладку)');
eq(B.normalizeButton(btn({ kind: 'callback', requires_password: true })).requiresPassword, true,
    'button: requires_password читается из snake_case');
eq(B.normalizeButton({ text: 'x', kind: 'callback', requiresPassword: true }).requiresPassword, true,
    'button: requiresPassword читается и из camelCase');
eq(B.normalizeButton(btn({ kind: 'url', url: '' })).url, null, 'button: пустой url → null');
eq(B.normalizeButton(btn({ kind: 'switch_inline', query: '' })).query, '',
    'button: пустой query — это ЗНАЧЕНИЕ (вставить пустую строку), а не отсутствие');

// ---------- normalizeMarkup ----------
eq(B.normalizeMarkup(null), null, 'markup: null');
eq(B.normalizeMarkup(undefined), null, 'markup: undefined (сообщение без клавиатуры)');
eq(B.normalizeMarkup({}), null, 'markup: без kind');
eq(B.normalizeMarkup(markup({ kind: 'zzz', rows: [[btn()]] })), null, 'markup: неизвестный kind');
eq(B.normalizeMarkup(markup({ rows: [] })), null, 'markup: inline без рядов — рисовать нечего');
eq(B.normalizeMarkup(markup({ rows: [[]] })), null, 'markup: пустой ряд отбрасывается');
eq(B.normalizeMarkup(markup({ rows: [[btn({ text: '' }), btn({ text: '   ' })]] })), null,
    'markup: ряд из одних пустых кнопок → разметки нет');
eq(B.normalizeMarkup(markup({ rows: 'не массив' })), null, 'markup: rows не массив');
eq(B.normalizeMarkup(markup({ rows: [[btn({ text: 'a' })], 'мусор', [[]]] })).rows.length, 1,
    'markup: ряды-нессылки на массив пропускаются');
eq(B.normalizeMarkup(markup({ rows: [[btn({ text: 'a' }), btn({ text: '' }), btn({ text: 'b' })]] })).rows,
    [[
        { text: 'a', kind: 'callback', data: null, url: null, query: null, requiresPassword: false },
        { text: 'b', kind: 'callback', data: null, url: null, query: null, requiresPassword: false },
    ]],
    'markup: пустая кнопка вырезается, соседние остаются');

const hide = B.normalizeMarkup(markup({ kind: 'hide', rows: [[btn({ text: 'x' })]] }));
eq(hide.kind, 'hide', 'markup: hide распознан');
eq(hide.rows, [], 'markup: у hide ряды всегда пусты (даже если сервер прислал)');

const force = B.normalizeMarkup(markup({ kind: 'force_reply', placeholder: 'введите код' }));
eq(force.kind, 'force_reply', 'markup: force_reply распознан');
eq(force.rows, [], 'markup: у force_reply рядов нет');
eq(force.placeholder, 'введите код', 'markup: placeholder переносится');

const kb = B.normalizeMarkup(markup({
    kind: 'keyboard', rows: [[btn({ text: 'да', kind: 'text' })]],
    resize: true, single_use: true, persistent: true, placeholder: '   ',
}));
eq(kb.kind, 'keyboard', 'markup: keyboard распознан');
eq([kb.resize, kb.singleUse, kb.persistent], [true, true, true], 'markup: флаги reply-клавиатуры');
eq(kb.placeholder, null, 'markup: пробельный placeholder не считается placeholder-ом');

// Потолки размера: «клавиатура-бомба» от чужого бота не должна вешать вкладку.
const huge = B.normalizeMarkup(markup({
    rows: Array.from({ length: 500 }, () => Array.from({ length: 200 }, () => btn({ text: 'x' }))),
}));
eq(huge.rows.length, B.MAX_ROWS, 'markup: число рядов ограничено потолком');
eq(huge.rows[0].length, B.MAX_ROW_BUTTONS, 'markup: число кнопок в ряду ограничено потолком');

// ---------- buttonAction ----------
eq(B.buttonAction(btn({ kind: 'callback', data: 'YWJj' })),
    { action: 'callback', data: 'YWJj' }, 'action: callback');
eq(B.buttonAction(btn({ kind: 'callback', data: null })),
    { action: 'callback', data: null }, 'action: callback без данных');
eq(B.buttonAction(btn({ kind: 'callback', requires_password: true })).action, 'unsupported',
    'action: callback c 2FA-паролем не поддержан');
eq(B.buttonAction(btn({ kind: 'url', url: 'https://a.example/x' })),
    { action: 'open-url', url: 'https://a.example/x' }, 'action: url');
eq(B.buttonAction(btn({ kind: 'url', url: 'javascript:alert(1)' })).action, 'unsupported',
    'action: javascript-url кнопки не открывается');
eq(B.buttonAction(btn({ kind: 'url', url: null })).action, 'unsupported',
    'action: url без ссылки');
eq(B.buttonAction(btn({ kind: 'login_url', url: 'https://a.example/l' })),
    { action: 'open-url', url: 'https://a.example/l' }, 'action: login_url открывается как обычная ссылка');
eq(B.buttonAction(btn({ kind: 'user_profile', url: 'tg://user?id=1' })),
    { action: 'open-url', url: 'tg://user?id=1' }, 'action: user_profile — tg-deeplink');
eq(B.buttonAction(btn({ kind: 'switch_inline', query: 'поиск' })),
    { action: 'inline-query', query: 'поиск', samePeer: false }, 'action: switch_inline');
eq(B.buttonAction(btn({ kind: 'switch_inline_current', query: 'q' })),
    { action: 'inline-query', query: 'q', samePeer: true }, 'action: switch_inline_current');
eq(B.buttonAction(btn({ kind: 'text', text: 'да' })),
    { action: 'send-text', text: 'да' }, 'action: кнопка reply-клавиатуры отправляет свой текст');
for (const kind of ['request_phone', 'request_location', 'request_poll', 'web_view', 'buy', 'game', 'other']) {
    eq(B.buttonAction(btn({ kind })).action, 'unsupported', `action: ${kind} не поддержан в v1`);
    ok(String(B.buttonAction(btn({ kind })).reason || '').length > 0,
        `action: ${kind} объясняет причину пользователю`);
}
eq(B.buttonAction(null).action, 'unsupported', 'action: null-кнопка не роняет обработчик');
eq(B.buttonAction({ kind: 'неизвестный' }).action, 'unsupported', 'action: неизвестный kind');

// ---------- describeAnswer ----------
eq(B.describeAnswer({ message: 'готово', alert: false, url: null, cache_time: 0 }),
    { kind: 'toast', message: 'готово', url: null, cacheTime: 0 },
    'answer: message + alert=false → тост');
eq(B.describeAnswer({ message: 'ошибка!', alert: true, url: null, cache_time: null }),
    { kind: 'alert', message: 'ошибка!', url: null, cacheTime: null },
    'answer: message + alert=true → громкое уведомление');
eq(B.describeAnswer({ message: null, alert: false, url: 'https://a.example/g', cache_time: 5 }),
    { kind: 'none', message: '', url: 'https://a.example/g', cacheTime: 5 },
    'answer: без текста, но с url — только переход');
eq(B.describeAnswer({ message: null, alert: false, url: 'javascript:alert(1)', cache_time: null }).url,
    null, 'answer: javascript-url из ответа бота не открывается');
eq(B.describeAnswer({ message: '   ', alert: true, url: null, cache_time: null }).kind, 'none',
    'answer: пробельный текст — не уведомление');
eq(B.describeAnswer(null), { kind: 'none', message: '', url: null, cacheTime: null },
    'answer: пустое тело не роняет обработчик');
eq(B.describeAnswer({ message: 'a\r\nb', alert: false }).message, 'a\nb',
    'answer: CRLF нормализуется, перевод строки СОХРАНЯЕТСЯ');
eq(Array.from(B.describeAnswer({ message: 'x'.repeat(1000), alert: false }).message).length,
    B.ANSWER_TEXT_MAX, 'answer: очень длинный ответ бота обрезается');
eq(B.describeAnswer({ message: 'ok', alert: 'true' }).kind, 'toast',
    'answer: alert признаётся только строгим true (строка "true" — не alert)');

// ---------- callbackErrorText / callbackRetrySecs ----------
eq(B.callbackErrorText({ code: 'flood_wait', status: 429, retryAfterSecs: 12 }),
    'слишком часто — повтор через 12s', 'error: flood_wait с countdown');
eq(B.callbackErrorText({ code: 'flood_wait', status: 429, retryAfterSecs: null }),
    'слишком часто — подождите', 'error: flood_wait без retry_after_secs');
eq(B.callbackErrorText({ code: 'bot_timeout', status: 504 }), 'бот не ответил',
    'error: 504 — понятная формулировка из контракта');
eq(B.callbackErrorText({ code: null, status: 504 }), 'бот не ответил',
    'error: 504 распознаётся и без кода');
eq(B.callbackErrorText({ code: 'unknown_message', status: 404 }),
    'сообщение недоступно — кнопки устарели', 'error: 404 unknown_message');
eq(B.callbackErrorText({ code: 'unknown_dialog', status: 404 }), 'диалог недоступен',
    'error: 404 unknown_dialog');
eq(B.callbackErrorText({ code: 'not_authorized', status: 409 }), 'нет авторизации в Telegram',
    'error: 409 not_authorized');
eq(B.callbackErrorText({ code: 'not_connected', status: 503 }), 'нет соединения с Telegram',
    'error: 503 not_connected');
eq(B.callbackErrorText({ code: 'telegram_error', status: 502 }), 'Telegram отклонил запрос',
    'error: 502 telegram_error');
eq(B.callbackErrorText({ code: 'invalid_input', status: 400 }), 'Telegram отклонил нажатие',
    'error: 400 invalid_input');
eq(B.callbackErrorText({ message: 'сеть отвалилась' }), 'сеть отвалилась',
    'error: незнакомый код — показываем текст сервера');
eq(B.callbackErrorText(null), 'не удалось нажать кнопку', 'error: null не роняет обработчик');
eq(B.callbackRetrySecs({ retryAfterSecs: 7 }), 7, 'retry: секунды как есть');
eq(B.callbackRetrySecs({ retryAfterSecs: 0 }), 0, 'retry: ноль — ждать не нужно');
eq(B.callbackRetrySecs({ retryAfterSecs: -5 }), 0, 'retry: отрицательное значение игнорируется');
eq(B.callbackRetrySecs({ retryAfterSecs: 1.2 }), 2, 'retry: дробное округляется вверх');
eq(B.callbackRetrySecs({ retryAfterSecs: 999999 }), 3600, 'retry: сутки в подписи кнопки не тикают');
eq(B.callbackRetrySecs(null), 0, 'retry: null');

// ---------- latestReplyMarkup: клавиатура — состояние ДИАЛОГА ----------
const mKb = (over) => ({ id: 1, out: false, reply_markup: markup(Object.assign({
    kind: 'keyboard', rows: [[btn({ text: 'да', kind: 'text' })]],
}, over || {})) });

eq(B.latestReplyMarkup([]), null, 'reply: пустая история');
eq(B.latestReplyMarkup(null), null, 'reply: не массив');
eq(B.latestReplyMarkup([{ id: 1, out: false, reply_markup: null }]), null,
    'reply: сообщение без разметки');
eq(B.latestReplyMarkup([{ id: 1, out: false, reply_markup: markup({ rows: [[btn()]] }) }]), null,
    'reply: inline-клавиатура НЕ становится состоянием диалога');
eq(B.latestReplyMarkup([Object.assign(mKb(), { id: 5 })]).messageId, 5,
    'reply: keyboard из единственного сообщения');
eq(B.latestReplyMarkup([
    Object.assign(mKb(), { id: 5 }),
    { id: 6, out: false, reply_markup: markup({ kind: 'hide' }) },
]), null, 'reply: hide после keyboard убирает клавиатуру');
eq(B.latestReplyMarkup([
    { id: 5, out: false, reply_markup: markup({ kind: 'hide' }) },
    Object.assign(mKb(), { id: 6 }),
]).messageId, 6, 'reply: keyboard после hide возвращает клавиатуру');
eq(B.latestReplyMarkup([
    Object.assign(mKb(), { id: 5 }),
    { id: 6, out: false, text: 'просто текст', reply_markup: null },
]).messageId, 5, 'reply: обычное сообщение НЕ гасит клавиатуру');
eq(B.latestReplyMarkup([
    Object.assign(mKb(), { id: 5 }),
    { id: 6, out: true, reply_markup: markup({ kind: 'hide' }) },
]).messageId, 5, 'reply: своё исходящее сообщение состояние клавиатуры не меняет');
eq(B.latestReplyMarkup([
    Object.assign(mKb(), { id: 5 }),
    { id: 7, out: false, reply_markup: markup({ kind: 'force_reply', placeholder: 'код' }) },
]).markup.kind, 'force_reply', 'reply: force_reply перекрывает keyboard');
eq(B.latestReplyMarkup([
    Object.assign(mKb(), { id: 5 }),
    { id: 7, out: false, reply_markup: markup({ rows: [[btn()]] }) },
]).messageId, 5, 'reply: inline-сообщение сверху не сбивает актуальную reply-клавиатуру');
eq(B.latestReplyMarkup([{ id: 8, out: false, reply_markup: markup({ kind: 'keyboard', rows: [[]] }) }]),
    null, 'reply: keyboard без пригодных кнопок — не клавиатура');

// ---------- barTitle ----------
eq(B.barTitle(null), '', 'bar: без разметки заголовка нет');
eq(B.barTitle({ kind: 'keyboard', persistent: false }), '// KEYBOARD', 'bar: обычная клавиатура');
eq(B.barTitle({ kind: 'keyboard', persistent: true }), '// KEYBOARD · PERSISTENT',
    'bar: persistent виден в заголовке');
eq(B.barTitle({ kind: 'force_reply' }), '// REPLY REQUESTED', 'bar: force_reply');

// ===================================================================
// КРАЕВЫЕ СЛУЧАИ
//
// Разметка кнопок приходит от ЧУЖОГО бота: типы полей не гарантированы ничем,
// кроме доброй воли его автора. Ниже — ровно те входы, на которых «почти
// правильный» разбор превращается либо в дыру (обход белого списка схем), либо
// в исчезнувшую клавиатуру.
// ===================================================================

// ---------- safeUrl: http/https обязаны иметь хост ----------
eq(B.safeUrl('https://'), null, 'url: https без хоста отвергается');
eq(B.safeUrl('https://?a=1'), null, 'url: https, где сразу query — хоста нет');
eq(B.safeUrl('https://#x'), null, 'url: https, где сразу фрагмент — хоста нет');
eq(B.safeUrl('https:/\\evil.example'), null, 'url: обратный слеш вместо второго прямого');
eq(B.safeUrl('https'), null, 'url: схема без двоеточия — не URL');
eq(B.safeUrl('x-javascript:alert(1)'), null, 'url: похожая схема в белый список не входит');
eq(B.safeUrl('mailto:a@b.example'), null, 'url: mailto не открываем');
eq(B.safeUrl('\uFEFFjavascript:alert(1)'), null, 'url: BOM перед схемой не спасает');
eq(B.safeUrl('\u2066https://a.example\u2069'), 'https://a.example',
    'url: bidi-изоляты вокруг валидной ссылки вычищаются, ссылка остаётся');
eq(B.safeUrl('\thttps://ok.example'), 'https://ok.example', 'url: ведущий таб вычищается');
eq(B.safeUrl('HTTP://a.example'), 'HTTP://a.example', 'url: регистр схемы сохраняется в результате');
// tg: — deep-link в приложение, у него authority не обязательна (tg:resolve...).
eq(B.safeUrl('tg://resolve?domain=x'), 'tg://resolve?domain=x', 'url: tg с authority');
eq(B.safeUrl('tg:'), 'tg:', 'url: голый tg: проходит — требование хоста только у http/https');
// Хост НЕ проверяется по репутации — белый список закрывает схемы, и только их.
// Ассерт фиксирует границу ответственности: подмену хоста ловит адресная
// строка браузера, а не фронт (URL всё равно целиком приходит от бота).
eq(B.safeUrl('https://good.example@evil.example/x'), 'https://good.example@evil.example/x',
    'url: userinfo не отвергается — safeUrl отвечает за схему, не за хост');

// ---------- cleanButtonText: границы обрезки ----------
eq(Array.from(B.cleanButtonText('a'.repeat(100), 0)).length, B.BUTTON_TEXT_MAX,
    'text: max=0 откатывается на общий потолок, а не режет всё');
eq(Array.from(B.cleanButtonText('a'.repeat(100), -5)).length, B.BUTTON_TEXT_MAX,
    'text: отрицательный max откатывается на потолок');
eq(Array.from(B.cleanButtonText('a'.repeat(100), NaN)).length, B.BUTTON_TEXT_MAX,
    'text: NaN вместо max откатывается на потолок');
eq(B.cleanButtonText('abcde', 5), 'abcde', 'text: длина ровно по границе не трогается');
eq(B.cleanButtonText('abcdef', 5), 'abcd…', 'text: на символ длиннее — обрезка с многоточием');
// max=1 даёт два символа ('a…'): один символ + многоточие. Это сознательный
// компромисс — пустая подпись хуже, чем подпись на символ длиннее.
eq(B.cleanButtonText('abc', 1), 'a…', 'text: при max=1 остаётся хотя бы один значимый символ');
eq(B.cleanButtonText('a\u00A0b', 48), 'a b', 'text: неразрывный пробел схлопывается как обычный');
eq(B.cleanButtonText('a\u2066b\u2069c', 48), 'a b c', 'text: bidi-изоляты вырезаны');
eq(B.cleanButtonText('\u200D', 48), '\u200D',
    'text: одинокий ZWJ сохраняется (вырезать его — рассыпать составные emoji)');

// ---------- normalizeButton: типы полей из чужого JSON ----------
eq(B.normalizeButton({ text: 'x', kind: 'callback', data: 123 }).data, null,
    'button: data не строкой — как будто её нет (в POST уйдёт null)');
eq(B.normalizeButton({ text: 'x', kind: 'url', url: 123 }).url, null, 'button: url не строкой');
eq(B.normalizeButton({ text: 'x', kind: 'switch_inline', query: 5 }).query, null,
    'button: query не строкой — это отсутствие, а не пустая подстановка');
eq(B.normalizeButton({ text: '\u202E\u202D', kind: 'callback' }), null,
    'button: подпись из одних bidi-переопределений — кнопки нет');
eq(B.normalizeButton({ text: 'x', kind: 7 }).kind, 'other', 'button: kind числом → other');
eq(B.normalizeButton([]), null, 'button: массив вместо кнопки');

// ---------- normalizeMarkup: границы ----------
eq(B.normalizeMarkup(markup({ kind: 3, rows: [[btn()]] })), null, 'markup: kind числом');
eq(B.normalizeMarkup([]), null, 'markup: массив вместо разметки');
eq(Array.from(B.normalizeMarkup(markup({ kind: 'force_reply', placeholder: 'п'.repeat(200) }))
    .placeholder).length, B.PLACEHOLDER_MAX, 'markup: длинный placeholder обрезается по потолку');
eq(B.normalizeMarkup(markup({ kind: 'hide', placeholder: 'скрыто' })).placeholder, 'скрыто',
    'markup: placeholder переживает hide (поле ввода его всё равно применит)');
eq(B.normalizeMarkup(markup({
    rows: Array.from({ length: B.MAX_ROWS + 1 }, () => [btn({ text: 'x' })]),
})).rows.length, B.MAX_ROWS, 'markup: ряд сверх потолка отбрасывается целиком');
// Потолок кнопок применяется ДО отсева пустых: ряд из 12 безымянных кнопок и
// одной живой теряется весь. Telegram пустые подписи запрещает, так что вход
// заведомо мусорный — фиксируем поведение, чтобы оно не менялось молча.
eq(B.normalizeMarkup(markup({
    rows: [Array.from({ length: B.MAX_ROW_BUTTONS }, () => btn({ text: '' }))
        .concat([btn({ text: 'жив' })])],
})), null, 'markup: потолок кнопок считается до отсева пустых');

// ---------- buttonAction: краевые входы ----------
// Пустой текст сюда дойти не должен — normalizeButton такую кнопку выбрасывает
// раньше. Ассерт стережёт именно это: если гейт наверху потеряется, в чат
// улетит пустое сообщение, и виноват будет не бот.
eq(B.buttonAction({ kind: 'text', text: '' }), { action: 'send-text', text: '' },
    'action: text без подписи отдаёт пустую строку — отсев живёт в normalizeButton');
eq(B.buttonAction({ kind: 'text' }).text, '', 'action: text без поля text не роняет отправку');
eq(B.buttonAction(btn({ kind: 'callback', data: 123 })), { action: 'callback', data: null },
    'action: data не строкой уходит как null');
eq(B.buttonAction(btn({ kind: 'switch_inline', query: null })).query, '',
    'action: query=null подставляет пустую строку, а не "null"');
eq(B.buttonAction(btn({ kind: 'url', url: '   ' })).action, 'unsupported',
    'action: url из одних пробелов недопустим');
eq(B.buttonAction(btn({ kind: 'login_url', url: '//evil.example' })).action, 'unsupported',
    'action: login_url без схемы не открывается');
eq(B.buttonAction(btn({ kind: 'user_profile', url: 'javascript:alert(1)' })).action, 'unsupported',
    'action: user_profile с javascript-схемой не открывается');
eq(B.buttonAction([]).action, 'unsupported', 'action: массив вместо кнопки');
eq(B.buttonAction({ text: 'x' }).action, 'unsupported', 'action: кнопка без kind');
eq(B.buttonAction({ kind: 'callback', requiresPassword: true }).action, 'unsupported',
    'action: 2FA-пароль в camelCase тоже блокирует нажатие');

// ---------- describeAnswer: краевые тела ответа ----------
eq(B.describeAnswer({ message: 'x', cache_time: '5' }).cacheTime, null,
    'answer: cache_time строкой — как будто его нет');
eq(B.describeAnswer({ message: 'x', cache_time: -1 }).cacheTime, -1,
    'answer: отрицательный cache_time сохраняется как есть (решает вызывающий)');
eq(B.describeAnswer({ message: 'x', alert: true, url: 'https://a.example' }),
    { kind: 'alert', message: 'x', url: 'https://a.example', cacheTime: null },
    'answer: alert и url приходят вместе — нужны оба действия');
eq(B.describeAnswer({ message: 42 }).message, '', 'answer: message числом — текста нет');
eq(B.describeAnswer([]).kind, 'none', 'answer: массив вместо тела');
eq(B.describeAnswer({ message: 'a\t\tb' }).message, 'a b', 'answer: табы схлопываются в пробел');
eq(B.describeAnswer({ message: 'a\n\n\n\n\nb' }).message, 'a\n\nb',
    'answer: пачка пустых строк схлопывается до одной');
eq(B.describeAnswer({ message: '\u202Eготово' }).message, 'готово',
    'answer: bidi-override из ответа бота вырезан');

// ---------- callbackErrorText / callbackRetrySecs: краевые ошибки ----------
eq(B.callbackRetrySecs({ retryAfterSecs: '7' }), 0,
    'retry: строка вместо секунд — ждать нечего (parseApiError отдаёт число или null)');
eq(B.callbackRetrySecs({ retryAfterSecs: Infinity }), 0, 'retry: Infinity не превращается в вечность');
eq(B.callbackRetrySecs({}), 0, 'retry: поля нет');
eq(B.callbackErrorText({ code: 'flood_wait' }), 'слишком часто — подождите',
    'error: flood_wait без status и без секунд');
eq(B.callbackErrorText({}), 'не удалось нажать кнопку', 'error: пустой объект');
eq(B.callbackErrorText({ message: 42 }), '42', 'error: нестроковое сообщение приводится к строке');
eq(B.callbackErrorText({ code: 'flood_wait', status: 429, retryAfterSecs: 1.2 }),
    'слишком часто — повтор через 2s', 'error: дробные секунды округляются вверх и в тексте');

// ---------- latestReplyMarkup: краевая история ----------
eq(B.latestReplyMarkup([Object.assign(mKb(), { id: '9' })]).messageId, 9,
    'reply: строковый id сообщения приводится к числу');
eq(B.latestReplyMarkup([Object.assign(mKb(), { id: undefined })]).messageId, 0,
    'reply: сообщение без id даёт 0, а не NaN');
eq(B.latestReplyMarkup([Object.assign(mKb(), { id: 3, out: undefined })]).messageId, 3,
    'reply: отсутствие out читается как «входящее»');
eq(B.latestReplyMarkup([null, undefined, Object.assign(mKb(), { id: 4 })]).messageId, 4,
    'reply: дырки в истории пропускаются');
eq(B.latestReplyMarkup([{ id: 2, out: false, reply_markup: markup({ kind: 'force_reply' }) }]).markup.kind,
    'force_reply', 'reply: force_reply без предыдущей клавиатуры — самостоятельное состояние');
eq(B.latestReplyMarkup([{ id: 2, out: true, reply_markup: markup({ kind: 'keyboard', rows: [[btn({ kind: 'text' })]] }) }]),
    null, 'reply: своё исходящее с клавиатурой игнорируется целиком');
eq(B.barTitle({ kind: 'inline' }), '// KEYBOARD',
    'bar: заголовок зовут только для reply-видов — inline сюда не доходит');

// ---------- итог ----------
if (failures) {
    console.error(`\n${failures} из ${checks} проверок провалилось`);
    process.exit(1);
}
console.log(`telegram_botkb: ${checks}/${checks} проверок прошли`);
