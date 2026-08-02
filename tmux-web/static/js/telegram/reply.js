// tmux-web — Telegram plugin: ответы на сообщения (reply) — ЧИСТАЯ логика.
//
// Модуль намеренно не трогает DOM и не ходит в сеть: из зависимостей — только
// такой же чистый format.js. Благодаря этому он грузится в Node как есть и
// покрыт регресс-тестом tmux-web/tests/frontend/telegram_reply.test.mjs, а
// chat.js остаётся тонким рисовальщиком.
//
// Здесь живут три вещи:
//
// 1. НОРМАЛИЗАЦИЯ `MessageDto.reply_to` — сервер отдаёт объект с восемью
//    полями, но у сообщений из потока апдейтов превью ещё нет (там только
//    `message_id`), а совсем старый ответ мог принести одно голое
//    `reply_to_id`. Все три формы сводятся к одному виду, чтобы рисовальщик
//    не разбирал варианты сам.
// 2. СОСТОЯНИЕ «на что отвечаем» — ровно одна цель на всю вкладку, привязанная
//    к диалогу. Смена диалога обесценивает цель автоматически (см.
//    [`replyTarget`]): хранить её в диалоге было бы честнее, но полоса ответа
//    в UI одна, и две «активные цели» разъехались бы с ней.
// 3. КЛАССИФИКАЦИЯ КЛИКА — двойной клик по строке начинает ответ, но внутри
//    строки живут чужие интерактивные элементы (плеер, кнопки бота, ссылки,
//    медиа-карточка), и перехват их кликов ломал бы их работу.

import { nickOf } from './format.js';

/**
 * Потолок превью в code points — совпадает с серверным
 * `dto::REPLY_PREVIEW_CHARS`.
 *
 * Сервер уже обрезал текст оригинала до этого предела, но фронт обрезает
 * повторно: превью собирается и локально (из уже загруженной ленты, см.
 * [`mergeReplyFromMessage`]), и там ограничения сервера не действуют.
 */
export const REPLY_PREVIEW_CHARS = 140;

/** Что показывать вместо превью, если оригинал недоступен. */
export const REPLY_GONE_TEXT = 'сообщение недоступно';

/**
 * Заглушка «превью ещё не приехало».
 *
 * Поток апдейтов сознательно не ходит в сеть за оригиналом (иначе пачка
 * ответов в активном чате дала бы RPC на каждое сообщение), поэтому у только
 * что пришедшего ответа известен лишь `message_id`. Пустая цитата выглядела бы
 * как баг вёрстки, многоточие — как «сейчас подгрузим».
 */
export const REPLY_PENDING_TEXT = '…';

/**
 * Селекторы, внутри которых двойной клик НЕ означает «ответить».
 *
 * Список именно из селекторов, а не одна склейка: так [`classifyRowClick`]
 * проверяется по одному правилу за раз и покрывается тестами без DOM.
 *
 * Почему каждый пункт:
 * - `.tg-media-player` — нативные controls ретаргетят события на сам
 *   `<video>`; двойной клик по ползунку перемотки не должен открывать ответ;
 * - `.tg-media-card` — карточка открывает медиа ОДИНАРНЫМ кликом, поэтому
 *   двойной уже запустил лайтбокс/плеер дважды, и третьим действием ещё и
 *   поймать ответ — перебор;
 * - `.tg-kb` — кнопки бота отправляют callback на каждый клик;
 * - `.tg-reply-quote` — у цитаты своё действие «перейти к оригиналу»;
 * - `button` / `a[href]` — любой прочий интерактив строки (в том числе кнопка
 *   «ответить», у которой есть собственный обработчик).
 */
export const REPLY_CLICK_IGNORE = [
    '.tg-media-player',
    '.tg-media-card',
    '.tg-kb',
    '.tg-reply-quote',
    'button',
    'a[href]',
];

/** Приводит значение к непустой строке или null. */
function str(value) {
    if (typeof value !== 'string') return null;
    const s = value.trim();
    return s ? s : null;
}

/** Положительный message_id или null. */
function msgId(value) {
    return Number.isFinite(value) && value > 0 ? value : null;
}

/**
 * Обрезка превью с многоточием ПО CODE POINTS.
 *
 * Три обязательных свойства (те же, что у серверного `reply_preview_text`):
 *
 * - режем по code points, а не по UTF-16-юнитам: `slice()` разрубает
 *   суррогатную пару, и от emoji остаётся одинокий «�»;
 * - любые пробелы и переводы строк схлопываются в один пробел — иначе
 *   многострочный оригинал раздул бы строку ленты на пол-экрана;
 * - многоточие ВХОДИТ в лимит, поэтому результат никогда не длиннее `max`.
 *
 * @param {*} text
 * @param {number} [max=REPLY_PREVIEW_CHARS]
 * @returns {string} пустая строка, если текста нет
 */
