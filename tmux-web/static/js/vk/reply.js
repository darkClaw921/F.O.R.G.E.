// tmux-web — VK plugin: ответы на сообщения (reply) — ЧИСТАЯ логика.
//
// Модуль намеренно не трогает DOM и не ходит в сеть: из зависимостей — только
// такие же чистые format.js и attachments.js. Благодаря этому он грузится в
// Node как есть и покрыт tmux-web/tests/frontend/vk_reply.test.mjs, а chat.js
// остаётся тонким рисовальщиком.
//
// ---------------------------------------------------------------------------
// ПОЧЕМУ ЭТО КОПИЯ telegram/reply.js, А НЕ ОБЩИЙ МОДУЛЬ
// ---------------------------------------------------------------------------
//
// Ключ цели ответа у VK — `conversation_message_id` (cmid), а не `message_id`.
// Это НЕ параметризация «возьми поле по имени», а другой инвариант:
//
//  * `message.id` у VK сквозной по аккаунту, но адресовать им ответ в БЕСЕДЕ
//    нельзя — метод `messages.send` принимает cmid внутри `forward` с
//    `is_reply`, а `reply_to` понимает только глобальный id;
//  * `cmid` нумеруется В КАЖДОЙ беседе с единицы, поэтому один и тот же cmid
//    в двух беседах — это два РАЗНЫХ сообщения. Любая функция, сравнивающая
//    цели, обязана сверять беседу явно (см. [`replyTarget`]);
//  * у оригинала, приехавшего инлайном в `reply_message`, собственного
//    `message.id` может не быть вовсе — VK его туда просто не кладёт.
//
// Общий модуль с флагом «какое поле считать ключом» пришлось бы читать в двух
// режимах сразу, а ошибка в выборе режима стоила бы ответа не в ту ветку.
//
// Здесь живут три вещи:
//
// 1. НОРМАЛИЗАЦИЯ ЦИТАТЫ — сервер отдаёт `MessageDto.reply_to` (собранный из
//    инлайненного `reply_message`, БЕЗ дозапроса), но кадр WS от старой
//    версии плагина мог принести сырую форму VK `reply_message`, а совсем
//    старый ответ — одно голое `reply_to_id`. Все три формы сводятся к одному
//    виду, чтобы рисовальщик не разбирал варианты сам.
// 2. СОСТОЯНИЕ «на что отвечаем» — ровно одна цель на всю панель, привязанная
//    к беседе. Смена беседы обесценивает цель автоматически (см.
//    [`replyTarget`]): хранить её в беседе было бы честнее, но полоса ответа
//    в UI одна, и две «активные цели» разъехались бы с ней. Хранилище —
//    СВОЙ модульный синглтон панели VK (у Telegram свой; общий дал бы
//    коллизию ключей ровно так же, как в follow-core.js).
// 3. КЛАССИФИКАЦИЯ КЛИКА — двойной клик по строке начинает ответ, но внутри
//    строки живут чужие интерактивные элементы (плеер, кнопки бота, ссылки,
//    медиа-карточка), и перехват их кликов ломал бы их работу.

import { attachmentKind } from './attachments.js';
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
 * У VK такое встречается редко (оригинал инлайнится в само сообщение), но
 * встречается: кадр WS, собранный предыдущей версией плагина, приносит один
 * `reply_to_id`. Пустая цитата выглядела бы как баг вёрстки, многоточие — как
 * «сейчас подгрузим».
 */
export const REPLY_PENDING_TEXT = '…';

/**
 * Селекторы, внутри которых двойной клик НЕ означает «ответить».
 *
 * Список именно из селекторов, а не одна склейка: так [`classifyRowClick`]
 * проверяется по одному правилу за раз и покрывается тестами без DOM.
 *
 * Почему каждый пункт:
 * - `.vk-media-player` — нативные controls ретаргетят события на сам
 *   `<video>`; двойной клик по ползунку перемотки не должен открывать ответ;
 * - `.vk-media-card` — карточка открывает медиа ОДИНАРНЫМ кликом, поэтому
 *   двойной уже запустил лайтбокс/плеер дважды, и третьим действием ещё и
 *   поймать ответ — перебор;
 * - `.vk-kb` — кнопки сообщества отправляют сообщение на каждый клик;
 * - `.vk-reply-quote` — у цитаты своё действие «перейти к оригиналу»;
 * - `button` / `a[href]` — любой прочий интерактив строки (в том числе кнопка
 *   «ответить», у которой есть собственный обработчик).
 */
