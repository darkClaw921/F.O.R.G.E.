// Регресс-тест ответов панели VK (js/vk/reply.js) — ЧИСТАЯ логика.
//
// Грузит РЕАЛЬНЫЙ static/js/vk/reply.js через await import(): модуль намеренно
// без DOM и без сети (зависит только от таких же чистых format.js и
// attachments.js), поэтому весь его контракт проверяется без стенда.
//
// ГЛАВНОЕ, ЧТО СТЕРЕЖЁТ ЭТОТ ФАЙЛ — КЛЮЧ ЦЕЛИ ОТВЕТА. У VK ответ адресуется
// `conversation_message_id`, а не `message_id`:
//
//   * cmid нумеруется В КАЖДОЙ беседе с единицы — «cmid 514» без беседы
//     указывает разом на 514-е сообщение каждого чата;
//   * у оригинала, приехавшего инлайном в `reply_message`, собственного
//     `message.id` может не быть вовсе.
//
// МУТАЦИОННАЯ ПРОВЕРКА: заменить ключ цели на `message_id` (в `buildReplyTarget`
// / `replyDtoFromTarget` / `replyTargetCmid`) — разделы 5 и 6 обязаны
// покраснеть.
//
// UI-половина (полоса ответа, переход к оригиналу) живёт в vk_reply_ui.test.mjs.
//
// Запуск: node tmux-web/tests/frontend/vk_reply.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const base = new URL('../../static/js/vk/', import.meta.url).href;
const R = await import(`${base}reply.js`);

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

/** MessageDto беседы 2000000007. */
function message(extra) {
    return Object.assign({
        id: 7003,
        dialog_id: '2000000007',
        out: false,
        date: 1_700_000_120,
        sender: { id: '1234', first_name: 'Павел', last_name: 'Дуров', screen_name: 'durov' },
        text: 'это ответ',
        media: null,
        attachments: [],
        reply_to_id: null,
        reply_to: null,
        edit_date: null,
        reply_markup: null,
        cmid: 514,
        fwd: [],
        action: null,
        payload: null,
    }, extra || {});
}

/** Серверный ReplyToDto. */
function replyDto(extra) {
    return Object.assign({
        message_id: null,
        cmid: 513,
        sender: 'Я Сам',
        sender_id: '999',
        text: 'оригинал',
        media_kind: null,
        unavailable: false,
    }, extra || {});
}

const DIALOG = { id: '2000000007', kind: 'chat', title: 'Проектная' };
const SELF = '999';

// ---------------------------------------------------------------------------
// 1. Нормализация: три формы, в которых ответ доезжает до фронта
// ---------------------------------------------------------------------------

{
    // Форма 1 — серверный DTO (основной путь: превью уже собрано инлайном).
    const r = R.normalizeReplyTo(message({ reply_to: replyDto() }));
    eq(r.cmid, 513, 'ключ цитаты — cmid');
    eq(r.messageId, null, 'у инлайненного оригинала своего message_id может не быть');
    eq(r.text, 'оригинал', 'текст превью');
    eq(r.sender, 'Я Сам', 'автор оригинала');
    eq(r.senderId, '999', 'id автора строкой');
    eq(r.unavailable, false, 'оригинал доступен');

    // Форма 2 — СЫРАЯ форма VK (кадр WS, собранный старой версией плагина).
    const raw = R.normalizeReplyTo({
        id: 7003,
        reply_message: {
            id: 7002,
            conversation_message_id: 513,
            from_id: 999,
            text: 'оригинал',
            attachments: [],
        },
    });
    eq(raw.cmid, 513, 'сырая форма: cmid из conversation_message_id');
    eq(raw.messageId, 7002, 'сырая форма: глобальный id едет рядом');
    eq(raw.senderId, '999', 'сырая форма: from_id строкой');
    eq(raw.text, 'оригинал', 'сырая форма: текст');

    // Форма 3 — голое reply_to_id (сервер без нового поля).
    const bare = R.normalizeReplyTo(message({ reply_to_id: 7002 }));
    eq(bare.cmid, null, 'голый reply_to_id cmid не даёт');
    eq(bare.messageId, 7002, 'зато даёт message_id');
    eq(R.replyPreviewText(bare), '…', 'и рисуется многоточием, а не пустой строкой');

    // Приоритет: reply_to сильнее reply_message и reply_to_id.
    const both = R.normalizeReplyTo(message({
        reply_to: replyDto({ text: 'из DTO' }),
        reply_message: { conversation_message_id: 1, text: 'из сырья' },
        reply_to_id: 42,
    }));
    eq(both.text, 'из DTO', 'серверный DTO приоритетнее сырой формы');
    eq(both.cmid, 513, 'и его cmid');

    // Не ответ.
    eq(R.normalizeReplyTo(message()), null, 'обычное сообщение цитаты не даёт');
    eq(R.normalizeReplyTo(null), null, 'null — не ответ');
    eq(R.normalizeReplyTo('строка'), null, 'мусор — не ответ');
    eq(R.normalizeReplyTo(message({ reply_to_id: 0 })), null, 'нулевой id — не ответ');

    // Недоступный оригинал: VK присылает пустой объект.
    const gone = R.normalizeReplyTo(message({ reply_to: replyDto({
        cmid: null, sender: null, sender_id: null, text: null, unavailable: true,
    }) }));
    eq(gone.unavailable, true, 'DTO помечен недоступным');
    eq(R.replyPreviewText(gone), R.REPLY_GONE_TEXT, 'и рисуется соответствующим текстом');

    const goneRaw = R.normalizeReplyTo({ reply_message: { text: '', attachments: [] } });
    eq(goneRaw.unavailable, true, 'сырой пустой объект — тоже «недоступен»');
}

