// tmux-web — VK plugin: лента переписки #vk-log и отправка.
//
// Формат строки лога — тот же, что у Telegram:
//
//   [12:03:41] durov $ привет            ← чужое (ник --t-grn)
//   [12:03:44] you@local $ и тебе        ← своё (ник --t-cyan)
//   ── 2026-07-29 ──                     ← разделитель календарного дня
//   >> reconnected                       ← системная строка
//
// ЧТО ЕСТЬ СЕЙЧАС: текст, пересланные, ответы (цитата, полоса над полем ввода,
// переход к оригиналу), «следование» ленты за растущим сообщением, карточки
// вложений и клавиатуры сообществ. Контейнер `.vk-line-media` держит N
// карточек: одиночный слот по образцу Telegram пришлось бы переделывать, а у
// сообщения VK вложений до десяти.
//
// ОТВЕТЫ АДРЕСУЮТСЯ `conversation_message_id`, А НЕ `message_id`. Это не
// косметика имён: cmid нумеруется В КАЖДОЙ беседе с единицы, поэтому строки
// индексируются ДВУМЯ картами — `rows` (по глобальному id, для правок и
// удалений из LongPoll) и `rowsByCmid` (по cmid, для перехода по цитате).
// Подробности инварианта — в шапке js/vk/reply.js.
//
// Ключевые решения (перенесены из telegram/chat.js дословно):
//
// * ВСЁ через textContent. Текст сообщений — данные из VK; innerHTML здесь
//   означал бы XSS с чужого аккаунта.
// * Спейсер `.vk-log-spacer` ВСЕГДА первый потомок #vk-log: он прижимает
//   короткую переписку к низу, как в терминале. Очистка ленты его сохраняет.
// * Подгрузка вверх держит якорь скролла (scrollTop += прирост высоты) —
//   иначе лента прыгает под курсором на каждой странице истории.
// * Stick-to-bottom: новые сообщения доскроллкивают ленту только если
//   пользователь и так был у нижней границы.
// * Прижатие к низу — ЗАЩЁЛКА, а не разовый `scrollTop = scrollHeight`.
//   Содержимое ленты дорастает уже ПОСЛЕ прокрутки: превью вложений грузятся
//   лениво (js/vk/media.js), и каждое показанное растит карточку с 92px до
//   184px. Один синхронный доскролл этого не переживает — низ уезжает под
//   кромку ровно на сумму догруженного, а дальше дефект самоусиливается:
//   `isAtBottom` уже ложно, и ветка `stick` не срабатывает больше НИКОГДА.
//   Поэтому `scrollToBottom` запоминает НАМЕРЕНИЕ (`local.stickBottom`), а
//   `onMediaGrew` возвращает ленту вниз на каждом приросте, пока пользователь
//   сам не уйдёт читать историю.
// * Optimistic send: строка появляется мгновенно, реальный MessageDto из
//   ответа POST её заменяет. Тот же message_id может ПРИЙТИ ЕЩЁ РАЗ через WS
//   `new_message` — дедупликация по id в insertMessage.
// * `import * as vkApi` — защита от SyntaxError на закэшированной старой
//   версии модуля: отсутствующая функция даёт «undefined is not a function» в
//   одном месте, а не роняет вкладку целиком.
//
// ЧТО ДОБАВИЛОСЬ ПРОТИВ TELEGRAM: переключение беседы поднимает ПОКОЛЕНИЕ и
// отменяет предыдущую загрузку (js/vk/ratelimit.js). У Telegram хватало
// проверки «активный диалог не сменился», потому что запросов было не жаль; у
// VK бюджет 3 rps, и десять быстрых переключений без отмены — это три секунды,
// в течение которых лента перерисовывается чужими данными.

import {
    $vkInput, $vkLightbox, $vkLog, $vkPrompt, $vkSend, $vkSessionHead,
} from '../core/dom.js';
import { createFollow, FOLLOW_SCROLL_PAD, FOLLOW_TTL_MS } from '../messenger/follow-core.js';

import * as vkApi from './api.js';
import {
    attachmentsSummary, flattenForwards, forwardPreview, forwardsTail, normalizeAttachments,
    primaryAttachment,
} from './attachments.js';
import {
    applyHistoryKeyboard, buildMessageKeyboard, clearReplyKeyboard, initBotKeyboards,
    noteReplyMarkup, resetKeyboards,
} from './botkb.js';
import {
    cmidOf, dateSepLabel, dayKey, formatTime, insertMessage, isAtBottom, mediaGlyph, needsDateSep,
    nickOf, oldestId, prependMessages, sessionHeadParts, shouldLoadOlder, typingAllowed,
} from './format.js';
import {
    dropMessageMedia, renderAttachmentGroup, resetMediaCards, revokeDialogMedia,
    setMediaGrowthHandler, teardownMedia,
} from './media.js';
import { createGeneration, PRIORITY_USER, vkCoalescer, vkLimiter } from './ratelimit.js';
import {
    buildReplyTarget, classifyRowClick, clearReplyTarget, mergeReplyFromMessage,
    normalizeReplyTo, replyAuthorLabel, replyDtoFromTarget, replyNeedsPreview, replyPreviewText,
    replySourceLine, replyTarget, setReplyTarget,
} from './reply.js';
import { vkState } from './state.js';

/** Сколько сообщений тянуть за одну страницу. */
export const PAGE_SIZE = 50;

/**
 * Сколько держится подсветка оригинала после перехода по цитате, мс.
 *
 * Достаточно, чтобы глаз нашёл строку, и мало, чтобы подсветка не осталась
 * висеть как «выделенное сообщение» — она сообщает «вот сюда я тебя привёл»,
 * а не «это выбрано».
 */
export const REPLY_FLASH_MS = 2000;

/**
 * Сколько страниц истории догружать в поисках оригинала.
 *
 * Потолок обязателен вдвойне против Telegram: у VK бюджет 3 запроса в секунду
 * на весь плагин, и «доскроллить до оригинала» в переписке на десять тысяч
 * сообщений означало бы минуты, в течение которых панель не отвечает ни на
 * что другое. Не нашли за пять страниц — честно спрашиваем сервер про одно
 * конкретное сообщение и показываем его текст строкой.
 */
export const REPLY_JUMP_PAGES = 5;

/**
 * «Следование» ленты за растущим сообщением — СВОЙ инстанс панели.
 *
 * Ровно поэтому общий слой отдаёт фабрику, а не готовый объект: `peer_id`
 * беседы VK `2000000001` и `dialog_id` супергруппы Telegram `2000000001` —
 * разные чаты, и общее состояние тянуло бы ленту не той панели (см. шапку
 * js/messenger/follow-core.js).
 */
const follow = createFollow();

const local = {
    /** Идёт загрузка страницы истории. */
    loading: false,
    /** Есть ли что подгружать вверх. */
    hasMore: false,
    /** message_id → DOM-строка (для reconcile и edited/deleted из LongPoll). */
    rows: new Map(),
    /**
     * conversation_message_id → DOM-строка.
     *
     * Вторая карта, а не поиск перебором: цитата знает об оригинале ТОЛЬКО
     * cmid (глобального id VK в `reply_message` может не прислать вовсе), а
     * переход по цитате обязан быть мгновенным.
     */
    rowsByCmid: new Map(),
    /** tempId → { li, text } — отправленные, но не подтверждённые. */
    pending: new Map(),
    /** Обработчики навешены (bindScroll/bindPrompt идемпотентны). */
    bound: false,
    /** Обработчики ответов навешены (свой флаг, чтобы не трогать `bound`). */
    replyBound: false,
    /** Полоса ответа над #vk-prompt и её части (создаются лениво). */
    replyBar: null,
    replyBarName: null,
    replyBarText: null,
    /**
     * cmid подсвеченной строки.
     *
     * Хранится ИМЕННО идентификатор, а не узел: подгрузка истории вверх
     * перерисовывает ленту целиком (renderHistory → clearLog), и ссылка на DOM
     * умерла бы вместе с подсветкой.
     */
    flashCmid: null,
    /** Таймер снятия подсветки. */
    flashTimer: null,
    /** Идёт переход к оригиналу (не запускаем второй поверх первого). */
    jumping: false,
    /**
     * Строка, за ростом которой следим.
     *
     * Хранится именно УЗЕЛ (в отличие от `flashCmid`): наблюдателю размера
     * нужен конкретный элемент. Поэтому все точки, где узел умирает (clearLog,
     * replaceWith в onEdited, onDeleted), обязаны переставить наблюдение —
     * иначе оно молча слушает detached-узел.
     */
    followRow: null,
    /**
     * Единственный ResizeObserver на всю панель (создаётся лениво).
     *
     * Один на всех, а не по одному на нажатие: пользователь жмёт кнопки
     * десятками, и «новый наблюдатель на каждый клик» — это утечка ровно того
     * рода, которую ResizeObserver и призван закрывать.
     */
    followObserver: null,
    /** Таймер истечения следования. */
    followTimer: null,
    /** Прошлый scrollTop ленты — по нему ловится прокрутка ВВЕРХ. */
    lastScrollTop: 0,
    /**
     * Лента прижата к низу: отложенный рост обязан утянуть её обратно.
     *
     * Отдельный флаг, а не повторный `isAtBottom` в момент роста: в момент
     * роста лента УЖЕ не внизу (её оторвало от кромки именно это событие), и
     * ответ был бы «пользователь читает историю» — то есть ровно неверный.
     * Значение ставит [`scrollToBottom`] (намерение «хотим вниз») и снимает
     * обработчик прокрутки, когда пользователь ушёл выше кромки.
     */
    stickBottom: true,
    /** Счётчик временных id — уникальность без crypto. */
    tempSeq: 0,
    /** Колбэк «беседа прочитана» (ставит main.js). */
    onRead: null,
    /** Колбэк «дай присутствие» (живёт в dialogs.js — см. initChat). */
    presenceOf: null,
    /** Когда последний раз слали «печатает…» (мс). */
    typingSentAt: 0,
    /** В какую беседу слали «печатает…». */
    typingDialogId: null,
    /**
     * Поколение открытия беседы.
     *
     * Растёт на КАЖДОМ открытии, включая повторное открытие той же беседы
     * (кнопка [retry]): иначе ответ первой, брошенной загрузки дорисовался бы
     * поверх второй.
     */
    gen: createGeneration(),
};

