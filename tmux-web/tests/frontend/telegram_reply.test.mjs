// Регресс-тест чистой логики ответов (reply) вкладки Telegram.
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/reply.js через
// `await import()`. Стаб DOM здесь НЕ нужен: reply.js зависит только от
// format.js, а тот специально не импортирует ни DOM, ни core/*.
//
// Что покрыто: нормализация `MessageDto.reply_to` во всех трёх формах, превью
// (текст / медиа / фрагмент-цитата / недоступный оригинал), обрезка по code
// points с emoji, состояние «на что отвечаем» и его обесценивание при смене
// диалога, классификация двойного клика по строке.
//
// Запуск: node tmux-web/tests/frontend/telegram_reply.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/telegram/reply.js', import.meta.url).href;
const R = await import(modUrl);

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

// ---------- truncatePreview: обрезка по code points ----------
eq(R.truncatePreview('привет', 140), 'привет', 'обрезка: короткий текст не трогаем');
eq(R.truncatePreview('abc', 3), 'abc', 'обрезка: ровно лимит остаётся целым');
eq(R.truncatePreview('abcd', 3), 'ab…', 'обрезка: многоточие ВХОДИТ в лимит');
eq(R.truncatePreview('  первая\n\nвторая  ', 140), 'первая вторая',
    'обрезка: переводы строк схлопываются в один пробел');
eq(R.truncatePreview('', 140), '', 'обрезка: пустая строка');
eq(R.truncatePreview('   ', 140), '', 'обрезка: одни пробелы — пусто');
eq(R.truncatePreview(null, 140), '', 'обрезка: null не роняет');
eq(R.truncatePreview(undefined, 140), '', 'обрезка: undefined не роняет');
// Суррогатные пары: slice() по UTF-16 разрубил бы emoji на половинки.
eq(R.truncatePreview('👍👍👍👍👍', 3), '👍👍…', 'обрезка: emoji режутся по code points');
// Одинокий суррогат — половина emoji, рисуется как «�». Ловим обе формы:
// старший без младшего и младший без старшего.
const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;
ok(!LONE_SURROGATE.test(R.truncatePreview('👍👍👍👍👍', 3)),
    'обрезка: в результате нет одинокого суррогата');
eq(Array.from(R.truncatePreview('я'.repeat(400))).length, R.REPLY_PREVIEW_CHARS,
    'обрезка: дефолтный лимит — REPLY_PREVIEW_CHARS code points');
eq(R.truncatePreview('日本語のテキスト', 4), '日本語…', 'обрезка: не-латиница считается посимвольно');

// ---------- replyMediaMark ----------
eq(R.replyMediaMark('photo'), '[photo]', 'медиа-пометка: фото');
eq(R.replyMediaMark('VOICE'), '[voice]', 'медиа-пометка: регистр не важен');
eq(R.replyMediaMark('gif'), '[media]', 'медиа-пометка: неизвестный вид — generic');
eq(R.replyMediaMark(null), '[media]', 'медиа-пометка: null — generic');

// ---------- normalizeReplyTo: три формы ответа ----------
const fullDto = {
    id: 10,
    dialog_id: '777000',
    reply_to_id: 7,
    reply_to: {
        message_id: 7,
        sender: 'sofia',
        sender_id: '4242',
        text: 'старое сообщение',
        media_kind: null,
        quote: null,
        out: false,
        unavailable: false,
    },
};
eq(R.normalizeReplyTo(fullDto), {
    messageId: 7,
    sender: 'sofia',
    senderId: '4242',
    text: 'старое сообщение',
    mediaKind: null,
    quote: null,
    out: false,
    unavailable: false,
}, 'нормализация: полный reply_to с превью');

eq(R.normalizeReplyTo({ id: 11, reply_to: { message_id: 7, unavailable: false } }), {
    messageId: 7,
    sender: null,
    senderId: null,
    text: null,
    mediaKind: null,
    quote: null,
    out: null,
    unavailable: false,
}, 'нормализация: сообщение из потока апдейтов — только message_id');

eq(R.normalizeReplyTo({ id: 12, reply_to_id: 5 }), {
    messageId: 5,
    sender: null,
    senderId: null,
    text: null,
    mediaKind: null,
    quote: null,
    out: null,
    unavailable: false,
}, 'нормализация: старый сервер — одно голое reply_to_id');

eq(R.normalizeReplyTo({ id: 13 }), null, 'нормализация: обычное сообщение — не ответ');
eq(R.normalizeReplyTo({ id: 13, reply_to: null, reply_to_id: null }), null,
    'нормализация: явные null — не ответ');
