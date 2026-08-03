// tmux-web — VK plugin: клавиатуры сообществ (inline + нижняя).
//
// Сообщества присылают вместе с сообщением `keyboard` (MessageDto.reply_markup,
// контракт см. plugins/vk/src/dto.rs::ReplyMarkupDto). Разновидности две, и
// путать их нельзя:
//
//   inline    — клавиатура ПОД КОНКРЕТНЫМ сообщением. Живёт ровно столько,
//               сколько живёт сообщение.
//   keyboard  — СОСТОЯНИЕ БЕСЕДЫ, а не свойство сообщения. Официальные клиенты
//               держат такую клавиатуру над полем ввода, пока сообщество не
//               пришлёт другую. Поэтому она рисуется полосой над `#vk-prompt`,
//               а не под строкой лога: иначе после десяти сообщений в ленте
//               висело бы десять одинаковых клавиатур, и ни одна не была бы
//               «текущей».
//
// ============================================================================
// ЧЕСТНАЯ ДЕГРАДАЦИЯ — ГЛАВНОЕ В ЭТОМ МОДУЛЕ
// ============================================================================
//
// Под ПОЛЬЗОВАТЕЛЬСКИМ токеном ВКонтакте позволяет нажать ровно два вида
// кнопок:
//
//   text      — нажатие это обычная отправка сообщения с подписью кнопки и её
//               `payload` (именно так работает официальный клиент). Отправку
//               делает БЭКЕНД: `payload` в DTO не ездит вовсе, и достать его
//               может только сервер — см. POST .../messages/:mid/button;
//   open_link — открывает браузер, API вообще не участвует.
//
// Не работают `callback`, `vkpay`, `open_app`, `location`. Про `callback`
// подробно, потому что это единственная функция телеграмной вкладки, которая
// в VK принципиально недостижима: `messages.sendMessageEventAnswer` — метод
// СТОРОНЫ СООБЩЕСТВА (им бот ОТВЕЧАЕТ на уже полученный `message_event`, и он
// требует ключа доступа сообщества), а обратной, клиентской половины —
// «отправить `message_event` от имени пользователя» — в публичном API VK нет
// ни в документации, ни в клиентских библиотеках. Прямого аналога телеграмному
// `messages.getBotCallbackAnswer` не существует.
//
// Следствия для этого файла:
//   * такие кнопки РИСУЮТСЯ, но отключены (`disabled`) и объясняют причину в
//     `title`. Прятать их нельзя: пользователь сверяет экран с телефоном, и
//     исчезнувшая кнопка читается как поломка панели, а не как ограничение
//     платформы;
//   * веток `applyAnswer` / тоста-ответа (телеграмный `answerCallbackQuery`)
//     здесь НЕТ — отвечать нечему;
//   * кода ошибки `bot_timeout` (504) в контракте VK тоже нет: ждать ответа
//     сообщества негде.
//
// ============================================================================
// КЛЮЧЕВЫЕ РЕШЕНИЯ
// ============================================================================
//
// * ГРАФ АЦИКЛИЧЕН: модуль НЕ импортирует chat.js. Отправка и «следование»
//   приходят колбэками через `initBotKeyboards({onSendText, onButtonPress})`.
// * Сети здесь нет вовсе. Нажатие уходит наружу вызовом `onSendText(label,
//   press)`, где `press = {messageId, row, col}` — координаты кнопки для
//   роута. Так у нажатия остаются optimistic-строка, дедупликация по
//   `insertMessage` и кнопка [retry] чужой ошибки — всё то, что живёт в
//   chat.js::send.
// * КООРДИНАТЫ, А НЕ PAYLOAD. `payload` уходит сообществу как команда, и
//   фронт его не видит принципиально (см. dto.rs::ReplyButtonDto): нажатие
//   адресуется местом кнопки в клавиатуре, а подпись и нагрузку сервер
//   достаёт из самого сообщения.
// * КРИТИЧНО — подпись кнопки приходит от ЧУЖОГО сообщества. Только
//   `textContent`, никакого `innerHTML`. Скобки `[ … ]` рисует CSS
//   (`::before`/`::after`), поэтому в текстовом узле лежит ровно подпись и
//   ничего больше.
// * КРИТИЧНО — ссылка кнопки тоже приходит от чужого сообщества.
//   `javascript:`-ссылка в `window.open` — это XSS с правами вкладки. Схема
//   проверяется белым списком (`format.js::safeUrl`) ПОСЛЕ вычистки
//   управляющих символов: браузер трактует "java\0script:" как
//   `javascript:`, а наивная регулярка — как «схемы нет».
// * Флаг `supported` из DTO — условие НЕОБХОДИМОЕ, но не достаточное: фронт
//   исполняет только те виды, которые умеет (`text`, `open_link`). Вид,
//   объявленный поддержанным, но не реализованный здесь, рисуется отключённым
//   — «нажатие в никуда» хуже честного пояснения.

