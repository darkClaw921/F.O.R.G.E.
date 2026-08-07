// tmux-web — Bitrix24 plugin: inline-клавиатуры сообщений (`params.KEYBOARD`).
//
// ============================================================================
// ШЕСТЬ ДЕЙСТВИЙ (forge-zw3d: было четыре, и ДВА САМЫХ ЖИВЫХ не работали)
// ============================================================================
//
//   PUT     — подставить текст в поле ввода. Работает КЛИЕНТСКИ, сети не
//             требует вовсе.
//   SEND    — отправить текст сообщением. Тоже клиентское: нажатие уходит
//             колбэком в ленту, а та отправляет его тем же путём, что и Enter
//             (`chat.js::submitPrompt`). Работает с Ф6; если ленту
//             инициализировали без отправки, кнопка честно выключается
//             ([`SEND_UNAVAILABLE`]) — молча ничего не делающая кнопка хуже
//             отсутствующей.
//   LINK    — открыть URL (поле портала `LINK`, forge-zw3d). Работает
//             КЛИЕНТСКИ, `window.open`. ЖИВОЙ и самый частый вид кнопки у
//             РЕАЛЬНЫХ ботов портала («Команда заботы Битрикс24» шлёт им
//             «Получить консультацию») — до forge-zw3d разбирался только
//             `ACTION`, и такая кнопка приезжала выключенной с ложным
//             «незнакомое действие».
//   COMMAND — выполнить команду бота (`COMMAND`+`COMMAND_PARAMS`+`BOT_ID`,
//             forge-zw3d). Требует СЕТЬ: `POST …/messages/:mid/command` →
//             бэкенд → `im.message.command`. В отличие от `CALL`/`DIALOG`
//             НЕ требует scope `imbot` (живая проба портала подтвердила
//             успех обычным входящим вебхуком). ЖИВОЙ пример — кнопки оценки
//             1…10 того же системного бота.
//   CALL    — позвонить.   ─┬─ требуют scope `imbot`, которого у ВХОДЯЩЕГО
//   DIALOG  — открыть чат. ─┘   вебхука нет и быть не может (§6). Приезжают
//                               выключенными; причину пишет бэкенд в
//                               `disabled_reason`, фронт лишь показывает её.
//
// Кнопка, которая выглядит рабочей и молча ничего не делает, хуже
// отсутствующей: пользователь жмёт её несколько раз и решает, что сломана вся
// панель. Поэтому «выключено + объяснение» — это не заглушка, а требование §6.
//
// ============================================================================
// ГРАФ ИМПОРТОВ И XSS
// ============================================================================
//
// Модуль НЕ импортирует ленту (chat.js): PUT/SEND уходят наверх колбэком,
// который раздаёт `chat.js::initChat` — про поле ввода и очередь отправки
// этот модуль знать не должен. COMMAND — исключение: он самодостаточен (сеть
// + собственное состояние кнопки, как у `telegram/botkb.js::doCallback`) и
// поэтому напрямую импортирует `./api.js`, а не бродит через колбэк лишним
// слоем. Разметка собирается только `createElement` и `textContent` —
// подпись кнопки пишет бот, и `[B]жирная[/B]` или `<img onerror=…>` в ней
// обязаны оказаться в документе ТЕКСТОМ. Адрес `LINK`-кнопки — тоже чужой
// текст: проверяется белым списком схем ДВАЖДЫ (бэкенд уже отфильтровал
// `http`/`https` в `attach.rs::http_url`, но второй рубеж дешевле, чем
// доверие чужой стороне) и открывается с `noopener,noreferrer`.
//
// Декоративные скобки `[ … ]` вокруг подписи рисует CSS через `::before`/
// `::after`: в текстовом узле обязана лежать ровно подпись от бота, и ничего
// больше.

import { notify } from '../echo/notifications.js';

import { sendCommand } from './api.js';
import { errorLabel } from './format.js';

/** Сколько рядов клавиатуры рисуется. */
export const MAX_ROWS = 32;

/** Сколько кнопок рисуется в одном ряду. */
export const MAX_ROW_BUTTONS = 12;

/** Длина подписи кнопки, символов (дальше — многоточие). */
export const BUTTON_TEXT_MAX = 48;

/** Почему `SEND` не работает, когда лента инициализирована без отправки. */
export const SEND_UNAVAILABLE = 'Отправка сообщений из панели пока недоступна';

/**
 * Схемы, которые разрешено открывать из `LINK`-кнопки.
 *
 * Только `http`/`https` — портал сам ничего другого в `LINK` не кладёт (см.
 * `attach.rs::http_url`), но это ВТОРОЙ рубеж, а не единственный: доверять
 * чужой стороне на клиенте нельзя, даже если она уже прошла бэкенд.
 */
const ALLOWED_LINK_SCHEMES = new Set(['http', 'https']);

const local = {
    /** PUT: подставить текст в поле ввода. */
    onPutText: null,
    /** SEND: отправить текст. `null` ⇒ кнопки SEND выключены. */
    onSendText: null,
};