eq(R.normalizeReplyTo(null), null, 'нормализация: null не роняет');
eq(R.normalizeReplyTo({ reply_to_id: 0 }), null, 'нормализация: reply_to_id=0 — не ответ');
// reply_to сильнее reply_to_id: на форум-топиках старое поле врёт (там лежит id
// топика у КАЖДОГО сообщения), и цитата шла бы под всей супергруппой.
eq(R.normalizeReplyTo({ reply_to_id: 1, reply_to: { message_id: null, unavailable: false } }).messageId,
    null, 'нормализация: reply_to главнее reply_to_id (форум-топик)');

// Ответ на сторис: сообщения-оригинала не существует в природе.
const story = R.normalizeReplyTo({ reply_to: { message_id: null, unavailable: true } });
eq(story.messageId, null, 'нормализация: сторис — оригинала нет');
eq(story.unavailable, true, 'нормализация: сторис помечен недоступным');

// ---------- replyNeedsPreview ----------
ok(R.replyNeedsPreview(R.normalizeReplyTo({ reply_to_id: 5 })),
    'догрузка: голый id — превью ещё нужно');
ok(!R.replyNeedsPreview(R.normalizeReplyTo(fullDto)),
    'догрузка: обогащённая цитата повторно не запрашивается');
ok(!R.replyNeedsPreview(R.normalizeReplyTo({ reply_to: { message_id: 7, unavailable: true } })),
    'догрузка: недоступный оригинал не запрашиваем снова');
ok(!R.replyNeedsPreview(story), 'догрузка: у сторис нечего догружать');
ok(!R.replyNeedsPreview(null), 'догрузка: null не роняет');
// Сообщение с одним вложением текста не имеет — признаком должен быть `out`.
ok(!R.replyNeedsPreview(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'photo', out: false, unavailable: false },
})), 'догрузка: медиа без текста уже обогащено (признак — out, а не text)');

// ---------- replyAuthorLabel ----------
const dialog = { id: '777000', title: 'Sofia', kind: 'user' };
eq(R.replyAuthorLabel(R.normalizeReplyTo(fullDto), dialog), 'sofia', 'автор: имя из превью');
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, out: true, text: 'моё' },
}), dialog), 'you@local', 'автор: своё сообщение в личке узнаётся по out, а не по sender');
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, out: false, text: 'пост' },
}), dialog), 'Sofia', 'автор: фолбэк на title диалога (пост от имени канала)');
eq(R.replyAuthorLabel(R.normalizeReplyTo({ reply_to_id: 7 }), null), 'unknown',
    'автор: ничего не известно — unknown');
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, unavailable: true },
}), dialog), '', 'автор: у недоступного оригинала имени не выдумываем');
eq(R.replyAuthorLabel(null, dialog), '', 'автор: null не роняет');

// ---------- replyPreviewText ----------
eq(R.replyPreviewText(R.normalizeReplyTo(fullDto)), 'старое сообщение',
    'превью: обычный текст');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'photo', out: false },
})), '[photo]', 'превью: вложение без подписи — только пометка вида');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'video', text: 'смотри', out: false },
})), '[video] смотри', 'превью: вложение с подписью — пометка + текст');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, text: 'весь длинный текст', quote: 'фрагмент', out: false },
})), 'фрагмент', 'превью: процитированный фрагмент важнее полного текста');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, unavailable: true },
})), R.REPLY_GONE_TEXT, 'превью: удалённый оригинал');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, quote: 'фрагмент', unavailable: true },
})), 'фрагмент', 'превью: фрагмент показываем даже у удалённого оригинала');
eq(R.replyPreviewText(R.normalizeReplyTo({ reply_to_id: 7 })), R.REPLY_PENDING_TEXT,
    'превью: ещё не приехало — многоточие, а не пустота');
eq(R.replyPreviewText(story), R.REPLY_GONE_TEXT, 'превью: сторис — недоступно');
eq(R.replyPreviewText(null), '', 'превью: null не роняет');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, text: 'я'.repeat(400), out: false },
})).length, R.REPLY_PREVIEW_CHARS, 'превью: длинный оригинал обрезан по лимиту');