/**
 * Привязывает внешние колбэки.
 *
 * @param {{onRead?: function(string, number): void,
 *   presenceOf?: function(string): ?object}} opts
 *   `onRead(dialogId, maxId)` — вызвать POST /read (сам запрос делает main.js,
 *   чтобы chat.js не зависел от dialogs.js и порядка импортов);
 *   `presenceOf(dialogId)` — присутствие собеседника для шапки сессии.
 */
export function initChat(opts) {
    if (opts && typeof opts.onRead === 'function') local.onRead = opts.onRead;
    if (opts && typeof opts.presenceOf === 'function') local.presenceOf = opts.presenceOf;
    // Ленивое превью вложения дорастает уже ПОСЛЕ прокрутки вниз и уводит
    // последнее сообщение под кромку — подписка возвращает ленту на место.
    // Ставится ДО bindScroll: подписчик ничего не делает без защёлки, но
    // порядок «сначала объявили намерение слушать, потом слушаем» дешевле
    // держать в голове, чем обратный.
    setMediaGrowthHandler(onMediaGrew);
    bindScroll();
    bindPrompt();
    // Строго ПОСЛЕ bindPrompt: у полосы ответа свой флаг `replyBound`, а её
    // узел встаёт непосредственно перед формой — формы к этому моменту уже
    // касались.
    bindReplyUi();
    // Кнопки сообществ отправляются ЧЕРЕЗ `send`, а не своим POST в botkb.js:
    // иначе у нажатия не было бы ни optimistic-строки, ни дедупликации по
    // insertMessage, ни кнопки [retry] — и в ленте появлялись бы дубли.
    // `press` (координаты кнопки) уезжает в тот же `send`: `payload` живёт
    // только на сервере, и подставляет его роут нажатия.
    initBotKeyboards({
        onSendText: (text, press) => send(text, null, null, press),
        // Нажатие включает «следование»: результат обязан доехать до глаз
        // пользователя сам (js/messenger/follow-core.js). Ключ — cmid: строки
        // ленты VK индексируются в `rowsByCmid`.
        onButtonPress: (dialogId, cmid) => noteButtonPress(dialogId, cmid),
    });
}

/** Сообщения активной беседы (всегда по возрастанию id). */
function activeList() {
    const id = vkState.activeDialogId;
    if (!id) return [];
    if (!vkState.messages.has(id)) vkState.messages.set(id, []);
    return vkState.messages.get(id);
}

/**
 * Беседа, открытая сейчас.
 *
 * Ищется в состоянии (а не хранится копией): список мог перезагрузиться, и
 * устаревшая ссылка давала бы старый title в шапке и никах. Смотрим ОБА
 * списка — беседа могла прийти только из серверного фильтра.
 */
function currentDialog() {
    const id = vkState.activeDialogId;
    if (!id) return null;
    const pick = (list) => (list || []).find((d) => String(d.id) === String(id)) || null;
    return pick(vkState.dialogs) || pick(vkState.dirDialogs);
}

/**
 * Очищает ленту, сохраняя спейсер первым потомком.
 *
 * Спейсер не пересоздаётся: он объявлен в index.html, и заводить его копию в
 * JS означало бы дублировать вёрстку в двух местах.
 */
function clearLog() {
    local.rows.clear();
    local.rowsByCmid.clear();
    local.pending.clear();
    // Единственная точка, через которую проходят ВСЕ снесения ленты. Само
    // следование живо (его цель — cmid, а не узел), но наблюдаемый узел сейчас
    // станет detached — наблюдение переставит renderHistory.
    unobserveFollowRow();
    if (!$vkLog) return;
    const spacer = $vkLog.querySelector('.vk-log-spacer');
    $vkLog.textContent = '';
    if (spacer) $vkLog.appendChild(spacer);
    else {
        const s = document.createElement('div');
        s.className = 'vk-log-spacer';
        $vkLog.appendChild(s);
    }
}

/**
 * Прокрутка в самый низ — и запоминание НАМЕРЕНИЯ там остаться.
 *
 * Защёлка обязательна, потому что высота ленты в этот момент ещё не
 * окончательная: ленивые превью вложений догрузятся через сотни миллисекунд и
 * вырастут (см. шапку модуля). Снимает защёлку только сам пользователь —
 * прокруткой выше нижней кромки (см. [`bindScroll`]).
 */
function scrollToBottom() {
    local.stickBottom = true;
    if ($vkLog) $vkLog.scrollTop = $vkLog.scrollHeight;
}

/**
 * Содержимое ленты доросло уже после прокрутки — вернуть её к низу.
 *
 * Зовётся из js/vk/media.js, когда декодировалось превью вложения (подписка
 * ставится в [`initChat`]). Экспортируется ради тестов и e2e: сама подписка
 * односторонняя, media.js о chat.js по-прежнему ничего не знает.
 *
 * Три случая, когда прирост игнорируется:
 *
 * * лента не прижата к низу — пользователь читает историю, и утаскивать его
 *   вниз из-за догрузившейся картинки было бы самоуправством;
 * * идёт подгрузка страницы вверх — `loadOlder` держит СВОЙ якорь между
 *   замером `scrollHeight` и восстановлением `scrollTop`, и посторонняя
 *   прокрутка внутри этого окна необратимо теряет позицию чтения;
 * * живо «следование» за строкой — у него своя цель (конец блока, а не низ
 *   ленты) и свой наблюдатель размера, и два доскролла спорили бы друг с
 *   другом.
 *
 * @returns {boolean} двигали ли ленту
 */
export function onMediaGrew() {
    if (!$vkLog || !local.stickBottom || local.loading) return false;
    if (follow.followTarget(Date.now())) return false;
    $vkLog.scrollTop = $vkLog.scrollHeight;
    return true;
}

// ---------------- следование за растущим сообщением ----------------

/**
 * Включает следование за сообщением и берёт его строку под наблюдение.
 *
 * Зовётся из Ф9 (нажатие кнопки сообщества) через `initBotKeyboards`; экспорт
 * нужен именно поэтому — botkb.js не имеет права импортировать chat.js, иначе
 * два модуля замкнутся друг на друга.
 *
 * Наблюдение ставится СРАЗУ, не дожидаясь правки: клавиатура может
 * перерисоваться и без `message_edited` (бот дописал ряд в ту же разметку), и
 * рост строки нужно поймать в любом случае.
 *
 * @param {*} dialogId
 * @param {*} cmid — ключ сообщения В БЕСЕДЕ (0 ⇒ следим за беседой целиком)
 */
export function noteButtonPress(dialogId, cmid) {
    if (!follow.noteFollow(dialogId, cmid, Date.now())) return;
    armFollowTimer();
    observeFollowRow(local.rowsByCmid.get(Number(cmid)) || null);
}

/**
 * Перезаводит таймер истечения следования.
 *
 * Таймер обязателен не ради самого срока (его проверяет `followTarget` по
 * часам), а ради СНЯТИЯ наблюдателя: без него ResizeObserver висел бы на
 * строке до следующей смены беседы.
 */
function armFollowTimer() {
    if (local.followTimer) {
        clearTimeout(local.followTimer);
        local.followTimer = null;
    }
    if (typeof setTimeout !== 'function') return;
    local.followTimer = setTimeout(() => {
        local.followTimer = null;
        stopFollow();
    }, FOLLOW_TTL_MS);
}

/** Снимает следование целиком: состояние, таймер и наблюдение за строкой. */
function stopFollow() {
    if (local.followTimer) {
        clearTimeout(local.followTimer);
        local.followTimer = null;
    }
    unobserveFollowRow();
    return follow.clearFollow();
}

/**
 * Наблюдатель размера — один на панель, создаётся лениво.
 *
 * `typeof` проверяется В МОМЕНТ ВЫЗОВА, а не на импорте: в окружении без
 * ResizeObserver (старый WebView, node-тесты) фича просто теряет добор
 * отложенного роста, а доскролл в момент правки продолжает работать.
 */
function ensureFollowObserver() {
    if (local.followObserver) return local.followObserver;
    if (typeof ResizeObserver !== 'function') return null;
    try {
        local.followObserver = new ResizeObserver(onFollowResize);
    } catch (_) {
        local.followObserver = null;
    }
    return local.followObserver;
}

/** Переставляет наблюдение на другую строку (null ⇒ просто снять). */
function observeFollowRow(row) {
    unobserveFollowRow();
    if (!row) return;
    local.followRow = row;
    const ro = ensureFollowObserver();
    if (!ro) return;
    try { ro.observe(row); } catch (_) { /* узел уже вне документа */ }
}

/** Снимает наблюдение, сохраняя сам наблюдатель для следующего нажатия. */
function unobserveFollowRow() {
    if (local.followObserver && local.followRow) {
        try { local.followObserver.unobserve(local.followRow); } catch (_) { /* уже снят */ }
    }
    local.followRow = null;
}

/**
 * Строка досчиталась в высоте (догрузилось медиа, перерисовалась клавиатура).
 *
 * Наблюдатель может пережить истечение следования на один кадр — поэтому срок
 * перепроверяется здесь, а не только по таймеру.
 */
function onFollowResize() {
    if (!follow.followTarget(Date.now())) {
        stopFollow();
        return;
    }
    scrollBlockIntoView(local.followRow);
}

/** Уважает системную настройку «меньше движения». */
function prefersReducedMotion() {
    if (typeof window === 'undefined' || !window || typeof window.matchMedia !== 'function') {
        return false;
    }
    try {
        return !!window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    } catch (_) {
        return false;
    }
}

/**
 * Координаты строки относительно верха видимой области ленты.
 *
 * Через `getBoundingClientRect`, а НЕ через `offsetTop`, по той же причине,
 * что и в [`scrollRowIntoView`]: у `#vk-log` нет своего `position`.
 *
 * @returns {?{top: number, bottom: number}} null ⇒ раскладка недоступна
 */
function rowOffset(row) {
    if (!row || !$vkLog) return null;
    if (typeof row.getBoundingClientRect !== 'function'
        || typeof $vkLog.getBoundingClientRect !== 'function') {
        return null;
    }
    const r = row.getBoundingClientRect();
    const log = $vkLog.getBoundingClientRect();
    return { top: r.top - log.top, bottom: r.bottom - log.top };
}

