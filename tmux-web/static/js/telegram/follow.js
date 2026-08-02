// tmux-web — Telegram plugin: «следование» за сообщением бота — ЧИСТАЯ логика.
//
// Модуль не трогает DOM и не ходит в сеть — ровно по образцу reply.js: за счёт
// этого он грузится в Node как есть и покрыт tests/frontend/telegram_follow.test.mjs,
// а chat.js остаётся тонким рисовальщиком, который только применяет решение.
//
// ЗАЧЕМ ЭТО ВООБЩЕ НУЖНО. Типовой сценарий инлайн-кнопок: пользователь жмёт
// кнопку → бот РЕДАКТИРУЕТ своё же сообщение (текст стал длиннее, клавиатура —
// на два ряда больше) или присылает новое. Строка в ленте пересобирается, но
// лента остаётся на месте, и выросший блок уезжает под нижнюю кромку. Обычного
// stick-to-bottom тут не хватает по двум причинам:
//
//  * нажатие кнопки — ЯВНЫЙ интент: за результатом своего нажатия пользователь
//    должен уехать, даже если чуть отлистал вверх (а он почти всегда отлистал:
//    полоса клавиатуры бота над полем ввода сама съедает высоту ленты, и
//    `isAtBottom` перестаёт быть истинным ровно в момент появления кнопок);
//  * рост бывает ОТЛОЖЕННЫМ — картинка догружается, шрифт подменяется,
//    клавиатура перерисовывается, — и одного доскролла в момент замены строки
//    не хватает.
//
// Поэтому нажатие включает «следование» за парой (диалог, message_id) на
// короткий срок. Пока оно живо, апдейты этого сообщения и новые сообщения в
// этом же диалоге тянут ленту к концу блока. Пассивное чтение истории при этом
// не ломается: без следования работает прежнее правило `isAtBottom`.

/**
 * Сколько живёт следование после нажатия кнопки, мс.
 *
 * Верхняя граница здравого смысла: бот отвечает за секунды, а вот через полминуты
 * «само уехало вниз» читалось бы уже как самовольная прокрутка. 12с покрывают и
 * медленного бота, и догрузку картинки по слабой сети, но не переживают паузу,
 * за которую пользователь успевает переключиться на чтение истории.
 */
export const FOLLOW_TTL_MS = 12000;

/**
 * Зазор под нижним краем блока при доскролле, px.
 *
 * Строка вплотную к кромке читается как «обрезано снизу», хотя обрезано ничего
 * не было. Значение отсекается потолком прокрутки, поэтому у последнего
 * сообщения ленты оно ни на что не влияет.
 */
export const FOLLOW_SCROLL_PAD = 8;

/**
 * На сколько px нужно уехать ВВЕРХ, чтобы следование отменилось.
 *
 * Порог, а не любое уменьшение: инерционная прокрутка и «подпрыгивание» ленты
 * при смене высоты полосы клавиатуры дают отскок в единицы пикселей, и по нему
 * следование гасло бы сразу после включения. Собственные доскроллы всегда идут
 * ВНИЗ, поэтому под этот порог они не попадают в принципе.
 */
export const FOLLOW_CANCEL_UP_PX = 24;

const state = {
    /** Диалог, в котором нажали кнопку ('' ⇒ не следим). */
    dialogId: '',
    /** message_id сообщения с кнопкой (0 ⇒ следим за диалогом целиком). */
    messageId: 0,
    /** До какого момента следование живо, мс эпохи (0 ⇒ не следим). */
    untilMs: 0,
};

/**
 * Регистрирует следование за сообщением.
 *
 * Повторное нажатие ПРОДЛЕВАЕТ окно и перецеливает его: пользователь жмёт
 * кнопки пачкой («меню → раздел → страница 2»), и каждое нажатие — свежий
 * интент, а актуально всегда последнее.
 *
 * @param {*} dialogId
 * @param {*} messageId
 * @param {number} nowMs — Date.now() вызывающего (инъекция часов для тестов)
 * @param {number} [ttlMs=FOLLOW_TTL_MS]
 * @returns {boolean} включилось ли следование
 */
export function noteFollow(dialogId, messageId, nowMs, ttlMs) {
    const id = dialogId == null ? '' : String(dialogId);
    if (!id) return false;
    const now = Number.isFinite(nowMs) ? nowMs : 0;
    const ttl = Number.isFinite(ttlMs) && ttlMs > 0 ? ttlMs : FOLLOW_TTL_MS;
    state.dialogId = id;
    state.messageId = Number.isFinite(messageId) && messageId > 0 ? Number(messageId) : 0;
    state.untilMs = now + ttl;
    return true;
}

/** Снимает следование (идемпотентно). @returns {boolean} было ли оно */
export function clearFollow() {
    const had = !!state.untilMs;
    state.dialogId = '';
    state.messageId = 0;
    state.untilMs = 0;
    return had;
}

/**
 * Цель следования с учётом срока годности.
 *
 * Часы, прыгнувшие НАЗАД (коррекция NTP, спящий ноутбук), следование не рвут:
 * `now < untilMs` — это «ещё не истекло». Тот же выбор, что у
 * `format.js::toastAllowed`.
 *
 * @param {number} nowMs
 * @returns {?{dialogId: string, messageId: number}}
 */