// ---------- replyFromMessage / mergeReplyFromMessage ----------
const original = {
    id: 7,
    dialog_id: '777000',
    out: false,
    text: 'старое сообщение',
    sender: { id: '4242', username: 'sofia' },
    media: null,
};
eq(R.replyFromMessage(original, dialog), {
    messageId: 7,
    sender: 'sofia',
    senderId: '4242',
    text: 'старое сообщение',
    mediaKind: null,
    quote: null,
    out: false,
    unavailable: false,
}, 'оригинал: описание чужого сообщения');
eq(R.replyFromMessage({ id: 8, out: true, text: 'моё' }, dialog).sender, null,
    'оригинал: своё сообщение автора не несёт (его даст out)');
eq(R.replyFromMessage(null, dialog), null, 'оригинал: null не роняет');

const bare = R.normalizeReplyTo({ reply_to_id: 7 });
const merged = R.mergeReplyFromMessage(bare, original, dialog);
eq(merged.text, 'старое сообщение', 'досборка: текст подставлен из ленты');
eq(merged.sender, 'sofia', 'досборка: автор подставлен из ленты');
eq(merged.out, false, 'досборка: out проставлен — повторной догрузки не будет');
ok(!R.replyNeedsPreview(merged), 'досборка: после неё превью считается готовым');
eq(bare.text, null, 'досборка: входной объект не мутируется');
// Защита от подстановки чужого текста — то же правило, что на сервере.
eq(R.mergeReplyFromMessage(bare, { id: 999, text: 'чужое' }, dialog).text, null,
    'досборка: оригинал с чужим id игнорируется');
eq(R.mergeReplyFromMessage(bare, null, dialog).text, null,
    'досборка: отсутствующий оригинал ничего не портит');
const quoted = R.normalizeReplyTo({
    reply_to: { message_id: 7, quote: 'фрагмент', unavailable: false },
});
eq(R.mergeReplyFromMessage(quoted, original, dialog).quote, 'фрагмент',
    'досборка: фрагмент принадлежит ОТВЕТУ и оригиналом не перетирается');

// ---------- buildReplyTarget / replyDtoFromTarget ----------
const target = R.buildReplyTarget('777000', original, dialog);
eq(target.dialogId, '777000', 'цель: диалог строкой');
eq(target.messageId, 7, 'цель: id оригинала');
eq(target.name, 'sofia', 'цель: имя для полосы');
eq(target.preview, 'старое сообщение', 'цель: превью для полосы');
eq(target.out, false, 'цель: чужое сообщение');
eq(R.buildReplyTarget('777000', { id: null, out: true, text: 'в пути' }, dialog), null,
    'цель: на неподтверждённую optimistic-строку ответить нельзя');
eq(R.buildReplyTarget('', original, dialog), null, 'цель: без диалога цели нет');
eq(R.buildReplyTarget('777000', null, dialog), null, 'цель: null не роняет');

eq(R.replyDtoFromTarget(target), {
    message_id: 7,
    sender: 'sofia',
    sender_id: '4242',
    text: 'старое сообщение',
    media_kind: null,
    quote: null,
    out: false,
    unavailable: false,
}, 'optimistic: цитата своего ответа рисуется сразу, до подтверждения сервером');
const ownTarget = R.buildReplyTarget('777000', { id: 8, out: true, text: 'моё' }, dialog);
eq(R.replyDtoFromTarget(ownTarget).out, true, 'optimistic: ответ на своё помечен out');
eq(R.replyDtoFromTarget(ownTarget).sender, null,
    'optimistic: у своего оригинала автора нет — его даст out');
eq(R.replyDtoFromTarget(null), null, 'optimistic: без цели цитаты нет');
// Круг замыкается: DTO из цели читается той же нормализацией, что серверный.
eq(R.replyPreviewText(R.normalizeReplyTo({ reply_to: R.replyDtoFromTarget(target) })),
    'старое сообщение', 'optimistic: DTO цели проходит обратную нормализацию');

// ---------- replySourceLine ----------
eq(R.replySourceLine(original, dialog), '#7 sofia: старое сообщение',
    'строка лога: оригинал вне ленты описан целиком');
eq(R.replySourceLine(null, dialog), '', 'строка лога: null не роняет');