// ---------------------------------------------------------------------------
// 2. Превью: у VK текст бывает ПУСТ при непустых вложениях
// ---------------------------------------------------------------------------

{
    // Голосовое без подписи — типовой случай, а не край.
    const voice = R.normalizeReplyTo(message({
        reply_to: replyDto({ text: null, media_kind: 'voice' }),
    }));
    eq(R.replyPreviewText(voice), '[voice]', 'пустой текст + вложение ⇒ вид вложения');

    // Фотография С подписью: пометка вида остаётся, иначе цитата на фото с
    // текстом неотличима от цитаты на обычное сообщение.
    const photo = R.normalizeReplyTo(message({
        reply_to: replyDto({ text: 'смотри', media_kind: 'photo' }),
    }));
    eq(R.replyPreviewText(photo), '[photo] смотри', 'подпись под вложением помечена видом');

    // Сырые имена типов VK тоже понимаются — они приезжают в старых кадрах.
    eq(R.replyMediaMark('audio_message'), '[voice]', 'сырой audio_message ⇒ [voice]');
    eq(R.replyMediaMark('doc'), '[document]', 'сырой doc ⇒ [document]');
    eq(R.replyMediaMark('graffiti'), '[graffiti]', 'граффити — вид VK, у Telegram его нет');
    eq(R.replyMediaMark('sticker'), '[sticker]', 'стикер');
    eq(R.replyMediaMark('wall'), '[other]', 'репост ⇒ прочее');
    eq(R.replyMediaMark('hologram'), '[media]', 'неизвестный вид ⇒ нейтральное [media]');
    eq(R.replyMediaMark(null), '[media]', 'отсутствие вида ⇒ [media]');

    // Пустая цитата без единого признака — пустая строка (рисовать нечего).
    eq(R.replyPreviewText({ cmid: null, messageId: null, text: null, mediaKind: null, unavailable: false }),
        '', 'без содержимого и без id превью пусто');
    eq(R.replyPreviewText(null), '', 'без цитаты превью пусто');
}

// ---------------------------------------------------------------------------
// 3. Обрезка превью по code points
// ---------------------------------------------------------------------------

{
    const long = 'я'.repeat(300);
    const cut = R.truncatePreview(long);
    eq(Array.from(cut).length, R.REPLY_PREVIEW_CHARS, 'многоточие ВХОДИТ в лимит');
    ok(cut.endsWith('…'), 'обрезка помечена многоточием');

    const emoji = '🙂'.repeat(300);
    const cutEmoji = R.truncatePreview(emoji);
    ok(!cutEmoji.includes('�'), 'суррогатные пары не разрублены');
    eq(Array.from(cutEmoji).length, R.REPLY_PREVIEW_CHARS, 'эмодзи считаются как code points');

    eq(R.truncatePreview('строка\n\nс   переносами'), 'строка с переносами',
        'пробелы схлопнуты — иначе цитата раздула бы строку ленты');
    eq(R.truncatePreview('   '), '', 'пробельная строка даёт пустое превью');
    eq(R.truncatePreview(null), '', 'null даёт пустое превью');
    eq(R.truncatePreview('точно', 5), 'точно', 'ровно на границе — без обрезки');
    eq(R.truncatePreview('точное', 5), 'точн…', 'на символ длиннее — обрезка');
}

// ---------------------------------------------------------------------------
// 4. Имя автора
// ---------------------------------------------------------------------------