export const REPLY_CLICK_IGNORE = [
    '.vk-media-player',
    '.vk-media-card',
    '.vk-kb',
    '.vk-reply-quote',
    'button',
    'a[href]',
];

/** Приводит значение к непустой строке или null. */
function str(value) {
    if (typeof value !== 'string') return null;
    const s = value.trim();
    return s ? s : null;
}

/**
 * Положительный `conversation_message_id` или null.
 *
 * Именно эта функция и есть точка расхождения с телеграмной копией: там на её
 * месте стоит `message_id`. Ноль у VK означает «cmid не заполнен» (сообщение
 * из личного диалога, доехавшее урезанным кадром), а не «сообщение номер ноль».
 */
function cmidValue(value) {
    const n = Number(value);
    return Number.isFinite(n) && n > 0 ? n : null;
}

/** Положительный глобальный `message.id` или null (едет рядом, ключом не служит). */
function msgIdValue(value) {
    const n = Number(value);
    return Number.isFinite(n) && n > 0 ? n : null;
}

/**
 * Обрезка превью с многоточием ПО CODE POINTS.
 *
 * Три обязательных свойства (те же, что у серверного `truncate_chars`):
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
 * Словарь берётся у `attachments.js::attachmentKind`, а не переписывается
 * здесь: цитата обязана называть вложение так же, как называет его сама
 * строка оригинала, иначе одно сообщение выглядит как два разных. Заодно
 * бесплатно поддерживаются СЫРЫЕ имена типов VK (`audio_message`, `doc`),
 * которые приезжают в кадрах WS старой версии.
 *
 * Неизвестный вид даёт нейтральное `[media]`, а не `[unsupported]`: цитата —
 * это одна строка, и служебное слово в ней сообщало бы пользователю о наших
 * внутренних делах вместо содержимого сообщения.
 *
 * @param {*} kind
 * @returns {string}
 */
export function replyMediaMark(kind) {
    const k = attachmentKind(kind);
    return `[${k === 'unsupported' ? 'media' : k}]`;
}

/**
 * Приводит `MessageDto` к описанию цитаты или `null`, если это не ответ.
 *
 * Поддержаны все три формы, в которых ответ доезжает до фронта:
 *
 * 1. `reply_to` — серверный `ReplyToDto`, собранный из инлайненного
 *    `reply_message` (основной путь: и история, и кадры LongPoll, и ответ на
 *    отправку — везде превью уже готово, дозапроса в плагине нет вовсе);
 * 2. `reply_message` — СЫРАЯ форма VK. Так выглядит кадр, собранный
 *    предыдущей версией плагина и пролежавший в кэше вкладки;
 * 3. только `reply_to_id` — совместимость с сервером без нового поля.
 *
 * @param {?object} msg — MessageDto
 * @returns {?{cmid: ?number, messageId: ?number, sender: ?string,
 *   senderId: ?string, text: ?string, mediaKind: ?string, unavailable: boolean}}
 */
export function normalizeReplyTo(msg) {
    if (!msg || typeof msg !== 'object') return null;

    const dto = msg.reply_to;
    if (dto && typeof dto === 'object') {
        return {
            cmid: cmidValue(dto.cmid),
            messageId: msgIdValue(dto.message_id),
            sender: str(dto.sender),
            senderId: dto.sender_id == null ? null : str(String(dto.sender_id)),
            text: str(dto.text),
            mediaKind: str(dto.media_kind),
            unavailable: dto.unavailable === true,
        };
    }

    // Сырая форма VK: те же данные, но под именами API.
    const raw = msg.reply_message;
    if (raw && typeof raw === 'object') {
        const attachments = Array.isArray(raw.attachments) ? raw.attachments : [];
        const first = attachments.length ? attachments[0] : null;
        const text = str(raw.text);
        return {
            cmid: cmidValue(raw.conversation_message_id),
            messageId: msgIdValue(raw.id),
            sender: null,
            senderId: raw.from_id == null ? null : str(String(raw.from_id)),
            text: text === null ? null : truncatePreview(text, REPLY_PREVIEW_CHARS),
            mediaKind: first ? attachmentKind(first) : null,
            // Пустой во всех отношениях объект — это «оригинал недоступен»
            // (удалён либо скрыт приватностью), а не «сообщение без текста».
            unavailable: !text && !first && !cmidValue(raw.conversation_message_id),
        };
    }

    const id = msgIdValue(msg.reply_to_id);
    if (id === null) return null;
    return {
        cmid: null,
        messageId: id,
        sender: null,
        senderId: null,
        text: null,
        mediaKind: null,
        unavailable: false,
    };
}

