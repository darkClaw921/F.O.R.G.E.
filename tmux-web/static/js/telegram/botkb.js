// tmux-web — Telegram plugin: клавиатуры ботов (inline + reply).
//
// Боты присылают вместе с сообщением `reply_markup` (MessageDto.reply_markup,
// контракт см. plugins/telegram/src/dto.rs::ReplyMarkupDto). Их две принципиально
// разных разновидности, и путать их нельзя:
//
//   inline  — клавиатура ПОД КОНКРЕТНЫМ сообщением. Живёт ровно столько,
//             сколько живёт сообщение; нажатие уходит в Telegram как
//             `messages.getBotCallbackAnswer(peer, msg_id, data)`.
//   keyboard/hide/force_reply — СОСТОЯНИЕ ДИАЛОГА, а не свойство сообщения.
//             Официальные клиенты держат такую клавиатуру над полем ввода до
//             тех пор, пока бот не пришлёт другую (или `hide`). Поэтому она
//             рисуется полосой над `#tg-prompt`, а не под строкой лога:
//             иначе после десяти сообщений в ленте висело бы десять
//             одинаковых клавиатур, и ни одна не была бы «текущей».
//
// Ключевые решения:
//
// * Чистая логика (нормализация разметки, классификация кнопки, разбор ответа
//   бота, валидация URL) вынесена в начало файла и НЕ трогает DOM — она
//   покрыта node-тестом tmux-web/tests/frontend/telegram_botkb.test.mjs.
//   Рисование — во второй половине, оно проверяется прогоном в браузере.
// * КРИТИЧНО — текст кнопки приходит от ЧУЖОГО бота. Только `textContent`,
//   никакого `innerHTML`. Скобки `[ … ]` рисует CSS (`::before`/`::after`),
//   поэтому в текстовом узле лежит ровно подпись кнопки и ничего больше.
// * КРИТИЧНО — URL кнопки тоже приходит от чужого бота. `javascript:`-ссылка
//   в `window.open` — это XSS с правами вкладки. Схема проверяется белым
//   списком (http/https/tg) ПОСЛЕ вычистки управляющих символов: браузер
//   трактует "java\0script:" как `javascript:`, а наивная регулярка — как
//   «схемы нет».
// * Повторные нажатия callback-кнопки блокируются на время запроса: Telegram
//   считает каждое нажатие обращением к боту, и дребезг мыши — прямой путь к
//   FLOOD_WAIT на аккаунте пользователя.
// * `429 flood_wait` НИКОГДА не ретраится сам. Кнопка гаснет с обратным
//   отсчётом, повтор — только явным кликом пользователя (тот же класс риска,
//   что у login/start в api.js).

import { $tgInput, $tgPrompt } from '../core/dom.js';
import { notify } from '../echo/notifications.js';

import { sendCallback } from './api.js';
import { tgState } from './state.js';

// ---------------------------------------------------------------------------
// Константы
// ---------------------------------------------------------------------------

/** Максимум видимых символов подписи кнопки (code points, не байты). */
export const BUTTON_TEXT_MAX = 48;

/** Максимум символов в placeholder'е поля ввода. */
export const PLACEHOLDER_MAX = 64;

/**
 * Потолки размера клавиатуры.
 *
 * Telegram ограничивает разметку своими лимитами, но пришла она от чужого
 * бота через WS — доверять её размеру нельзя. Без потолка «клавиатура» из
 * десяти тысяч рядов подвесила бы вкладку на рендере.
 */
export const MAX_ROWS = 32;
export const MAX_ROW_BUTTONS = 12;

/** Максимум символов ответа бота в тосте. */
export const ANSWER_TEXT_MAX = 400;

/** Виды контейнеров разметки (контракт dto.rs::ReplyMarkupDto.kind). */
const MARKUP_KINDS = new Set(['inline', 'keyboard', 'hide', 'force_reply']);

/** Виды кнопок (контракт dto.rs::ReplyButtonDto.kind). */
const BUTTON_KINDS = new Set([
    'callback', 'url', 'switch_inline', 'switch_inline_current', 'text',
    'request_phone', 'request_location', 'request_poll', 'web_view', 'buy',
    'game', 'login_url', 'user_profile', 'other',
]);

/**
 * Схемы, которые разрешено открывать из кнопки бота.
 *
 * `tg:` оставлен намеренно: это deep-link в сам Telegram (профиль, чат,
 * приглашение), браузер отдаст его установленному приложению. Всё остальное —
 * `javascript:`, `data:`, `blob:`, `file:`, `vbscript:` — запрещено.
 */
const ALLOWED_SCHEMES = new Set(['http', 'https', 'tg']);