/**
 * Доскролл к концу блока строки (решение считает follow-core.js).
 *
 * Во время подгрузки истории вверх лента НЕ трогается: `loadOlder` держит
 * якорь между замером `scrollHeight` и восстановлением `scrollTop`, и
 * посторонняя прокрутка внутри этого окна необратимо теряет позицию чтения.
 */
function scrollBlockIntoView(row) {
    if (!$vkLog || local.loading) return;
    const top = follow.scrollTargetForRow($vkLog, rowOffset(row), FOLLOW_SCROLL_PAD);
    if (top === null) return;
    const behavior = follow.scrollBehavior(prefersReducedMotion());
    // `scrollTo` есть не везде (и не в node-стендах) — фолбэк обязателен,
    // иначе безусловный вызов уронил бы всю ленту.
    if (typeof $vkLog.scrollTo === 'function') {
        try {
            $vkLog.scrollTo({ top, behavior });
            return;
        } catch (_) { /* фолбэк ниже */ }
    }
    $vkLog.scrollTop = top;
}

/**
 * Что делать с лентой после того, как строка появилась или пересобралась.
 *
 * `stick`/`following` замеряются ВЫЗЫВАЮЩИМ до мутации DOM — после пересборки
 * строки `isAtBottom` считается уже по новой высоте и врёт.
 */
function afterRowChanged(row, ctx) {
    const decision = follow.scrollDecision(ctx);
    if (decision === 'follow') {
        // Наблюдение переезжает на актуальный узел: после replaceWith старый
        // уже вне документа, а рост (медиа, клавиатура) ещё впереди.
        observeFollowRow(row);
        scrollBlockIntoView(row);
        return;
    }
    // Прежнее поведение: доскролл только тому, кто и так был у нижней кромки.
    if (decision === 'stick') scrollToBottom();
}

/** Строка-разделитель даты. */
function buildDateSep(key) {
    const el = document.createElement('div');
    el.className = 'vk-date-sep';
    el.textContent = dateSepLabel(key);
    el.dataset.dateSep = key;
    return el;
}

/**
 * Строка сообщения: `[HH:MM:SS] nick $ text`.
 *
 * Части — отдельные span'ы: так CSS красит время/ник/текст независимо, а
 * точечные апдейты правят только нужный кусок.
 *
 * КОНТЕЙНЕР ПОД ВЛОЖЕНИЯ создаётся уже сейчас, хотя карточки приезжают в Ф7:
 * у сообщения VK до десяти вложений, и «слот на одно», заложенный по образцу
 * Telegram, пришлось бы переделывать вместе со всей разметкой строки.
 */
function buildMessageRow(msg, dialog) {
    const li = document.createElement('div');
    li.className = 'vk-line' + (msg.out ? ' vk-line-own' : '');
    if (Number.isFinite(msg.id)) li.dataset.msgId = String(msg.id);
    li.dataset.date = String(msg.date || 0);
    // cmid в разметке — им адресуются ответы: строка, нарисованная без него,
    // не смогла бы стать целью ответа без перерисовки.
    if (Number.isFinite(msg.cmid)) li.dataset.cmid = String(msg.cmid);

    // Цитата — ПЕРЕД временем и текстом: «на что отвечают» читается раньше
    // самого ответа. Отдельным блоком, а не span'ом: строка объявлена
    // `white-space: pre-wrap`, и инлайновая цитата встроилась бы в тот же ряд.
    const quote = buildQuoteRow(msg, dialog);
    if (quote) li.appendChild(quote);

    const time = document.createElement('span');
    time.className = 'vk-line-time';
    time.textContent = `[${formatTime(msg.date)}] `;

    const nick = document.createElement('span');
    nick.className = msg.out ? 'vk-line-nick-own' : 'vk-line-nick';
    nick.textContent = nickOf(msg, dialog);

    const sep = document.createElement('span');
    sep.className = 'vk-line-time';
    sep.textContent = ' $ ';

    const text = document.createElement('span');
    text.className = 'vk-line-text';
    text.textContent = messageText(msg);

    const suffix = document.createElement('span');
    suffix.className = 'vk-line-suffix';
    suffix.textContent = msg.edit_date ? ' (edited)' : '';

    li.appendChild(time);
    li.appendChild(nick);
    li.appendChild(sep);
    li.appendChild(text);
    li.appendChild(suffix);

    // Пересланные — блоком под строкой, с отступом по глубине. У Telegram
    // аналога не было вовсе: там пересылка это плоский заголовок.
    const fwd = buildForwards(msg);
    if (fwd) li.appendChild(fwd);

    // Вложения рисуются КАРТОЧКАМИ под строкой-подписью: сама строка остаётся
    // терминальной («▣ [photo] подпись»), а превью, размер и кнопки живут в
    // отдельном блоке. Карточек может быть НЕСКОЛЬКО — у сообщения VK до десяти
    // вложений, и контейнер заложен под группу, а не под одну.
    const media = document.createElement('div');
    media.className = 'vk-line-media';
    const group = renderAttachmentGroup(msg);
    if (group) media.appendChild(group);
    media.hidden = !group;
    li.appendChild(media);

    // Инлайн-клавиатура сообщества — ПОД карточками: сначала содержимое
    // сообщения, потом то, что с ним можно сделать. Нижняя клавиатура
    // (`kind == "keyboard"`) сюда не попадает: она состояние беседы, а не
    // сообщения, и живёт полосой над полем ввода (см. js/vk/botkb.js).
    const kb = buildMessageKeyboard(msg);
    if (kb) li.appendChild(kb);

    // Кнопка «ответить» — ПОСЛЕДНЕЙ и вне потока (CSS позиционирует её
    // абсолютно): в потоке она разрывала бы перенос текста и налезала на
    // маркер «⋯ в пути» у неподтверждённых строк.
    const replyBtn = buildReplyButton(msg, dialog);
    if (replyBtn) li.appendChild(replyBtn);

    li._vk = { text, suffix, msg, media, fwd, quote, kb };
    return li;
}

/**
 * Строка цитаты над текстом: `durov  привет` приглушённым.
 *
 * Возвращает `null`, если сообщение не является ответом. Недоступный оригинал
 * (удалён или скрыт приватностью) рисуется как обычная цитата с текстом
 * «сообщение недоступно» — пустая цитата выглядела бы как баг вёрстки.
 *
 * @param {object} msg — MessageDto
 * @param {?object} dialog — DialogDto
 * @returns {?HTMLElement}
 */
function buildQuoteRow(msg, dialog) {
    let reply = normalizeReplyTo(msg);
    if (!reply) return null;
    // У VK превью приезжает вместе с сообщением, поэтому досборка нужна ровно
    // для урезанных кадров старой версии плагина, пролежавших в кэше вкладки.
    // Она бесплатна: оригинал ищется в уже загруженной ленте.
    if (replyNeedsPreview(reply)) {
        const original = loadedMessage(msg.dialog_id, reply.cmid, reply.messageId);
        if (original) reply = mergeReplyFromMessage(reply, original, dialog);
    }

    const el = document.createElement('div');
    el.className = 'vk-reply-quote';
    const name = document.createElement('span');
    name.className = 'vk-reply-quote-name';
    name.textContent = replyAuthorLabel(reply, dialog, selfId());
    const text = document.createElement('span');
    text.className = 'vk-reply-quote-text';
    text.textContent = replyPreviewText(reply);
    el.appendChild(name);
    el.appendChild(text);

    // Кликабельна только цитата с известным cmid: именно он адресует оригинал
    // внутри беседы, и без него переходить некуда.
    if (reply.cmid !== null) {
        el.dataset.replyTo = String(reply.cmid);
        // Глобальный id — ТОЛЬКО для дозапроса оригинала, оказавшегося вне
        // загруженной страницы (`messages.getById` понимает лишь его). Ключом
        // перехода он не служит и в поиске строки не участвует.
        if (reply.messageId !== null) el.dataset.replyMid = String(reply.messageId);
        el.setAttribute('role', 'button');
        el.tabIndex = 0;
        el.setAttribute(
            'aria-label',
            `Перейти к сообщению${name.textContent ? ' от ' + name.textContent : ''}`,
        );
    }
    return el;
}

/**
 * Кнопка «ответить» на строке сообщения.
 *
 * Кнопка, а не иконка на псевдоэлементе: она обязана быть доступна с
 * клавиатуры (табом) и уметь объявить себя скринридеру. Появление по наведению
 * — задача CSS; фокус её показывает тем же правилом.
 *
 * У строк без подтверждённого cmid (optimistic-отправка ещё в пути) кнопки
 * нет: VK нечем адресовать ответ на несуществующий идентификатор.
 *
 * @param {object} msg — MessageDto
 * @param {?object} dialog
 * @returns {?HTMLElement}
 */
function buildReplyButton(msg, dialog) {
    if (cmidOf(msg) === null) return null;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'vk-reply-btn';
    btn.textContent = '↰';
    btn.title = 'Ответить на это сообщение';
    btn.setAttribute('aria-label', `Ответить: ${nickOf(msg, dialog)}`);
    btn.addEventListener('click', (e) => {
        e.preventDefault();
        // Клик по кнопке не должен всплыть в делегированные обработчики ленты.
        e.stopPropagation();
        startReply(msg, dialog);
    });
    return btn;
}

/** id своего аккаунта строкой или null (по нему узнаётся своя цитата). */
function selfId() {
    const self = vkState.selfUser;
    return self && self.id != null ? String(self.id) : null;
}

/**
 * Сообщение из уже загруженной ленты беседы.
 *
 * Ключ — cmid; глобальный id используется только как запасной, когда cmid у
 * цитаты нет вовсе (урезанный кадр из личного диалога). Поиск с конца:
 * оригинал цитаты почти всегда рядом со своим ответом.
 *
 * @param {string|number} dialogId
 * @param {?number} cmid
 * @param {?number} [messageId]
 * @returns {?object}
 */
function loadedMessage(dialogId, cmid, messageId) {
    const list = vkState.messages.get(String(dialogId)) || [];
    for (let i = list.length - 1; i >= 0; i -= 1) {
        const m = list[i];
        if (!m) continue;
        if (cmid !== null && cmid !== undefined && cmidOf(m) === cmid) return m;
        if ((cmid === null || cmid === undefined)
            && Number.isFinite(messageId) && m.id === messageId) return m;
    }
    return null;
}