// ---------- состояние «на что отвечаем» ----------
eq(R.replyTarget('777000'), null, 'состояние: на старте цели нет');
R.setReplyTarget(target);
eq(R.replyTarget('777000').messageId, 7, 'состояние: цель видна в своём диалоге');
eq(R.replyTargetId('777000'), 7, 'состояние: id для reply_to_id');
// Смена диалога обесценивает цель — ответ не уйдёт в чужой чат.
eq(R.replyTarget('999'), null, 'состояние: в другом диалоге цели нет');
eq(R.replyTargetId('999'), null, 'состояние: чужой диалог не даёт reply_to_id');
eq(R.replyTarget(null), null, 'состояние: без диалога цели нет');
eq(R.replyTargetId(undefined), null, 'состояние: undefined не роняет');
eq(R._debugReplyState().messageId, 7, 'состояние: снимок для e2e');
ok(R.clearReplyTarget(), 'состояние: снятие сообщает, что цель была');
eq(R.replyTarget('777000'), null, 'состояние: после снятия цели нет');
ok(!R.clearReplyTarget(), 'состояние: повторное снятие идемпотентно');
eq(R._debugReplyState(), { dialogId: null, messageId: null, name: null, preview: null },
    'состояние: пустой снимок');
// Мусор целью не становится: иначе полоса показывала бы «отвечаю на undefined».
eq(R.setReplyTarget({ dialogId: '777000' }), null, 'состояние: цель без id отвергнута');
eq(R.setReplyTarget(null), null, 'состояние: null отвергнут');

// ---------- классификация двойного клика по строке ----------
// От DOM нужен только closest — стаб отдаёт «попадание» по списку селекторов.
function hit(...selectors) {
    return { closest: (sel) => (selectors.includes(sel) ? {} : null) };
}
eq(R.classifyRowClick(hit('.tg-line')), 'reply', 'клик: обычная строка — ответить');
eq(R.classifyRowClick(hit('.tg-line-own', '.tg-line')), 'reply',
    'клик: своя строка тоже даёт ответ');
eq(R.classifyRowClick(hit('.tg-media-player', '.tg-media-card', '.tg-line')), 'ignore',
    'клик: перемотка в плеере не начинает ответ');
eq(R.classifyRowClick(hit('.tg-kb', 'button', '.tg-line')), 'ignore',
    'клик: кнопка бота шлёт callback, а не ответ');
eq(R.classifyRowClick(hit('a[href]', '.tg-line')), 'ignore',
    'клик: ссылка открывается, а не отвечается');
eq(R.classifyRowClick(hit('.tg-media-card', '.tg-line')), 'ignore',
    'клик: медиа-карточка открывается одинарным кликом — двойной её не трогает');
eq(R.classifyRowClick(hit('.tg-reply-quote', '.tg-line')), 'ignore',
    'клик: у цитаты своё действие — переход к оригиналу');
eq(R.classifyRowClick(hit('button', '.tg-line')), 'ignore',
    'клик: кнопка «ответить» отрабатывает своим обработчиком');
eq(R.classifyRowClick(hit('.tg-log-spacer')), 'ignore', 'клик: мимо строк — ничего');
eq(R.classifyRowClick(null), 'ignore', 'клик: null не роняет обработчик');
eq(R.classifyRowClick({}), 'ignore', 'клик: узел без closest (текстовый) — ничего');
ok(R.REPLY_CLICK_IGNORE.includes('.tg-media-player'),
    'клик: плеер обязан быть в стоп-листе явно');

// ---------------------------------------------------------------------------
// Краевые случаи
// ---------------------------------------------------------------------------

// ---------- обрезка: границы лимита ----------
eq(R.truncatePreview('abcd', 1), '…', 'край обрезки: лимит 1 — остаётся одно многоточие');
eq(Array.from(R.truncatePreview('я'.repeat(200), 0)).length, R.REPLY_PREVIEW_CHARS,
    'край обрезки: max=0 читается как «лимит не задан» и откатывается к дефолту');
eq(R.truncatePreview('я'.repeat(200), -5), '…',
    'край обрезки: отрицательный лимит вырождается в один символ, но не роняет обрезку');
eq(R.truncatePreview('я'.repeat(140), 140), 'я'.repeat(140),
    'край обрезки: ровно лимит — многоточия НЕ добавляем');
eq(R.truncatePreview('я'.repeat(141), 140), 'я'.repeat(139) + '…',
    'край обрезки: лимит + 1 символ — режем на один и ставим многоточие');
eq(R.truncatePreview(42, 140), '42', 'край обрезки: число из битого JSON не роняет');
eq(R.truncatePreview('a  b', 140), 'a b',
    'край обрезки: неразрывные пробелы схлопываются наравне с обычными');
eq(R.truncatePreview('\n\n\nтолько переводы\n\n', 140), 'только переводы',
    'край обрезки: многострочный оригинал не раздувает строку ленты');
eq(R.truncatePreview('первая\nвторая\nтретья', 140), 'первая вторая третья',
    'край обрезки: несколько строк склеиваются пробелами');