import { $vkPrompt } from '../core/dom.js';

import { safeUrl } from './format.js';
import { openExternal } from './media.js';
import { vkState } from './state.js';

// ---------------------------------------------------------------------------
// Константы
// ---------------------------------------------------------------------------

/** Максимум видимых символов подписи кнопки (code points, не байты). */
export const BUTTON_TEXT_MAX = 48;

/**
 * Потолки размера клавиатуры.
 *
 * ВКонтакте ограничивает разметку своими лимитами, но пришла она от чужого
 * сообщества — доверять её размеру нельзя. Числа совпадают с серверными
 * (`dto.rs::MAX_KB_ROWS` / `MAX_KB_ROW_BUTTONS`): обрезка обязана случаться
 * одинаково на обеих сторонах, иначе координаты нажатия разъедутся.
 */
export const MAX_ROWS = 32;
export const MAX_ROW_BUTTONS = 12;

/** Виды контейнеров разметки (контракт dto.rs::ReplyMarkupDto.kind). */
const MARKUP_KINDS = new Set(['inline', 'keyboard']);

/** Виды кнопок VK (контракт dto.rs::ReplyButtonDto.kind). */
const BUTTON_KINDS = new Set([
    'text', 'open_link', 'callback', 'vkpay', 'open_app', 'location', 'other',
]);

/**
 * Почему кнопку нельзя нажать — по видам.
 *
 * Причина показывается в `title` отключённой кнопки: молча гасить кнопку
 * нельзя, пользователь решит, что сломалась панель или сам бот.
 */
const UNSUPPORTED_REASON = {
    callback: 'ВКонтакте не позволяет нажать кнопку этого типа из стороннего клиента: '
        + 'механика message_event доступна только самому сообществу',
    vkpay: 'оплата VK Pay работает только в официальном приложении ВКонтакте',
    open_app: 'мини-приложения ВКонтакте открываются только в официальном клиенте',
    location: 'отправка геопозиции доступна только в официальном приложении ВКонтакте',
    other: 'ВКонтакте не позволяет нажать кнопку этого типа из стороннего клиента',
};

/** Кнопка есть, а координат нет — нажимать нечем (кадр старой версии плагина). */
const NO_COORDS_REASON = 'кнопка пришла без координат — обновите страницу';

/** Сообщение ещё не подтверждено VK: у него нет id, и адресовать нажатие нечем. */
const NO_MESSAGE_REASON = 'сообщение ещё не подтверждено ВКонтакте';

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
 * Вычищаются управляющие символы и bidi-переопределения: сообщество может
 * прислать U+202E (RIGHT-TO-LEFT OVERRIDE) и «перевернуть» остаток строки —
 * визуальный спуфинг подписи, из-за которого кнопка «отменить» читается как
 * «подтвердить». ZWJ (U+200D) СОХРАНЁН: без него составные эмодзи (семья,
 * флаги профессий) рассыпались бы на отдельные символы.
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
 * Координата кнопки из DTO.
 *
 * `null` вместо нуля на любой мусор — принципиально: «нажать первую кнопку
 * вместо той, что просили» это худшая из возможных реакций на битую разметку.
 *
 * @param {*} raw
 * @param {number} max — верхняя граница (не включая)
 * @returns {?number}
 */