/**
 * Блок пересланных сообщений или `null`.
 *
 * Глубина ограничена `attachments.js::MAX_FWD_DEPTH`, остаток сворачивается в
 * хвост «…ещё N пересланных»: у VK цепочка пересылок не ограничена ничем, и
 * наивная отрисовка превращает одно сообщение в экран.
 */
function buildForwards(msg) {
    const { rows, truncatedCount } = flattenForwards(msg);
    if (!rows.length) return null;
    const box = document.createElement('div');
    box.className = 'vk-fwd';
    for (const row of rows) {
        const line = document.createElement('div');
        line.className = 'vk-fwd-line';
        line.dataset.depth = String(row.depth);
        // Отступ текстом, а не CSS-классом на каждый уровень: уровней всего
        // два, а классы пришлось бы заводить парой и в css/vk.css.
        line.textContent = `${'│ '.repeat(row.depth)}↱ ${forwardPreview(row.msg)}`;
        box.appendChild(line);
    }
    const tail = forwardsTail(truncatedCount);
    if (tail) {
        const more = document.createElement('div');
        more.className = 'vk-fwd-more';
        more.textContent = tail;
        box.appendChild(more);
    }
    return box;
}

/**
 * Текст сообщения для ленты.
 *
 * Вложения на этой фазе карточками не рисуются, но проглатывать сообщение
 * нельзя: без плейсхолдера строка «[12:03] durov $» выглядела бы как баг.
 * Поэтому в текст уходит глиф первого вложения, а при нескольких — ещё и
 * суффикс «▣▣ 2 вложения». Служебные сообщения (`action`) подписываются своим
 * текстом: «пригласил участника» без пояснения читается как пустая строка.
 */
function messageText(msg) {
    const text = typeof msg.text === 'string' ? msg.text : '';
    const parts = [];

    if (msg.action && msg.action.kind) {
        const who = msg.sender ? nickOf(msg, null) : '';
        const what = msg.action.text || msg.action.kind;
        parts.push(who ? `* ${who} ${what}` : `* ${what}`);
    }

    const list = normalizeAttachments(msg);
    const first = primaryAttachment(list);
    if (first) {
        const dur = Number.isFinite(first.durationSecs) && first.durationSecs > 0
            ? ` ${first.durationSecs}s`
            : '';
        const name = first.fileName ? ` ${first.fileName}` : '';
        parts.push(`${mediaGlyph(first.kind)} [${first.kind}${dur}${name}]`);
    }
    if (text) parts.push(text);
    const summary = attachmentsSummary(list);
    if (summary) parts.push(`· ${summary}`);
    return parts.join(' ');
}

/** Системная строка `>> …`. */
export function systemLine(text) {
    if (!$vkLog) return null;
    const stick = isAtBottom($vkLog);
    const el = document.createElement('div');
    el.className = 'vk-line vk-line-system';
    el.textContent = `>> ${text}`;
    $vkLog.appendChild(el);
    if (stick) scrollToBottom();
    return el;
}

/** Строка ошибки (красная) с опциональным «retry». */
function errorLine(text, onRetry) {
    if (!$vkLog) return null;
    const el = document.createElement('div');
    el.className = 'vk-line vk-line-error';
    el.textContent = `>> ERR: ${text}`;
    if (typeof onRetry === 'function') {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'vk-retry-btn';
        btn.textContent = '[retry]';
        btn.addEventListener('click', () => {
            el.remove();
            onRetry();
        });
        el.appendChild(document.createTextNode(' '));
        el.appendChild(btn);
    }
    $vkLog.appendChild(el);
    scrollToBottom();
    return el;
}

/**
 * Рисует ленту с нуля из массива сообщений (по возрастанию id).
 *
 * Разделители дат вставляются между соседними сообщениями разных календарных
 * дней и перед самым первым — так начало страницы истории всегда подписано.
 */
export function renderHistory(messages, dialog) {
    clearLog();
    // Карточки прошлого рендера больше не в DOM: их контроллеры и наблюдатели
    // превью надо снять, иначе download_progress пойдёт в осиротевшие узлы.
    resetMediaCards();
    if (!$vkLog) return;
    let prev = null;
    for (const msg of messages) {
        if (needsDateSep(prev, msg)) $vkLog.appendChild(buildDateSep(dayKey(msg.date)));
        const row = buildMessageRow(msg, dialog);
        $vkLog.appendChild(row);
        indexRow(msg, row);
        prev = msg;
    }
    // Нижняя клавиатура пересчитывается ПО ВСЕЙ ленте, а не накапливается по
    // событиям: страница истории вверх приносит СТАРЫЕ сообщения, и актуальной
    // обязана остаться клавиатура из самого нижнего сообщения сообщества.
    applyHistoryKeyboard(messages);
    // Подсветка живёт по cmid, а не по узлу: переход к оригиналу сам вызывает
    // подгрузку истории, а та перерисовывает ленту с нуля — без этой строки
    // класс исчезал бы ровно в тот момент, когда пользователь его ждёт.
    if (local.flashCmid !== null) {
        const row = local.rowsByCmid.get(local.flashCmid);
        if (row && row.classList) row.classList.add('vk-msg-highlight');
    }
    // По той же причине переезжает и наблюдение за ростом: строки пересозданы,
    // и наблюдатель, оставшийся на старом узле, молча замолчал бы навсегда.
    const target = follow.followTarget(Date.now());
    if (target && target.messageId) {
        observeFollowRow(local.rowsByCmid.get(target.messageId) || null);
    }
}

/**
 * Кладёт строку в ОБЕ карты.
 *
 * Двух карт не избежать: правки и удаления из LongPoll приходят с глобальным
 * `message_id`, а цитаты — с `conversation_message_id`, и ни одна из сторон
 * второго идентификатора не знает.
 */
function indexRow(msg, row) {
    if (Number.isFinite(msg.id)) local.rows.set(msg.id, row);
    const cmid = cmidOf(msg);
    if (cmid !== null) local.rowsByCmid.set(cmid, row);
}

/**
 * Открывает беседу: грузит последнюю страницу и прокручивает вниз.
 *
 * Отмена предыдущей загрузки обязательна (см. шапку модуля): пользователь,
 * прощёлкавший десять бесед, иначе честно оплатит десять историй из бюджета
 * 3 rps и увидит, как лента перерисовывается чужими данными.
 *
 * @param {string} dialogId
 * @param {?object} dialog — DialogDto (для ников и шапки)
 */
export async function openDialog(dialogId, dialog) {
    const id = String(dialogId);
    const previous = vkState.activeDialogId;
    // Уходим из прошлой беседы — снимаем там «печатает…»: иначе собеседник ещё
    // несколько секунд видел бы, что мы ему набираем.
    if (local.typingDialogId && local.typingDialogId !== id) cancelTyping();
    vkState.activeDialogId = id;
    // Блобы прошлой беседы освобождаются СРАЗУ при переключении: иначе двадцать
    // переходов по беседам с фотографиями остаются в памяти вкладки до
    // перезагрузки страницы (браузер не собирает objectURL сам), а у VK одно
    // сообщение способно принести десять блобов.
    if (previous !== null && String(previous) !== id) revokeDialogMedia();
    // Следование принадлежит ПОКИНУТОЙ беседе: тянуть новую ленту вниз из-за
    // кнопки, нажатой в другом чате, — чистая самодеятельность. Повторное
    // открытие той же беседы (кнопка [retry]) следование сохраняет.
    if (previous === null || String(previous) !== id) stopFollow();
    // Поколение растёт ДО запроса и на каждом открытии, включая повторное.
    const gen = local.gen.bump();
    local.loading = true;
    local.hasMore = false;
    clearLog();
    // Клавиатура сообщества принадлежит ПРЕДЫДУЩЕЙ беседе: пока грузится
    // история, показывать её нельзя — нажатие ушло бы не в тот чат. Состояние
    // бесед при этом не стирается, renderHistory восстановит нужную из ленты.
    clearReplyKeyboard();
    // Цель ответа принадлежит ПОКИНУТОЙ беседе: без сброса полоса врала бы про
    // то, кому мы отвечаем (сам reply.js ответ туда уже не отдаст — cmid
    // сверяется с беседой, — но видимая полоса это обещание пользователю).
    cancelReply();
    // Подсветка от прошлого перехода к оригиналу — тоже чужая.
    clearFlash();
    updateHead(dialog, 0);
    systemLine(`opening ${id}…`);

    try {
        await vkLimiter.acquire(PRIORITY_USER);
        if (local.gen.stale(gen)) return;
        const resp = await vkCoalescer.submit('history', (signal) => vkApi.getMessages(id, {
            limit: PAGE_SIZE,
            signal,
        }));
        // Пока грузили, пользователь мог переключить беседу — не рисуем чужую
        // историю поверх новой. Проверка по ПОКОЛЕНИЮ, а не по activeDialogId:
        // повторное открытие ТОЙ ЖЕ беседы тоже отменяет предыдущую загрузку.
        if (local.gen.stale(gen)) return;
        const messages = Array.isArray(resp && resp.messages) ? resp.messages : [];
        local.hasMore = !!(resp && resp.has_more);
        vkState.messages.set(id, messages.slice());
        renderHistory(messages, dialog);
        updateHead(dialog, messages.length);
        scrollToBottom();
        const maxId = messages.length ? messages[messages.length - 1].id : 0;
        if (typeof local.onRead === 'function') local.onRead(id, maxId);
    } catch (e) {
        // Отмена — наше собственное решение, а не сбой.
        if (local.gen.stale(gen) || (e && e.name === 'AbortError')) return;
        errorLine(`не удалось загрузить историю: ${(e && e.message) || e}`, () => {
            openDialog(id, dialog);
        });
    } finally {
        if (!local.gen.stale(gen)) local.loading = false;
    }
}