{
    const mine = R.normalizeReplyTo(message({ reply_to: replyDto({ sender_id: SELF }) }));
    eq(R.replyAuthorLabel(mine, DIALOG, SELF), 'you@local',
        'своя цитата подписана you@local, а не числовым id');
    eq(R.replyAuthorLabel(mine, DIALOG, null), 'Я Сам',
        'без известного id аккаунта берётся имя от сервера');

    const theirs = R.normalizeReplyTo(message({ reply_to: replyDto({ sender_id: '1234', sender: 'Павел Дуров' }) }));
    eq(R.replyAuthorLabel(theirs, DIALOG, SELF), 'Павел Дуров', 'чужая цитата — именем автора');

    const anon = R.normalizeReplyTo(message({ reply_to: replyDto({ sender: null, sender_id: null }) }));
    eq(R.replyAuthorLabel(anon, DIALOG, SELF), 'Проектная', 'без автора — название беседы');
    eq(R.replyAuthorLabel(anon, null, SELF), 'unknown', 'и без беседы — честное unknown');

    const gone = R.normalizeReplyTo(message({ reply_to: replyDto({
        sender: null, sender_id: null, text: null, unavailable: true,
    }) }));
    eq(R.replyAuthorLabel(gone, DIALOG, SELF), '',
        'у недоступного оригинала имени нет — «недоступно» уже сказано превью');
    eq(R.replyAuthorLabel(null, DIALOG, SELF), '', 'без цитаты имени нет');
}

// ---------------------------------------------------------------------------
// 5. КЛЮЧ ЦЕЛИ: cmid, а не message_id
// ---------------------------------------------------------------------------

{
    const target = R.buildReplyTarget('2000000007', message(), DIALOG, SELF);
    eq(target.cmid, 514, 'цель адресуется conversation_message_id');
    eq(target.messageId, 7003, 'глобальный id едет рядом — но ключом не служит');
    eq(target.dialogId, '2000000007', 'беседа зафиксирована вместе с целью');
    eq(target.name, 'durov', 'подпись цели — ник автора');
    eq(target.preview, 'это ответ', 'превью цели');
    eq(target.out, false, 'чужое сообщение');

    // Сообщение БЕЗ cmid целью стать не может: адресовать ответ нечем.
    eq(R.buildReplyTarget('2000000007', message({ cmid: null }), DIALOG, SELF), null,
        'без cmid цели нет, даже если message_id известен');
    eq(R.buildReplyTarget('2000000007', message({ cmid: 0 }), DIALOG, SELF), null,
        'нулевой cmid — это «не заполнен»');
    eq(R.buildReplyTarget('', message(), DIALOG, SELF), null, 'без беседы цели нет');
    eq(R.buildReplyTarget('2000000007', null, DIALOG, SELF), null, 'без сообщения цели нет');

    // Своё сообщение подписывается you@local.
    const own = R.buildReplyTarget('2000000007', message({ out: true, sender: { id: SELF } }), DIALOG, SELF);
    eq(own.name, 'you@local', 'своя цель подписана you@local');
    eq(own.out, true, 'и помечена как своя');

    // Оптимистичная цитата: форма ровно серверная, ключ — cmid.
    const dto = R.replyDtoFromTarget(target);
    eq(dto.cmid, 514, 'в оптимистичный reply_to едет cmid');
    eq(dto.message_id, 7003, 'глобальный id — справочно');
    eq(dto.unavailable, false, 'свежая цитата доступна');
    // Круг замкнулся: нарисованная до подтверждения строка разбирается тем же
    // кодом, что и пришедшая с сервера.
    const back = R.normalizeReplyTo({ reply_to: dto });
    eq(back.cmid, 514, 'нормализация оптимистичной цитаты даёт тот же ключ');
    eq(R.replyDtoFromTarget(null), null, 'без цели DTO не собирается');
}

// ---------------------------------------------------------------------------
// 6. Хранилище цели: одинаковый cmid в РАЗНЫХ беседах не путается
// ---------------------------------------------------------------------------

{
    R.clearReplyTarget();
    eq(R.replyTarget('2000000007'), null, 'старт: цели нет');
    eq(R.replyTargetCmid('2000000007'), null, 'и cmid для отправки нет');

    const target = R.buildReplyTarget('2000000007', message(), DIALOG, SELF);
    R.setReplyTarget(target);
    eq(R.replyTargetCmid('2000000007'), 514, 'цель отдаётся своей беседе');

    // ГЛАВНОЕ: тот же cmid 514 существует в КАЖДОЙ беседе VK.
    eq(R.replyTarget('1234'), null, 'в другой беседе цель невидима');
    eq(R.replyTargetCmid('1234'), null, 'и ответ туда не уйдёт');
    eq(R.replyTarget(2000000007), target, 'числовой id беседы — та же беседа');
    eq(R.replyTargetCmid(null), null, 'без беседы цели нет');

    eq(R.clearReplyTarget(), true, 'снятие сообщает, что цель была');
    eq(R.clearReplyTarget(), false, 'повторное снятие идемпотентно');

    // Мусор целью не становится.
    eq(R.setReplyTarget({ dialogId: '1', cmid: null }), null, 'цель без cmid не сохраняется');
    eq(R.setReplyTarget({ cmid: 5 }), null, 'цель без беседы не сохраняется');
    eq(R.setReplyTarget(null), null, 'null очищает хранилище');

    R.setReplyTarget(target);
    eq(R._debugReplyState(), {
        dialogId: '2000000007', cmid: 514, messageId: 7003, name: 'durov', preview: 'это ответ',
    }, 'снимок состояния для диагностики');
    R.clearReplyTarget();
}