function coord(raw, max) {
    // Тип проверяется ДО `Number`: `Number(null)`, `Number(false)` и
    // `Number('')` дают НОЛЬ, то есть «первая кнопка первого ряда» — ровно то,
    // чего здесь быть не должно.
    if (typeof raw !== 'number' && typeof raw !== 'string') return null;
    if (typeof raw === 'string' && raw.trim() === '') return null;
    const v = Number(raw);
    if (!Number.isFinite(v)) return null;
    const i = Math.floor(v);
    return i >= 0 && i < max ? i : null;
}

/**
 * Нормализует одну кнопку из DTO.
 *
 * Координаты берутся ИЗ DTO, а не из позиции в массиве: кнопки без подписи
 * здесь выбрасываются (рисовать пустой прямоугольник, по которому нельзя
 * принять решение, незачем), и позиционная нумерация после этого разъехалась
 * бы с серверной.
 *
 * @param {*} btn — ReplyButtonDto
 * @returns {?{text: string, kind: string, url: ?string, color: ?string,
 *   supported: boolean, row: ?number, col: ?number}} null ⇒ кнопку не рисуем
 */
export function normalizeButton(btn) {
    if (!btn || typeof btn !== 'object') return null;
    const text = cleanButtonText(btn.text, BUTTON_TEXT_MAX);
    if (!text) return null;
    const rawKind = String(btn.kind || '');
    return {
        text,
        kind: BUTTON_KINDS.has(rawKind) ? rawKind : 'other',
        url: typeof btn.url === 'string' && btn.url ? btn.url : null,
        color: typeof btn.color === 'string' && btn.color ? btn.color : null,
        // Решение о поддержке принимает БЭКЕНД (он знает, что умеет), фронт
        // только рисует по флагу — см. шапку модуля.
        supported: btn.supported === true,
        row: coord(btn.row, MAX_ROWS),
        col: coord(btn.col, MAX_ROW_BUTTONS),
    };
}

/**
 * Нормализует разметку сообщения.
 *
 * Пустые ряды и кнопки без подписи отбрасываются; если после чистки не
 * осталось ни одной кнопки, разметки нет вовсе (null) — рисовать пустую рамку
 * под сообщением незачем. У VK, в отличие от Telegram, нет ни `hide`, ни
 * `force_reply`: вид клавиатуры — это ровно один флаг `inline`.
 *
 * @param {*} markup — ReplyMarkupDto
 * @returns {?{kind: string, rows: Array<Array<object>>, singleUse: boolean}}
 */