// Составные emoji: обрезка по code points может разделить ZWJ-последовательность,
// но НИКОГДА не должна оставить одинокий суррогат — от него в ленте «�».
ok(!LONE_SURROGATE.test(R.truncatePreview('\u{1F469}‍\u{1F4BB}'.repeat(30), 5)),
    'край обрезки: ZWJ-последовательность не рвётся на половинки суррогатов');
ok(!LONE_SURROGATE.test(R.truncatePreview('\u{1F3F4}\u{E0067}\u{E0062}', 2)),
    'край обрезки: флаг из тегов-символов тоже режется безопасно');

// ---------- превью: краевые оригиналы ----------
// Сообщение из одного вложения БЕЗ подписи: пометка вида — единственное, что
// вообще можно показать.
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'voice', text: null, out: false },
})), '[voice]', 'край превью: голосовое без подписи');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'sticker', out: true },
})), '[sticker]', 'край превью: свой стикер');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'gif', out: false },
})), '[media]', 'край превью: незнакомый вид вложения не роняет цитату');
// Подпись под вложением — многострочная: пометка вида остаётся ПЕРЕД текстом.
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'photo', text: 'первая\nвторая', out: false },
})), '[photo] первая вторая', 'край превью: многострочная подпись под фото');
// Пометка вида входит в строку СВЕРХ лимита текста — это осознанно: обрезать
// «[photo]» значило бы получить «[pho…» и потерять сам вид вложения.
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'video', text: 'я'.repeat(400), out: false },
})), `[video] ${'я'.repeat(139)}…`, 'край превью: подпись обрезана, пометка вида цела');
// Фрагмент сильнее всего: и полного текста, и вида вложения.
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, media_kind: 'photo', text: 'подпись', quote: 'кусок', out: false },
})), 'кусок', 'край превью: процитированный фрагмент важнее пометки вложения');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, quote: 'я'.repeat(400), out: false },
})).length, R.REPLY_PREVIEW_CHARS, 'край превью: длинный фрагмент обрезается по тому же лимиту');
// Оригинал без текста и без вложения (сервисное сообщение: «закрепил», «зашёл
// в чат»). Показывать нечего, но цитата остаётся кликабельной — переход к
// оригиналу объяснит пользователю больше, чем любая выдуманная подпись.
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, text: null, media_kind: null, out: false },
})), R.REPLY_PENDING_TEXT, 'край превью: сервисный оригинал показывает многоточие, а не пустоту');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, text: '   ', out: false },
})), R.REPLY_PENDING_TEXT, 'край превью: текст из одних пробелов равносилен его отсутствию');
// «Недоступно» перебивает текст: если сервер пометил оригинал удалённым, его
// старое превью (из кэша ответа) показывать нельзя — оно врало бы.
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: { message_id: 7, text: 'то, что было', unavailable: true },
})), R.REPLY_GONE_TEXT, 'край превью: удаление сильнее сохранившегося текста');

// Ответ в ЧУЖОЙ чат (`reply_to_peer_id`): оригинал лежит в другом диалоге,
// поэтому сервер шлёт автора и вид медиа, но НЕ шлёт ни id (под этим номером
// в нашем диалоге постороннее сообщение), ни текст. Полностью пустой такой
// ответ сервер помечает `unavailable` сам (dto.rs::reply_to_from_header),
// поэтому здесь цитата всегда что-то показывает.
const foreign = R.normalizeReplyTo({
    reply_to: {
        message_id: null, sender: 'Иван', sender_id: '777000', text: null,
        media_kind: 'photo', quote: null, out: null, unavailable: false,
    },
});
eq(foreign.messageId, null, 'край чужого чата: id оригинала не отдаётся — переходить некуда');
eq(R.replyPreviewText(foreign), '[photo]', 'край чужого чата: показываем то немногое, что прислали');
eq(R.replyAuthorLabel(foreign, dialog), 'Иван', 'край чужого чата: автор известен');
ok(!R.replyNeedsPreview(foreign), 'край чужого чата: батчить нечего — оригинал в другом диалоге');
eq(R.replyPreviewText(R.normalizeReplyTo({
    reply_to: {
        message_id: null, sender: 'Иван', text: null, media_kind: null,
        quote: 'кусок чужого текста', out: null, unavailable: false,
    },
})), 'кусок чужого текста', 'край чужого чата: процитированный фрагмент приходит с ответом');

