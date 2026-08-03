// tmux-web — общий слой мессенджеров: «следование» ленты за растущим
// сообщением. ЧИСТАЯ логика: ни DOM, ни сети, ни часов.
//
// ЗАЧЕМ ЭТО ВООБЩЕ НУЖНО. Типовой сценарий кнопок бота: пользователь жмёт
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
// Поэтому нажатие включает «следование» за парой (диалог, сообщение) на
// короткий срок. Пока оно живо, апдейты этого сообщения и новые сообщения в
// этом же диалоге тянут ленту к концу блока. Пассивное чтение истории при этом
// не ломается: без следования работает прежнее правило `isAtBottom`.
//
// ---------------------------------------------------------------------------
// ПОЧЕМУ ФАБРИКА, А НЕ МОДУЛЬНЫЙ СИНГЛТОН
// ---------------------------------------------------------------------------
//
// Исходник (`js/telegram/follow.js`) держит состояние в модульной переменной —
// и это правильно, пока панель ровно одна. Здесь панелей две, и общий инстанс
// дал бы КОЛЛИЗИЮ КЛЮЧЕЙ: `peer_id` беседы VK `2000000001` и `dialog_id`
// супергруппы Telegram `2000000001` — это РАЗНЫЕ чаты, а состояние сравнивает
// их строкой. Нажатие кнопки в одной панели тянуло бы ленту другой.
//
// Поэтому наружу отдаётся `createFollow()`: каждая панель делает свой инстанс
// (`const follow = createFollow()` в её `chat.js`), и пересечься им нечем.
//
// ЧАСЫ ИНЖЕКТИРУЮТСЯ (`nowMs` параметром), а не берутся из `Date.now()` внутри:
// иначе тест окна в 12 секунд зависел бы от скорости машины, а «часы прыгнули
// назад» (коррекция NTP, разбуженный ноутбук) вообще нельзя было бы проверить.
//
// TELEGRAM НА ЭТОТ МОДУЛЬ НЕ МИГРИРОВАН НАМЕРЕННО: его `follow.js` остаётся
// как есть вместе со своими 15 зелёными тестами. Миграция — отдельная задача
// после стабилизации VK; до тех пор оба файла живут параллельно, а расхождение
// ловится тем, что этот покрыт messenger_follow_factory.test.mjs.

/**
 * Сколько живёт следование после нажатия кнопки, мс.
 *
 * Верхняя граница здравого смысла: бот отвечает за секунды, а вот через
 * полминуты «само уехало вниз» читалось бы уже как самовольная прокрутка. 12 с
 * покрывают и медленного бота, и догрузку картинки по слабой сети, но не
 * переживают паузу, за которую пользователь успевает переключиться на чтение
 * истории.
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

/**
 * Создаёт НЕЗАВИСИМЫЙ инстанс «следования».
 *
 * Два инстанса не видят состояния друг друга — это и есть причина
 * существования фабрики (см. шапку модуля).
 *
 * @param {{ttlMs?: number, scrollPad?: number, cancelUpPx?: number}} [opts]
 *   Настройки инстанса. Некорректные значения (не число, ноль, отрицательное)
 *   молча заменяются дефолтами: панель не обязана валидировать свой конфиг, а
 *   `ttlMs: 0` означал бы следование, истекающее до первого кадра.
 * @returns {{noteFollow: function, clearFollow: function, followTarget: function,
 *   isFollowing: function, followsMessage: function, scrollDecision: function,
 *   scrollTargetForRow: function, cancelledByScroll: function,
 *   scrollBehavior: function, _debug: function}}
 */