/**
 * Почему кнопка не работает в v1 — по видам.
 *
 * Причина показывается в `title` кнопки и в тосте по клику: молча
 * проглатывать нажатие нельзя, пользователь решит, что вкладка сломана.
 */
const UNSUPPORTED_REASON = {
    request_phone: 'отправка номера телефона не поддерживается в v1',
    request_location: 'отправка геопозиции не поддерживается в v1',
    request_poll: 'создание опросов не поддерживается в v1',
    web_view: 'мини-приложения (Web App) не поддерживаются в v1',
    buy: 'оплата не поддерживается в v1',
    game: 'игры не поддерживаются в v1',
    other: 'этот тип кнопки не поддерживается в v1',
};

/** Placeholder поля ввода по умолчанию (см. index.html — он значащий). */
const DEFAULT_PLACEHOLDER = ' ';

// ---------------------------------------------------------------------------
// Чистая логика (без DOM) — покрыта node-тестом
// ---------------------------------------------------------------------------

/**
 * Вычищает подпись кнопки и обрезает её по числу СИМВОЛОВ.
 *
 * Обрезка идёт по code points (`Array.from`), а не по `String.slice`: у
 * `slice` единица — UTF-16 code unit, и обрыв ровно между суррогатами даёт
 * «половину эмодзи» — невалидную строку, которую браузер рисует как U+FFFD.
 *
 * Вычищаются управляющие символы и bidi-переопределения: бот может прислать
 * U+202E (RIGHT-TO-LEFT OVERRIDE) и «перевернуть» остаток строки — визуальный
 * спуфинг подписи, из-за которого кнопка «удалить» читается как «оставить».
 * ZWJ (U+200D) СОХРАНЁН: без него составные эмодзи (семья, флаги профессий)
 * рассыпались бы на отдельные символы.
 *
 * @param {*} raw
 * @param {number} max — максимум символов результата
 * @returns {string} '' ⇒ подписи нет (кнопку рисовать нельзя)
 */