export function normalizeMarkup(markup) {
    if (!markup || typeof markup !== 'object') return null;
    const kind = String(markup.kind || '');
    if (!MARKUP_KINDS.has(kind)) return null;

    const out = {
        kind,
        rows: [],
        singleUse: markup.single_use === true || markup.singleUse === true,
    };
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
 * Разбор по видам:
 * - `text` → отправка подписи с `payload` через роут нажатия (координаты
 *   `{row, col}` уезжают наружу в `onSendText`);
 * - `open_link` → внешний переход через белый список схем;
 * - остальное → `unsupported` с человеческой причиной.
 *
 * Флаг `supported` проверяется ПЕРВЫМ: он приходит с бэкенда, который один
 * знает, что умеет. Но и известного вида мало — исполнимость проверяется
 * отдельно (см. шапку модуля).
 *
 * @param {*} btn — нормализованная кнопка либо сырой ReplyButtonDto
 * @returns {{action: 'press'|'open-url'|'unsupported', text?: string,
 *   url?: string, row?: number, col?: number, reason?: string}}
 */
export function buttonAction(btn) {
    if (!btn || typeof btn !== 'object') return unsupported('пустая кнопка');
    const kind = String(btn.kind || 'other');
    if (btn.supported !== true) {
        return unsupported(UNSUPPORTED_REASON[kind] || UNSUPPORTED_REASON.other);
    }
    if (kind === 'open_link') {
        const url = safeUrl(btn.url);
        if (!url) return unsupported('ссылка недопустима или пуста');
        return { action: 'open-url', url };
    }
    if (kind === 'text') {
        const row = coord(btn.row, MAX_ROWS);
        const col = coord(btn.col, MAX_ROW_BUTTONS);
        if (row === null || col === null) return unsupported(NO_COORDS_REASON);
        return { action: 'press', text: String(btn.text || ''), row, col };
    }
    // Сервер объявил вид поддержанным, а исполнить его здесь нечем: нажимать
    // вслепую нельзя — см. шапку модуля.
    return unsupported(UNSUPPORTED_REASON[kind] || UNSUPPORTED_REASON.other);
}

/**
 * Актуальная нижняя клавиатура беседы по загруженной истории.
 *
 * Клавиатура — состояние БЕСЕДЫ: берётся последнее ВХОДЯЩЕЕ сообщение, у
 * которого есть разметка вида `keyboard`. Свои сообщения (`out`) игнорируются:
 * клавиатуры у них не бывает, а если бы и была — это была бы наша собственная
 * разметка, а не сообщества.
 *
 * У VK нет `hide`: сообщество убирает клавиатуру, прислав сообщение БЕЗ неё,
 * — поэтому «ниже по ленте клавиатуры нет» и означает «клавиатуры нет».
 *
 * @param {Array<object>} messages — по возрастанию id
 * @returns {?{markup: object, messageId: number, cmid: number}}
 */
export function latestReplyMarkup(messages) {
    if (!Array.isArray(messages)) return null;
    for (let i = messages.length - 1; i >= 0; i -= 1) {
        const msg = messages[i];
        if (!msg || msg.out) continue;
        const markup = normalizeMarkup(msg.reply_markup);
        if (!markup || markup.kind === 'inline') continue;
        return {
            markup,
            messageId: Number(msg.id) || 0,
            cmid: Number(msg.cmid) || 0,
        };
    }
    return null;
}

/**
 * Подпись полосы нижней клавиатуры.
 *
 * @param {?object} markup — нормализованная разметка
 * @returns {string}
 */
export function barTitle(markup) {
    if (!markup) return '';
    return markup.singleUse ? '// KEYBOARD · ONE-TIME' : '// KEYBOARD';
}

// ---------------------------------------------------------------------------
// Состояние рисовальщика
// ---------------------------------------------------------------------------

const local = {
    /** Колбэк «отправить нажатие» — ставит chat.js (см. initBotKeyboards). */
    onSendText: null,
    /** Колбэк «кнопку нажали» — ставит chat.js (см. initBotKeyboards). */
    onButtonPress: null,
    /** Полоса нижней клавиатуры над #vk-prompt (создаётся лениво). */
    bar: null,
    /** Контейнер рядов внутри полосы. */
    barRows: null,
    /** Заголовок полосы. */
    barTitleEl: null,
    /** dialogId → {markup, messageId, cmid, collapsed}. */
    reply: new Map(),
};

/**
 * Привязывает внешние колбэки.
 *
 * `onSendText(label, press)` — это `chat.js::send`. Своего POST здесь нет
 * СОЗНАТЕЛЬНО: иначе у нажатия не было бы ни optimistic-строки, ни
 * дедупликации по `insertMessage`, ни кнопки [retry] — и в ленте появлялись бы
 * дубли. `press` = `{messageId, row, col}` (нажатие через роут
 * `POST .../messages/:mid/button`, `payload` подставит сервер) либо `null`
 * (страховка для сообщения без id: подпись уходит обычным текстом, БЕЗ
 * `payload` — ровно то же, что произошло бы, набери пользователь её руками).
 *
 * `onButtonPress(dialogId, cmid)` — регистрация «следования» в chat.js.
 * Через колбэк, а не прямым импортом: про прокрутку ленты этот модуль знать не
 * должен, иначе клавиатуры перестанут быть переносимыми и тестируемыми без
 * DOM. Ключ — `cmid`, а не `message_id`: строки ленты VK индексируются в
 * `rowsByCmid` (см. шапку js/vk/chat.js).
 *
 * @param {{onSendText?: function(string, ?object): void,
 *   onButtonPress?: function(string, number): void}} opts
 */
export function initBotKeyboards(opts) {
    if (opts && typeof opts.onSendText === 'function') local.onSendText = opts.onSendText;
    if (opts && typeof opts.onButtonPress === 'function') local.onButtonPress = opts.onButtonPress;
}

// ---------------------------------------------------------------------------
// Инлайн-клавиатура (под сообщением)
// ---------------------------------------------------------------------------

/**
 * Кнопка клавиатуры в терминальном стиле (скобки `[ … ]` рисует CSS).
 *
 * Неподдерживаемая кнопка ОТКЛЮЧАЕТСЯ (`disabled`), а не просто красится:
 * клик по ней не должен ни отправлять запрос, ни выглядеть «сработавшим».
 * Причина уезжает в `title` — она и есть объяснение ограничения платформы.
 */
function buttonEl(btn, action) {
    const el = document.createElement('button');
    el.type = 'button';
    el.className = 'vk-kb-btn';
    // ТОЛЬКО textContent: подпись пришла от чужого сообщества.
    el.textContent = btn.text;
    el.dataset.kbKind = btn.kind;
    el.dataset.kbAction = action.action;
    if (btn.color) el.dataset.kbColor = btn.color;
    if (action.action === 'unsupported') {
        el.classList.add('vk-kb-btn-unsupported');
        el.title = action.reason || UNSUPPORTED_REASON.other;
        el.disabled = true;
    } else if (action.action === 'open-url') {
        el.classList.add('vk-kb-btn-url');
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
 * дописывает его в строку лога после карточек вложений.
 *
 * @param {object} msg — MessageDto
 * @returns {?HTMLElement}
 */
export function buildMessageKeyboard(msg) {
    const markup = normalizeMarkup(msg && msg.reply_markup);
    if (!markup || markup.kind !== 'inline') return null;

    const ctx = messageCtx(msg);
    const wrap = document.createElement('div');
    wrap.className = 'vk-kb';
    wrap.dataset.dialogId = ctx.dialogId;
    wrap.dataset.msgId = String(ctx.messageId);

    for (const row of markup.rows) {
        const rowEl = document.createElement('div');
        rowEl.className = 'vk-kb-row';
        for (const btn of row) {
            rowEl.appendChild(mountButton(btn, ctx, null));
        }
        wrap.appendChild(rowEl);
    }
    return wrap;
}

/** Реквизиты сообщения, которыми адресуется нажатие. */
function messageCtx(msg) {
    return {
        dialogId: String((msg && msg.dialog_id) == null ? '' : msg.dialog_id),
        messageId: Number(msg && msg.id) || 0,
        cmid: Number(msg && msg.cmid) || 0,
    };
}

/**
 * Собирает кнопку и вешает обработчик, если нажимать её вообще есть чем.
 *
 * `onDone` зовётся только после действия, которое ДЕЙСТВИТЕЛЬНО что-то
 * отправило: `one_time` не имеет права прятать клавиатуру из-за клика по
 * ссылке.
 */
function mountButton(btn, ctx, onDone) {
    let action = buttonAction(btn);
    // Нажатие адресуется парой (сообщение, координаты). Сообщения без id у VK
    // не бывает, но кадр мог приехать из кэша вкладки старой версии — тогда
    // честнее погасить кнопку, чем отправить «что-то похожее».
    if (action.action === 'press' && !(ctx.messageId > 0) && !local.onSendText) {
        action = unsupported(NO_MESSAGE_REASON);
    }
    const el = buttonEl(btn, action);
    if (action.action !== 'unsupported') {
        el.addEventListener('click', () => {
            if (runAction(action, ctx) && typeof onDone === 'function') onDone(action);
        });
    }
    return el;
}

// ---------------------------------------------------------------------------
// Действия
// ---------------------------------------------------------------------------

/**
 * Разводит нажатие по видам действий.
 *
 * @returns {boolean} что-то действительно отправилось
 */
function runAction(action, ctx) {
    if (action.action === 'open-url') {
        openExternal(action.url);
        return false;
    }
    if (action.action !== 'press') return false;

    // Нажатие кнопки — явный интент пользователя, и его результат обязан
    // приехать к нему сам (см. js/messenger/follow-core.js). Сообщаем о
    // нажатии ДО запроса: ответ сообщества приходит отдельным кадром LongPoll
    // и вполне может опередить ответ роута.
    if (typeof local.onButtonPress === 'function') {
        try {
            // Автопрокрутка — удобство поверх нажатия; её падение не должно
            // отменять само нажатие.
            local.onButtonPress(ctx.dialogId, ctx.cmid);
        } catch (e) {
            console.debug('[vk-botkb] onButtonPress failed', e);
        }
    }
    if (typeof local.onSendText !== 'function') return false;
    const press = ctx.messageId > 0
        ? { messageId: ctx.messageId, row: action.row, col: action.col }
        : null;
    local.onSendText(action.text, press);
    return true;
}

// ---------------------------------------------------------------------------
// Нижняя клавиатура (полоса над полем ввода)
// ---------------------------------------------------------------------------

/**
 * Создаёт полосу над `#vk-prompt` при первой надобности.
 *
 * Узел создаётся в JS, а не в index.html, по двум причинам: у большинства
 * аккаунтов бесед с нижней клавиатурой нет вовсе (пустой div в разметке
 * навсегда), и вкладку VK можно выключить целиком, не трогая вёрстку.
 *
 * Полоса встаёт перед `#vk-prompt`; полоса ответа (chat.js::ensureReplyBar)
 * перепроверяет своё место на каждый показ и остаётся вплотную к полю ввода,
 * поэтому порядок «клавиатура → ответ → форма» держится сам собой.
 */
function ensureBar() {
    if (local.bar) return local.bar;
    if (typeof document === 'undefined' || !$vkPrompt || !$vkPrompt.parentNode) return null;

    const bar = document.createElement('div');
    bar.className = 'vk-kb-bar';
    bar.hidden = true;

    const head = document.createElement('div');
    head.className = 'vk-kb-bar-head';
    const title = document.createElement('span');
    title.className = 'vk-kb-bar-title';
    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'vk-kb-btn vk-kb-bar-close';
    close.textContent = '✕';
    close.title = 'Свернуть клавиатуру';
    close.addEventListener('click', () => collapseBar(true));
    head.appendChild(title);
    head.appendChild(close);

    const rows = document.createElement('div');
    rows.className = 'vk-kb-bar-rows';

    bar.appendChild(head);
    bar.appendChild(rows);
    $vkPrompt.parentNode.insertBefore(bar, $vkPrompt);

    local.bar = bar;
    local.barRows = rows;
    local.barTitleEl = title;
    return bar;
}

/** Сворачивает/разворачивает полосу активной беседы. */
function collapseBar(collapsed) {
    const id = activeId();
    const entry = id ? local.reply.get(id) : null;
    if (!entry) return;
    entry.collapsed = !!collapsed;
    syncReplyBar();
}

/** id активной беседы строкой ('' ⇒ беседа не открыта). */
function activeId() {
    return vkState.activeDialogId ? String(vkState.activeDialogId) : '';
}

/** Прячет полосу. */
function hideBar() {
    if (local.bar) local.bar.hidden = true;
}

/**
 * Перерисовывает полосу под состояние активной беседы.
 *
 * Ряды пересобираются целиком: клавиатура меняется редко (только когда
 * сообщество прислало новую), а точечный патч кнопок потребовал бы отдельного
 * реестра, который разъезжался бы с состоянием при каждой смене беседы.
 *
 * @returns {boolean} видна ли полоса
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
    bar.classList.toggle('vk-kb-bar-collapsed', !!entry.collapsed);
    bar.hidden = false;

    if (entry.collapsed) {
        // Свёрнутое состояние оставляет ровно один способ вернуть клавиатуру.
        // Без него `one_time` означал бы «клавиатура исчезла навсегда», а
        // ВКонтакте обещает пользователю обратное.
        const show = document.createElement('button');
        show.type = 'button';
        show.className = 'vk-kb-btn vk-kb-bar-show';
        show.textContent = '⌨ keyboard';
        show.title = 'Показать клавиатуру сообщества';
        show.addEventListener('click', () => collapseBar(false));
        local.barRows.appendChild(show);
        return true;
    }

    const ctx = { dialogId: id, messageId: entry.messageId, cmid: entry.cmid };
    for (const row of markup.rows) {
        const rowEl = document.createElement('div');
        rowEl.className = 'vk-kb-row';
        for (const btn of row) {
            // `one_time`: ВКонтакте просит спрятать клавиатуру после первого
            // же нажатия. Прячем ТОЛЬКО после действия, которое действительно
            // что-то отправило — иначе клик по ссылке убирал бы рабочие
            // соседние кнопки.
            rowEl.appendChild(mountButton(btn, ctx, () => {
                if (markup.singleUse) collapseBar(true);
            }));
        }
        local.barRows.appendChild(rowEl);
    }
    return true;
}

/**
 * Учитывает разметку одного сообщения (WS `new_message` / `message_edited`).
 *
 * Инлайн-разметка здесь игнорируется: она рисуется под своим сообщением и
 * состоянием беседы не является.
 *
 * @param {object} msg — MessageDto
 * @returns {boolean} изменилось ли состояние клавиатуры беседы
 */
export function noteReplyMarkup(msg) {
    if (!msg || msg.out) return false;
    const markup = normalizeMarkup(msg.reply_markup);
    if (!markup || markup.kind === 'inline') return false;
    const ctx = messageCtx(msg);
    if (!ctx.dialogId) return false;

    local.reply.set(ctx.dialogId, {
        markup,
        messageId: ctx.messageId,
        cmid: ctx.cmid,
        collapsed: false,
    });
    if (ctx.dialogId === activeId()) syncReplyBar();
    return true;
}

/**
 * Пересчитывает клавиатуру беседы по загруженной истории.
 *
 * Зовётся из chat.js::renderHistory. Пересчёт (а не накопление) обязателен:
 * страница истории вверх приносит СТАРЫЕ сообщения, и «последней» клавиатура
 * должна остаться та, что ниже по ленте, а не та, что пришла позже по времени
 * загрузки.
 *
 * @param {Array<object>} messages — по возрастанию id
 * @param {string} [dialogId] — по умолчанию активная беседа
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
 * Убирает полосу с экрана (смена беседы).
 *
 * Состояние бесед НЕ стирается: возврат в беседу перерисует клавиатуру из
 * истории, а до этого момента показывать чужую клавиатуру нельзя — нажатие
 * ушло бы не в тот чат.
 */
export function clearReplyKeyboard() {
    hideBar();
}

/** Полный сброс (logout, смена аккаунта). */
export function resetKeyboards() {
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
        messageId: entry ? entry.messageId : 0,
    };
}