/**
 * Нужно ли ещё досбирать превью оригинала.
 *
 * У VK превью приезжает ВМЕСТЕ с сообщением, поэтому «нужно» здесь означает
 * ровно один случай: цитата пришла урезанной (старый кадр из кэша вкладки), и
 * от неё есть только идентификатор. Признак — отсутствие ВСЕГО содержимого
 * сразу: одного лишь пустого `text` мало, у сообщения с единственным
 * вложением текста нет и не будет.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @returns {boolean}
 */
export function replyNeedsPreview(reply) {
    if (!reply || reply.unavailable) return false;
    if (reply.cmid === null && reply.messageId === null) return false;
    return !reply.text && !reply.mediaKind && !reply.sender;
}

/**
 * Имя автора оригинала для цитаты.
 *
 * Своё сообщение узнаётся по `senderId`, совпавшему с id аккаунта: у VK в
 * `reply_message` нет флага `out` — там только `from_id`, и без сверки
 * собственная цитата подписывалась бы числовым id.
 *
 * Недоступный оригинал без автора даёт ПУСТОЕ имя: рисовать «unknown» рядом с
 * «сообщение недоступно» — значит сообщать одно и то же дважды.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @param {?object} [dialog] — DialogDto для фолбэка (сообщения от имени
 *   сообщества приходят с отрицательным `from_id`)
 * @param {*} [selfId] — id своего аккаунта (`vkState.selfUser.id`)
 * @returns {string}
 */
export function replyAuthorLabel(reply, dialog, selfId) {
    if (!reply) return '';
    const self = selfId == null ? null : String(selfId);
    if (self && reply.senderId && reply.senderId === self) return nickOf({ out: true });
    if (reply.sender) return reply.sender;
    if (reply.unavailable) return '';
    if (dialog && dialog.title) return String(dialog.title);
    if (reply.senderId) return reply.senderId;
    return 'unknown';
}

/**
 * Превью оригинала одной строкой.
 *
 * ГЛАВНОЕ ОТЛИЧИЕ ОТ TELEGRAM: у VK текст цитируемого регулярно ПУСТ при
 * непустых вложениях (голосовое, стикер, фотография без подписи), и ветка
 * «есть вид вложения, текста нет» здесь не редкий край, а типовой случай.
 * Поэтому пометка вида идёт и вместо текста, и вместе с ним.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @returns {string}
 */