export function cleanButtonText(raw, max) {
    if (typeof raw !== 'string') return '';
    const limit = Number.isFinite(max) && max > 0 ? Math.floor(max) : BUTTON_TEXT_MAX;
    const clean = raw
        .replace(/[\u0000-\u001F\u007F-\u009F\u200B\u200C\u200E\u200F\u202A-\u202E\u2066-\u2069]/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();
    if (!clean) return '';
    const chars = Array.from(clean);
    if (chars.length <= limit) return clean;
    return `${chars.slice(0, Math.max(1, limit - 1)).join('')}…`;
}

/**
 * Вычищает текст ответа бота для тоста.
 *
 * В отличие от подписи кнопки переводы строк СОХРАНЯЮТСЯ: боты присылают
 * многострочные ответы, и схлопывание их в одну строку теряет смысл. Тело
 * тоста ставится через `textContent`, поэтому перевод строки безопасен.
 *
 * @param {*} raw
 * @returns {string}
 */
export function cleanAnswerText(raw) {
    if (typeof raw !== 'string') return '';
    const clean = raw
        .replace(/\r\n?/g, '\n')
        .replace(/[\u0000-\u0009\u000B-\u001F\u007F-\u009F\u202A-\u202E\u2066-\u2069]/g, ' ')
        .replace(/[ \t]+/g, ' ')
        .replace(/\n{3,}/g, '\n\n')
        .trim();
    if (!clean) return '';
    const chars = Array.from(clean);
    if (chars.length <= ANSWER_TEXT_MAX) return clean;
    return `${chars.slice(0, ANSWER_TEXT_MAX - 1).join('')}…`;
}

/**
 * Проверяет URL кнопки и возвращает его же, если открывать безопасно.
 *
 * Порядок операций критичен:
 *   1. вырезать управляющие и невидимые символы — браузер их игнорирует при
 *      разборе схемы, и "java\0script:alert(1)" для него `javascript:`;
 *   2. только потом разбирать схему регуляркой.
 * Обратный порядок даёт обход белого списка одним нулевым байтом.
 *
 * Относительные ссылки (`/x`, `//evil.com`) отвергаются: у кнопки бота URL
 * всегда абсолютный, а `//evil.com` в `window.open` — переход на чужой хост
 * по текущей схеме.
 *
 * @param {*} raw
 * @returns {?string} нормализованный URL или null
 */
export function safeUrl(raw) {
    if (typeof raw !== 'string') return null;
    const clean = raw
        .replace(/[\u0000-\u0020\u007F-\u00A0\u200B-\u200F\u2028\u2029\u202A-\u202E\u2066-\u2069\uFEFF]/g, '')
        .trim();
    if (!clean) return null;
    const m = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(clean);
    if (!m) return null;
    const scheme = m[1].toLowerCase();
    if (!ALLOWED_SCHEMES.has(scheme)) return null;
    // http/https без authority («https:evil») браузеры разбирают
    // непредсказуемо — требуем канонический вид с непустым хостом.
    if ((scheme === 'http' || scheme === 'https') && !/^https?:\/\/[^/?#\s]+/i.test(clean)) {
        return null;
    }
    return clean;
}

/**
 * Нормализует одну кнопку из DTO.
 *
 * @param {*} btn — ReplyButtonDto
 * @returns {?{text: string, kind: string, data: ?string, url: ?string,
 *   query: ?string, requiresPassword: boolean}} null ⇒ кнопку рисовать нельзя
 */
export function normalizeButton(btn) {
    if (!btn || typeof btn !== 'object') return null;
    const text = cleanButtonText(btn.text, BUTTON_TEXT_MAX);
    // Кнопка без подписи — это пустой прямоугольник, по которому пользователь
    // не может принять решение. Такую выбрасываем, а не рисуем «[  ]».
    if (!text) return null;
    const rawKind = String(btn.kind || '');
    return {
        text,
        kind: BUTTON_KINDS.has(rawKind) ? rawKind : 'other',
        data: typeof btn.data === 'string' && btn.data ? btn.data : null,
        url: typeof btn.url === 'string' && btn.url ? btn.url : null,
        query: typeof btn.query === 'string' ? btn.query : null,
        requiresPassword: btn.requires_password === true || btn.requiresPassword === true,
    };
}

/**
 * Нормализует разметку сообщения.
 *
 * Пустые ряды и кнопки без подписи отбрасываются; если после чистки не
 * осталось ни одной кнопки, разметки нет вовсе (null) — рисовать пустую
 * рамку под сообщением незачем. Исключение — `hide`/`force_reply`: у них
 * рядов нет ПО КОНТРАКТУ, и они значимы сами по себе.
 *
 * @param {*} markup — ReplyMarkupDto
 * @returns {?{kind: string, rows: Array<Array<object>>, resize: boolean,
 *   singleUse: boolean, persistent: boolean, placeholder: ?string}}
 */
export function normalizeMarkup(markup) {
    if (!markup || typeof markup !== 'object') return null;
    const kind = String(markup.kind || '');
    if (!MARKUP_KINDS.has(kind)) return null;

    const placeholder = cleanButtonText(markup.placeholder, PLACEHOLDER_MAX);
    const out = {
        kind,
        rows: [],
        resize: markup.resize === true,
        singleUse: markup.single_use === true || markup.singleUse === true,
        persistent: markup.persistent === true,
        placeholder: placeholder || null,
    };
    if (kind === 'hide' || kind === 'force_reply') return out;

    const rawRows = Array.isArray(markup.rows) ? markup.rows.slice(0, MAX_ROWS) : [];
    for (const rawRow of rawRows) {
        if (!Array.isArray(rawRow)) continue;
        const row = [];
        for (const rawBtn of rawRow.slice(0, MAX_ROW_BUTTONS)) {
            const btn = normalizeButton(rawBtn);
            if (btn) row.push(btn);
        }
        if (row.length) out.rows.push(row);
    }
    return out.rows.length ? out : null;
}

/** Служебный конструктор «кнопка есть, но нажать её нечем». */
function unsupported(reason) {
    return { action: 'unsupported', reason };
}

/**
 * Что делать по нажатию кнопки.
 *
 * Единственная точка истины — принимает и нормализованную кнопку, и сырой
 * `ReplyButtonDto` (поля `requires_password`/`requiresPassword` читаются оба).
 *
 * Разбор по видам:
 * - `callback` → запрос к боту; `requires_password` не поддержан (нужен SRP —
 *   расчёт 2FA-пароля, которого нет ни на фронте, ни в бэкенде плагина);
 * - `url`/`login_url`/`user_profile` → внешний переход. `login_url` открывается
 *   БЕЗ авторизации (`messages.requestUrlAuth` в v1 нет) — это ровно то, что
 *   делает официальный клиент, когда пользователь отказывается авторизоваться;
 * - `switch_inline*` → подстановка запроса в поле ввода без отправки;
 * - `text` → обычная отправка текста (кнопка reply-клавиатуры);
 * - остальное → `unsupported` с человеческой причиной.
 *
 * @param {*} btn
 * @returns {{action: 'callback'|'open-url'|'send-text'|'inline-query'|'unsupported',
 *   data?: ?string, url?: string, text?: string, query?: string,
 *   samePeer?: boolean, reason?: string}}
 */
export function buttonAction(btn) {
    if (!btn || typeof btn !== 'object') return unsupported('пустая кнопка');
    const kind = String(btn.kind || 'other');
    const requiresPassword = btn.requires_password === true || btn.requiresPassword === true;

    switch (kind) {
        case 'callback':
            if (requiresPassword) return unsupported('кнопка требует пароль 2FA — не поддержано в v1');
            return { action: 'callback', data: typeof btn.data === 'string' ? btn.data : null };
        case 'url':
        case 'login_url':
        case 'user_profile': {
            const url = safeUrl(btn.url);
            if (!url) return unsupported('ссылка недопустима или пуста');
            return { action: 'open-url', url };
        }
        case 'switch_inline':
        case 'switch_inline_current':
            return {
                action: 'inline-query',
                query: typeof btn.query === 'string' ? btn.query : '',
                samePeer: kind === 'switch_inline_current',
            };
        case 'text':
            return { action: 'send-text', text: String(btn.text || '') };
        default:
            return unsupported(UNSUPPORTED_REASON[kind] || UNSUPPORTED_REASON.other);
    }
}

/**
 * Как показать ответ бота на нажатие callback-кнопки.
 *
 * Правила Telegram (см. messages.botCallbackAnswer):
 * - `message` + `alert == false` → неблокирующее уведомление;
 * - `message` + `alert == true`  → уведомление, которое пользователь обязан
 *   заметить (у нас — «громкий» тост с долгим ttl);
 * - `url` → открыть БЕЗ подтверждения (сервер гарантирует, что это deep-link
 *   самого бота или game-URL). Белый список схем всё равно применяется:
 *   доверять «гарантии» чужой стороны на клиенте нельзя.
 *
 * @param {*} resp — тело 200 POST .../callback
 * @returns {{kind: 'none'|'toast'|'alert', message: string, url: ?string,
 *   cacheTime: ?number}}
 */
export function describeAnswer(resp) {
    const out = { kind: 'none', message: '', url: null, cacheTime: null };
    if (!resp || typeof resp !== 'object') return out;
    out.message = cleanAnswerText(resp.message);
    out.url = safeUrl(resp.url);
    out.cacheTime = Number.isFinite(resp.cache_time) ? resp.cache_time : null;
    if (out.message) out.kind = resp.alert === true ? 'alert' : 'toast';
    return out;
}

/**
 * Человеческий текст ошибки нажатия.
 *
 * Решение принимается по КОДУ контракта, а не по тексту сообщения (тексты
 * меняются, коды — контракт, см. api.js). `504 bot_timeout` вынесен отдельно:
 * это не поломка панели, а «бот думает слишком долго», и повтор осмыслен.
 *
 * @param {*} err — Error из api.js с полями code/status/retryAfterSecs
 * @returns {string}
 */
export function callbackErrorText(err) {
    const code = (err && err.code) || null;
    const status = (err && err.status) || 0;
    if (code === 'flood_wait' || status === 429) {
        const secs = callbackRetrySecs(err);
        return secs ? `слишком часто — повтор через ${secs}s` : 'слишком часто — подождите';
    }
    if (code === 'bot_timeout' || status === 504) return 'бот не ответил';
    if (code === 'unknown_message') return 'сообщение недоступно — кнопки устарели';
    if (code === 'unknown_dialog') return 'диалог недоступен';
    if (code === 'not_authorized') return 'нет авторизации в Telegram';
    if (code === 'not_connected') return 'нет соединения с Telegram';
    if (code === 'invalid_input') return 'Telegram отклонил нажатие';
    if (code === 'telegram_error') return 'Telegram отклонил запрос';
    return (err && err.message) ? String(err.message) : 'не удалось нажать кнопку';
}

/**
 * Сколько секунд ждать до повтора (0 ⇒ ждать не нужно).
 *
 * @param {*} err
 * @returns {number}
 */
export function callbackRetrySecs(err) {
    const secs = err && err.retryAfterSecs;
    if (!Number.isFinite(secs) || secs <= 0) return 0;
    // Верхняя граница чисто косметическая: тикать сутки в подписи кнопки
    // бессмысленно, пользователь давно перезагрузит вкладку.
    return Math.min(Math.ceil(secs), 3600);
}

/**
 * Актуальная reply-клавиатура диалога по загруженной истории.
 *
 * Клавиатура — состояние ЧАТА: берётся последнее ВХОДЯЩЕЕ сообщение, у
 * которого есть `keyboard`/`hide`/`force_reply`. `hide` означает «клавиатуры
 * нет» — поэтому возвращается null, а не объект: иначе полоса осталась бы на
 * экране после того, как бот её убрал.
 *
 * Свои сообщения (`out`) игнорируются: `reply_markup` у них не бывает, а если
 * бы и был — это была бы наша собственная разметка, а не бота.
 *
 * @param {Array<object>} messages — по возрастанию id
 * @returns {?{markup: object, messageId: number}}
 */
export function latestReplyMarkup(messages) {
    if (!Array.isArray(messages)) return null;
    for (let i = messages.length - 1; i >= 0; i -= 1) {
        const msg = messages[i];
        if (!msg || msg.out) continue;
        const markup = normalizeMarkup(msg.reply_markup);
        if (!markup || markup.kind === 'inline') continue;
        if (markup.kind === 'hide') return null;
        return { markup, messageId: Number(msg.id) || 0 };
    }
    return null;
}

/**
 * Подпись полосы reply-клавиатуры.
 *
 * @param {?object} markup — нормализованная разметка
 * @returns {string}
 */
export function barTitle(markup) {
    if (!markup) return '';
    if (markup.kind === 'force_reply') return '// REPLY REQUESTED';
    return markup.persistent ? '// KEYBOARD · PERSISTENT' : '// KEYBOARD';
}

// ---------------------------------------------------------------------------
// Состояние рисовальщика
// ---------------------------------------------------------------------------

const local = {
    /** Колбэк «отправить текст» — ставит chat.js (см. initBotKeyboards). */
    onSendText: null,
    /** Колбэк «кнопку нажали» — ставит chat.js (см. initBotKeyboards). */
    onButtonPress: null,
    /** Полоса reply-клавиатуры над #tg-prompt (создаётся лениво). */
    bar: null,
    /** Контейнер рядов внутри полосы. */
    barRows: null,
    /** Заголовок полосы. */
    barTitleEl: null,
    /** dialogId → {markup, messageId, collapsed}. */
    reply: new Map(),
    /** Активные таймеры обратного отсчёта (гасятся в resetKeyboards). */
    timers: new Set(),
};

/**
 * Привязывает внешние колбэки.
 *
 * `onSendText` — это `chat.js::send`. Отправлять текст своим POST'ом из этого
 * модуля НЕЛЬЗЯ: потерялись бы optimistic-строка, дедупликация по
 * `insertMessage` и снятие статуса «печатает…» — в ленте появлялись бы дубли.
 *
 * `onButtonPress` — это регистрация «следования» в chat.js (см. follow.js).
 * Через колбэк, а не прямым импортом: про прокрутку ленты этот модуль знать не
 * должен, иначе клавиатуры перестанут быть переносимыми и тестируемыми без DOM.
 *
 * @param {{onSendText?: function(string): void,
 *   onButtonPress?: function(string, number): void}} opts
 */
export function initBotKeyboards(opts) {
    if (opts && typeof opts.onSendText === 'function') local.onSendText = opts.onSendText;
    if (opts && typeof opts.onButtonPress === 'function') local.onButtonPress = opts.onButtonPress;
}

// ---------------------------------------------------------------------------
// Инлайн-клавиатура (под сообщением)
// ---------------------------------------------------------------------------

/** Кнопка клавиатуры в терминальном стиле (скобки `[ … ]` рисует CSS). */
function buttonEl(btn, action) {
    const el = document.createElement('button');
    el.type = 'button';
    el.className = 'tg-kb-btn';
    // ТОЛЬКО textContent: подпись пришла от чужого бота.
    el.textContent = btn.text;
    el.dataset.kbKind = btn.kind;
    el.dataset.kbAction = action.action;
    if (action.action === 'unsupported') {
        el.classList.add('tg-kb-btn-unsupported');
        el.title = action.reason || '';
    } else if (action.action === 'open-url') {
        el.classList.add('tg-kb-btn-url');
        el.title = action.url;
    } else {
        el.title = btn.text;
    }
    return el;
}

/**
 * Инлайн-клавиатура сообщения.
 *
 * Возвращает готовый узел или null — вызывающий (chat.js::buildMessageRow)
 * просто дописывает его в строку лога после медиа-карточки.
 *
 * @param {object} msg — MessageDto
 * @returns {?HTMLElement}
 */
export function buildMessageKeyboard(msg) {
    const markup = normalizeMarkup(msg && msg.reply_markup);
    if (!markup || markup.kind !== 'inline') return null;

    const dialogId = String(msg.dialog_id);
    const messageId = Number(msg.id);
    const wrap = document.createElement('div');
    wrap.className = 'tg-kb';
    wrap.dataset.dialogId = dialogId;
    wrap.dataset.msgId = String(messageId);

    for (const row of markup.rows) {
        const rowEl = document.createElement('div');
        rowEl.className = 'tg-kb-row';
        for (const btn of row) {
            const action = buttonAction(btn);
            const el = buttonEl(btn, action);
            // Инлайн-кнопка без валидного message_id нажата быть не может:
            // callback уходит по (peer, msg_id), и без id сервер ответит 404.
            if (action.action === 'callback' && !(Number.isFinite(messageId) && messageId > 0)) {
                el.classList.add('tg-kb-btn-unsupported');
                el.title = 'сообщение ещё не подтверждено Telegram';
                el.disabled = true;
            } else {
                el.addEventListener('click', () => runAction(action, el, dialogId, messageId));
            }
            rowEl.appendChild(el);
        }
        wrap.appendChild(rowEl);
    }
    return wrap;
}

// ---------------------------------------------------------------------------
// Действия
// ---------------------------------------------------------------------------

/** Открывает внешнюю ссылку в новой вкладке. */
export function openExternal(rawUrl) {
    const url = safeUrl(rawUrl);
    if (!url) return false;
    // noopener,noreferrer обязательны: открытая страница не должна получить
    // ни ссылку на нашу вкладку (window.opener), ни Referer с токеном в пути.
    const w = window.open(url, '_blank', 'noopener,noreferrer');
    if (w) {
        try { w.opener = null; } catch (_) { /* браузер уже обнулил сам */ }
    }
    return true;
}

/** Подставляет inline-запрос в поле ввода, не отправляя его. */
function insertQuery(query) {
    if (!$tgInput) return false;
    $tgInput.value = String(query || '');
    try { $tgInput.focus(); } catch (_) { /* поле могло уехать из DOM */ }
    return true;
}

/** Разводит нажатие по видам действий. */
function runAction(action, el, dialogId, messageId) {
    // Нажатие кнопки — явный интент пользователя, и его результат обязан
    // приехать к нему сам (см. follow.js). Сообщаем о нажатии ДО запроса:
    // правка сообщения ботом приходит отдельным WS-кадром и вполне может
    // опередить `.then` нашего callback'а. Только действия, реально меняющие
    // ленту: `open-url` и `inline-query` её не трогают.
    if (action.action === 'callback' || action.action === 'send-text') {
        if (typeof local.onButtonPress === 'function') {
            // Автопрокрутка — удобство поверх нажатия; её падение не должно
            // отменять сам callback боту.
            try {
                local.onButtonPress(String(dialogId == null ? '' : dialogId), Number(messageId) || 0);
            } catch (e) {
                console.debug('[tg-botkb] onButtonPress failed', e);
            }
        }
    }
    switch (action.action) {
        case 'callback':
            doCallback(el, dialogId, messageId, action.data);
            return;
        case 'open-url':
            if (!openExternal(action.url)) {
                notify({ level: 'warn', title: 'BOT', body: 'ссылка кнопки недопустима' });
            }
            return;
        case 'inline-query':
            insertQuery(action.query);
            return;
        case 'send-text':
            if (typeof local.onSendText === 'function') local.onSendText(action.text);
            return;
        default:
            notify({
                level: 'warn',
                title: 'BOT',
                body: action.reason || UNSUPPORTED_REASON.other,
            });
    }
}

/** Переводит кнопку в состояние «ждём бота». */
function setPending(el, pending) {
    if (!el) return;
    el.disabled = !!pending;
    if (pending) el.classList.add('tg-kb-btn-pending');
    else el.classList.remove('tg-kb-btn-pending');
}

/**
 * Нажатие callback-кнопки.
 *
 * Пока запрос в полёте, кнопка `disabled` — не косметика: каждое нажатие
 * Telegram доставляет боту, и три клика подряд означают три обращения (и
 * возможный FLOOD_WAIT на аккаунте пользователя).
 */
function doCallback(el, dialogId, messageId, data) {
    if (el && el.disabled) return;
    setPending(el, true);
    sendCallback(dialogId, messageId, { data })
        .then((resp) => {
            setPending(el, false);
            applyAnswer(resp);
        })
        .catch((err) => {
            setPending(el, false);
            handleCallbackError(el, err);
        });
}

/** Показывает ответ бота по правилам describeAnswer. */
export function applyAnswer(resp) {
    const d = describeAnswer(resp);
    if (d.kind === 'alert') {
        // alert — это «пользователь обязан прочитать». Модального окна в
        // терминальной панели нет, поэтому громкий тост с долгим ttl и
        // отдельным уровнем: он не исчезнет, пока человек читает строку лога.
        notify({ level: 'warn', title: 'BOT ALERT', body: d.message, ttl: 15000 });
    } else if (d.kind === 'toast') {
        notify({ level: 'info', title: 'BOT', body: d.message });
    }
    if (d.url) openExternal(d.url);
    return d;
}

/** Ошибка нажатия: тост + (для 429) обратный отсчёт прямо на кнопке. */
function handleCallbackError(el, err) {
    notify({ level: 'error', title: 'BOT', body: callbackErrorText(err) });
    const secs = callbackRetrySecs(err);
    if (secs > 0) startCountdown(el, secs);
}

/**
 * Гасит кнопку на `secs` секунд, показывая остаток в подписи.
 *
 * Автоповтора НЕТ сознательно: `flood_wait` от Telegram лечится только
 * ожиданием, а самостоятельный ретрай продлевает блокировку.
 */
function startCountdown(el, secs) {
    if (!el || typeof setInterval !== 'function') return;
    const base = el.dataset.kbLabel || el.textContent;
    el.dataset.kbLabel = base;
    let left = secs;
    el.disabled = true;
    el.classList.add('tg-kb-btn-wait');
    el.textContent = `${base} ${left}s`;
    const timer = setInterval(() => {
        left -= 1;
        if (left > 0) {
            el.textContent = `${base} ${left}s`;
            return;
        }
        clearInterval(timer);
        local.timers.delete(timer);
        el.textContent = base;
        el.classList.remove('tg-kb-btn-wait');
        el.disabled = false;
    }, 1000);
    local.timers.add(timer);
}

// ---------------------------------------------------------------------------
// Reply-клавиатура (полоса над полем ввода)
// ---------------------------------------------------------------------------

/**
 * Создаёт полосу над `#tg-prompt` при первой надобности.
 *
 * Узел создаётся в JS, а не в index.html, по двум причинам: у большинства
 * аккаунтов ботов с reply-клавиатурой нет вовсе (пустой div в разметке
 * навсегда), и вкладку Telegram можно выключить целиком, не трогая вёрстку.
 */
function ensureBar() {
    if (local.bar) return local.bar;
    if (!$tgPrompt || !$tgPrompt.parentNode || typeof document === 'undefined') return null;

    const bar = document.createElement('div');
    bar.className = 'tg-kb-bar';
    bar.hidden = true;

    const head = document.createElement('div');
    head.className = 'tg-kb-bar-head';
    const title = document.createElement('span');
    title.className = 'tg-kb-bar-title';
    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'tg-kb-btn tg-kb-bar-close';
    close.textContent = '✕';
    close.title = 'Свернуть клавиатуру';
    close.addEventListener('click', () => collapseBar(true));
    head.appendChild(title);
    head.appendChild(close);

    const rows = document.createElement('div');
    rows.className = 'tg-kb-bar-rows';

    bar.appendChild(head);
    bar.appendChild(rows);
    $tgPrompt.parentNode.insertBefore(bar, $tgPrompt);

    local.bar = bar;
    local.barRows = rows;
    local.barTitleEl = title;
    return bar;
}

/** Сворачивает/разворачивает полосу активного диалога. */
function collapseBar(collapsed) {
    const id = activeId();
    const entry = id ? local.reply.get(id) : null;
    if (!entry) return;
    entry.collapsed = !!collapsed;
    syncReplyBar();
}

/** id активного диалога строкой ('' ⇒ диалог не открыт). */
function activeId() {
    return tgState.activeDialogId ? String(tgState.activeDialogId) : '';
}

/** Ставит placeholder поля ввода (пустой ⇒ дефолтный пробел из разметки). */
function setInputPlaceholder(text) {
    if (!$tgInput) return;
    $tgInput.placeholder = text || DEFAULT_PLACEHOLDER;
}

/** Прячет полосу и возвращает поле ввода в исходный вид. */
function hideBar() {
    if (local.bar) local.bar.hidden = true;
    setInputPlaceholder(null);
}

/**
 * Перерисовывает полосу под состояние активного диалога.
 *
 * Ряды пересобираются целиком: клавиатура меняется редко (только когда бот
 * прислал новую), а точечный патч кнопок потребовал бы отдельного реестра,
 * который разъезжался бы с состоянием при каждой смене диалога.
 */
export function syncReplyBar() {
    const id = activeId();
    const entry = id ? local.reply.get(id) : null;
    if (!entry) {
        hideBar();
        return false;
    }
    const bar = ensureBar();
    if (!bar) return false;

    const markup = entry.markup;
    local.barTitleEl.textContent = barTitle(markup);
    local.barRows.textContent = '';
    bar.classList.toggle('tg-kb-bar-collapsed', !!entry.collapsed);
    bar.hidden = false;
    setInputPlaceholder(markup.placeholder);

    if (entry.collapsed) {
        // Свёрнутое состояние оставляет ровно один способ вернуть клавиатуру.
        // Без него `single_use` означал бы «клавиатура исчезла навсегда», а
        // Telegram обещает пользователю обратное.
        const show = document.createElement('button');
        show.type = 'button';
        show.className = 'tg-kb-btn tg-kb-bar-show';
        show.textContent = '⌨ keyboard';
        show.title = 'Показать клавиатуру бота';
        show.addEventListener('click', () => collapseBar(false));
        local.barRows.appendChild(show);
        return true;
    }

    if (markup.kind === 'force_reply') {
        // Кнопок нет по контракту: бот просит именно ответ текстом.
        const hint = document.createElement('span');
        hint.className = 'tg-kb-bar-hint';
        hint.textContent = markup.placeholder || 'бот ждёт ответа сообщением';
        local.barRows.appendChild(hint);
        return true;
    }

    for (const row of markup.rows) {
        const rowEl = document.createElement('div');
        rowEl.className = 'tg-kb-row';
        for (const btn of row) {
            const action = buttonAction(btn);
            const el = buttonEl(btn, action);
            el.addEventListener('click', () => {
                runAction(action, el, id, entry.messageId);
                // single_use: Telegram просит спрятать клавиатуру после
                // первого же нажатия. Прячем ТОЛЬКО после действия, которое
                // действительно что-то отправило — иначе клик по
                // неподдерживаемой кнопке убирал бы рабочие соседние.
                if (markup.singleUse && action.action === 'send-text') collapseBar(true);
            });
            rowEl.appendChild(el);
        }
        local.barRows.appendChild(rowEl);
    }
    return true;
}

/**
 * Учитывает разметку одного сообщения (WS `new_message` / `message_edited`).
 *
 * Инлайн-разметка здесь игнорируется: она рисуется под своим сообщением и
 * состоянием диалога не является.
 *
 * @param {object} msg — MessageDto
 * @returns {boolean} изменилось ли состояние клавиатуры диалога
 */
export function noteReplyMarkup(msg) {
    if (!msg || msg.out) return false;
    const markup = normalizeMarkup(msg.reply_markup);
    if (!markup || markup.kind === 'inline') return false;
    const id = String(msg.dialog_id || '');
    if (!id) return false;

    if (markup.kind === 'hide') local.reply.delete(id);
    else local.reply.set(id, { markup, messageId: Number(msg.id) || 0, collapsed: false });

    if (id === activeId()) syncReplyBar();
    return true;
}

/**
 * Пересчитывает клавиатуру диалога по загруженной истории.
 *
 * Зовётся из chat.js::renderHistory. Пересчёт (а не накопление) обязателен:
 * страница истории вверх приносит СТАРЫЕ сообщения, и «последней» клавиатура
 * должна остаться та, что ниже по ленте, а не та, что пришла позже по времени
 * загрузки.
 *
 * @param {Array<object>} messages — по возрастанию id
 * @param {string} [dialogId] — по умолчанию активный диалог
 * @returns {boolean} есть ли теперь клавиатура
 */
export function applyHistoryKeyboard(messages, dialogId) {
    const id = String(dialogId != null ? dialogId : activeId());
    if (!id) return false;
    const found = latestReplyMarkup(messages);
    if (found) local.reply.set(id, { ...found, collapsed: false });
    else local.reply.delete(id);
    if (id === activeId()) syncReplyBar();
    return !!found;
}

/**
 * Убирает полосу с экрана (смена диалога).
 *
 * Состояние диалогов НЕ стирается: возврат в диалог перерисует клавиатуру из
 * истории, а до этого момента показывать чужую клавиатуру нельзя.
 */
export function clearReplyKeyboard() {
    hideBar();
}

/** Полный сброс (logout, смена аккаунта). */
export function resetKeyboards() {
    for (const timer of local.timers) {
        try { clearInterval(timer); } catch (_) { /* таймер уже снят */ }
    }
    local.timers.clear();
    local.reply.clear();
    hideBar();
    if (local.barRows) local.barRows.textContent = '';
}

/** Снимок для e2e/диагностики. */
export function _debugBotKbState() {
    const id = activeId();
    const entry = id ? local.reply.get(id) : null;
    return {
        dialogs: local.reply.size,
        barVisible: !!(local.bar && !local.bar.hidden),
        kind: entry ? entry.markup.kind : null,
        collapsed: !!(entry && entry.collapsed),
        timers: local.timers.size,
    };
}