/**
 * Принимает колбэки от точки сборки. Идемпотентна — зовётся на каждом входе на
 * вкладку.
 *
 * @param {{onPutText?: ?function, onSendText?: ?function}} [handlers]
 */
export function initBotKeyboards(handlers = {}) {
    local.onPutText = typeof handlers.onPutText === 'function' ? handlers.onPutText : null;
    local.onSendText = typeof handlers.onSendText === 'function' ? handlers.onSendText : null;
}

/**
 * Обрезает подпись кнопки по СИМВОЛАМ.
 *
 * `Array.from`, а не `slice`: эмодзи занимают две единицы UTF-16, и `slice`
 * умеет разрезать их пополам, оставляя в DOM «сломанный» символ.
 *
 * @param {?string} raw
 * @returns {string}
 */
export function cleanButtonText(raw) {
    const text = String(raw == null ? '' : raw).replace(/\s+/g, ' ').trim();
    const chars = Array.from(text);
    if (chars.length <= BUTTON_TEXT_MAX) return text;
    return `${chars.slice(0, BUTTON_TEXT_MAX - 1).join('')}…`;
}

/**
 * Второй рубеж проверки URL `LINK`-кнопки (первый — `attach.rs::http_url`).
 *
 * @param {*} raw
 * @returns {?string} тот же URL, если открывать безопасно, иначе `null`
 */
export function safeUrl(raw) {
    if (typeof raw !== 'string') return null;
    const clean = raw.trim();
    if (!clean) return null;
    const m = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(clean);
    if (!m || !ALLOWED_LINK_SCHEMES.has(m[1].toLowerCase())) return null;
    return clean;
}

/**
 * Можно ли нажать кнопку ЗДЕСЬ И СЕЙЧАС и почему нет.
 *
 * Два источника отказа, и они разные: бэкенд знает про права портала
 * (`enabled: false` + `disabled_reason`), фронт — про то, какие обработчики у
 * него сейчас есть (отправки нет до Ф6) либо про свою повторную проверку
 * (`LINK` — второй рубеж URL, forge-zw3d).
 *
 * @param {object} button — ButtonDto
 * @returns {{enabled: boolean, reason: string}}
 */
export function buttonState(button) {
    if (!button || button.enabled !== true) {
        return {
            enabled: false,
            reason: String((button && button.disabled_reason) || 'Кнопка недоступна'),
        };
    }
    if (button.action === 'send' && !local.onSendText) {
        return { enabled: false, reason: SEND_UNAVAILABLE };
    }
    if (button.action === 'put' && !local.onPutText) {
        return { enabled: false, reason: 'Поле ввода недоступно' };
    }
    if (button.action === 'link' && !safeUrl(button.value)) {
        return { enabled: false, reason: 'Ссылка недопустима' };
    }
    return { enabled: true, reason: '' };
}

/**
 * Выполняет СИНХРОННОЕ нажатие (`put`/`send`/`link`) — сети здесь нет вовсе,
 * только колбэки наверх и `window.open`. `command` сюда не входит: у него
 * есть сеть и собственное состояние кнопки, см. [`doCommand`] в
 * [`buildButton`].
 *
 * @param {object} button — ButtonDto
 * @param {?object} msg — сообщение, которому принадлежит клавиатура
 * @returns {boolean} было ли действие выполнено
 */
export function pressButton(button, msg) {
    const state = buttonState(button);
    if (!state.enabled) return false;
    const value = String(button.value == null ? '' : button.value);
    if (button.action === 'put') {
        local.onPutText(value, msg);
        return true;
    }
    if (button.action === 'send') {
        local.onSendText(value, msg);
        return true;
    }
    if (button.action === 'link') {
        // noopener,noreferrer: открытая страница не должна получить ни
        // window.opener, ни Referer с адресом нашей вкладки.
        const w = globalThis.open(value, '_blank', 'noopener,noreferrer');
        if (w) {
            try { w.opener = null; } catch (_) { /* браузер уже обнулил сам */ }
        }
        return true;
    }
    return false;
}

/**
 * Нажатие `COMMAND`-кнопки: сеть + собственное состояние (forge-zw3d).
 *
 * Пока запрос в полёте, кнопка `disabled` — повтор нажатия означал бы
 * повторное исполнение бот-команды (`im.message.command` НЕ идемпотентен, см.
 * `api/methods.rs::M_MESSAGE_COMMAND`). `lock_after_press` (портальское
 * `BLOCK: Y`) после УСПЕХА гасит кнопку НАВСЕГДА — то же самое просит сделать
 * портал у официальных клиентов; ошибка кнопку не блокирует, чтобы можно было
 * попробовать ещё раз.
 *
 * @param {object} el — узел кнопки
 * @param {object} button — ButtonDto
 * @param {?object} msg — сообщение, которому принадлежит клавиатура
 */