export function truncatePreview(text, max) {
    const s = String(text == null ? '' : text).replace(/\s+/g, ' ').trim();
    if (!s) return '';
    const n = Math.max(1, Number(max) || REPLY_PREVIEW_CHARS);
    const points = Array.from(s);
    if (points.length <= n) return s;
    return points.slice(0, n - 1).join('') + '…';
}

/**
 * Пометка вида вложения для цитаты: `[photo]`, `[voice]`.
 *
 * Словарь тот же, что у `MediaDto.kind` и у подписи строки лога
 * (`chat.js::messageText`): цитата обязана называть вложение так же, как
 * называет его сама строка оригинала, иначе одно сообщение выглядит как два
 * разных.
 *
 * @param {*} kind
 * @returns {string}
 */
export function replyMediaMark(kind) {
    const k = String(kind == null ? '' : kind).toLowerCase();
    const known = ['photo', 'video', 'document', 'sticker', 'voice', 'audio'];
    return `[${known.includes(k) ? k : 'media'}]`;
}

/**
 * Приводит `MessageDto` к описанию цитаты или `null`, если это не ответ.
 *
 * Поддержаны все три формы, в которых ответ доезжает до фронта:
 *
 * 1. полный `reply_to` с превью (история, ответ на отправку);
 * 2. `reply_to` с одним `message_id` (поток апдейтов — сеть там запрещена);
 * 3. только `reply_to_id` (совместимость: закешированный фронт против сервера
 *    без нового поля).
 *
 * `reply_to_id` НЕ используется, когда пришёл `reply_to`: серверное поле
 * сознательно расходится со старым на форум-топиках (там Telegram кладёт id
 * топика в reply_to_msg_id у КАЖДОГО сообщения, и старое поле рисовало бы
 * цитату под всей супергруппой).
 *
 * @param {?object} msg — MessageDto
 * @returns {?{messageId: ?number, sender: ?string, senderId: ?string,
 *   text: ?string, mediaKind: ?string, quote: ?string, out: ?boolean,
 *   unavailable: boolean}}
 */
export function normalizeReplyTo(msg) {
    const raw = msg && typeof msg === 'object' ? msg.reply_to : null;
    if (raw && typeof raw === 'object') {
        return {
            messageId: msgId(raw.message_id),
            sender: str(raw.sender),
            senderId: str(raw.sender_id),
            text: str(raw.text),
            mediaKind: str(raw.media_kind),
            quote: str(raw.quote),
            out: typeof raw.out === 'boolean' ? raw.out : null,
            unavailable: raw.unavailable === true,
        };
    }
    const id = msgId(msg && msg.reply_to_id);
    if (id === null) return null;
    return {
        messageId: id,
        sender: null,
        senderId: null,
        text: null,
        mediaKind: null,
        quote: null,
        out: null,
        unavailable: false,
    };
}

/**
 * Нужно ли ещё добирать превью оригинала.
 *
 * Признак тот же, что на сервере (`dto::reply_needs_preview`): `out` заполняет
 * ТОЛЬКО обогащение, поэтому его отсутствие при известном id и означает «текст
 * и автор ещё не приехали». Проверять по `text` нельзя — у сообщения с одним
 * лишь вложением текста нет и не будет.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @returns {boolean}
 */
export function replyNeedsPreview(reply) {
    if (!reply) return false;
    return reply.messageId !== null && !reply.unavailable && reply.out === null;
}

/**
 * Имя автора оригинала для цитаты.
 *
 * Опора на `out`, а не только на `sender`, обязательна: у СВОИХ сообщений в
 * личке Telegram не присылает отправителя вовсе (с layer 119 `from_id` в
 * личных чатах отсутствует), и без этой ветки собственная цитата осталась бы
 * безымянной.
 *
 * Недоступный оригинал без автора даёт ПУСТОЕ имя: рисовать «unknown» рядом с
 * «сообщение недоступно» — значит сообщать одно и то же дважды.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @param {?object} [dialog] — DialogDto для фолбэка (посты от имени канала)
 * @returns {string}
 */