// ---------- автор: краевые имена ----------
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, sender: '   ', text: 'x', out: false },
}), dialog), 'Sofia', 'край автора: имя из одних пробелов равносильно его отсутствию');
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, sender: '👍🔥', text: 'x', out: false },
}), dialog), '👍🔥', 'край автора: имя из одних эмодзи сохраняется как есть');
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, sender: '  sofia  ', text: 'x', out: false },
}), dialog), 'sofia', 'край автора: обрамляющие пробелы срезаются');
// Имя НЕ обрезается по длине сознательно: за ширину отвечает CSS полосы и
// цитаты (nowrap + ellipsis), а обрезка здесь дала бы двойное многоточие.
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, sender: 'и'.repeat(300), out: false },
}), dialog).length, 300, 'край автора: длину имени ограничивает вёрстка, а не логика');
// `out` сильнее любого имени: своё сообщение подписывается одинаково везде.
eq(R.replyAuthorLabel(R.normalizeReplyTo({
    reply_to: { message_id: 7, sender: 'sofia', out: true },
}), dialog), 'you@local', 'край автора: out перебивает присланное имя');
eq(R.replyAuthorLabel(R.normalizeReplyTo({ reply_to_id: 7 }), { id: '1', title: '' }), 'unknown',
    'край автора: пустой title диалога не становится именем');

// ---------- нормализация: битые типы с сервера ----------
eq(R.normalizeReplyTo({ reply_to_id: '7' }), null,
    'край нормализации: строковый reply_to_id не принимается (id — число по контракту)');
eq(R.normalizeReplyTo({ reply_to_id: -7 }), null,
    'край нормализации: отрицательный id — не сообщение');
eq(R.normalizeReplyTo({ reply_to: 'мусор', reply_to_id: 7 }).messageId, 7,
    'край нормализации: reply_to не-объект откатывается на reply_to_id');
eq(R.normalizeReplyTo({ reply_to: { message_id: 7, unavailable: 'да' } }).unavailable, false,
    'край нормализации: unavailable читается строго как true, а не по truthy');
eq(R.normalizeReplyTo({ reply_to: { message_id: 7, out: 'true' } }).out, null,
    'край нормализации: строковый out не считается признаком «наше сообщение»');
eq(R.normalizeReplyTo('строка'), null, 'край нормализации: не-объект вместо MessageDto');

// ---------- досборка и цель: краевые случаи ----------
const ownOriginal = { id: 8, dialog_id: '777000', out: true, text: 'моё старое' };
eq(R.replyAuthorLabel(R.mergeReplyFromMessage(
    R.normalizeReplyTo({ reply_to_id: 8 }), ownOriginal, dialog
), dialog), 'you@local', 'край досборки: свой оригинал из ленты подписывается «you@local»');
eq(R.replySourceLine({ id: 7, out: false, media: { kind: 'photo' }, sender: { username: 'sofia' } }, dialog),
    '#7 sofia: [photo]', 'край строки лога: оригинал из одного вложения');
eq(R.replySourceLine({ id: 7, out: true, text: 'моё' }, dialog), '#7 you@local: моё',
    'край строки лога: своё сообщение');
// Диалог приходит числом там, где активный id — строка: цель обязана найтись.
const numericTarget = R.buildReplyTarget(777000, original, dialog);
R.setReplyTarget(numericTarget);
eq(R.replyTargetId('777000'), 7, 'край состояния: числовой dialogId сводится к строке');
eq(R.replyTargetId(777000), 7, 'край состояния: и обратно — число в запросе тоже совпадает');
eq(R.replyTargetId('7770001'), null, 'край состояния: похожий id — не тот же диалог');
R.clearReplyTarget();
// Цель на сообщение с id 0 (Telegram не подтвердил номер) недопустима: ответ на
// такой id сервер отвергнет с 400.
eq(R.buildReplyTarget('777000', { id: 0, out: true, text: 'x' }, dialog), null,
    'край цели: id=0 не годится в reply_to_id');
eq(R.setReplyTarget({ dialogId: '777000', messageId: NaN }), null,
    'край состояния: NaN целью не становится');
eq(R.replyDtoFromTarget({ dialogId: '777000', messageId: 7 }), {
    message_id: 7, sender: null, sender_id: null, text: null,
    media_kind: null, quote: null, out: false, unavailable: false,
}, 'край optimistic: цель без вложенного reply даёт валидный DTO');

// ---------- итог ----------
if (failures) {
    console.error(`\n${failures} из ${checks} проверок провалилось`);
    process.exit(1);
}
console.log(`telegram_reply: ${checks}/${checks} проверок прошли`);