function doCommand(el, button, msg) {
    if (!el || el.disabled) return;
    const messageId = Number(msg && msg.id);
    if (!Number.isFinite(messageId) || messageId <= 0) return;
    el.disabled = true;
    el.classList.add('b24-kb-btn-pending');
    sendCommand(messageId, {
        botId: button.bot_id,
        command: button.command,
        commandParams: button.command_params,
    })
        .then(() => {
            el.classList.remove('b24-kb-btn-pending');
            if (button.lock_after_press) {
                el.classList.add('b24-kb-btn-off');
                el.title = 'Действие уже выполнено';
                return;
            }
            el.disabled = false;
        })
        .catch((err) => {
            el.classList.remove('b24-kb-btn-pending');
            el.disabled = false;
            notify({ level: 'error', title: 'BOT', body: errorLabel(err && err.code) });
        });
}

/** Подсказка кнопки по виду действия (для ВКЛЮЧЁННОЙ кнопки). */
function buttonTitle(action) {
    switch (action) {
        case 'put': return 'Подставить текст в поле ввода';
        case 'link': return 'Открыть ссылку';
        case 'command': return 'Выполнить команду бота';
        default: return 'Отправить сообщением';
    }
}

/**
 * Одна кнопка.
 *
 * @param {object} button — ButtonDto
 * @param {?object} msg
 * @param {object} doc
 * @returns {object} узел
 */
function buildButton(button, msg, doc) {
    const node = doc.createElement('button');
    node.className = 'b24-kb-btn';
    node.type = 'button';
    node.dataset.action = String(button.action || 'other');
    // Подпись — ТЕКСТОВЫЙ узел: её пишет бот.
    node.textContent = cleanButtonText(button.text);

    const state = buttonState(button);
    if (!state.enabled) {
        node.disabled = true;
        node.classList.add('b24-kb-btn-off');
        // Объяснение обязательно (§6): выключенная кнопка без причины
        // читается как поломка панели.
        node.title = state.reason;
    } else {
        node.title = buttonTitle(button.action);
        if (button.action === 'command') {
            // COMMAND сам себе владелец состояния (сеть + pending/lock) —
            // pressButton его не касается (см. шапку модуля).
            node.addEventListener('click', () => doCommand(node, button, msg));
        } else {
            node.addEventListener('click', () => {
                pressButton(button, msg);
            });
        }
    }

    // Цвета уже проверены бэкендом (`attach.rs::hex_color`); повторная
    // проверка здесь — второй рубеж перед записью в `style`.
    //
    // ЖИВАЯ НАХОДКА (forge-zw3d): `bg_color`/`text_color` — ПАРА, которую
    // подбирал автор бота под заливку кнопки (портал заполняет ею весь
    // прямоугольник в своих официальных клиентах), а не под наш прозрачный
    // терминальный фон. `BG_COLOR: "#fff2b7"` + `TEXT_COLOR: "#000"` («Команда
    // заботы Битрикс24») читаемы вместе — но раньше `bg_color` красил только
    // РАМКУ, а чёрный `text_color` ложился на прозрачный тёмный фон панели и
    // был НЕВИДИМ (кнопка выглядела пустым прямоугольником). Заливка кнопки —
    // единственный способ гарантировать контраст, который уже подобрал
    // отправитель: гадать свой контраст под обе темы панели смысла нет.
    const hex = /^#[0-9a-f]{3}([0-9a-f]{3})?$/i;
    if (typeof button.bg_color === 'string' && hex.test(button.bg_color)) {
        node.style.backgroundColor = button.bg_color;
        node.style.borderColor = button.bg_color;
    }
    if (typeof button.text_color === 'string' && hex.test(button.text_color)) {
        node.style.color = button.text_color;
    }
    return node;
}

/**
 * Клавиатура сообщения целиком.
 *
 * @param {?object} markup — ReplyMarkupDto (`msg.reply_markup`)
 * @param {?object} msg — сообщение, которому она принадлежит
 * @param {object} [doc]
 * @returns {?object} узел либо `null`, если клавиатуры нет
 */
export function buildKeyboard(markup, msg, doc = globalThis.document) {
    const rows = (markup && Array.isArray(markup.rows) ? markup.rows : []).slice(0, MAX_ROWS);
    if (rows.length === 0) return null;

    const box = doc.createElement('div');
    box.className = 'b24-kb';
    let drawn = 0;
    for (const row of rows) {
        const buttons = (Array.isArray(row) ? row : []).slice(0, MAX_ROW_BUTTONS);
        if (buttons.length === 0) continue;
        const line = doc.createElement('div');
        line.className = 'b24-kb-row';
        for (const button of buttons) {
            if (!button || !button.text) continue;
            line.appendChild(buildButton(button, msg, doc));
            drawn += 1;
        }
        box.appendChild(line);
    }
    return drawn > 0 ? box : null;
}

/** Снимок для e2e и диагностики. */
export function _debugBotKbState() {
    return {
        canPut: !!local.onPutText,
        canSend: !!local.onSendText,
    };
}