export function replyAuthorLabel(reply, dialog) {
    if (!reply) return '';
    if (reply.out === true) return nickOf({ out: true });
    if (reply.sender) return reply.sender;
    if (reply.unavailable) return '';
    if (dialog && dialog.title) return String(dialog.title);
    return 'unknown';
}

/**
 * Превью оригинала одной строкой.
 *
 * Приоритет намеренно ставит цитату фрагмента выше всего: если собеседник
 * выделил кусок сообщения, отвечает он именно на него, и показывать вместо
 * этого начало полного текста — терять смысл переписки. По той же причине
 * фрагмент показывается и у УДАЛЁННОГО оригинала: он приходит вместе с самим
 * ответом, а не с оригиналом, и остаётся единственным, что вообще можно
 * показать (так же ведут себя официальные клиенты).
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @returns {string}
 */
export function replyPreviewText(reply) {
    if (!reply) return '';
    if (reply.quote) return truncatePreview(reply.quote, REPLY_PREVIEW_CHARS);
    if (reply.unavailable) return REPLY_GONE_TEXT;
    if (reply.text) {
        const text = truncatePreview(reply.text, REPLY_PREVIEW_CHARS);
        // Подпись под вложением — это подпись: без пометки вида цитата на
        // фотографию с текстом выглядит как цитата на обычное сообщение.
        return reply.mediaKind ? `${replyMediaMark(reply.mediaKind)} ${text}` : text;
    }
    if (reply.mediaKind) return replyMediaMark(reply.mediaKind);
    if (reply.messageId !== null) return REPLY_PENDING_TEXT;
    return '';
}

/**
 * Описание сообщения В КАЧЕСТВЕ ОРИГИНАЛА цитаты.
 *
 * Нужно там, где оригинал уже есть на руках: выбор цели ответа
 * ([`buildReplyTarget`]) и локальная досборка превью из загруженной ленты
 * ([`mergeReplyFromMessage`]).
 *
 * @param {?object} msg — MessageDto
 * @param {?object} [dialog] — DialogDto для фолбэка ника
 * @returns {?object} тот же вид, что у [`normalizeReplyTo`]
 */
export function replyFromMessage(msg, dialog) {
    if (!msg || typeof msg !== 'object') return null;
    const out = msg.out === true;
    const media = msg.media && typeof msg.media === 'object' ? msg.media : null;
    return {
        messageId: msgId(msg.id),
        // Своё сообщение автора не получает: его подставит replyAuthorLabel по
        // `out`, и «you@local» останется в одном месте (format.js::nickOf).
        sender: out ? null : str(nickOf(msg, dialog)),
        senderId: msg.sender && msg.sender.id ? String(msg.sender.id) : null,
        text: str(msg.text),
        mediaKind: media ? str(media.kind) : null,
        quote: null,
        out,
        unavailable: false,
    };
}

/**
 * Досбирает превью цитаты из ОРИГИНАЛА, уже лежащего в ленте.
 *
 * Это бесплатная замена запросу: типовой случай «ответили соседу в открытом
 * чате» полностью закрывается тем, что уже загружено, и ходить за этим в
 * Telegram незачем.
 *
 * Оригинал с ЧУЖИМ id игнорируется — защита от подстановки постороннего
 * текста в цитату (то же правило, что у серверного `enrich_reply_preview`).
 * `quote` оригиналом не перетирается: фрагмент принадлежит ответу.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @param {?object} original — MessageDto оригинала
 * @param {?object} [dialog]
 * @returns {?object} новый объект цитаты (входные не мутируются)
 */
export function mergeReplyFromMessage(reply, original, dialog) {
    if (!reply) return reply;
    const src = replyFromMessage(original, dialog);
    if (!src || src.messageId === null || src.messageId !== reply.messageId) return reply;
    return {
        ...src,
        messageId: reply.messageId,
        quote: reply.quote,
        unavailable: false,
    };
}

/**
 * Строка «нашли оригинал, но он вне ленты» для системной строки лога.
 *
 * Показывается, когда переход к оригиналу не удался прокруткой (сообщение
 * старше загруженной истории), но сервер его отдал: пользователь всё равно
 * узнаёт, на что был ответ, — это лучше, чем «не найдено».
 *
 * @param {?object} msg — MessageDto оригинала
 * @param {?object} [dialog]
 * @returns {string}
 */
export function replySourceLine(msg, dialog) {
    const src = replyFromMessage(msg, dialog);
    if (!src) return '';
    const name = replyAuthorLabel(src, dialog);
    const preview = replyPreviewText(src);
    const head = src.messageId === null ? '' : `#${src.messageId} `;
    return `${head}${name ? name + ': ' : ''}${preview}`.trim();
}