/**
 * Обновляет шапку `SESSION ▸ …`.
 *
 * Присутствие собеседника берётся у dialogs.js через колбэк: дублировать карту
 * присутствия во втором модуле — верный способ получить два расходящихся
 * состояния.
 *
 * Шапка собирается ИЗ УЗЛОВ, а не одной строкой: @ник обязан быть
 * приглушённым, а покрасить кусок текста можно только отдельным элементом.
 * Итоговый `textContent` при этом совпадает с `sessionHeadText` символ в
 * символ — форма шапки описана в format.js ровно один раз.
 */
export function updateHead(dialog, count) {
    if (!$vkSessionHead) return;
    const presence = (dialog && typeof local.presenceOf === 'function')
        ? local.presenceOf(String(dialog.id))
        : null;
    const parts = sessionHeadParts(dialog, count, presence);
    $vkSessionHead.textContent = parts.name;
    if (parts.nick) {
        const nick = document.createElement('span');
        nick.className = 'vk-session-nick';
        nick.textContent = parts.nick;
        $vkSessionHead.appendChild(document.createTextNode(' '));
        $vkSessionHead.appendChild(nick);
    }
    if (parts.tail) $vkSessionHead.appendChild(document.createTextNode(parts.tail));
}

/** Перерисовывает шапку по текущему состоянию (изменилось присутствие). */
export function refreshHead() {
    const dialog = currentDialog();
    if (!dialog) return;
    updateHead(dialog, activeList().length);
}

/**
 * Подгружает страницу истории вверх, сохраняя якорь скролла.
 *
 * Порядок операций критичен: запомнить scrollHeight → вставить → сдвинуть
 * scrollTop на прирост. Иначе лента прыгает на высоту вставленного блока.
 *
 * Курсор — МИНИМАЛЬНЫЙ `message.id`, а не cmid: параметр VK называется
 * `start_message_id` и принимает глобальный идентификатор (см.
 * format.js::oldestId).
 */
export async function loadOlder(dialog) {
    const id = vkState.activeDialogId;
    if (!id || local.loading || !local.hasMore || !$vkLog) return;
    const before = oldestId(activeList());
    if (!Number.isFinite(before)) return;

    const gen = local.gen.current();
    local.loading = true;
    const prevHeight = $vkLog.scrollHeight;
    const prevTop = $vkLog.scrollTop;
    try {
        await vkLimiter.acquire(PRIORITY_USER);
        if (local.gen.stale(gen)) return;
        const resp = await vkCoalescer.submit('history', (signal) => vkApi.getMessages(id, {
            limit: PAGE_SIZE,
            beforeId: before,
            signal,
        }));
        if (local.gen.stale(gen)) return;
        const older = Array.isArray(resp && resp.messages) ? resp.messages : [];
        local.hasMore = !!(resp && resp.has_more);
        if (!older.length) return;

        const merged = prependMessages(older, activeList());
        vkState.messages.set(id, merged);
        renderHistory(merged, dialog);
        updateHead(dialog, merged.length);
        // Якорь: то, что читал пользователь, остаётся под тем же курсором.
        $vkLog.scrollTop = prevTop + ($vkLog.scrollHeight - prevHeight);
    } catch (e) {
        if (!local.gen.stale(gen) && !(e && e.name === 'AbortError')) {
            console.debug('[vk-chat] loadOlder failed', e);
        }
    } finally {
        if (!local.gen.stale(gen)) local.loading = false;
    }
}

/**
 * ДОГОН открытой беседы: перечитать её свежую страницу и дорисовать пропущенное
 * (forge-oy0c).
 *
 * # Зачем это существует
 *
 * Лента открытой беседы живёт ОДНИМ источником — кадрами `new_message`. Кадр,
 * которого не было, лентой не восполняется ничем, и таких дыр ровно три:
 *
 * 1. **фон вкладки.** `main.js::suspendVk` РВЁТ соединение, когда страница
 *    уходит в фон, а `resumeVk` его поднимает. Всё, что пришло между ними, в
 *    открытой беседе не появлялось никогда — пока пользователь не уйдёт в
 *    другую беседу и не вернётся (это `openDialog`, то есть чтение по REST);
 * 2. **обрыв связи.** Реконнект возвращает поток «с сейчас»;
 * 3. **`resync`.** Клиент отстал (broadcast Lagged) либо обогащение пачки
 *    отказало — сервер честно просит перечитать состояние. До forge-oy0c
 *    `reloadData` перечитывал только КОЛОНКУ, и открытая лента оставалась
 *    прежней.
 *
 * # Как склеивается
 *
 * Страница у VK — это ПОСЛЕДНИЕ `PAGE_SIZE` сообщений. Если она пересекается с
 * загруженным (обычный случай), новые строки доклеиваются, а совпавшие по id
 * заменяются свежей версией. Если пересечения НЕТ — за время отсутствия
 * набралось больше страницы, — лента заменяется страницей целиком и `hasMore`
 * взводится: дыру посреди истории показывать нельзя, а подгрузка вверх её
 * закроет по требованию.
 *
 * # Прокрутка
 *
 * Решение принимается ДО перерисовки: стоял у нижней кромки — уезжает вниз к
 * новому, читал историю — остаётся на месте. Отметка о прочтении уходит только
 * в первом случае: дочитанным считается увиденное.
 *
 * @returns {Promise<number>} сколько строк добавилось
 */
export async function refreshActive() {
    const id = vkState.activeDialogId;
    if (!id || local.loading || !$vkLog) return 0;

    const gen = local.gen.current();
    local.loading = true;
    const stick = local.stickBottom || isAtBottom($vkLog);
    const prevTop = Number($vkLog.scrollTop) || 0;
    try {
        await vkLimiter.acquire(PRIORITY_USER);
        if (local.gen.stale(gen)) return 0;
        const resp = await vkCoalescer.submit('history', (signal) => vkApi.getMessages(id, {
            limit: PAGE_SIZE,
            signal,
        }));
        if (local.gen.stale(gen)) return 0;
        const page = Array.isArray(resp && resp.messages) ? resp.messages : [];
        if (page.length === 0) return 0;

        const list = activeList();
        const dialog = currentDialog();
        const newestKnown = list.length ? list[list.length - 1].id : null;
        const oldestPage = page[0].id;

        let merged;
        let added;
        if (newestKnown === null || oldestPage > newestKnown) {
            // Пересечения нет: между загруженным и страницей — дыра. Показывать
            // её нельзя, поэтому лента начинается заново, как при открытии.
            merged = page.slice();
            added = page.length;
            // Выше страницы история точно есть, если мы только что выбросили
            // загруженное: подгрузка вверх обязана остаться доступной.
            local.hasMore = !!(resp && resp.has_more) || list.length > 0;
        } else {
            merged = list;
            added = 0;
            for (const m of page) {
                const { inserted, index } = insertMessage(merged, m);
                if (inserted) added += 1;
                // Совпавшую строку заменяем свежей версией: за время отсутствия
                // её могли отредактировать, лайкнуть или удалить.
                else if (index >= 0) merged[index] = m;
            }
        }
        vkState.messages.set(id, merged);
        renderHistory(merged, dialog);
        updateHead(dialog, merged.length);
        if (stick) {
            scrollToBottom();
            const maxId = merged.length ? merged[merged.length - 1].id : 0;
            if (added > 0 && typeof local.onRead === 'function') local.onRead(id, maxId);
        } else {
            $vkLog.scrollTop = prevTop;
        }
        return added;
    } catch (e) {
        // Догон — фоновое действие: у него нет пользовательского намерения, и
        // ошибочная строка в ленте объясняла бы то, чего человек не просил.
        if (!local.gen.stale(gen) && !(e && e.name === 'AbortError')) {
            console.debug('[vk-chat] refreshActive failed', e);
        }
        return 0;
    } finally {
        if (!local.gen.stale(gen)) local.loading = false;
    }
}

/**
 * Подписка на скролл: подгрузка вверх у верхней границы и отметка о прочтении
 * у нижней.
 *
 * «Докрутил до низа» — законный триггер прочтения: пользователь дочитал ленту
 * глазами. Повторных запросов это не плодит — решение принимает `scheduleRead`
 * в main.js (debounce + память о последнем отмеченном id).
 */
export function bindScroll() {
    if (!$vkLog || local.bound) return;
    local.lastScrollTop = Number($vkLog.scrollTop) || 0;
    $vkLog.addEventListener('scroll', () => {
        const top = Number($vkLog.scrollTop) || 0;
        // Своя прокрутка вверх важнее автоматики: пользователь передумал ждать
        // ответ и ушёл читать историю. Решение — по НАПРАВЛЕНИЮ движения (см.
        // follow-core.js::cancelledByScroll), собственные доскроллы идут вниз
        // и под порог не попадают.
        if (follow.cancelledByScroll(local.lastScrollTop, top)) stopFollow();
        const atBottom = isAtBottom($vkLog);
        // Прижатие снимается только УХОДОМ ВВЕРХ и только от кромки. Обе
        // оговорки обязательны:
        //
        // * «от кромки» — иначе дребезг инерционной прокрутки в пару пикселей
        //   отменял бы прижатие, стоя при этом внизу;
        // * «уходом вверх» — иначе прижатие снимал бы НАШ СОБСТВЕННЫЙ доскролл.
        //   Событие прокрутки приходит асинхронно, к этому моменту успевает
        //   догрузиться ещё одно превью, и `isAtBottom` уже ложно — хотя рука
        //   пользователя ленты не касалась. Ровно на этом дефект и держался.
        //
        // Любой шаг вверх, а не порог в 24px, как у отмены следования:
        // медленная прокрутка идёт шагами по 5–10px и порога не набирает ни
        // разу, а собственные доскроллы идут ВНИЗ и под правило не попадают.
        if (atBottom) local.stickBottom = true;
        else if (top < local.lastScrollTop) local.stickBottom = false;
        local.lastScrollTop = top;
        if (atBottom) noteReachedBottom();
        if (!shouldLoadOlder($vkLog, { hasMore: local.hasMore, loading: local.loading })) return;
        void loadOlder(currentDialog());
    });
}

/** Лента докручена до низа — сообщаем «просмотрено до последнего id». */
function noteReachedBottom() {
    if (typeof local.onRead !== 'function') return;
    const id = vkState.activeDialogId;
    if (!id) return;
    const list = activeList();
    if (!list.length) return;
    local.onRead(String(id), list[list.length - 1].id);
}

// ---------------- ответы (reply) ----------------