// ---------------------------------------------------------------------------
// 7. Локальная досборка превью из ленты
// ---------------------------------------------------------------------------

{
    const bare = R.normalizeReplyTo({ reply_message: { conversation_message_id: 513, attachments: [] } });
    ok(R.replyNeedsPreview(bare), 'урезанная цитата помечена «нужно досбирать»');

    const original = message({ id: 7002, cmid: 513, text: 'оригинал', sender: { id: '1234', screen_name: 'durov' } });
    const merged = R.mergeReplyFromMessage(bare, original, DIALOG);
    eq(merged.text, 'оригинал', 'превью досбрано из уже загруженной ленты — без единого запроса');
    eq(merged.cmid, 513, 'ключ цитаты сохранён');
    ok(!R.replyNeedsPreview(merged), 'после досборки добирать больше нечего');

    // Чужой cmid игнорируется — защита от подстановки постороннего текста.
    const foreign = R.mergeReplyFromMessage(bare, message({ cmid: 999, text: 'чужое' }), DIALOG);
    eq(foreign.text, null, 'оригинал с чужим cmid в цитату не попадает');

    // Готовая цитата досборке не подлежит.
    const full = R.normalizeReplyTo(message({ reply_to: replyDto() }));
    eq(R.replyNeedsPreview(full), false, 'полная цитата уже собрана (дозапроса в VK нет вовсе)');
    const gone = R.normalizeReplyTo(message({ reply_to: replyDto({ unavailable: true }) }));
    eq(R.replyNeedsPreview(gone), false, 'недоступный оригинал добирать бессмысленно');
    eq(R.mergeReplyFromMessage(null, original, DIALOG), null, 'без цитаты досборка не падает');
    eq(R.mergeReplyFromMessage(bare, null, DIALOG), bare, 'без оригинала цитата не меняется');
}

// ---------------------------------------------------------------------------
// 8. Строка «оригинал вне загруженной страницы»
// ---------------------------------------------------------------------------

{
    const line = R.replySourceLine(message({ cmid: 500, text: 'старое' }), DIALOG, SELF);
    eq(line, '#500 durov: старое', 'в шапке строки — cmid, им адресуется ответ');
    const own = R.replySourceLine(message({ cmid: 501, out: true, text: 'моё' }), DIALOG, SELF);
    eq(own, '#501 you@local: моё', 'своё сообщение подписано you@local');
    const media = R.replySourceLine(
        message({ cmid: 502, text: '', attachments: [{ index: 0, kind: 'voice' }] }), DIALOG, SELF,
    );
    eq(media, '#502 durov: [voice]', 'сообщение без текста называет вложение');
    eq(R.replySourceLine(null, DIALOG, SELF), '', 'без сообщения строки нет');
}

// ---------------------------------------------------------------------------
// 9. Классификация клика по строке
// ---------------------------------------------------------------------------

{
    /** Минимальный узел с `closest` — ровно то, что нужно функции. */
    function node(chain) {
        return {
            closest(sel) {
                return chain.includes(sel) ? { tag: sel } : null;
            },
        };
    }

    eq(R.classifyRowClick(node(['.vk-line'])), 'reply', 'двойной клик по строке — ответ');
    eq(R.classifyRowClick(node(['.vk-line', '.vk-media-card'])), 'ignore',
        'карточка медиа открывается одинарным кликом — ответ перехватывать нельзя');
    eq(R.classifyRowClick(node(['.vk-line', '.vk-media-player'])), 'ignore',
        'ползунок плеера ретаргетит события на <video>');
    eq(R.classifyRowClick(node(['.vk-line', '.vk-kb'])), 'ignore', 'кнопка сообщества');
    eq(R.classifyRowClick(node(['.vk-line', '.vk-reply-quote'])), 'ignore',
        'у цитаты своё действие — переход к оригиналу');
    eq(R.classifyRowClick(node(['.vk-line', 'button'])), 'ignore', 'любая кнопка строки');
    eq(R.classifyRowClick(node(['.vk-line', 'a[href]'])), 'ignore', 'ссылка');
    eq(R.classifyRowClick(node([])), 'ignore', 'клик мимо строк (спейсер, разделитель дат)');
    eq(R.classifyRowClick(null), 'ignore', 'без цели клика — ignore');
    eq(R.classifyRowClick({}), 'ignore', 'узел без closest — ignore');
}

console.log(`vk_reply: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