export function replyPreviewText(reply) {
    if (!reply) return '';
    if (reply.unavailable) return REPLY_GONE_TEXT;
    if (reply.text) {
        const text = truncatePreview(reply.text, REPLY_PREVIEW_CHARS);
        // Подпись под вложением — это подпись: без пометки вида цитата на
        // фотографию с текстом выглядит как цитата на обычное сообщение.
        return reply.mediaKind ? `${replyMediaMark(reply.mediaKind)} ${text}` : text;
    }
    if (reply.mediaKind) return replyMediaMark(reply.mediaKind);
    if (reply.cmid !== null || reply.messageId !== null) return REPLY_PENDING_TEXT;
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
 * @returns {?object} тот же вид, что у [`normalizeReplyTo`], плюс `out`
 */
export function replyFromMessage(msg, dialog) {
    if (!msg || typeof msg !== 'object') return null;
    const out = msg.out === true;
    const list = Array.isArray(msg.attachments) ? msg.attachments : [];
    const first = list.length ? list[0] : (msg.media || null);
    return {
        cmid: cmidValue(msg.cmid),
        messageId: msgIdValue(msg.id),
        // Своё сообщение автора не получает: его подставит replyAuthorLabel по
        // совпадению id, и «you@local» останется в одном месте
        // (format.js::nickOf).
        sender: out ? null : str(nickOf(msg, dialog)),
        senderId: msg.sender && msg.sender.id != null ? String(msg.sender.id) : null,
        text: str(msg.text),
        mediaKind: first ? attachmentKind(first) : null,
        unavailable: false,
        out,
    };
}

/**
 * Досбирает превью цитаты из ОРИГИНАЛА, уже лежащего в ленте.
 *
 * Это бесплатная замена запросу: урезанный кадр из кэша вкладки полностью
 * закрывается тем, что уже загружено.
 *
 * Оригинал с ЧУЖИМ cmid игнорируется — защита от подстановки постороннего
 * текста в цитату. Сверка идёт по cmid, а при его отсутствии (личный диалог,
 * урезанный кадр) — по глобальному id: подменить одно другим нельзя, иначе
 * сообщение №5 беседы подтянуло бы превью сообщения №5 другой.
 *
 * @param {?object} reply — результат [`normalizeReplyTo`]
 * @param {?object} original — MessageDto оригинала
 * @param {?object} [dialog]
 * @returns {?object} новый объект цитаты (входные не мутируются)
 */
export function mergeReplyFromMessage(reply, original, dialog) {
    if (!reply) return reply;
    const src = replyFromMessage(original, dialog);
    if (!src) return reply;
    const sameCmid = reply.cmid !== null && src.cmid === reply.cmid;
    const sameId = reply.cmid === null && reply.messageId !== null
        && src.messageId === reply.messageId;
    if (!sameCmid && !sameId) return reply;
    return {
        ...src,
        cmid: reply.cmid !== null ? reply.cmid : src.cmid,
        messageId: reply.messageId !== null ? reply.messageId : src.messageId,
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
 * @param {*} [selfId]
 * @returns {string}
 */
export function replySourceLine(msg, dialog, selfId) {
    const src = replyFromMessage(msg, dialog);
    if (!src) return '';
    const name = src.out === true ? nickOf({ out: true }) : replyAuthorLabel(src, dialog, selfId);
    const preview = replyPreviewText(src);
    // В шапке — cmid: именно им адресуется ответ, и именно его пользователь
    // видит в цитате.
    const head = src.cmid === null ? '' : `#${src.cmid} `;
    return `${head}${name ? name + ': ' : ''}${preview}`.trim();
}

/**
 * Цель ответа для полосы над полем ввода.
 *
 * `null`, если у сообщения нет подтверждённого cmid: optimistic-строка ещё в
 * пути, а VK не примет ответ на несуществующий идентификатор.
 *
 * @param {string} dialogId
 * @param {?object} msg — MessageDto, на который отвечаем
 * @param {?object} [dialog]
 * @param {*} [selfId]
 * @returns {?{dialogId: string, cmid: number, messageId: ?number, name: string,
 *   preview: string, out: boolean, reply: object}}
 */
export function buildReplyTarget(dialogId, msg, dialog, selfId) {
    const id = dialogId == null ? '' : String(dialogId);
    if (!id) return null;
    const reply = replyFromMessage(msg, dialog);
    if (!reply || reply.cmid === null) return null;
    return {
        dialogId: id,
        cmid: reply.cmid,
        messageId: reply.messageId,
        name: reply.out === true ? nickOf({ out: true }) : replyAuthorLabel(reply, dialog, selfId),
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
 * как сбой. Форма — ровно серверный `ReplyToDto`, чтобы строка, нарисованная
 * до ответа, и строка, пересобранная после него, разбирались одним кодом.
 *
 * @param {?object} target — результат [`buildReplyTarget`]
 * @returns {?object}
 */
export function replyDtoFromTarget(target) {
    if (!target || !Number.isFinite(target.cmid)) return null;
    const src = target.reply || {};
    return {
        message_id: src.messageId == null ? null : src.messageId,
        cmid: target.cmid,
        sender: src.sender || null,
        sender_id: src.senderId || null,
        text: src.text || null,
        media_kind: src.mediaKind || null,
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
    state.target = (target && target.dialogId && Number.isFinite(target.cmid))
        ? target
        : null;
    return state.target;
}

/**
 * Цель ответа ДЛЯ УКАЗАННОЙ беседы.
 *
 * Проверка беседы — это и есть сброс при переключении, и у VK она обязательна
 * вдвойне: cmid нумеруется в каждой беседе с единицы, поэтому цель «cmid 514»
 * без беседы указывала бы разом на 514-е сообщение КАЖДОГО чата.
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
 * `reply_to_cmid` для отправки в активной беседе.
 *
 * @param {*} dialogId
 * @returns {?number}
 */
export function replyTargetCmid(dialogId) {
    const t = replyTarget(dialogId);
    return t ? t.cmid : null;
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
 * node-тестами.
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
    return target.closest('.vk-line') ? 'reply' : 'ignore';
}

/** Снимок состояния для e2e/диагностики. */
export function _debugReplyState() {
    const t = state.target;
    return {
        dialogId: t ? t.dialogId : null,
        cmid: t ? t.cmid : null,
        messageId: t ? t.messageId : null,
        name: t ? t.name : null,
        preview: t ? t.preview : null,
    };
}