/**
 * Навешивает обработчики ответов: двойной клик по строке, клик по цитате и
 * Esc.
 *
 * Всё делегировано на `#vk-log`, а не навешано на строки: лента
 * перерисовывается целиком при каждой подгрузке истории, и построчные
 * слушатели пришлось бы снимать вручную (а забытые — держали бы в памяти
 * мёртвые узлы). Исключение — кнопка «ответить»: у неё всё равно есть
 * собственный DOM-узел, и свой слушатель там дешевле.
 */
function bindReplyUi() {
    if (local.replyBound) return;
    local.replyBound = true;
    if ($vkLog) {
        $vkLog.addEventListener('dblclick', onLogDblClick);
        $vkLog.addEventListener('click', onLogClick);
        $vkLog.addEventListener('keydown', onLogKeyDown);
    }
    if (typeof document !== 'undefined' && document.addEventListener) {
        document.addEventListener('keydown', onReplyEscape);
    }
}

/** Двойной клик по строке — начать ответ на неё. */
function onLogDblClick(e) {
    if (classifyRowClick(e.target) !== 'reply') return;
    const row = e.target.closest('.vk-line');
    // Системные строки, ошибки и заглушки удалённых сообщений `_vk` не несут.
    if (!row || !row._vk || !row._vk.msg) return;
    startReply(row._vk.msg, currentDialog());
}

/** Клик по цитате — перейти к оригиналу. */
function onLogClick(e) {
    const quote = quoteTarget(e.target);
    if (!quote) return;
    e.preventDefault();
    void jumpToMessage(quote.cmid, quote.messageId);
}

/** Enter/Space на цитате — то же, что клик (цитата в таб-порядке). */
function onLogKeyDown(e) {
    if (e.key !== 'Enter' && e.key !== ' ' && e.key !== 'Spacebar') return;
    const quote = quoteTarget(e.target);
    if (!quote) return;
    // Пробел на цитате иначе прокрутил бы ленту на экран вниз.
    e.preventDefault();
    void jumpToMessage(quote.cmid, quote.messageId);
}

/**
 * Идентификаторы оригинала, если событие пришло из кликабельной цитаты.
 *
 * @returns {?{cmid: number, messageId: ?number}}
 */
function quoteTarget(target) {
    if (!target || typeof target.closest !== 'function') return null;
    const quote = target.closest('.vk-reply-quote');
    if (!quote || !quote.dataset) return null;
    const cmid = Number(quote.dataset.replyTo);
    if (!Number.isFinite(cmid) || cmid <= 0) return null;
    const mid = Number(quote.dataset.replyMid);
    return { cmid, messageId: Number.isFinite(mid) && mid > 0 ? mid : null };
}

/**
 * Esc снимает выбранный ответ.
 *
 * Гард на лайтбокс обязателен: этот обработчик регистрируется раньше
 * лайтбоксного (initChat идёт до initMedia), и без проверки одно нажатие Esc
 * снимало бы полосу ответа вместо закрытия картинки. `preventDefault` — только
 * когда действительно что-то отменили, чтобы не глотать Esc у соседей.
 */
function onReplyEscape(e) {
    if (e.key !== 'Escape') return;
    if ($vkLightbox && !$vkLightbox.hidden) return;
    if (!replyTarget(vkState.activeDialogId)) return;
    e.preventDefault();
    cancelReply();
    focusInput();
}

/** Фокус в поле ввода (поля может не быть — старый кэш index.html). */
function focusInput() {
    if (!$vkInput) return;
    try { $vkInput.focus(); } catch (_) { /* поля может не быть */ }
}

/**
 * Выбирает сообщение целью ответа и показывает полосу.
 *
 * @param {object} msg — MessageDto
 * @param {?object} [dialog]
 * @returns {boolean}
 */
export function startReply(msg, dialog) {
    const id = vkState.activeDialogId;
    if (!id) return false;
    const target = buildReplyTarget(id, msg, dialog || currentDialog(), selfId());
    if (!target) return false;
    setReplyTarget(target);
    showReplyBar(target);
    // Фокус уходит в поле ввода: выбор ответа — это начало набора, и лишний
    // клик по полю здесь был бы чистой потерей.
    focusInput();
    return true;
}

/** Снимает выбранный ответ и прячет полосу. */
export function cancelReply() {
    const had = clearReplyTarget();
    if (local.replyBar) local.replyBar.hidden = true;
    return had;
}

/**
 * Сбрасывает полосу, если она указывает именно на это сообщение.
 *
 * @param {*} dialogId
 * @param {?number} cmid
 * @returns {boolean}
 */
function cancelReplyTo(dialogId, cmid) {
    if (!Number.isFinite(cmid)) return false;
    const target = replyTarget(dialogId);
    if (!target || target.cmid !== cmid) return false;
    return cancelReply();
}

/**
 * Создаёт (при первой надобности) полосу ответа над `#vk-prompt`.
 *
 * Узел создаётся в JS, а не в index.html: у пользователя, который ни разу не
 * отвечал на конкретное сообщение, пустой div в разметке жил бы всегда.
 *
 * Позиция перепроверяется на КАЖДЫЙ показ: полоса клавиатуры сообщества (Ф9)
 * вставляется тем же `insertBefore($vkPrompt)` и лениво — бот может прислать
 * клавиатуру уже после выбора ответа и вклиниться между полосой и формой.
 * Полоса ответа обязана оставаться вплотную к полю ввода: она про то, чем мы
 * сейчас пишем.
 */
function ensureReplyBar() {
    if (typeof document === 'undefined' || !$vkPrompt || !$vkPrompt.parentNode) return null;
    if (!local.replyBar) {
        const bar = document.createElement('div');
        bar.className = 'vk-reply-bar';
        bar.hidden = true;

        const name = document.createElement('span');
        name.className = 'vk-reply-bar-name';
        const text = document.createElement('span');
        text.className = 'vk-reply-bar-text';

        const close = document.createElement('button');
        close.type = 'button';
        close.className = 'vk-reply-cancel';
        close.textContent = '✕';
        close.title = 'Отменить ответ (Esc)';
        close.setAttribute('aria-label', 'Отменить ответ');
        close.addEventListener('click', () => {
            cancelReply();
            focusInput();
        });

        bar.appendChild(name);
        bar.appendChild(text);
        bar.appendChild(close);
        local.replyBar = bar;
        local.replyBarName = name;
        local.replyBarText = text;
    }
    if ($vkPrompt.previousElementSibling !== local.replyBar) {
        $vkPrompt.parentNode.insertBefore(local.replyBar, $vkPrompt);
    }
    return local.replyBar;
}

/**
 * Показывает полосу с именем автора и превью.
 *
 * Placeholder поля ввода НЕ трогаем: у клавиатуры VK нет поля-подсказки
 * (телеграмный `placeholder` разметки в её контракте отсутствует), поэтому
 * менять его некому и незачем.
 *
 * @param {object} target — цель из reply.js::buildReplyTarget
 */
function showReplyBar(target) {
    const bar = ensureReplyBar();
    if (!bar) return;
    local.replyBarName.textContent = target.name;
    local.replyBarText.textContent = target.preview;
    bar.hidden = false;
}

/** Приставка сообщений «оригинал недоступен» — один текст на все ветки. */
const REPLY_GONE_PREFIX = 'сообщение недоступно:';

/**
 * Переход к оригиналу цитаты: прокрутка + кратковременная подсветка.
 *
 * Порядок попыток:
 *
 * 1. строка уже в ленте — просто показать её;
 * 2. оригинал выше загруженного — тянуть историю вверх, но не более
 *    [`REPLY_JUMP_PAGES`] страниц (см. её doc-комментарий);
 * 3. не нашли — спросить сервер про одно сообщение. Только он отличает
 *    «оригинал далеко» от «оригинал удалён», и в первом случае мы хотя бы
 *    покажем текст строкой лога.
 *
 * Шаг 3 возможен только при известном глобальном `message_id`: маршрут
 * `GET /messages/:mid` ведёт в `messages.getById`, а он понимает лишь его.
 * Инлайненная цитата глобального id может не нести вовсе — тогда честная
 * системная строка вместо молчания.
 *
 * @param {number} cmid — ключ оригинала В БЕСЕДЕ
 * @param {?number} messageId — глобальный id для дозапроса, если он известен
 */
async function jumpToMessage(cmid, messageId) {
    if (local.jumping) return;
    const dialogId = vkState.activeDialogId ? String(vkState.activeDialogId) : '';
    if (!dialogId) return;
    // Переход к оригиналу уезжает ВВЕРХ — прямо против следования. Живое
    // следование вернуло бы ленту вниз, и клик по цитате выглядел бы как
    // «ничего не произошло».
    stopFollow();
    if (revealMessage(cmid)) return;

    local.jumping = true;
    try {
        for (let i = 0; i < REPLY_JUMP_PAGES && local.hasMore; i += 1) {
            await loadOlder(currentDialog());
            // Пока грузили, пользователь мог уйти в другую беседу.
            if (String(vkState.activeDialogId) !== dialogId) return;
            if (revealMessage(cmid)) return;
        }
        // Обёртка могла не приехать со старым кэшем api.js — тогда честно
        // говорим, что дальше не пошли, вместо тихого игнора клика.
        const fetchMessage = typeof vkApi.getMessage === 'function' ? vkApi.getMessage : null;
        if (!fetchMessage || messageId === null) {
            systemLine(`сообщение #${cmid} не найдено в загруженной истории`);
            return;
        }
        await vkLimiter.acquire(PRIORITY_USER);
        if (String(vkState.activeDialogId) !== dialogId) return;
        const resp = await fetchMessage(dialogId, messageId);
        const dto = resp && resp.message ? resp.message : resp;
        if (String(vkState.activeDialogId) !== dialogId) return;
        if (!dto || !Number.isFinite(dto.id)) {
            systemLine(`${REPLY_GONE_PREFIX} #${cmid}`);
            return;
        }
        systemLine(`↳ ${replySourceLine(dto, currentDialog(), selfId())}`);
    } catch (e) {
        systemLine(e && e.code === 'unknown_message'
            ? `${REPLY_GONE_PREFIX} #${cmid} (удалено)`
            : `не удалось открыть #${cmid}: ${(e && e.message) || e}`);
    } finally {
        local.jumping = false;
    }
}