export function createFollow(opts) {
    const cfg = opts || {};
    const defaultTtl = positive(cfg.ttlMs, FOLLOW_TTL_MS);
    const defaultPad = finite(cfg.scrollPad, FOLLOW_SCROLL_PAD);
    const defaultCancelUp = finite(cfg.cancelUpPx, FOLLOW_CANCEL_UP_PX);

    // Состояние ЗАМКНУТО в инстансе. Ни одной модульной переменной ниже — это
    // не стилистика, а весь смысл фабрики.
    const state = {
        /** Диалог, в котором нажали кнопку ('' ⇒ не следим). */
        dialogId: '',
        /** Идентификатор сообщения с кнопкой (0 ⇒ следим за диалогом целиком). */
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
     * @param {*} messageId — у VK это `conversation_message_id` либо `id`;
     *   модуль ключ не интерпретирует, а только сравнивает.
     * @param {number} nowMs — `Date.now()` вызывающего (инъекция часов)
     * @param {number} [ttlMs] — переопределение окна для одного нажатия
     * @returns {boolean} включилось ли следование
     */
    function noteFollow(dialogId, messageId, nowMs, ttlMs) {
        const id = dialogId == null ? '' : String(dialogId);
        if (!id) return false;
        const now = Number.isFinite(nowMs) ? nowMs : 0;
        state.dialogId = id;
        state.messageId = Number.isFinite(messageId) && messageId > 0 ? Number(messageId) : 0;
        state.untilMs = now + positive(ttlMs, defaultTtl);
        return true;
    }

    /** Снимает следование (идемпотентно). @returns {boolean} было ли оно */
    function clearFollow() {
        const had = !!state.untilMs;
        state.dialogId = '';
        state.messageId = 0;
        state.untilMs = 0;
        return had;
    }

    /**
     * Цель следования с учётом срока годности.
     *
     * Часы, прыгнувшие НАЗАД (коррекция NTP, спящий ноутбук), следование не
     * рвут: `now < untilMs` — это «ещё не истекло». Тот же выбор, что у
     * `format.js::toastAllowed`.
     *
     * @param {number} nowMs
     * @returns {?{dialogId: string, messageId: number}}
     */
    function followTarget(nowMs) {
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
    function isFollowing(dialogId, nowMs) {
        const t = followTarget(nowMs);
        if (!t || dialogId == null) return false;
        return String(dialogId) === t.dialogId;
    }

    /**
     * Следим ли именно за этим сообщением.
     *
     * Диалог сверяется ЯВНО: `chat.js::onEdited` находит строку только по
     * идентификатору сообщения и вообще не проверяет чат, а идентификаторы в
     * разных диалогах пересекаются свободно (у VK `conversation_message_id`
     * так и вовсе нумеруется с единицы в КАЖДОЙ беседе) — без этой сверки
     * чужая правка дёргала бы ленту.
     *
     * @param {*} dialogId
     * @param {*} messageId
     * @param {number} nowMs
     * @returns {boolean}
     */
    function followsMessage(dialogId, messageId, nowMs) {
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
     *   `follow` — доскролл к концу блока (плавно), `stick` — прежнее
     *   прилипание к низу, `none` — не трогать ленту (читают историю).
     */
    function scrollDecision(ctx) {
        if (!ctx) return 'none';
        if (ctx.following === true) return 'follow';
        return ctx.atBottom === true ? 'stick' : 'none';
    }

    /**
     * Куда прокрутить ленту, чтобы КОНЕЦ блока строки оказался виден.
     *
     * @param {?{scrollTop: number, scrollHeight: number, clientHeight: number}} log
     * @param {?{top: number, bottom: number}} row — координаты строки
     *   ОТНОСИТЕЛЬНО верха видимой области ленты (`rowRect.top - logRect.top`).
     *   `null` ⇒ раскладка неизвестна, тогда цель — самый низ.
     * @param {number} [pad]
     * @returns {?number} новый scrollTop или `null`, если двигать не нужно
     *
     * Вверх функция не тянет НИКОГДА (`want <= scrollTop` ⇒ null): следование
     * обязано только догонять уехавший вниз конец сообщения, а не возвращать
     * пользователя к нему сверху — иначе оно перебивало бы переход по цитате.
     */
    function scrollTargetForRow(log, row, pad) {
        if (!log) return null;
        const top = Number(log.scrollTop) || 0;
        const height = Number(log.scrollHeight) || 0;
        const view = Number(log.clientHeight) || 0;
        const maxTop = Math.max(0, height - view);
        if (!row) return maxTop > top ? maxTop : null;
        const gap = finite(pad, defaultPad);
        const bottom = Number(row.bottom) || 0;
        let want = top + (bottom - view) + gap;
        if (want > maxTop) want = maxTop;
        return want > top ? want : null;
    }

    /**
     * Отменил ли пользователь следование собственной прокруткой.
     *
     * Решение по НАПРАВЛЕНИЮ, а не по флагу «это был я»: присваивание
     * `scrollTop` порождает событие `scroll` асинхронно, и синхронный флаг
     * успел бы сброситься до его доставки. Свои доскроллы всегда увеличивают
     * `scrollTop`, поэтому «уменьшилось сверх порога» однозначно означает руку
     * пользователя.
     *
     * @param {number} prevTop
     * @param {number} nextTop
     * @param {number} [threshold]
     * @returns {boolean}
     */
    function cancelledByScroll(prevTop, nextTop, threshold) {
        const prev = Number(prevTop) || 0;
        const next = Number(nextTop) || 0;
        return prev - next > finite(threshold, defaultCancelUp);
    }

    /**
     * Режим прокрутки для CSSOM `scrollTo`.
     *
     * @param {boolean} reduceMotion — результат медиазапроса
     *   prefers-reduced-motion
     * @returns {'auto'|'smooth'}
     */
    function scrollBehavior(reduceMotion) {
        return reduceMotion === true ? 'auto' : 'smooth';
    }

    /** Снимок ИМЕННО ЭТОГО инстанса для e2e/диагностики. */
    function _debug() {
        return {
            dialogId: state.dialogId || null,
            messageId: state.messageId || null,
            untilMs: state.untilMs,
            ttlMs: defaultTtl,
        };
    }

    return {
        noteFollow,
        clearFollow,
        followTarget,
        isFollowing,
        followsMessage,
        scrollDecision,
        scrollTargetForRow,
        cancelledByScroll,
        scrollBehavior,
        _debug,
    };
}

/** Положительное число или фолбэк (ноль и отрицательные — тоже мусор). */
function positive(value, fallback) {
    return Number.isFinite(value) && value > 0 ? Number(value) : fallback;
}

/** Конечное число или фолбэк (ноль здесь законен: «без зазора»). */
function finite(value, fallback) {
    return Number.isFinite(value) ? Number(value) : fallback;
}