/**
 * Цель ответа для полосы над полем ввода.
 *
 * @param {string} dialogId
 * @param {?object} msg — MessageDto, на который отвечаем
 * @param {?object} [dialog]
 * @returns {?{dialogId: string, messageId: number, name: string,
 *   preview: string, out: boolean, reply: object}}
 *   `null`, если у сообщения нет подтверждённого id (optimistic-строка ещё в
 *   пути — Telegram не примет ответ на несуществующий id).
 */
export function buildReplyTarget(dialogId, msg, dialog) {
    const id = dialogId == null ? '' : String(dialogId);
    if (!id) return null;
    const reply = replyFromMessage(msg, dialog);
    if (!reply || reply.messageId === null) return null;
    return {
        dialogId: id,
        messageId: reply.messageId,
        name: replyAuthorLabel(reply, dialog),
        preview: replyPreviewText(reply),
        out: reply.out === true,
        reply,
    };
}

/**
 * `reply_to` для ОПТИМИСТИЧНОЙ строки своего ответа.
 *
 * Цитата обязана появиться сразу вместе со строкой: если ждать подтверждения
 * сервера, подпись «отвечаю Васе» мигнёт в момент reconcile, и это читается
 * как сбой.
 *
 * @param {?object} target — результат [`buildReplyTarget`]
 * @returns {?object} объект в формате серверного `MessageDto.reply_to`
 */
export function replyDtoFromTarget(target) {
    if (!target || !Number.isFinite(target.messageId)) return null;
    const src = target.reply || {};
    return {
        message_id: target.messageId,
        sender: src.sender || null,
        sender_id: src.senderId || null,
        text: src.text || null,
        media_kind: src.mediaKind || null,
        quote: null,
        out: target.out === true,
        unavailable: false,
    };
}

// ---------------------------------------------------------------------------
// Состояние «на что отвечаем»
// ---------------------------------------------------------------------------

const state = {
    /** Единственная цель ответа или null (см. шапку модуля). */
    target: null,
};

/**
 * Запоминает цель ответа.
 *
 * @param {?object} target — результат [`buildReplyTarget`]
 * @returns {?object} сохранённая цель
 */
export function setReplyTarget(target) {
    state.target = (target && target.dialogId && Number.isFinite(target.messageId))
        ? target
        : null;
    return state.target;
}

/**
 * Цель ответа ДЛЯ УКАЗАННОГО диалога.
 *
 * Проверка диалога — это и есть сброс при переключении: цель, выбранная в
 * другом чате, невидима, и ответ туда не уйдёт даже если полосу забыли убрать.
 *
 * @param {*} dialogId
 * @returns {?object}
 */
export function replyTarget(dialogId) {
    const t = state.target;
    if (!t || dialogId == null) return null;
    return String(dialogId) === t.dialogId ? t : null;
}

/**
 * `reply_to_id` для отправки в активном диалоге.
 *
 * @param {*} dialogId
 * @returns {?number}
 */
export function replyTargetId(dialogId) {
    const t = replyTarget(dialogId);
    return t ? t.messageId : null;
}

/**
 * Снимает цель ответа.
 *
 * @returns {boolean} была ли цель (чтобы вызывающий знал, гасить ли Esc)
 */
export function clearReplyTarget() {
    const had = !!state.target;
    state.target = null;
    return had;
}

// ---------------------------------------------------------------------------
// Классификация клика по строке
// ---------------------------------------------------------------------------

/**
 * Что означает двойной клик по ленте: `'reply'` или `'ignore'`.
 *
 * От DOM нужен только `closest` — поэтому функция чистая и покрыта
 * node-тестами (тот же приём, что у `media.js::isPlayerClick`).
 *
 * @param {?EventTarget} target — `e.target`
 * @returns {'reply'|'ignore'}
 */
export function classifyRowClick(target) {
    if (!target || typeof target.closest !== 'function') return 'ignore';
    for (const sel of REPLY_CLICK_IGNORE) {
        if (target.closest(sel)) return 'ignore';
    }
    // Двойной клик мимо строк (по спейсеру, по разделителю дат) — не ответ.
    return target.closest('.tg-line') ? 'reply' : 'ignore';
}

/** Снимок состояния для e2e/диагностики. */
export function _debugReplyState() {
    const t = state.target;
    return {
        dialogId: t ? t.dialogId : null,
        messageId: t ? t.messageId : null,
        name: t ? t.name : null,
        preview: t ? t.preview : null,
    };
}