/**
 * Прокручивает к строке и подсвечивает её.
 *
 * @param {number} cmid
 * @returns {boolean} была ли строка в ленте
 */
function revealMessage(cmid) {
    const row = local.rowsByCmid.get(cmid);
    if (!row) return false;
    scrollRowIntoView(row);
    flashRow(cmid);
    return true;
}

/**
 * Прокручивает ленту так, чтобы строка оказалась по центру.
 *
 * Через `getBoundingClientRect`, а НЕ через `offsetTop`: у `#vk-log` нет
 * своего `position`, и смещения его детей считаются от `#vk` — прямой
 * `scrollTop = row.offsetTop` промахнулся бы на высоту титульбара и шапки
 * сессии.
 */
function scrollRowIntoView(row) {
    if (!$vkLog || !row) return;
    if (typeof row.getBoundingClientRect !== 'function'
        || typeof $vkLog.getBoundingClientRect !== 'function') {
        if (typeof row.scrollIntoView === 'function') row.scrollIntoView();
        return;
    }
    const r = row.getBoundingClientRect();
    const log = $vkLog.getBoundingClientRect();
    const centered = (r.top - log.top) - ($vkLog.clientHeight - r.height) / 2;
    $vkLog.scrollTop += centered;
}

/**
 * Кратковременно подсвечивает строку.
 *
 * Подсвеченная строка запоминается по cmid: renderHistory вернёт класс после
 * любой перерисовки (см. там), а таймер снимет его один раз в конце.
 */
function flashRow(cmid) {
    clearFlash();
    local.flashCmid = cmid;
    const row = local.rowsByCmid.get(cmid);
    if (row && row.classList) row.classList.add('vk-msg-highlight');
    if (typeof setTimeout !== 'function') return;
    local.flashTimer = setTimeout(() => {
        local.flashTimer = null;
        clearFlash();
    }, REPLY_FLASH_MS);
}

/** Снимает подсветку и её таймер (идемпотентно). */
function clearFlash() {
    if (local.flashTimer) {
        clearTimeout(local.flashTimer);
        local.flashTimer = null;
    }
    if (local.flashCmid === null) return;
    const row = local.rowsByCmid.get(local.flashCmid);
    if (row && row.classList) row.classList.remove('vk-msg-highlight');
    local.flashCmid = null;
}

// ---------------- отправка ----------------

/** Подписка на prompt: Enter, кнопка TRANSMIT и индикатор набора. */
export function bindPrompt() {
    if (local.bound) return;
    local.bound = true;
    if ($vkPrompt) {
        // submit покрывает и Enter в input, и клик по кнопке TRANSMIT.
        $vkPrompt.addEventListener('submit', (e) => {
            e.preventDefault();
            submitPrompt();
        });
    } else if ($vkSend) {
        $vkSend.addEventListener('click', (e) => {
            e.preventDefault();
            submitPrompt();
        });
    }
    if ($vkInput) {
        // Набор текста → «печатает…» собеседнику. Очистка поля (в том числе
        // Backspace до пустоты) гасит статус досрочно.
        $vkInput.addEventListener('input', () => {
            if (String($vkInput.value || '').trim()) noteTyping();
            else cancelTyping();
        });
    }
}

function submitPrompt() {
    if (!$vkInput) return;
    const text = String($vkInput.value || '');
    if (!text.trim()) return;
    $vkInput.value = '';
    // Сообщение ушло — статус набора больше не соответствует правде.
    cancelTyping();
    void send(text);
}

/**
 * Сообщает VK, что мы печатаем — не чаще одного запроса в TYPING_THROTTLE_MS.
 *
 * Троттлинг не косметика: VK держит статус около десяти секунд сам, а бюджет
 * запросов — три в секунду. Набор длинного сообщения не имеет права съесть его
 * на индикатор, который собеседник и так видит непрерывно. Ошибки глотаются
 * молча: из-за индикатора набора UI ломаться не должен.
 */
export function noteTyping() {
    const id = vkState.activeDialogId ? String(vkState.activeDialogId) : null;
    if (!id) return false;
    const now = Date.now();
    if (!typingAllowed(local.typingSentAt, now, local.typingDialogId === id)) return false;
    local.typingSentAt = now;
    local.typingDialogId = id;
    vkApi.sendTyping(id, 'typing').catch(() => { /* не критично, см. doc */ });
    return true;
}

/**
 * Досрочно снимает статус набора.
 *
 * В VK НЕ ХОДИТ: у `messages.setActivity` нет вида «перестал печатать», статус
 * гаснет сам через ~10 секунд. Телеграмный `sendTyping(id, 'cancel')` здесь
 * означал бы лишний запрос из бюджета 3 rps, который ничего не меняет.
 */
export function cancelTyping() {
    const had = local.typingDialogId !== null;
    local.typingDialogId = null;
    local.typingSentAt = 0;
    return had;
}

/**
 * Отправка с optimistic-рендером.
 *
 * 1. строка появляется сразу, с классом `.vk-msg-pending` («⋯ в пути»);
 * 2. ответ POST несёт РЕАЛЬНЫЙ MessageDto — им строка заменяется (reconcile);
 * 3. если VK не подтвердил id (`id <= 0`), строка снимается: настоящую дорисует
 *    `new_message` из LongPoll — иначе в ленте осталась бы строка с неизвестным
 *    id, которую нельзя ни дедуплицировать, ни сделать целью ответа;
 * 4. ошибка сети — красная строка с рабочим [retry], а pending-строка
 *    убирается (висящая «в пути» строка врала бы про отправку).
 *
 * Ответ на сообщение: цель берётся из активной полосы, но фиксируется ОДИН раз
 * — в локальную переменную и в замыкание [retry]. Иначе повтор после ошибки
 * ушёл бы туда, куда полоса указывает СЕЙЧАС (или вообще без ответа, если её
 * успели снять), а это уже другое сообщение в чужую ветку.
 *
 * @param {string} text
 * НАЖАТИЕ КНОПКИ СООБЩЕСТВА идёт этим же путём — ради тех же четырёх свойств.
 * Отличается оно ровно одним: запросом, который уходит на сервер. Обычный
 * текст едет в `POST /messages`, нажатие — в `POST /messages/:mid/button` с
 * КООРДИНАТАМИ кнопки, потому что `payload` во фронте не бывает вовсе (см.
 * шапку js/vk/botkb.js и plugins/vk/src/dto.rs::ReplyButtonDto).
 *
 * @param {string} text
 * @param {?number} [replyToCmid] — `undefined` ⇒ взять цель из полосы,
 *   `null` ⇒ отправить БЕЗ ответа (так повтор не подхватывает чужую цель).
 *   Ключ — именно `conversation_message_id`: им VK адресует ответ в беседе.
 * @param {?string} [payload] — payload текстовой кнопки сообщества; фронт его
 *   не знает и всегда передаёт `null` (поле оставлено ради формы Telegram)
 * @param {?{messageId: number, row: number, col: number}} [press] — нажатая
 *   кнопка сообщества: сообщение с клавиатурой и место кнопки в ней
 */
export async function send(text, replyToCmid, payload, press) {
    const id = vkState.activeDialogId;
    if (!id) {
        systemLine('no dialog selected');
        return;
    }
    const dialog = currentDialog();
    const target = replyTarget(id);
    const replyCmid = replyToCmid === undefined
        ? (target ? target.cmid : null)
        : (Number.isFinite(replyToCmid) && replyToCmid > 0 ? replyToCmid : null);
    // Цитату в optimistic-строке рисуем только если отвечаем на ТУ САМУЮ цель,
    // что сейчас в полосе: у повтора превью уже неоткуда взять, там останется
    // «…», и её дособерёт reconcile ответом сервера.
    const replyDto = (target && target.cmid === replyCmid)
        ? replyDtoFromTarget(target)
        : null;
    local.tempSeq += 1;
    const tempId = `tmp-${Date.now()}-${local.tempSeq}`;
    const optimistic = {
        id: null,
        dialog_id: id,
        out: true,
        date: Math.floor(Date.now() / 1000),
        sender: vkState.selfUser || null,
        text,
        media: null,
        attachments: [],
        reply_to_id: null,
        reply_to: replyDto,
        edit_date: null,
        cmid: null,
        fwd: [],
        action: null,
        payload: payload || null,
    };
    const row = buildMessageRow(optimistic, dialog);
    row.classList.add('vk-msg-pending');
    row.dataset.tempId = tempId;
    if ($vkLog) $vkLog.appendChild(row);
    local.pending.set(tempId, { li: row, text });
    scrollToBottom();

    try {
        // Отправка — действие пользователя: она обгоняет фоновые запросы, но
        // всё равно проходит через общий бюджет 3 rps.
        await vkLimiter.acquire(PRIORITY_USER);
        let resp;
        if (press) {
            // Подпись и `payload` кнопки сервер достанет из самого сообщения —
            // мы называем только её место в клавиатуре.
            resp = await vkApi.pressButton(id, press.messageId, {
                row: press.row,
                col: press.col,
            });
        } else {
            const body = { text };
            if (replyCmid !== null) body.replyToCmid = replyCmid;
            if (payload) body.payload = String(payload);
            resp = await vkApi.sendMessage(id, body);
        }
        const dto = resp && resp.message;
        local.pending.delete(tempId);
        // Полоса снимается ТОЛЬКО после подтверждения отправки: при ошибке
        // пользователь жмёт [retry], и цель обязана быть на месте. Сбрасываем
        // адресно — за время запроса он мог выбрать уже другое сообщение.
        cancelReplyTo(id, replyCmid);
        if (!dto || !Number.isFinite(dto.id) || dto.id <= 0) {
            // Сообщение ушло, но id неизвестен: ждём его из LongPoll.
            row.remove();
            return;
        }
        // Беседу могли переключить, пока летел запрос: сообщение всё равно надо
        // запомнить в её ленте, но DOM трогать нельзя.
        const list = vkState.messages.get(String(dto.dialog_id)) || [];
        const { inserted } = insertMessage(list, dto);
        vkState.messages.set(String(dto.dialog_id), list);
        if (String(dto.dialog_id) !== String(vkState.activeDialogId)) {
            row.remove();
            return;
        }
        if (!inserted) {
            // Событие LongPoll опередило ответ POST — строка уже нарисована.
            row.remove();
            return;
        }
        const real = buildMessageRow(dto, dialog);
        row.replaceWith(real);
        indexRow(dto, real);
        updateHead(dialog, list.length);
        scrollToBottom();
    } catch (e) {
        local.pending.delete(tempId);
        row.remove();
        // Повтор уходит в ТО ЖЕ сообщение: цель зафиксирована замыканием, а не
        // прочитана из полосы заново (её могли уже снять или перенацелить).
        errorLine(
            `не отправлено: ${(e && e.message) || e}`,
            () => send(text, replyCmid, payload, press),
        );
    }
}