export function followTarget(nowMs) {
    if (!state.untilMs) return null;
    const now = Number.isFinite(nowMs) ? nowMs : 0;
    if (now > state.untilMs) return null;
    return { dialogId: state.dialogId, messageId: state.messageId };
}

/**
 * Следим ли сейчас за этим диалогом (любым его сообщением).
 *
 * @param {*} dialogId
 * @param {number} nowMs
 * @returns {boolean}
 */
export function isFollowing(dialogId, nowMs) {
    const t = followTarget(nowMs);
    if (!t || dialogId == null) return false;
    return String(dialogId) === t.dialogId;
}

/**
 * Следим ли именно за этим сообщением.
 *
 * Диалог сверяется ЯВНО: `chat.js::onEdited` находит строку только по
 * `message_id` и вообще не проверяет чат, а id сообщений в разных диалогах
 * пересекаются свободно — без этой сверки чужая правка дёргала бы ленту.
 *
 * @param {*} dialogId
 * @param {*} messageId
 * @param {number} nowMs
 * @returns {boolean}
 */
export function followsMessage(dialogId, messageId, nowMs) {
    if (!isFollowing(dialogId, nowMs)) return false;
    const t = followTarget(nowMs);
    // messageId == 0 ⇒ следим за диалогом целиком (кнопка reply-клавиатуры:
    // она принадлежит чату, а не конкретному сообщению).
    if (!t.messageId) return true;
    return Number(messageId) === t.messageId;
}

/**
 * Скроллить ли ленту после того, как строка выросла.
 *
 * @param {{atBottom: boolean, following: boolean}} ctx
 * @returns {'follow'|'stick'|'none'}
 *   `follow` — доскролл к концу блока (плавно), `stick` — прежнее прилипание к
 *   низу, `none` — не трогать ленту (пользователь читает историю).
 */
export function scrollDecision(ctx) {
    if (!ctx) return 'none';
    if (ctx.following === true) return 'follow';
    return ctx.atBottom === true ? 'stick' : 'none';
}

/**
 * Куда прокрутить ленту, чтобы КОНЕЦ блока строки оказался виден.
 *
 * @param {?{scrollTop: number, scrollHeight: number, clientHeight: number}} log
 * @param {?{top: number, bottom: number}} row — координаты строки ОТНОСИТЕЛЬНО
 *   верха видимой области ленты (`rowRect.top - logRect.top`). `null` ⇒
 *   раскладка неизвестна, тогда цель — самый низ.
 * @param {number} [pad=FOLLOW_SCROLL_PAD]
 * @returns {?number} новый scrollTop или `null`, если двигать не нужно
 *
 * Вверх функция не тянет НИКОГДА (`want <= scrollTop` ⇒ null): следование
 * обязано только догонять уехавший вниз конец сообщения, а не возвращать
 * пользователя к нему сверху — иначе оно перебивало бы переход по цитате.
 */
export function scrollTargetForRow(log, row, pad) {
    if (!log) return null;
    const top = Number(log.scrollTop) || 0;
    const height = Number(log.scrollHeight) || 0;
    const view = Number(log.clientHeight) || 0;
    const maxTop = Math.max(0, height - view);
    if (!row) return maxTop > top ? maxTop : null;
    const gap = Number.isFinite(pad) ? pad : FOLLOW_SCROLL_PAD;
    const bottom = Number(row.bottom) || 0;
    let want = top + (bottom - view) + gap;
    if (want > maxTop) want = maxTop;
    return want > top ? want : null;
}

/**
 * Отменил ли пользователь следование собственной прокруткой.
 *
 * Решение по НАПРАВЛЕНИЮ, а не по флагу «это был я»: присваивание `scrollTop`
 * порождает событие `scroll` асинхронно, и синхронный флаг успел бы сброситься
 * до его доставки. Свои доскроллы всегда увеличивают `scrollTop`, поэтому
 * «уменьшилось сверх порога» однозначно означает руку пользователя.
 *
 * @param {number} prevTop
 * @param {number} nextTop
 * @param {number} [threshold=FOLLOW_CANCEL_UP_PX]
 * @returns {boolean}
 */
export function cancelledByScroll(prevTop, nextTop, threshold) {
    const prev = Number(prevTop) || 0;
    const next = Number(nextTop) || 0;
    const t = Number.isFinite(threshold) ? threshold : FOLLOW_CANCEL_UP_PX;
    return prev - next > t;
}

/**
 * Режим прокрутки для CSSOM `scrollTo`.
 *
 * @param {boolean} reduceMotion — результат медиазапроса prefers-reduced-motion
 * @returns {'auto'|'smooth'}
 */
export function scrollBehavior(reduceMotion) {
    return reduceMotion === true ? 'auto' : 'smooth';
}

/** Снимок для e2e/диагностики. */
export function _debugFollowState() {
    return {
        dialogId: state.dialogId || null,
        messageId: state.messageId || null,
        untilMs: state.untilMs,
    };
}
