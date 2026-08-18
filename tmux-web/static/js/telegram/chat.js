// tmux-web — Telegram plugin: лента переписки #tg-log и отправка.
//
// Формат строки лога:
//
//   [12:03:41] sofia $ привет            ← чужое (ник --t-grn)
//   [12:03:44] you@local $ и тебе        ← своё (ник --t-cyan)
//   ── 2026-07-29 ──                     ← разделитель календарного дня
//   >> reconnected                       ← системная строка
//   << message purged >>                 ← удалённое сообщение
//
// Ключевые решения:
//
// * ВСЁ через textContent. Текст сообщений — данные из Telegram; innerHTML
//   здесь означал бы XSS с чужого аккаунта.
// * Спейсер `.tg-log-spacer` ВСЕГДА первый потомок #tg-log: он прижимает
//   короткую переписку к низу, как в терминале. Очистка ленты его сохраняет.
// * Подгрузка вверх держит якорь скролла (scrollTop += прирост высоты) —
//   иначе лента прыгает под курсором на каждой странице истории.
// * Stick-to-bottom: новые сообщения доскроллкивают ленту только если
//   пользователь и так был у нижней границы. Читающего историю не дёргаем.
// * Прижатие к низу — ЗАЩЁЛКА, а не разовый `scrollTop = scrollHeight`.
//   Содержимое ленты дорастает уже ПОСЛЕ прокрутки: превью вложений грузятся
//   лениво (js/telegram/media.js), и каждое показанное растит карточку с 92px
//   до 184px. Один синхронный доскролл этого не переживает — низ уезжает под
//   кромку ровно на сумму догруженного, а дальше дефект самоусиливается:
//   `isAtBottom` уже ложно, и ветка `stick` не срабатывает больше НИКОГДА.
//   Поэтому `scrollToBottom` запоминает НАМЕРЕНИЕ (`local.stickBottom`), а
//   `onMediaGrew` возвращает ленту вниз на каждом приросте, пока пользователь
//   сам не уйдёт читать историю. Со «следованием» (follow.js) защёлка не
//   спорит: у того своя цель — конец строки бота, а не низ ленты, и пока оно
//   живо, прирост медиа отдаётся ему (см. [`onMediaGrew`]).
// * Optimistic send: строка появляется мгновенно, реальный `MessageDto` из
//   ответа POST её заменяет (reconcile). Тот же message_id может ПРИЙТИ ЕЩЁ
//   РАЗ через WS `new_message` — дедупликация по id в insertMessage.
// * Ответы (reply): цитата рисуется ПЕРВЫМ потомком строки, кнопка «ответить»
//   — последним (она позиционируется абсолютно и в потоке мешала бы переносу
//   текста). Вся логика без DOM вынесена в js/telegram/reply.js.

import { $tgInput, $tgLightbox, $tgLog, $tgPrompt, $tgSend, $tgSessionHead } from '../core/dom.js';
// Кнопка встречи — общая на три панели (js/messenger/meet-row.js). При
// выключенном плагине `attachMeetButton` выходит первой строкой и не стоит
// ленте ничего: ни детекта, ни узла, ни запроса.
import { ESC_DEPTH, registerEscLayer } from '../messenger/esc-stack.js';
import { attachMeetButton } from '../messenger/meet-row.js';

import { getMessages, sendMessage, sendTyping } from './api.js';
// `getMessage` появился вместе с роутом одного сообщения. Берём его через
// namespace, а не именованным импортом: статика отдаётся без сборщика и без
// хеша в имени, поэтому браузер вполне может держать в кэше СТАРЫЙ api.js — с
// именованным импортом это был бы SyntaxError, гасящий всю вкладку целиком, а
// так отвалится ровно одна необязательная функция (переход к оригиналу).
import * as tgApi from './api.js';
import {
    applyHistoryKeyboard, buildMessageKeyboard, clearReplyKeyboard, initBotKeyboards,
    noteReplyMarkup, resetKeyboards,
} from './botkb.js';
import {
    dayKey, dateSepLabel, formatTime, insertMessage, isAtBottom, needsDateSep, nickOf,
    oldestId, prependMessages, sessionHeadParts, shouldLoadOlder, typingAllowed,
} from './format.js';
import {
    FOLLOW_SCROLL_PAD, FOLLOW_TTL_MS, cancelledByScroll, clearFollow, followTarget,
    followsMessage, isFollowing, noteFollow, scrollBehavior, scrollDecision,
    scrollTargetForRow, _debugFollowState,
} from './follow.js';
import {
    buildMediaCard, dropMessageMedia, initMedia, resetMediaCards, revokeDialogMedia,
    setMediaGrowthHandler,
} from './media.js';
import {
    buildReplyTarget, classifyRowClick, clearReplyTarget, mergeReplyFromMessage,
    normalizeReplyTo, replyAuthorLabel, replyDtoFromTarget, replyNeedsPreview,
    replyPreviewText, replySourceLine, replyTarget,
    setReplyTarget,
} from './reply.js';
import { tgState } from './state.js';

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
 * Потолок обязателен: у переписки на десять тысяч сообщений «доскроллить до
 * оригинала» означало бы сотню запросов подряд (и почти гарантированный
 * FLOOD_WAIT). Не нашли за пять страниц — честно спрашиваем сервер про одно
 * конкретное сообщение и показываем его текст строкой.
 */
export const REPLY_JUMP_PAGES = 5;