// ---------------- realtime ----------------

/**
 * WS `new_message`.
 *
 * @param {object} msg — MessageDto
 * @returns {boolean} дорисовано ли в активную ленту
 */
export function onNewMessage(msg) {
    if (!msg || !Number.isFinite(msg.id)) return false;
    // Нижняя клавиатура — состояние БЕСЕДЫ, и запоминается независимо от того,
    // видна ли сейчас её лента: вернувшись в беседу, пользователь обязан
    // увидеть ту клавиатуру, которую сообщество прислало последней.
    noteReplyMarkup(msg);
    const id = String(msg.dialog_id);
    const list = vkState.messages.get(id) || [];
    const { inserted, index } = insertMessage(list, msg);
    vkState.messages.set(id, list);
    if (!inserted) return false;
    if (String(vkState.activeDialogId) !== id || !$vkLog) return false;

    const dialog = currentDialog();
    // Замер ДО вставки строки: после appendChild высота ленты уже другая.
    const stick = isAtBottom($vkLog);
    // Собеседник (или бот) ответил НОВЫМ сообщением, а не правкой старого —
    // это тот же результат нажатия, и уехать к нему пользователь должен так же.
    const following = follow.isFollowing(id, Date.now());
    const row = buildMessageRow(msg, dialog);
    const prev = index > 0 ? list[index - 1] : null;

    if (index === list.length - 1) {
        if (needsDateSep(prev, msg)) $vkLog.appendChild(buildDateSep(dayKey(msg.date)));
        $vkLog.appendChild(row);
    } else {
        // Сообщение «из середины» (пришло вне порядка после догона истории) —
        // вставляем перед следующим по id, иначе лента разъедется с моделью.
        const nextRow = local.rows.get(list[index + 1].id);
        if (nextRow) $vkLog.insertBefore(row, nextRow);
        else $vkLog.appendChild(row);
    }
    indexRow(msg, row);
    updateHead(dialog, list.length);
    afterRowChanged(row, { atBottom: stick, following });
    return true;
}

/**
 * WS `message_edited` — обновить текст и приписать «(edited)».
 *
 * Строка ищется по одному лишь `message_id`, а у VK он сквозной по аккаунту —
 * тем не менее сверка беседы обязательна: правка из соседней беседы не должна
 * дёргать открытую ленту.
 *
 * @param {object} msg — MessageDto
 */
export function onEdited(msg) {
    if (!msg || !Number.isFinite(msg.id)) return;
    // Правка сообщения — штатный способ сообщества поменять клавиатуру.
    noteReplyMarkup(msg);
    const id = String(msg.dialog_id);
    const list = vkState.messages.get(id) || [];
    const idx = list.findIndex((m) => m.id === msg.id);
    if (idx >= 0) list[idx] = msg;
    const row = local.rows.get(msg.id);
    if (!row || !row._vk) return;
    if (String(vkState.activeDialogId) !== id) return;

    const stick = isAtBottom($vkLog);
    const following = follow.followsMessage(id, cmidOf(msg), Date.now());
    // Вложения и пересылки могли измениться — строка пересобирается целиком.
    let live = row;
    if (row._vk.fwd || (msg.attachments && msg.attachments.length) || msg.fwd) {
        // Сервер в этот момент инвалидирует свой файловый кэш; держать во
        // вкладке старую картинку значит показывать то, чего в VK уже нет.
        dropMessageMedia(id, msg.id);
        const fresh = buildMessageRow(msg, currentDialog());
        fresh._vk.suffix.textContent = ' (edited)';
        row.replaceWith(fresh);
        indexRow(msg, fresh);
        live = fresh;
    } else {
        row._vk.text.textContent = messageText(msg);
        row._vk.suffix.textContent = ' (edited)';
        row._vk.msg = msg;
    }
    afterRowChanged(live, { atBottom: stick, following });
}

/**
 * WS `message_deleted` — заменить строки заглушкой.
 *
 * @param {{dialog_id: string, message_ids: Array<number>}} evt
 */
export function onDeleted(evt) {
    const ids = (evt && Array.isArray(evt.message_ids)) ? evt.message_ids : [];
    const dialogId = evt && evt.dialog_id ? String(evt.dialog_id) : '';
    for (const id of ids) {
        // cmid читается ДО удаления из модели: после splice сообщение искать
        // уже негде, а именно он связывает удалённое с полосой ответа и с
        // индексом строк.
        const cmid = cmidOfLoaded(dialogId, id);
        if (dialogId) {
            dropFromList(dialogId, id);
            dropMessageMedia(dialogId, id);
        } else {
            for (const key of vkState.messages.keys()) {
                dropFromList(key, id);
                dropMessageMedia(key, id);
            }
        }
        // Отвечать на удалённое нельзя: цель молча указывала бы в пустоту, а
        // VK отверг бы отправку уже после нажатия Enter.
        if (cmid !== null) cancelReplyTo(dialogId || vkState.activeDialogId, cmid);
        const row = local.rows.get(id);
        if (!row) continue;
        // Узел сейчас перестанет быть строкой сообщения — снимаем наблюдение,
        // иначе следование слушало бы заглушку.
        if (local.followRow === row) unobserveFollowRow();
        row.className = 'vk-line vk-line-system vk-line-deleted';
        row.textContent = '<< message purged >>';
        row._vk = null;
        local.rows.delete(id);
        if (cmid !== null) local.rowsByCmid.delete(cmid);
    }
}

/** cmid уже загруженного сообщения по его глобальному id. */
function cmidOfLoaded(dialogId, msgId) {
    const keys = dialogId ? [String(dialogId)] : Array.from(vkState.messages.keys());
    for (const key of keys) {
        const list = vkState.messages.get(key) || [];
        const found = list.find((m) => m && m.id === msgId);
        if (found) return cmidOf(found);
    }
    return null;
}

function dropFromList(dialogId, msgId) {
    const list = vkState.messages.get(String(dialogId));
    if (!list) return;
    const idx = list.findIndex((m) => m.id === msgId);
    if (idx >= 0) list.splice(idx, 1);
}

/**
 * WS `history_cleared` — беседу очистили до `up_to_id` (событие LongPoll 13).
 *
 * ОТДЕЛЬНЫЙ кадр, а не `message_deleted`, потому что VK не присылает список
 * удалённых идентификаторов — только границу. Ставить на каждое сообщение
 * заглушку «<< message purged >>», как это делает `onDeleted`, здесь нельзя:
 * заглушек оказалось бы столько, сколько было сообщений, и лента превратилась
 * бы в стену надгробий вместо пустой переписки.
 *
 * `up_to_id === 0` означает «удалено всё»: у VK это форма события, когда
 * граница неизвестна.
 *
 * @param {{dialog_id: string, up_to_id: number}} evt
 * @returns {number} сколько сообщений убрано из модели
 */
export function onHistoryCleared(evt) {
    if (!evt || !evt.dialog_id) return 0;
    const id = String(evt.dialog_id);
    const upTo = Math.max(0, Number(evt.up_to_id) || 0);
    const list = vkState.messages.get(id) || [];
    const keep = upTo > 0 ? list.filter((m) => Number(m.id) > upTo) : [];
    const removed = list.length - keep.length;
    if (removed <= 0) return 0;

    vkState.messages.set(id, keep);
    if (String(vkState.activeDialogId) !== id) return removed;

    const dialog = currentDialog();
    renderHistory(keep, dialog);
    updateHead(dialog, keep.length);
    systemLine(upTo > 0 ? `history cleared up to #${upTo}` : 'history cleared');
    scrollToBottom();
    return removed;
}

/**
 * WS `read` с `direction: "out"` — собеседник прочитал наши сообщения.
 *
 * В терминальном UI галочек нет, поэтому одна системная строка на событие (и
 * только для открытой беседы, чтобы не засорять ленту).
 */
export function onRead(evt) {
    if (!evt || evt.direction !== 'out') return;
    if (String(evt.dialog_id) !== String(vkState.activeDialogId)) return;
    systemLine(`peer read up to #${evt.max_id}`);
}

/** Полностью сбрасывает ленту (logout, смена аккаунта). */
export function resetChat() {
    // Поколение растёт: ответы в полёте больше не имеют права дорисоваться.
    local.gen.bump();
    vkCoalescer.abort('history');
    clearLog();
    cancelTyping();
    // Цель ответа, подсветка и следование принадлежали прошлому аккаунту.
    cancelReply();
    clearFlash();
    stopFollow();
    // Карточки, блобы и лайтбокс принадлежали прошлому аккаунту: после logout
    // ни один objectURL не должен остаться живым.
    teardownMedia();
    // Клавиатуры сообществ — тоже: чужая клавиатура над полем ввода после
    // смены аккаунта означала бы нажатие в несуществующую беседу.
    resetKeyboards();
    local.hasMore = false;
    local.loading = false;
    // Пустая лента — это лента у низа: следующая открытая беседа обязана
    // приехать вниз, а не унаследовать «пользователь читал историю».
    local.stickBottom = true;
    updateHead(null, 0);
}

/** Снимок для e2e/диагностики. */
export function _debugChatState() {
    return {
        activeDialogId: vkState.activeDialogId,
        rendered: local.rows.size,
        pending: local.pending.size,
        hasMore: local.hasMore,
        loading: local.loading,
        messages: (vkState.messages.get(String(vkState.activeDialogId)) || []).length,
        generation: local.gen.current(),
        replyBarVisible: !!(local.replyBar && !local.replyBar.hidden),
        flashCmid: local.flashCmid,
        following: follow._debug(),
        observingRow: !!local.followRow,
        stickBottom: local.stickBottom,
    };
}