const local = {
    /** Идёт загрузка страницы истории. */
    loading: false,
    /** Есть ли что подгружать вверх. */
    hasMore: false,
    /** message_id → DOM-строка (для edited/deleted и reconcile). */
    rows: new Map(),
    /** tempId → { li, text } — отправленные, но не подтверждённые. */
    pending: new Map(),
    /** Обработчики навешены (bindScroll/bindPrompt идемпотентны). */
    bound: false,
    /** Счётчик временных id — уникальность без crypto. */
    tempSeq: 0,
    /** Колбэк «диалог прочитан» (ставит main.js). */
    onRead: null,
    /** Когда последний раз слали «печатает…» (мс). */
    typingSentAt: 0,
    /** В какой диалог слали «печатает…» — его же гасит cancelTyping. */
    typingDialogId: null,
    /** Колбэк «дай присутствие диалога» (ставит main.js, см. initChat). */
    presenceOf: null,
    /** Обработчики ответов навешены (свой флаг, чтобы не трогать `bound`). */
    replyBound: false,
    /** Полоса ответа над #tg-prompt и её части (создаются лениво). */
    replyBar: null,
    replyBarName: null,
    replyBarText: null,
    /**
     * message_id подсвеченной строки.
     *
     * Хранится ИМЕННО id, а не узел: подгрузка истории вверх перерисовывает
     * ленту целиком (renderHistory → clearLog), и ссылка на DOM умерла бы
     * вместе с подсветкой.
     */
    flashId: null,
    /** Таймер снятия подсветки. */
    flashTimer: null,
    /** Идёт переход к оригиналу (не запускаем второй поверх первого). */
    jumping: false,
    /**
     * Строка, за ростом которой следим после нажатия кнопки бота.
     *
     * Хранится именно УЗЕЛ (в отличие от `flashId`): наблюдателю размера нужен
     * конкретный элемент. Поэтому все точки, где узел умирает (clearLog,
     * replaceWith в onEdited, onDeleted), обязаны переставить наблюдение —
     * иначе оно молча слушает detached-узел.
     */
    followRow: null,
    /**
     * Единственный ResizeObserver на всю вкладку (создаётся лениво).
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
};

/**
 * Привязывает внешние колбэки.
 *
 * @param {{onRead?: function(string, number): void,
 *   presenceOf?: function(string): ?object}} opts
 *   `onRead(dialogId, maxId)` — вызвать POST /read (сам запрос делает main.js,
 *   чтобы chat.js не зависел от dialogs.js и порядка импортов);
 *   `presenceOf(dialogId)` — присутствие собеседника для шапки сессии (живёт
 *   в dialogs.js по той же причине).
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
    // Строго ПОСЛЕ bindPrompt: у полосы ответа свой флаг `replyBound`, а
    // общий `local.bound` проверяет ещё и bindScroll — вклиниваться в него
    // означало бы молча отключить подгрузку истории.
    bindReplyUi();
    // Лайтбокс медиа: обработчики Esc/фон/✕ навешиваются один раз.
    initMedia();
    // Кнопки reply-клавиатуры отправляют обычный текст. Передаём именно
    // `send`, а не свой POST в botkb.js: иначе у таких сообщений не было бы
    // optimistic-строки, дедупликации по insertMessage и снятия «печатает…»,
    // и в ленте появлялись бы дубли.
    initBotKeyboards({
        onSendText: (text) => send(text),
        // Нажатие кнопки включает «следование»: результат нажатия обязан
        // доехать до глаз пользователя сам (см. follow.js).
        onButtonPress: (dialogId, messageId) => noteButtonPress(dialogId, messageId),
    });
}

/** Сообщения активного диалога (всегда по возрастанию id). */
function activeList() {
    const id = tgState.activeDialogId;
    if (!id) return [];
    if (!tgState.messages.has(id)) tgState.messages.set(id, []);
    return tgState.messages.get(id);
}

/**
 * Очищает ленту, сохраняя спейсер первым потомком.
 *
 * Спейсер не пересоздаётся: он объявлен в index.html, и заводить его копию в
 * JS означало бы дублировать вёрстку в двух местах.
 */
function clearLog() {
    local.rows.clear();
    local.pending.clear();
    // Единственная точка, через которую проходят ВСЕ снесения ленты. Само
    // следование живо (его цель — id, а не узел), но наблюдаемый узел сейчас
    // станет detached — наблюдение переставит renderHistory.
    unobserveFollowRow();
    if (!$tgLog) return;
    const spacer = $tgLog.querySelector('.tg-log-spacer');
    $tgLog.textContent = '';
    if (spacer) $tgLog.appendChild(spacer);
    else {
        const s = document.createElement('div');
        s.className = 'tg-log-spacer';
        $tgLog.appendChild(s);
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
    if ($tgLog) $tgLog.scrollTop = $tgLog.scrollHeight;
}

/**
 * Содержимое ленты доросло уже после прокрутки — вернуть её к низу.
 *
 * Зовётся из js/telegram/media.js, когда декодировалось превью вложения
 * (подписка ставится в [`initChat`]). Экспортируется ради тестов и e2e: сама
 * подписка односторонняя, media.js о chat.js по-прежнему ничего не знает.
 *
 * Три случая, когда прирост игнорируется:
 *
 * * лента не прижата к низу — пользователь читает историю, и утаскивать его
 *   вниз из-за догрузившейся картинки было бы самоуправством;
 * * идёт подгрузка страницы вверх — `loadOlder` держит СВОЙ якорь между
 *   замером `scrollHeight` и восстановлением `scrollTop`, и посторонняя
 *   прокрутка внутри этого окна необратимо теряет позицию чтения;
 * * живо «следование» за строкой бота — у него своя цель (конец блока, а не
 *   низ ленты) и свой ResizeObserver, и два доскролла спорили бы друг с
 *   другом. Прирост тем же превью до `следуемой` строки всё равно доедет:
 *   его поймает `onFollowResize`.
 *
 * @returns {boolean} двигали ли ленту
 */
export function onMediaGrew() {
    if (!$tgLog || !local.stickBottom || local.loading) return false;
    if (followTarget(Date.now())) return false;
    $tgLog.scrollTop = $tgLog.scrollHeight;
    return true;
}

// ---------------- следование за сообщением бота ----------------

/**
 * Нажали кнопку бота — включаем следование и берём строку под наблюдение.
 *
 * Наблюдение ставится СРАЗУ, не дожидаясь правки: клавиатура может
 * перерисоваться и без `message_edited` (например, бот дописал ряд в ту же
 * разметку), и рост строки нужно поймать в любом случае.
 */
function noteButtonPress(dialogId, messageId) {
    if (!noteFollow(dialogId, messageId, Date.now())) return;
    armFollowTimer();
    observeFollowRow(local.rows.get(Number(messageId)) || null);
}

/**
 * Перезаводит таймер истечения следования.
 *
 * Таймер обязателен не ради самого срока (его проверяет `followTarget` по
 * часам), а ради СНЯТИЯ наблюдателя: без него ResizeObserver висел бы на
 * строке до следующей смены диалога.
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
    return clearFollow();
}

/**
 * Наблюдатель размера — один на вкладку, создаётся лениво.
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
    if (!followTarget(Date.now())) {
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
 * Через `getBoundingClientRect`, а НЕ через `offsetTop`, по той же причине, что
 * и в [`scrollRowIntoView`]: у `#tg-log` нет своего `position`.
 *
 * @returns {?{top: number, bottom: number}} null ⇒ раскладка недоступна
 */
function rowOffset(row) {
    if (!row || !$tgLog) return null;
    if (typeof row.getBoundingClientRect !== 'function'
        || typeof $tgLog.getBoundingClientRect !== 'function') {
        return null;
    }
    const r = row.getBoundingClientRect();
    const log = $tgLog.getBoundingClientRect();
    return { top: r.top - log.top, bottom: r.bottom - log.top };
}

/**
 * Доскролл к концу блока строки (решение считает follow.js).
 *
 * Во время подгрузки истории вверх лента НЕ трогается: `loadOlder` держит якорь
 * между замером `scrollHeight` и восстановлением `scrollTop`, и посторонняя
 * прокрутка внутри этого окна необратимо теряет позицию чтения.
 */
function scrollBlockIntoView(row) {
    if (!$tgLog || local.loading) return;
    const top = scrollTargetForRow($tgLog, rowOffset(row), FOLLOW_SCROLL_PAD);
    if (top === null) return;
    const behavior = scrollBehavior(prefersReducedMotion());
    // `scrollTo` есть не везде (и не в node-стендах) — фолбэк обязателен,
    // иначе безусловный вызов уронил бы всю ленту.
    if (typeof $tgLog.scrollTo === 'function') {
        try {
            $tgLog.scrollTo({ top, behavior });
            return;
        } catch (_) { /* фолбэк ниже */ }
    }
    $tgLog.scrollTop = top;
}

/**
 * Что делать с лентой после того, как строка появилась или пересобралась.
 *
 * `stick`/`following` замеряются ВЫЗЫВАЮЩИМ до мутации DOM — после пересборки
 * строки `isAtBottom` считается уже по новой высоте и врёт.
 */
function afterRowChanged(row, ctx) {
    const decision = scrollDecision(ctx);
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
    el.className = 'tg-date-sep';
    el.textContent = dateSepLabel(key);
    el.dataset.dateSep = key;
    return el;
}

/**
 * Строка сообщения: `[HH:MM:SS] nick $ text`.
 *
 * Части — отдельные span'ы: так CSS красит время/ник/текст независимо, а
 * точечные апдейты (edited/deleted) правят только нужный кусок.
 */
function buildMessageRow(msg, dialog) {
    const li = document.createElement('div');
    li.className = 'tg-line' + (msg.out ? ' tg-line-own' : '');
    if (Number.isFinite(msg.id)) li.dataset.msgId = String(msg.id);
    li.dataset.date = String(msg.date || 0);

    // Цитата — ПЕРЕД временем и текстом: «на что отвечают» читается раньше
    // самого ответа. Отдельным блоком, а не span'ом: строка объявлена
    // `white-space: pre-wrap`, и инлайновая цитата встроилась бы в тот же ряд.
    const quote = buildQuoteRow(msg, dialog);
    if (quote) li.appendChild(quote);

    const time = document.createElement('span');
    time.className = 'tg-line-time';
    time.textContent = `[${formatTime(msg.date)}] `;

    const nick = document.createElement('span');
    nick.className = msg.out ? 'tg-line-nick-own' : 'tg-line-nick';
    nick.textContent = nickOf(msg, dialog);

    const sep = document.createElement('span');
    sep.className = 'tg-line-time';
    sep.textContent = ' $ ';

    const text = document.createElement('span');
    text.className = 'tg-line-text';
    text.textContent = messageText(msg);

    const suffix = document.createElement('span');
    suffix.className = 'tg-line-suffix';
    suffix.textContent = msg.edit_date ? ' (edited)' : '';

    li.appendChild(time);
    li.appendChild(nick);
    li.appendChild(sep);
    li.appendChild(text);
    li.appendChild(suffix);

    // Медиа рисуется КАРТОЧКОЙ под строкой-подписью: сама строка остаётся
    // терминальной («[photo] caption»), а превью, размер и кнопки живут в
    // отдельном блоке. Так лента читается как лог, но файлы можно открыть.
    const card = msg.media ? buildMediaCard(msg) : null;
    if (card) li.appendChild(card);

    // Инлайн-клавиатура бота — ПОД карточкой медиа: сначала содержимое
    // сообщения, потом то, что с ним можно сделать. Reply-клавиатура
    // (kind=="keyboard") сюда не попадает: она состояние диалога, а не
    // сообщения, и живёт полосой над полем ввода (см. botkb.js).
    const kb = buildMessageKeyboard(msg);
    if (kb) li.appendChild(kb);

    // Действия строки — ПОСЛЕДНИМИ и вне потока (CSS позиционирует абсолютно
    // саму обёртку): в потоке они разрывали бы перенос текста и налезали на
    // маркер «⋯ в пути» у неподтверждённых строк.
    //
    // Обёртка, а не две кнопки подряд: кнопка встречи меняет ширину каждую
    // секунду (таймер записи), и раскладывать соседей руками пришлось бы на
    // каждый тик. Порядок внутри — встреча, потом ответ: ответ остаётся у
    // правого края там же, где был до появления соседа.
    const actions = document.createElement('span');
    actions.className = 'tg-line-actions';
    li.appendChild(actions);
    const meetBtn = attachMeetButton(li, {
        panel: 'telegram',
        msg,
        chatId: msg.dialog_id,
        container: actions,
        root: $tgLog,
    });
    const replyBtn = buildReplyButton(msg, dialog);
    if (replyBtn) actions.appendChild(replyBtn);
    // Пустая обёртка не нужна никому: у строки без подтверждённого id нет ни
    // ответа, ни встречи, а невидимый бокс поверх текста — лишний узел.
    if (actions.childNodes.length === 0) actions.remove();

    li._tg = { text, suffix, msg, card, kb, quote, actions, meetBtn };
    return li;
}

/**
 * Строка цитаты над текстом: `sofia  привет` приглушённым.
 *
 * Возвращает `null`, если сообщение не является ответом. Недоступный оригинал
 * (удалён или не достался серверу) рисуется как обычная цитата с текстом
 * «сообщение недоступно» — пустая цитата выглядела бы как баг вёрстки.
 *
 * @param {object} msg — MessageDto
 * @param {?object} dialog — DialogDto
 * @returns {?HTMLElement}
 */
function buildQuoteRow(msg, dialog) {
    let reply = normalizeReplyTo(msg);
    if (!reply) return null;
    // Сообщения из потока апдейтов приходят с одним `message_id` (сеть в
    // hot-path'е воркера запрещена). Если оригинал уже лежит в ленте, превью
    // собирается локально — это бесплатно и снимает нужду в запросе за тем,
    // что и так в памяти вкладки.
    if (replyNeedsPreview(reply)) {
        const original = loadedMessage(msg.dialog_id, reply.messageId);
        if (original) reply = mergeReplyFromMessage(reply, original, dialog);
    }

    const el = document.createElement('div');
    el.className = 'tg-reply-quote';
    const name = document.createElement('span');
    name.className = 'tg-reply-quote-name';
    name.textContent = replyAuthorLabel(reply, dialog);
    const text = document.createElement('span');
    text.className = 'tg-reply-quote-text';
    text.textContent = replyPreviewText(reply);
    el.appendChild(name);
    el.appendChild(text);

    // Кликабельна только цитата с известным id: у ответа на сторис оригинала
    // не существует в природе, и «перейти» там некуда.
    if (reply.messageId !== null) {
        el.dataset.replyTo = String(reply.messageId);
        el.setAttribute('role', 'button');
        el.tabIndex = 0;
        el.setAttribute(
            'aria-label',
            `Перейти к сообщению${name.textContent ? ' от ' + name.textContent : ''}`
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
 * У строк без подтверждённого id (optimistic-отправка ещё в пути) кнопки нет:
 * Telegram отвергнет ответ на несуществующий message_id.
 *
 * @param {object} msg — MessageDto
 * @param {?object} dialog
 * @returns {?HTMLElement}
 */
function buildReplyButton(msg, dialog) {
    if (!msg || !Number.isFinite(msg.id) || msg.id <= 0) return null;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tg-reply-btn';
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

/**
 * Сообщение из уже загруженной ленты диалога по id.
 *
 * Поиск с конца: оригинал цитаты почти всегда рядом со своим ответом.
 *
 * @param {string|number} dialogId
 * @param {?number} messageId
 * @returns {?object}
 */
function loadedMessage(dialogId, messageId) {
    if (!Number.isFinite(messageId)) return null;
    const list = tgState.messages.get(String(dialogId)) || [];
    for (let i = list.length - 1; i >= 0; i -= 1) {
        if (list[i] && list[i].id === messageId) return list[i];
    }
    return null;
}

/**
 * Текст сообщения для ленты.
 *
 * Медиа в Фазе 3 ещё не рисуется карточкой, но проглатывать сообщение
 * нельзя: без плейсхолдера пустая строка выглядела бы как баг. Поэтому
 * «[photo]» / «[voice 7s]» плюс подпись, если она есть.
 */
function messageText(msg) {
    const text = typeof msg.text === 'string' ? msg.text : '';
    const media = msg.media;
    if (!media) return text;
    const dur = Number.isFinite(media.duration_secs) && media.duration_secs > 0
        ? ` ${media.duration_secs}s`
        : '';
    const name = media.file_name ? ` ${media.file_name}` : '';
    const tag = `[${media.kind || 'media'}${dur}${name}]`;
    return text ? `${tag} ${text}` : tag;
}

/** Системная строка `>> …`. */
export function systemLine(text) {
    if (!$tgLog) return null;
    const stick = isAtBottom($tgLog);
    const el = document.createElement('div');
    el.className = 'tg-line tg-line-system';
    el.textContent = `>> ${text}`;
    $tgLog.appendChild(el);
    if (stick) scrollToBottom();
    return el;
}

/** Строка ошибки (красная) с опциональным «retry». */
function errorLine(text, onRetry) {
    if (!$tgLog) return null;
    const el = document.createElement('div');
    el.className = 'tg-line tg-line-error';
    el.textContent = `>> ERR: ${text}`;
    if (typeof onRetry === 'function') {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'tg-retry-btn';
        btn.textContent = '[retry]';
        btn.addEventListener('click', () => {
            el.remove();
            onRetry();
        });
        el.appendChild(document.createTextNode(' '));
        el.appendChild(btn);
    }
    $tgLog.appendChild(el);
    scrollToBottom();
    return el;
}

/**
 * Рисует ленту с нуля из массива сообщений (по возрастанию id).
 *
 * Разделители дат вставляются между соседними сообщениями разных
 * календарных дней и перед самым первым — так начало страницы истории всегда
 * подписано датой.
 */
export function renderHistory(messages, dialog) {
    clearLog();
    // Карточки прошлого рендера больше не в DOM: их контроллеры и наблюдатели
    // превью надо снять, иначе download_progress пойдёт в осиротевшие узлы.
    resetMediaCards();
    if (!$tgLog) return;
    let prev = null;
    for (const msg of messages) {
        if (needsDateSep(prev, msg)) $tgLog.appendChild(buildDateSep(dayKey(msg.date)));
        const row = buildMessageRow(msg, dialog);
        $tgLog.appendChild(row);
        local.rows.set(msg.id, row);
        prev = msg;
    }
    // Reply-клавиатура пересчитывается ПО ВСЕЙ ленте, а не накапливается по
    // событиям: страница истории вверх приносит СТАРЫЕ сообщения, и актуальной
    // обязана остаться клавиатура из самого нижнего сообщения бота.
    applyHistoryKeyboard(messages);
    // Подсветка живёт по id, а не по узлу: переход к оригиналу сам вызывает
    // подгрузку истории, а та перерисовывает ленту с нуля — без этой строки
    // класс исчезал бы ровно в тот момент, когда пользователь его ждёт.
    if (local.flashId !== null) {
        const row = local.rows.get(local.flashId);
        if (row && row.classList) row.classList.add('tg-msg-highlight');
    }
    // По той же причине переезжает и наблюдение за ростом: строки пересозданы,
    // и наблюдатель, оставшийся на старом узле, молча замолчал бы навсегда.
    const follow = followTarget(Date.now());
    if (follow && follow.messageId) {
        observeFollowRow(local.rows.get(follow.messageId) || null);
    }
}

/**
 * Открывает диалог: грузит последнюю страницу и прокручивает вниз.
 *
 * @param {string} dialogId
 * @param {?object} dialog — DialogDto (для ников и шапки)
 */
export async function openDialog(dialogId, dialog) {
    const id = String(dialogId);
    const previous = tgState.activeDialogId;
    // Уходим из прошлого диалога — снимаем там «печатает…»: иначе собеседник
    // ещё несколько секунд видел бы, что мы набираем ему сообщение.
    if (local.typingDialogId && local.typingDialogId !== id) cancelTyping();
    tgState.activeDialogId = id;
    // Блобы прошлого диалога освобождаются СРАЗУ при переключении: иначе
    // двадцать переходов по диалогам с фотографиями остаются в памяти вкладки
    // до перезагрузки страницы (браузер не собирает objectURL сам).
    if (previous !== null && String(previous) !== id) revokeDialogMedia();
    // Следование принадлежит ПОКИНУТОМУ диалогу: тянуть новую ленту вниз из-за
    // кнопки, нажатой в другом чате, — чистая самодеятельность. Повторное
    // открытие того же диалога (кнопка [retry]) следование сохраняет.
    if (previous === null || String(previous) !== id) stopFollow();
    local.loading = true;
    local.hasMore = false;
    clearLog();
    // Клавиатура бота принадлежит ПРЕДЫДУЩЕМУ диалогу: пока грузится история,
    // показывать её нельзя — нажатие ушло бы не в тот чат. Состояние диалогов
    // при этом не стирается, renderHistory восстановит нужную из ленты.
    clearReplyKeyboard();
    // Цель ответа принадлежит ПОКИНУТОМУ диалогу: без сброса полоса врала бы
    // про то, кому мы отвечаем (сам reply.js ответ туда уже не отдаст, но
    // видимая полоса — это обещание пользователю).
    cancelReply();
    // Подсветка от прошлого перехода к оригиналу — тоже чужая.
    clearFlash();
    updateHead(dialog, 0);
    systemLine(`opening ${id}…`);

    try {
        const resp = await getMessages(id, { limit: PAGE_SIZE });
        // Пока грузили, пользователь мог переключить диалог — не рисуем
        // чужую историю поверх новой.
        if (tgState.activeDialogId !== id) return;
        const messages = Array.isArray(resp && resp.messages) ? resp.messages : [];
        local.hasMore = !!(resp && resp.has_more);
        tgState.messages.set(id, messages.slice());
        renderHistory(messages, dialog);
        updateHead(dialog, messages.length);
        scrollToBottom();
        const maxId = messages.length ? messages[messages.length - 1].id : 0;
        if (typeof local.onRead === 'function') local.onRead(id, maxId);
    } catch (e) {
        if (tgState.activeDialogId !== id) return;
        errorLine(`не удалось загрузить историю: ${e.message || e}`, () => {
            openDialog(id, dialog);
        });
    } finally {
        local.loading = false;
    }
}

/**
 * Обновляет шапку `SESSION ▸ …`.
 *
 * Присутствие собеседника берётся у dialogs.js через колбэк: в шапке
 * открытого диалога статус нужен чаще всего, а дублировать карту присутствия
 * во втором модуле — верный способ получить два расходящихся состояния.
 *
 * Шапка собирается ИЗ УЗЛОВ, а не одной строкой: @ник обязан быть
 * приглушённым, а покрасить кусок текста можно только отдельным элементом.
 * Итоговый `textContent` при этом совпадает с `sessionHeadText` символ в
 * символ — форма шапки описана в format.js ровно один раз.
 */
export function updateHead(dialog, count) {
    if (!$tgSessionHead) return;
    const presence = (dialog && typeof local.presenceOf === 'function')
        ? local.presenceOf(String(dialog.id))
        : null;
    const parts = sessionHeadParts(dialog, count, presence);
    $tgSessionHead.textContent = parts.name;
    if (parts.nick) {
        const nick = document.createElement('span');
        nick.className = 'tg-session-nick';
        nick.textContent = parts.nick;
        $tgSessionHead.appendChild(document.createTextNode(' '));
        $tgSessionHead.appendChild(nick);
    }
    if (parts.tail) $tgSessionHead.appendChild(document.createTextNode(parts.tail));
}

/**
 * Перерисовывает шапку по текущему состоянию (диалог тот же, изменилось
 * присутствие). Вызывается из main.js на WS-событие `online`.
 */
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
 */
export async function loadOlder(dialog) {
    const id = tgState.activeDialogId;
    if (!id || local.loading || !local.hasMore || !$tgLog) return;
    const before = oldestId(activeList());
    if (!Number.isFinite(before)) return;

    local.loading = true;
    const prevHeight = $tgLog.scrollHeight;
    const prevTop = $tgLog.scrollTop;
    try {
        const resp = await getMessages(id, { limit: PAGE_SIZE, beforeId: before });
        if (tgState.activeDialogId !== id) return;
        const older = Array.isArray(resp && resp.messages) ? resp.messages : [];
        local.hasMore = !!(resp && resp.has_more);
        if (!older.length) return;

        const merged = prependMessages(older, activeList());
        tgState.messages.set(id, merged);
        renderHistory(merged, dialog);
        updateHead(dialog, merged.length);
        // Якорь: то, что читал пользователь, остаётся под тем же курсором.
        $tgLog.scrollTop = prevTop + ($tgLog.scrollHeight - prevHeight);
    } catch (e) {
        console.debug('[tg-chat] loadOlder failed', e);
    } finally {
        local.loading = false;
    }
}

/**
 * Подписка на скролл: подгрузка вверх у верхней границы и отметка о
 * прочтении у нижней.
 *
 * «Докрутил до низа» — законный триггер прочтения: пользователь дочитал
 * ленту глазами. Повторных запросов это не плодит — решение принимает
 * `scheduleRead` в main.js (debounce + память о последнем отмеченном id).
 */
export function bindScroll() {
    if (!$tgLog || local.bound) return;
    local.lastScrollTop = Number($tgLog.scrollTop) || 0;
    $tgLog.addEventListener('scroll', () => {
        const top = Number($tgLog.scrollTop) || 0;
        // Своя прокрутка вверх важнее автоматики: пользователь передумал ждать
        // ответ бота и ушёл читать историю. Решение — по направлению движения
        // (см. follow.js::cancelledByScroll), собственные доскроллы идут вниз
        // и под порог не попадают.
        if (cancelledByScroll(local.lastScrollTop, top)) stopFollow();
        const atBottom = isAtBottom($tgLog);
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
        if (!shouldLoadOlder($tgLog, { hasMore: local.hasMore, loading: local.loading })) return;
        const dialog = currentDialog();
        loadOlder(dialog);
    });
}

/** Лента докручена до низа — сообщаем «просмотрено до последнего id». */
function noteReachedBottom() {
    if (typeof local.onRead !== 'function') return;
    const id = tgState.activeDialogId;
    if (!id) return;
    const list = activeList();
    if (!list.length) return;
    local.onRead(String(id), list[list.length - 1].id);
}

/**
 * Диалог, открытый сейчас.
 *
 * Ищется в tgState (а не хранится копией): список мог перезагрузиться, и
 * устаревшая ссылка давала бы старый title в шапке и никах.
 */
function currentDialog() {
    const id = tgState.activeDialogId;
    if (!id) return null;
    return (tgState.dialogs || []).find((d) => String(d.id) === String(id)) || null;
}

// ---------------- отправка ----------------

/** Подписка на prompt: Enter, кнопка TRANSMIT и индикатор набора. */
export function bindPrompt() {
    if (local.bound) return;
    local.bound = true;
    if ($tgPrompt) {
        // submit покрывает и Enter в input, и клик по кнопке TRANSMIT.
        $tgPrompt.addEventListener('submit', (e) => {
            e.preventDefault();
            submitPrompt();
        });
    } else if ($tgSend) {
        $tgSend.addEventListener('click', (e) => {
            e.preventDefault();
            submitPrompt();
        });
    }
    if ($tgInput) {
        // Набор текста → «печатает…» собеседнику. Очистка поля (в том числе
        // Backspace до пустоты) гасит статус досрочно.
        $tgInput.addEventListener('input', () => {
            if (String($tgInput.value || '').trim()) noteTyping();
            else cancelTyping();
        });
    }
}

function submitPrompt() {
    if (!$tgInput) return;
    const text = String($tgInput.value || '');
    if (!text.trim()) return;
    $tgInput.value = '';
    // Сообщение ушло — статус набора больше не соответствует правде.
    cancelTyping();
    send(text);
}

// ---------------- ответы (reply) ----------------

/**
 * Навешивает обработчики ответов: двойной клик по строке, клик по цитате и
 * Esc.
 *
 * Всё делегировано на `#tg-log`, а не навешано на строки: лента
 * перерисовывается целиком при каждой подгрузке истории, и построчные
 * слушатели пришлось бы снимать вручную (а забытые — держали бы в памяти
 * мёртвые узлы). Исключение — кнопка «ответить»: у неё всё равно есть
 * собственный DOM-узел, и свой слушатель там дешевле.
 */
function bindReplyUi() {
    if (local.replyBound) return;
    local.replyBound = true;
    if ($tgLog) {
        $tgLog.addEventListener('dblclick', onLogDblClick);
        $tgLog.addEventListener('click', onLogClick);
        $tgLog.addEventListener('keydown', onLogKeyDown);
    }
    // Esc — слоем в общем реестре (`messenger/esc-stack.js`), а не своим
    // слушателем на документе: там же слушают попап анализа и карточка встречи,
    // и одно нажатие снимало полосу цитаты ЗАОДНО с закрытием карточки, стирая
    // набранный контекст ответа (forge-o0sdg). Полоса — самый нижний слой:
    // любое открытое окно забирает Esc себе.
    if (typeof document !== 'undefined' && document.addEventListener) {
        registerEscLayer({
            name: 'tg-reply-bar',
            depth: ESC_DEPTH.REPLY_BAR,
            isOpen: replyEscapeArmed,
            close: onReplyEscape,
        });
    }
}

/** Двойной клик по строке — начать ответ на неё. */
function onLogDblClick(e) {
    if (classifyRowClick(e.target) !== 'reply') return;
    const row = e.target.closest('.tg-line');
    // Системные строки, ошибки и заглушки удалённых сообщений `_tg` не несут.
    if (!row || !row._tg || !row._tg.msg) return;
    startReply(row._tg.msg, currentDialog());
}

/** Клик по цитате — перейти к оригиналу. */
function onLogClick(e) {
    const id = quoteTargetId(e.target);
    if (id === null) return;
    e.preventDefault();
    jumpToMessage(id);
}

/** Enter/Space на цитате — то же, что клик (цитата в таб-порядке). */
function onLogKeyDown(e) {
    if (e.key !== 'Enter' && e.key !== ' ' && e.key !== 'Spacebar') return;
    const id = quoteTargetId(e.target);
    if (id === null) return;
    // Пробел на цитате иначе прокрутил бы ленту на экран вниз.
    e.preventDefault();
    jumpToMessage(id);
}

/** message_id оригинала, если событие пришло из кликабельной цитаты. */
function quoteTargetId(target) {
    if (!target || typeof target.closest !== 'function') return null;
    const quote = target.closest('.tg-reply-quote');
    if (!quote || !quote.dataset) return null;
    const id = Number(quote.dataset.replyTo);
    return Number.isFinite(id) && id > 0 ? id : null;
}

/**
 * Готова ли полоса цитаты принять Esc — условие слоя в реестре.
 *
 * Гард на лайтбокс обязателен: лайтбокс реестру не известен и слушает Esc сам,
 * поэтому при открытой картинке полоса объявляет себя ЗАКРЫТОЙ и нажатие
 * достаётся лайтбоксу. Без этого одно нажатие снимало бы цитату вместо закрытия
 * картинки.
 */
function replyEscapeArmed() {
    if ($tgLightbox && !$tgLightbox.hidden) return false;
    return !!replyTarget(tgState.activeDialogId);
}

/**
 * Esc снимает выбранный ответ.
 *
 * Зовётся реестром слоёв Esc — и только когда полоса действительно верхняя:
 * `preventDefault` и выбор окна остались на его стороне.
 */
function onReplyEscape() {
    if (!replyTarget(tgState.activeDialogId)) return;
    cancelReply();
    if ($tgInput) {
        try { $tgInput.focus(); } catch (_) { /* поля может не быть */ }
    }
}

/**
 * Выбирает сообщение целью ответа и показывает полосу.
 *
 * @param {object} msg — MessageDto
 * @param {?object} [dialog]
 * @returns {boolean}
 */
export function startReply(msg, dialog) {
    const id = tgState.activeDialogId;
    if (!id) return false;
    const target = buildReplyTarget(id, msg, dialog || currentDialog());
    if (!target) return false;
    setReplyTarget(target);
    showReplyBar(target);
    // Фокус уходит в поле ввода: выбор ответа — это начало набора, и лишний
    // клик по полю здесь был бы чистой потерей.
    if ($tgInput) {
        try { $tgInput.focus(); } catch (_) { /* поля может не быть */ }
    }
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
 * @param {?number} messageId
 * @returns {boolean}
 */
function cancelReplyTo(dialogId, messageId) {
    if (!Number.isFinite(messageId)) return false;
    const target = replyTarget(dialogId);
    if (!target || target.messageId !== messageId) return false;
    return cancelReply();
}

/**
 * Создаёт (при первой надобности) полосу ответа над `#tg-prompt`.
 *
 * Узел создаётся в JS, а не в index.html: у пользователя, который ни разу не
 * отвечал на конкретное сообщение, пустой div в разметке жил бы всегда.
 *
 * Позиция перепроверяется на КАЖДЫЙ показ: полоса reply-клавиатуры бота
 * (botkb.js) вставляется тем же `insertBefore($tgPrompt)` и лениво — бот может
 * прислать клавиатуру уже после выбора ответа и вклиниться между полосой и
 * формой. Полоса ответа обязана оставаться вплотную к полю ввода: она про то,
 * чем мы сейчас пишем.
 */
function ensureReplyBar() {
    if (typeof document === 'undefined' || !$tgPrompt || !$tgPrompt.parentNode) return null;
    if (!local.replyBar) {
        const bar = document.createElement('div');
        bar.className = 'tg-reply-bar';
        bar.hidden = true;

        const name = document.createElement('span');
        name.className = 'tg-reply-bar-name';
        const text = document.createElement('span');
        text.className = 'tg-reply-bar-text';

        const close = document.createElement('button');
        close.type = 'button';
        close.className = 'tg-reply-cancel';
        close.textContent = '✕';
        close.title = 'Отменить ответ (Esc)';
        close.setAttribute('aria-label', 'Отменить ответ');
        close.addEventListener('click', () => {
            cancelReply();
            if ($tgInput) {
                try { $tgInput.focus(); } catch (_) { /* поля может не быть */ }
            }
        });

        bar.appendChild(name);
        bar.appendChild(text);
        bar.appendChild(close);
        local.replyBar = bar;
        local.replyBarName = name;
        local.replyBarText = text;
    }
    if ($tgPrompt.previousElementSibling !== local.replyBar) {
        $tgPrompt.parentNode.insertBefore(local.replyBar, $tgPrompt);
    }
    return local.replyBar;
}

/**
 * Показывает полосу с именем автора и превью.
 *
 * Placeholder поля ввода НЕ трогаем: им владеет полоса клавиатуры бота
 * (botkb.js::setInputPlaceholder), и подмена затёрла бы подсказку бота.
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
 * @param {number} messageId
 */
async function jumpToMessage(messageId) {
    if (local.jumping) return;
    const dialogId = tgState.activeDialogId ? String(tgState.activeDialogId) : '';
    if (!dialogId) return;
    // Переход к оригиналу уезжает ВВЕРХ — прямо против следования. Живое
    // следование вернуло бы ленту вниз, и клик по цитате выглядел бы как
    // «ничего не произошло».
    stopFollow();
    if (revealMessage(messageId)) return;

    local.jumping = true;
    try {
        for (let i = 0; i < REPLY_JUMP_PAGES && local.hasMore; i += 1) {
            await loadOlder(currentDialog());
            // Пока грузили, пользователь мог уйти в другой чат.
            if (String(tgState.activeDialogId) !== dialogId) return;
            if (revealMessage(messageId)) return;
        }
        // Обёртка могла не приехать со старым кэшем api.js — тогда честно
        // говорим, что дальше не пошли, вместо тихого игнора клика.
        const fetchMessage = typeof tgApi.getMessage === 'function' ? tgApi.getMessage : null;
        if (!fetchMessage) {
            systemLine(`сообщение #${messageId} не найдено в загруженной истории`);
            return;
        }
        const dto = await fetchMessage(dialogId, messageId);
        if (String(tgState.activeDialogId) !== dialogId) return;
        if (!dto || !Number.isFinite(dto.id)) {
            systemLine(`${REPLY_GONE_PREFIX} #${messageId}`);
            return;
        }
        systemLine(`↳ ${replySourceLine(dto, currentDialog())}`);
    } catch (e) {
        systemLine(e && e.code === 'unknown_message'
            ? `${REPLY_GONE_PREFIX} #${messageId} (удалено)`
            : `не удалось открыть #${messageId}: ${(e && e.message) || e}`);
    } finally {
        local.jumping = false;
    }
}

/**
 * Прокручивает к строке и подсвечивает её.
 *
 * @param {number} messageId
 * @returns {boolean} была ли строка в ленте
 */
function revealMessage(messageId) {
    const row = local.rows.get(messageId);
    if (!row) return false;
    scrollRowIntoView(row);
    flashRow(messageId);
    return true;
}

/**
 * Прокручивает ленту так, чтобы строка оказалась по центру.
 *
 * Через `getBoundingClientRect`, а НЕ через `offsetTop`: у `#tg-log` нет
 * своего `position`, и смещения его детей считаются от `#telegram` — прямой
 * `scrollTop = row.offsetTop` промахнулся бы на высоту титульбара и шапки
 * сессии.
 */
function scrollRowIntoView(row) {
    if (!$tgLog || !row) return;
    if (typeof row.getBoundingClientRect !== 'function'
        || typeof $tgLog.getBoundingClientRect !== 'function') {
        if (typeof row.scrollIntoView === 'function') row.scrollIntoView();
        return;
    }
    const r = row.getBoundingClientRect();
    const log = $tgLog.getBoundingClientRect();
    const centered = (r.top - log.top) - ($tgLog.clientHeight - r.height) / 2;
    $tgLog.scrollTop += centered;
}

/**
 * Кратковременно подсвечивает строку.
 *
 * Подсвеченная строка запоминается по id: renderHistory вернёт класс после
 * любой перерисовки (см. там), а таймер снимет его один раз в конце.
 */
function flashRow(messageId) {
    clearFlash();
    local.flashId = messageId;
    const row = local.rows.get(messageId);
    if (row && row.classList) row.classList.add('tg-msg-highlight');
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
    if (local.flashId === null) return;
    const row = local.rows.get(local.flashId);
    if (row && row.classList) row.classList.remove('tg-msg-highlight');
    local.flashId = null;
}

/**
 * Сообщает Telegram, что мы печатаем — не чаще одного запроса в 4с.
 *
 * Троттлинг не косметика: Telegram и так держит статус ~6с, а частые вызовы
 * `messages.setTyping` — прямой путь к FLOOD_WAIT на аккаунте. Ошибки
 * глотаются молча (в том числе `429`): из-за индикатора набора UI ломаться
 * не должен, и показывать пользователю ошибку, которой он не вызывал, незачем.
 */
export function noteTyping() {
    const id = tgState.activeDialogId ? String(tgState.activeDialogId) : null;
    if (!id) return false;
    const now = Date.now();
    if (!typingAllowed(local.typingSentAt, now, local.typingDialogId === id)) return false;
    local.typingSentAt = now;
    local.typingDialogId = id;
    sendTyping(id, 'typing').catch(() => { /* не критично, см. doc-комментарий */ });
    return true;
}

/**
 * Досрочно снимает статус набора.
 *
 * Зовётся при отправке сообщения, очистке поля, смене диалога и из
 * upload.js (текст ушёл подписью к файлу). Идемпотентна: без активного
 * статуса ничего не делает и в сеть не ходит.
 */
export function cancelTyping() {
    const id = local.typingDialogId;
    local.typingDialogId = null;
    local.typingSentAt = 0;
    if (!id) return false;
    sendTyping(id, 'cancel').catch(() => { /* не критично */ });
    return true;
}

/**
 * Отправка с optimistic-рендером.
 *
 * 1. строка появляется сразу, с классом `.tg-msg-pending` («⋯ в пути»);
 * 2. ответ POST несёт РЕАЛЬНЫЙ MessageDto — им строка заменяется (reconcile);
 * 3. если Telegram не подтвердил id (`id <= 0`), строка снимается: настоящую
 *    дорисует `new_message` из WS — иначе в ленте осталась бы строка с
 *    неизвестным id, которую нельзя ни отредактировать, ни дедуплицировать;
 * 4. ошибка сети — красная строка с рабочим [retry], а pending-строка
 *    убирается (висящая «в пути» строка врала бы про отправку).
 *
 * Ответ на сообщение: цель берётся из активной полосы, но фиксируется ОДИН раз
 * — в локальную переменную и в замыкание [retry]. Иначе повтор после ошибки
 * ушёл бы туда, куда полоса указывает СЕЙЧАС (или вообще без ответа, если её
 * успели снять), а это уже другое сообщение в чужую ветку.
 *
 * @param {string} text
 * @param {?number} [replyToId] — `undefined` ⇒ взять цель из полосы,
 *   `null` ⇒ отправить БЕЗ ответа (так повтор не подхватывает чужую цель)
 */
export async function send(text, replyToId) {
    const id = tgState.activeDialogId;
    if (!id) {
        systemLine('no dialog selected');
        return;
    }
    const dialog = currentDialog();
    const target = replyTarget(id);
    const replyId = replyToId === undefined
        ? (target ? target.messageId : null)
        : (Number.isFinite(replyToId) && replyToId > 0 ? replyToId : null);
    // Цитату в optimistic-строке рисуем только если отвечаем на ТУ САМУЮ цель,
    // что сейчас в полосе: у повтора превью уже неоткуда взять, там останется
    // «…», и её дособерёт reconcile ответом сервера.
    const replyDto = (target && target.messageId === replyId)
        ? replyDtoFromTarget(target)
        : null;
    local.tempSeq += 1;
    const tempId = `tmp-${Date.now()}-${local.tempSeq}`;
    const optimistic = {
        id: null,
        dialog_id: id,
        out: true,
        date: Math.floor(Date.now() / 1000),
        sender: tgState.selfUser || null,
        text,
        media: null,
        reply_to_id: replyId,
        reply_to: replyDto,
        edit_date: null,
    };
    const row = buildMessageRow(optimistic, dialog);
    row.classList.add('tg-msg-pending');
    row.dataset.tempId = tempId;
    if ($tgLog) $tgLog.appendChild(row);
    local.pending.set(tempId, { li: row, text });
    scrollToBottom();

    try {
        const payload = { text };
        if (replyId !== null) payload.replyToId = replyId;
        const resp = await sendMessage(id, payload);
        const dto = resp && resp.message;
        local.pending.delete(tempId);
        // Полоса снимается ТОЛЬКО после подтверждения отправки: при ошибке
        // пользователь жмёт [retry], и цель обязана быть на месте. Сбрасываем
        // адресно — за время запроса он мог выбрать уже другое сообщение.
        cancelReplyTo(id, replyId);
        if (!dto || !Number.isFinite(dto.id) || dto.id <= 0) {
            // Сообщение ушло, но id неизвестен: ждём его из updates.
            row.remove();
            return;
        }
        // Диалог могли переключить, пока летел запрос: сообщение всё равно
        // надо запомнить в его ленте, но DOM трогать нельзя.
        const list = tgState.messages.get(String(dto.dialog_id)) || [];
        const { inserted } = insertMessage(list, dto);
        tgState.messages.set(String(dto.dialog_id), list);
        if (String(dto.dialog_id) !== String(tgState.activeDialogId)) {
            row.remove();
            return;
        }
        if (!inserted) {
            // Апдейт опередил ответ POST — строка уже нарисована.
            row.remove();
            return;
        }
        const real = buildMessageRow(dto, dialog);
        row.replaceWith(real);
        local.rows.set(dto.id, real);
        updateHead(dialog, list.length);
        scrollToBottom();
    } catch (e) {
        local.pending.delete(tempId);
        row.remove();
        errorLine(`не отправлено: ${e.message || e}`, () => send(text, replyId));
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
    const id = String(msg.dialog_id);
    const list = tgState.messages.get(id) || [];
    const { inserted, index } = insertMessage(list, msg);
    tgState.messages.set(id, list);
    if (!inserted) return false;
    // Reply-клавиатура диалога обновляется ДО проверки «диалог открыт»: бот
    // мог прислать её в чат, который сейчас не на экране, и при возврате туда
    // она обязана быть уже актуальной.
    noteReplyMarkup(msg);
    if (String(tgState.activeDialogId) !== id || !$tgLog) return false;

    const dialog = currentDialog();
    // Замер ДО вставки строки: после appendChild высота ленты уже другая.
    const stick = isAtBottom($tgLog);
    // Бот ответил на нажатие НОВЫМ сообщением, а не правкой старого — это тот
    // же результат нажатия, и уехать к нему пользователь должен так же.
    const following = isFollowing(id, Date.now());
    const row = buildMessageRow(msg, dialog);
    const prev = index > 0 ? list[index - 1] : null;

    if (index === list.length - 1) {
        if (needsDateSep(prev, msg)) $tgLog.appendChild(buildDateSep(dayKey(msg.date)));
        $tgLog.appendChild(row);
    } else {
        // Сообщение «из середины» (пришло вне порядка после catch_up) —
        // вставляем перед следующим по id, иначе лента разъедется с моделью.
        const nextRow = local.rows.get(list[index + 1].id);
        if (nextRow) $tgLog.insertBefore(row, nextRow);
        else $tgLog.appendChild(row);
    }
    local.rows.set(msg.id, row);
    updateHead(dialog, list.length);
    afterRowChanged(row, { atBottom: stick, following });
    return true;
}

/**
 * WS `message_edited` — обновить текст и приписать «(edited)».
 *
 * Если в сообщении есть (или было) медиа, строка пересобирается ЦЕЛИКОМ:
 * сервер в этот момент уже выбросил файл из своего кэша
 * (`media_cache::invalidate`), а наши objectURL'ы указывают на старое
 * содержимое — точечной правкой текста тут не обойтись.
 *
 * То же и с клавиатурой бота: типовой сценарий inline-кнопок — «нажал →
 * бот отредактировал сообщение и прислал другие кнопки». Без пересборки
 * строки в ленте навсегда осталась бы устаревшая клавиатура, и дефект
 * выглядел бы как «кнопки не реагируют».
 *
 * @param {object} msg — MessageDto
 */
export function onEdited(msg) {
    if (!msg || !Number.isFinite(msg.id)) return;
    const id = String(msg.dialog_id);
    const list = tgState.messages.get(id) || [];
    const idx = list.findIndex((m) => m.id === msg.id);
    if (idx >= 0) list[idx] = msg;
    noteReplyMarkup(msg);
    const row = local.rows.get(msg.id);
    if (!row || !row._tg) return;

    // Строка ищется по одному лишь `message_id`, а он в разных чатах
    // повторяется свободно: в каналах и супергруппах нумерация своя и
    // начинается с единицы. Без явной сверки диалога правка из СОСЕДНЕГО чата
    // подменяла бы текст открытой ленты (и дёргала бы её прокрутку).
    if (String(tgState.activeDialogId) !== id) return;

    // Замер ОБЯЗАН быть здесь — до любой мутации DOM. После `replaceWith`
    // высота строки уже новая, и `isAtBottom` ответит про ленту, которой
    // пользователь ещё не видел.
    const stick = isAtBottom($tgLog);
    const following = followsMessage(id, msg.id, Date.now());

    if (row._tg.card || msg.media || row._tg.kb || msg.reply_markup) {
        dropMessageMedia(id, msg.id);
        const fresh = buildMessageRow(msg, currentDialog());
        fresh._tg.suffix.textContent = ' (edited)';
        row.replaceWith(fresh);
        local.rows.set(msg.id, fresh);
        afterRowChanged(fresh, { atBottom: stick, following });
        return;
    }
    row._tg.text.textContent = messageText(msg);
    row._tg.suffix.textContent = ' (edited)';
    row._tg.msg = msg;
    afterRowChanged(row, { atBottom: stick, following });
}

/**
 * WS `message_deleted` — заменить строки заглушкой.
 *
 * `dialog_id` бывает ПУСТЫМ: для личных чатов Telegram не сообщает диалог
 * (см. updates.rs::deleted_event). Тогда чистим по id везде, где нашли —
 * message_id в ЛС уникален в пределах аккаунта, так что промаха не будет.
 *
 * @param {{dialog_id: string, message_ids: Array<number>}} evt
 */
export function onDeleted(evt) {
    const ids = (evt && Array.isArray(evt.message_ids)) ? evt.message_ids : [];
    const dialogId = evt && evt.dialog_id ? String(evt.dialog_id) : '';
    for (const id of ids) {
        if (dialogId) {
            dropFromList(dialogId, id);
            dropMessageMedia(dialogId, id);
        } else {
            for (const key of tgState.messages.keys()) {
                dropFromList(key, id);
                // Диалог неизвестен (удаление в ЛС) — чистим блобы по всем
                // загруженным диалогам: ключ включает id сообщения, промаха нет.
                dropMessageMedia(key, id);
            }
        }
        // Отвечать на удалённое нечем: Telegram отверг бы такой reply_to_id, а
        // пользователь до последнего видел бы полосу «отвечаю Васе».
        cancelReplyTo(tgState.activeDialogId, id);
        if (local.flashId === id) clearFlash();
        // Следовать больше не за чем: сообщение с кнопкой стёрли.
        if (followsMessage(dialogId || tgState.activeDialogId, id, Date.now())) stopFollow();
        const row = local.rows.get(id);
        if (!row) continue;
        // Строка переиспользуется как заглушка — наблюдать её размер незачем.
        if (local.followRow === row) unobserveFollowRow();
        row.className = 'tg-line tg-line-system tg-line-deleted';
        row.textContent = '<< message purged >>';
        row._tg = null;
        local.rows.delete(id);
    }
}

function dropFromList(dialogId, msgId) {
    const list = tgState.messages.get(String(dialogId));
    if (!list) return;
    const idx = list.findIndex((m) => m.id === msgId);
    if (idx >= 0) list.splice(idx, 1);
}

/**
 * WS `read` с `direction: "out"` — собеседник прочитал наши сообщения.
 *
 * В терминальном UI галочек нет, поэтому одна системная строка на событие
 * (и только для открытого диалога, чтобы не засорять ленту).
 */
export function onRead(evt) {
    if (!evt || evt.direction !== 'out') return;
    if (String(evt.dialog_id) !== String(tgState.activeDialogId)) return;
    systemLine(`peer read up to #${evt.max_id}`);
}

/** Полностью сбрасывает ленту (logout, смена аккаунта). */
export function resetChat() {
    clearLog();
    // Блобы прошлого аккаунта обязаны исчезнуть вместе с лентой: иначе после
    // logout скачанные файлы остаются доступны по objectURL.
    resetMediaCards();
    revokeDialogMedia();
    // Клавиатуры ботов прошлого аккаунта — тоже. Плюс гасятся таймеры
    // обратного отсчёта flood_wait, иначе они тикали бы в осиротевшие узлы.
    resetKeyboards();
    // Цель ответа и подсветка принадлежат ленте прошлого аккаунта.
    cancelReply();
    clearFlash();
    // Вместе с таймером следования: живой таймер держал бы вкладку (и node-тест)
    // и стрелял бы в ленту, которой больше нет.
    stopFollow();
    local.hasMore = false;
    local.loading = false;
    local.jumping = false;
    local.lastScrollTop = 0;
    // Пустая лента — это лента у низа: следующий открытый диалог обязан
    // приехать вниз, а не унаследовать «пользователь читал историю».
    local.stickBottom = true;
    updateHead(null, 0);
}

/**
 * Полная остановка ленты при уходе страницы (main.js::teardownTelegram).
 *
 * Отдельно от [`resetChat`]: тот чистит ДАННЫЕ аккаунта и оставляет вкладку
 * рабочей, а здесь надо ещё и распустить ResizeObserver — иначе он переживёт
 * страницу вместе со ссылками на её узлы.
 */
export function teardownChat() {
    stopFollow();
    if (local.followObserver) {
        try { local.followObserver.disconnect(); } catch (_) { /* уже распущен */ }
        local.followObserver = null;
    }
    clearFlash();
}

/** Снимок для e2e/диагностики. */
export function _debugChatState() {
    const target = replyTarget(tgState.activeDialogId);
    const follow = _debugFollowState();
    return {
        activeDialogId: tgState.activeDialogId,
        rendered: local.rows.size,
        pending: local.pending.size,
        hasMore: local.hasMore,
        loading: local.loading,
        messages: (tgState.messages.get(String(tgState.activeDialogId)) || []).length,
        replyToId: target ? target.messageId : null,
        replyBarVisible: !!(local.replyBar && !local.replyBar.hidden),
        flashId: local.flashId,
        // Следование за сообщением бота: цель, живо ли окно и висит ли
        // наблюдатель размера. Без этого срок и адресность следования пришлось
        // бы выводить только по побочным эффектам прокрутки.
        followDialogId: follow.dialogId,
        followMessageId: follow.messageId,
        followActive: !!followTarget(Date.now()),
        followObserving: !!local.followRow,
        // Защёлка «лента прижата к низу»: по ней видно, догонит ли лента
        // отложенный рост превью (см. [`onMediaGrew`]).
        stickBottom: local.stickBottom,
    };
}
