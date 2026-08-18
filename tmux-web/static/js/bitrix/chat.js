// tmux-web — Bitrix24 plugin: лента диалога `#b24-log`.
//
// ============================================================================
// STICK-TO-BOTTOM — ЗАЩЁЛКА, А НЕ ВЫЧИСЛЕНИЕ
// ============================================================================
//
// Состояние «пользователь читает конец ленты» хранится БУЛЕВЫМ ФЛАГОМ
// (`local.stickBottom`), который переключается ТОЛЬКО в обработчике прокрутки.
// Вычислять `isAtBottom()` в момент вставки строки нельзя: высота ленты к тому
// моменту уже изменилась, и ответ будет про новую ленту, а не про намерение
// пользователя. Ровно поэтому лента VK и Telegram дёргались бы при догрузке
// сверху — там защёлка есть по той же причине.
//
// ============================================================================
// ДОГРУЗКА СВЕРХУ КОМПЕНСИРУЕТ ВЫСОТУ
// ============================================================================
//
// Вставка блока НАД видимой областью сдвигает содержимое вниз ровно на высоту
// вставленного. Если не поправить `scrollTop` на ту же величину, читаемая
// строка уезжает с экрана, и пользователь видит скачок. Компенсация делается
// синхронно, в том же кадре, что и вставка (`renderOlder`).
//
// ============================================================================
// XSS-ИНВАРИАНТ
// ============================================================================
//
// `innerHTML` в модуле нет вовсе: текст сообщения приходит с портала, где его
// пишут люди и боты. Разметка собирается УЗЛАМИ — `format.js::renderSegments`
// по структурным сегментам, которые отдал бэкенд (`bbcode.rs`). Ссылка не
// становится `<a href>`: адрес прислал собеседник.
//
// ============================================================================
// ОПТИМИСТИЧНАЯ ОТПРАВКА И ОТСУТСТВИЕ АВТОПОВТОРА (Р9)
// ============================================================================
//
// Строка появляется в ленте МГНОВЕННО, с временным идентификатором
// (`TEMP_ID_BASE` + счётчик), и заменяется настоящей, когда портал ответит.
// Временные идентификаторы заведомо больше любого реального, поэтому строка
// встаёт в конец ленты, не ломая сортировку по возрастанию.
//
// АВТОПОВТОРА НЕТ И БЫТЬ НЕ МОЖЕТ. У `im.message.add` нет дедуп-идентификатора
// (аналога `random_id` у VK): повтор после обрыва — это ВТОРОЕ сообщение у
// собеседника, а не то же самое. Поэтому при ошибке строка остаётся в ленте с
// кнопкой «⟳ повторить», текст не теряется, и решение принимает человек.
//
// ============================================================================
// СЧЁТЧИК СИМВОЛОВ (Р12)
// ============================================================================
//
// Портал принимает текст любой длины, отвечает успехом и МОЛЧА режет его на
// 20 006 символах. «Успешно отправленный» обрубок хуже честного отказа —
// отправитель о нём не узнает. Поэтому счётчик появляется на подходе к лимиту,
// а кнопка отправки блокируется ДО того, как запрос уйдёт.
//
// ============================================================================
// ГРАФ ИМПОРТОВ
// ============================================================================
//
// Лента импортирует карточки (`attachments.js`) и клавиатуры (`botkb.js`), но
// НЕ наоборот: те получают колбэки. Колонка (`dialogs.js`) ленту тоже не
// импортирует — их соединяет точка сборки `main.js`.

import {
    $b24CharCount, $b24Input, $b24Log, $b24Prompt, $b24PromptNote, $b24Send, $b24SessionHead,
} from '../core/dom.js';
// Кнопка встречи — общая на три панели (js/messenger/meet-row.js). При
// выключенном плагине `attachMeetButton` выходит первой строкой и не стоит
// ленте ничего: ни детекта, ни узла, ни запроса.
import { topEscLayer } from '../messenger/esc-stack.js';
import { attachMeetButton } from '../messenger/meet-row.js';
import * as b24Api from './api.js';
import { buildCards } from './attachments.js';
import { buildKeyboard, initBotKeyboards } from './botkb.js';
import {
    dateSepLabel, displayName, errorLabel, formatTime, isAtBottom, mergeMessages,
    needsDateSep, newestId, oldestId, renderSegments, segmentsText, senderLabel,
    shouldLoadOlder,
} from './format.js';
import {
    attachAvatar, dropMessageMedia, renderMediaGroup, resetMediaCards, revokeDialogMedia,
} from './media.js';
import { findQuoteOrigin, splitQuote } from './reply.js';
import { b24State } from './state.js';

/** Сколько сообщений просим за раз (потолок платформы — 50). */
export const PAGE_SIZE = 50;

/**
 * Потолок длины сообщения, символов (Р12).
 *
 * Дословно совпадает с `crate::dto::MAX_MESSAGE_CHARS`: сервер отвечает 400 на
 * превышение, а фронт обязан не доводить до него.
 */
export const MAX_MESSAGE_CHARS = 20000;

/** С какой длины показывается счётчик символов. */
export const CHAR_WARN_AT = MAX_MESSAGE_CHARS - 1000;

/**
 * База временных идентификаторов оптимистичных строк.
 *
 * Заведомо больше любого идентификатора портала (там счёт идёт десятками
 * тысяч) и заведомо меньше `Number.MAX_SAFE_INTEGER` — строка встаёт в конец
 * ленты, а арифметика сортировки остаётся точной.
 */
export const TEMP_ID_BASE = 9_000_000_000_000;

/** Окно отметки о прочтении, мс (сервер держит такое же). */
export const READ_DEBOUNCE_MS = 800;

/**
 * Сколько держится подсветка оригинала после перехода по цитате, мс.
 *
 * Столько же, сколько у VK (`vk/chat.js::REPLY_FLASH_MS`): достаточно, чтобы
 * глаз нашёл строку, и мало, чтобы подсветка не осталась висеть как
 * «выделенное сообщение» — она отвечает на вопрос «куда меня перенесло», а не
 * «что выбрано».
 */
export const REPLY_FLASH_MS = 2000;

/**
 * Сколько страниц истории догружать в поисках оригинала (forge-92x6).
 *
 * Потолок обязателен: у портала бюджет 2 запроса в секунду на весь плагин, и
 * «листать до оригинала» в переписке на десять тысяч строк означало бы минуты
 * молчащей панели. Не нашли за три страницы — честно говорим об этом, а не
 * листаем дальше: доспросить портал про КОНКРЕТНОЕ сообщение (как это делает
 * VK) здесь нечем — идентификатора оригинала у нас нет вовсе (Р10).
 */
export const REPLY_JUMP_PAGES = 3;

/** Окно индикатора «печатает…», мс (сервер держит такое же). */
export const TYPING_INTERVAL_MS = 8000;

/**
 * Текст, которым помечается удалённая строка (Ф7).
 *
 * Своя формулировка, а не портальная («Это сообщение было удалено»): кадр
 * `message_delete` текста не несёт вовсе, и подставлять сюда чужую строку
 * значило бы гадать её дословную формулировку и локаль портала.
 */
export const DELETED_TEXT = 'Сообщение удалено';

const local = {
    /** Слушатели навешены (init идемпотентен). */
    inited: false,
    /**
     * ЗАЩЁЛКА: пользователь стоит у нижнего края ленты.
     *
     * Значение по умолчанию — `true`: открытая лента показывает конец
     * истории, и первая же вставка обязана прокрутить её вниз.
     */
    stickBottom: true,
    /** Идёт загрузка страницы — второй запрос не начинаем. */
    loading: false,
    /** Есть ли ещё история выше загруженной. */
    hasOlder: false,
    /**
     * Поколение открытия диалога.
     *
     * Ответ на страницу ПРОШЛОГО диалога не имеет права нарисоваться в ленте
     * нового: пользователь щёлкает по колонке быстрее, чем отвечает портал.
     */
    generation: 0,
    /** Диалог, лента которого сейчас на экране. */
    dialogId: null,
    /** Колбэк «портал подтвердил прочтение» — им main.js освежает счётчики. */
    onRead: null,
    /** Счётчик временных идентификаторов оптимистичных строк. */
    tempSeq: 0,
    /**
     * Режим ввода: `null` (обычный), `{kind:'reply', …}` или
     * `{kind:'edit', …}`.
     *
     * Один слот на оба режима: «правлю сообщение и одновременно отвечаю на
     * другое» — состояние, которого не существует, и разрешать его значило бы
     * решать, что делать с цитатой при отправке правки.
     */
    mode: null,
    /** Идёт отправка — второй Enter не должен породить второе сообщение. */
    sending: false,
    /** Таймер отложенной отметки о прочтении. */
    readTimer: null,
    /** Что отметить прочитанным, когда таймер сработает. */
    readPending: null,
    /** Когда «печатает…» уходило в последний раз (ключ — диалог). */
    typingAt: new Map(),
    /**
     * Идёт переход к оригиналу цитаты — второй клик не запускает второй обход.
     *
     * Переход умеет ЖДАТЬ страницы истории, и без этого флага пять кликов по
     * цитате дали бы пять параллельных обходов, каждый со своей догрузкой.
     */
    jumping: false,
    /**
     * Подсвеченная строка (id) либо `null`.
     *
     * Хранится ИДЕНТИФИКАТОР, а не узел: `renderHistory` перерисовывает ленту
     * целиком, и ссылка на узел протухла бы на первой же новой строке. По id
     * подсветка возвращается на своё место сама ([`buildLine`]).
     */
    flashId: null,
    /** Таймер снятия подсветки. */
    flashTimer: null,
    /**
     * Цель ответа СВОИХ сообщений этой сессии: id ответа → id оригинала.
     *
     * Точное знание вместо угадывания по тексту: при отправке цель известна
     * достоверно, и терять её только потому, что портал не отражает `REPLY_ID`
     * при чтении (Р10), незачем. Живёт до перезагрузки вкладки — дальше
     * работает поиск по тексту цитаты (`reply.js::findQuoteOrigin`).
     */
    replyById: new Map(),
};

// ---------------------------------------------------------------------------
// Состояние ленты
// ---------------------------------------------------------------------------

/** Сообщения открытого диалога (всегда по возрастанию id). */
function currentMessages() {
    const key = String(local.dialogId == null ? '' : local.dialogId);
    if (!key) return [];
    if (!b24State.messages.has(key)) b24State.messages.set(key, []);
    return b24State.messages.get(key);
}

/** Прокручивает ленту в самый низ. */
function scrollToBottom() {
    if (!$b24Log) return;
    $b24Log.scrollTop = Number($b24Log.scrollHeight) || 0;
}

// ---------------------------------------------------------------------------
// Рендер строки
// ---------------------------------------------------------------------------

/**
 * Строка ленты.
 *
 * Системное сообщение (`system: true`) рисуется ОТДЕЛЬНЫМ рядом — без автора,
 * без бабла, по центру: «Игорь создал чат» это событие ленты, а не реплика, и
 * оформление обязано это показывать.
 *
 * @param {object} msg — MessageDto
 * @returns {object} узел `li`
 */
export function buildLine(msg) {
    const doc = document;
    const li = doc.createElement('li');
    li.className = 'b24-line';
    li.dataset.msgId = String(msg.id);
    if (msg.date) li.dataset.date = String(msg.date);

    if (msg.system) {
        li.classList.add('b24-line-system');
        if (msg.action) li.dataset.action = String(msg.action);
        const text = doc.createElement('span');
        text.className = 'b24-line-text';
        text.appendChild(renderSegments(msg.segments, doc));
        li.appendChild(text);
        appendExtras(li, msg, doc);
        return li;
    }

    if (msg.out) li.classList.add('b24-line-own');
    // Оптимистичная строка и строка, отправка которой не удалась: оформление
    // обязано отличать «ещё летит» от «не улетело», иначе пользователь ждёт
    // доставки того, чего уже не будет.
    if (msg.pending) li.classList.add('b24-line-pending');
    if (msg.failed) li.classList.add('b24-line-failed');
    // Удалённое сообщение портал НЕ СТИРАЕТ: он подменяет текст заглушкой и
    // оставляет строку в истории (params.IS_DELETED). Прятать её нельзя —
    // после перезагрузки ленты она вернулась бы; поэтому строка остаётся на
    // месте, но выглядит вычеркнутой и теряет кнопки действий.
    if (msg.deleted) li.classList.add('b24-line-deleted');
    // Подсветка перехода по цитате переживает перерисовку ленты: класс
    // возвращается по запомненному id, а снимет его таймер ([`flashRow`]).
    if (local.flashId != null && Number(msg.id) === local.flashId) {
        li.classList.add('b24-line-flash');
    }

    const time = doc.createElement('span');
    time.className = 'b24-line-time';
    time.textContent = formatTime(msg.date);
    li.appendChild(time);

    const nick = doc.createElement('span');
    nick.className = msg.out ? 'b24-line-nick b24-line-nick-own' : 'b24-line-nick';
    nick.textContent = senderLabel(msg);
    if (msg.sender && msg.sender.color) nick.style.color = String(msg.sender.color);
    appendSenderAvatar(nick, msg, doc);
    li.appendChild(nick);

    const body = doc.createElement('span');
    body.className = 'b24-line-body';

    // Ответ показывается ЦИТАТОЙ ОТДЕЛЬНОЙ СТРОКОЙ, а не «> …» в начале
    // текста: у Битрикс24 это единственный след ответа, доживающий до чтения
    // (Р10), и без выделения он неотличим от реплики, начатой со стрелочки.
    const split = splitQuote(msg.segments);
    if (split) body.appendChild(buildQuoteNode(split.quote, msg, doc));

    const text = doc.createElement('span');
    text.className = 'b24-line-text';
    text.appendChild(renderSegments(split ? split.body : msg.segments, doc));
    body.appendChild(text);

    const suffix = buildSuffix(msg, doc);
    if (suffix) body.appendChild(suffix);
    const actions = buildActions(msg, doc);
    if (actions) body.appendChild(actions);
    li.appendChild(body);

    // Кнопка встречи — В ТОТ ЖЕ контейнер действий, что и остальные кнопки
    // строки: обёртка Telegram и VK здесь не нужна, у Битрикса контейнер уже
    // есть и селекторы у него через ПОТОМКА.
    //
    // Ставится ПОСЛЕ сборки строки, а не внутри `buildActions`: закрепление
    // видимости вешается атрибутом на САМУ СТРОКУ (`opacity` контейнера
    // создаёт группу композиции, и изнутри него закрепить одну кнопку
    // невозможно), а строки на момент сборки действий ещё нет.
    //
    // Ветвление `buildActions` по `pending`/`deleted` намеренно НЕ трогаем:
    // там кнопок нет вовсе, и это чужое поведение панели.
    if (actions) {
        attachMeetButton(li, {
            panel: 'bitrix',
            msg,
            chatId: local.dialogId,
            container: actions,
            root: $b24Log,
            doc,
            // Кнопка объявляет себя действием строки — И ЭТО ВЕСЬ КОД, который
            // понадобился для хоткея: `keynav.js::rowActionButton` обходит
            // строку как `.b24-line-btn` и выбирает по `data-action`. Без
            // класса клавиатура кнопку не нашла бы, хотя атрибут на месте.
            extraClass: 'b24-line-btn',
        });
    }

    appendExtras(li, msg, doc);
    return li;
}

/**
 * Кликабельная цитата над текстом ответа (forge-92x6).
 *
 * `button`, а не `span`: узел действительно нажимается, и кнопка приносит
 * фокус с клавиатуры, Enter/Space и роль — бесплатно, без `tabindex`+`role`
 * руками. Заодно она попадает под `REPLY_DBLCLICK_IGNORE`, и двойной клик по
 * цитате не превращается в «ответить на ответ».
 *
 * Обработчик вешается ПРЯМО на узел, а не делегированием на ленту: цитата
 * знает свой разбор (`quote`), и передавать его через `dataset` значило бы
 * сериализовать структуру ради того же самого вызова.
 *
 * @param {{author: string, text: string, raw: string}} quote
 * @param {object} msg — сообщение-ОТВЕТ, в котором нарисована цитата
 * @param {object} doc
 * @returns {object}
 */
function buildQuoteNode(quote, msg, doc) {
    const node = doc.createElement('button');
    node.type = 'button';
    node.className = 'b24-line-quote';
    node.dataset.quoteFor = String(msg.id);
    node.title = 'Перейти к сообщению, на которое дан ответ';
    node.setAttribute('aria-label', `Ответ на сообщение: ${quote.raw}`);

    if (quote.author) {
        const name = doc.createElement('span');
        name.className = 'b24-line-quote-name';
        name.textContent = quote.author;
        node.appendChild(name);
    }
    const text = doc.createElement('span');
    text.className = 'b24-line-quote-text';
    text.textContent = quote.text || quote.raw;
    node.appendChild(text);

    node.addEventListener('click', () => {
        jumpToQuote(msg.id, quote);
    });
    return node;
}

/**
 * Дорисовывает к нику маленький аватар автора (Р6).
 *
 * Адрес берётся ТОЛЬКО из `sender.avatar_url`, а бэкенд кладёт туда путь прокси
 * `/api/bitrix/media/avatar/:user_id` — CDN-адреса портала в DOM панели не
 * бывает никогда. Сами байты приезжают `blob`-ом через `media.js`: прямой
 * `src="/api/…"` не пронёс бы `Authorization: Bearer` в remote-mode.
 *
 * Аватара может не быть (у сотрудника нет фото либо портал ещё не рассказал о
 * нём) — тогда узел не создаётся вовсе, и остаётся один цветной ник.
 *
 * @param {object} host — узел, к которому крепится картинка
 * @param {object} msg — MessageDto
 * @param {object} doc
 */
function appendSenderAvatar(host, msg, doc) {
    const url = msg.sender && typeof msg.sender.avatar_url === 'string'
        ? msg.sender.avatar_url
        : '';
    // Только собственный путь плагина: любой другой адрес — чужой хост в DOM.
    if (!url.startsWith('/api/bitrix/media/avatar/')) return;
    const img = doc.createElement('img');
    img.className = 'b24-line-avatar';
    img.alt = '';
    img.hidden = true;
    host.appendChild(img);
    attachAvatar(img, msg.sender.id);
}

/**
 * Кнопки действий строки.
 *
 * Три набора, взаимоисключающих по состоянию строки:
 *
 * | состояние | кнопки |
 * |---|---|
 * | отправляется | нет вовсе — идентификатора у строки ещё не существует |
 * | не отправилось | `⟳ повторить`, `✕ убрать` |
 * | обычная строка | `♥`, `↩`, плюс `✎`/`✕` у своей |
 *
 * Правку и удаление портал разрешает только автору, и предлагать их на чужой
 * строке значило бы обещать действие, которое кончится `409` (Р14: прав чата
 * панель не знает и знать не будет).
 *
 * @param {object} msg
 * @param {object} doc
 * @returns {?object}
 */
function buildActions(msg, doc) {
    const box = doc.createElement('span');
    box.className = 'b24-line-actions';

    if (msg.pending) return null;
    // Удалённой строке действия не предлагаются вовсе: лайкнуть, изменить или
    // удалить её портал не даст, а кнопка, которая всегда кончается 409, —
    // обещание, а не функция.
    if (msg.deleted) return null;
    if (msg.failed) {
        box.appendChild(actionButton(doc, '⟳', 'Повторить отправку', 'retry', () => {
            retrySend(msg.id);
        }));
        box.appendChild(actionButton(doc, '✕', 'Убрать строку и текст', 'discard', () => {
            removeMessage(msg.id);
        }));
        return box;
    }

    box.appendChild(actionButton(doc, '♥', 'Лайк', 'like', () => {
        toggleLike(msg.id);
    }));
    box.appendChild(actionButton(doc, '↩', 'Ответить', 'reply', () => {
        startReply(msg.id);
    }));
    if (msg.out) {
        box.appendChild(actionButton(doc, '✎', 'Изменить', 'edit', () => {
            startEdit(msg.id);
        }));
        box.appendChild(actionButton(doc, '✕', 'Удалить', 'delete', () => {
            deleteLine(msg.id);
        }));
    }
    return box;
}

/**
 * Одна кнопка действия строки.
 *
 * @param {object} doc
 * @param {string} glyph
 * @param {string} title
 * @param {string} action — значение `data-action` (по нему их находит e2e)
 * @param {function} onClick
 * @returns {object}
 */
function actionButton(doc, glyph, title, action, onClick) {
    const node = doc.createElement('button');
    node.className = 'b24-line-btn';
    node.type = 'button';
    node.dataset.action = action;
    node.title = title;
    node.setAttribute('aria-label', title);
    node.textContent = glyph;
    node.addEventListener('click', onClick);
    return node;
}

/**
 * Хвост строки: пометка о правке и счётчик лайков.
 *
 * Лайк — ЕДИНСТВЕННАЯ доступная вебхуку реакция (§6), поэтому он рисуется
 * счётчиком, а не набором эмодзи: набор обещал бы выбор, которого нет.
 *
 * @param {object} msg
 * @param {object} doc
 * @returns {?object}
 */
function buildSuffix(msg, doc) {
    const likes = Array.isArray(msg.likes) ? msg.likes.length : 0;
    if (!msg.edited && likes === 0) return null;
    const box = doc.createElement('span');
    box.className = 'b24-line-suffix';
    if (msg.edited) {
        const mark = doc.createElement('span');
        mark.className = 'b24-line-edited';
        mark.textContent = '(изм.)';
        mark.title = 'Сообщение отредактировано';
        box.appendChild(mark);
    }
    if (likes > 0) {
        const mark = doc.createElement('span');
        mark.className = 'b24-line-likes';
        mark.textContent = `♥ ${likes}`;
        box.appendChild(mark);
    }
    return box;
}

/**
 * Вложения, карточки и клавиатура — всё, что рисуется ПОД текстом строки.
 *
 * @param {object} li
 * @param {object} msg
 * @param {object} doc
 */
function appendExtras(li, msg, doc) {
    // Карточки вложений строит и оживляет `media.js`: разметка приходит из
    // `attachments.js`, а ленивая загрузка, лайтбокс и плеер — оттуда.
    const media = renderMediaGroup(msg);
    if (media) li.appendChild(media);

    const cards = buildCards(msg.cards, doc);
    if (cards) li.appendChild(cards);

    const kb = buildKeyboard(msg.reply_markup, msg, doc);
    if (kb) li.appendChild(kb);
}

/** Разделитель дня. */
function buildDateSep(msg) {
    const li = document.createElement('li');
    li.className = 'b24-date-sep';
    li.dataset.dateSep = String(msg.date || '');
    const label = document.createElement('span');
    label.textContent = dateSepLabel(msg.date);
    li.appendChild(label);
    return li;
}

/**
 * Собирает узлы для куска ленты — строки плюс разделители дней.
 *
 * Стык с уже нарисованным куском разделителя ЗДЕСЬ не получает: им
 * занимается вызывающий ([`renderOlder`]), потому что над бывшей первой
 * строкой разделитель уже стоит, и второй дал бы две даты подряд.
 *
 * @param {Array<object>} list — сообщения по возрастанию
 * @returns {Array<object>} узлы в порядке вставки
 */
function buildChunk(list) {
    const nodes = [];
    let prev = null;
    for (const msg of list) {
        if (needsDateSep(prev, msg)) nodes.push(buildDateSep(msg));
        nodes.push(buildLine(msg));
        prev = msg;
    }
    return nodes;
}

// ---------------------------------------------------------------------------
// Рендер ленты
// ---------------------------------------------------------------------------

/**
 * Полностью перерисовывает ленту открытого диалога.
 *
 * Полный рендер, а не точечные правки: страница истории — полсотни строк, а
 * инкрементальное обновление стоило бы второго индекса узлов, который
 * разъезжается с данными при первой же ошибке.
 */
export function renderHistory() {
    if (!$b24Log) return;
    // Полный рендер выбрасывает ВСЕ узлы ленты, включая карточки вложений:
    // реестр контроллеров и наблюдатель ленивых превью обязаны обнулиться
    // вместе с ними, иначе `IntersectionObserver` продолжал бы держать
    // ссылки на узлы, которых уже нет в документе.
    resetMediaCards();
    $b24Log.textContent = '';

    // Первый потомок ленты ВСЕГДА распорка: она прижимает короткую историю к
    // низу, чтобы лента не висела в верхней трети экрана.
    const spacer = document.createElement('li');
    spacer.className = 'b24-log-spacer';
    spacer.setAttribute('aria-hidden', 'true');
    $b24Log.appendChild(spacer);

    for (const node of buildChunk(currentMessages())) {
        $b24Log.appendChild(node);
    }
    if (local.stickBottom) scrollToBottom();
}

/**
 * Вставляет догруженную СВЕРХУ историю, сохраняя позицию чтения.
 *
 * Компенсация обязательна: вставка над видимой областью сдвигает содержимое
 * вниз ровно на высоту вставленного, и без поправки `scrollTop` читаемая
 * строка уезжает с экрана.
 *
 * @param {Array<object>} older — сообщения по возрастанию, все старее текущих
 */
export function renderOlder(older) {
    if (!$b24Log || !Array.isArray(older) || older.length === 0) return;
    const heightBefore = Number($b24Log.scrollHeight) || 0;
    const topBefore = Number($b24Log.scrollTop) || 0;

    const list = currentMessages();
    const firstExisting = list.length > 0 ? list[0] : null;
    mergeMessages(list, older);

    // Вставка идёт ПОСЛЕ распорки: она обязана оставаться первым потомком.
    let anchor = $b24Log.childNodes[1] || null;
    // Над бывшей первой строкой уже стоит разделитель дня — он был верен,
    // пока она была первой. Если догруженный хвост того же дня, разделитель
    // оказывается посреди дня и должен уйти.
    const lastOlder = older[older.length - 1];
    if (
        anchor
        && anchor.className === 'b24-date-sep'
        && firstExisting
        && !needsDateSep(lastOlder, firstExisting)
    ) {
        const stale = anchor;
        anchor = $b24Log.childNodes[2] || null;
        $b24Log.removeChild(stale);
    }

    for (const node of buildChunk(older)) {
        if (anchor) $b24Log.insertBefore(node, anchor);
        else $b24Log.appendChild(node);
    }

    const grew = (Number($b24Log.scrollHeight) || 0) - heightBefore;
    if (grew > 0) $b24Log.scrollTop = topBefore + grew;
}

/**
 * Вставляет одно сообщение в конец ленты.
 *
 * Прокручивает ленту ТОЛЬКО при взведённой защёлке: пользователь, читающий
 * историю, не должен уезжать вниз от чужого сообщения.
 *
 * Отметку о прочтении ЖИВОГО сообщения эта функция НЕ ставит (forge-uo7b) —
 * ставить её надо, только когда действительно видна и лента, и вкладка
 * Битрикс24 в devforge, а этого здесь не знают: `local.stickBottom` — это
 * позиция прокрутки, а не видимость панели. Решение принимает вызывающий
 * (`main.js::message_new`, там уже есть готовый флаг `open`), тем же путём,
 * что у Telegram-плагина (`telegram/main.js::new_message` →
 * `scheduleRead` при `isActive`).
 *
 * @param {object} msg — MessageDto
 */
export function appendMessage(msg) {
    if (!$b24Log || !msg || msg.id == null) return;
    const list = currentMessages();
    const known = list.some((m) => Number(m.id) === Number(msg.id));
    mergeMessages(list, [msg]);

    // Правка показанной строки и сообщение, вставшее НЕ в конец (портал
    // отдал его позже соседей), перерисовывают ленту целиком: точечная
    // вставка в середину стоила бы второго индекса узлов, который
    // разъезжается с данными при первой же ошибке.
    const last = list.length > 0 ? list[list.length - 1] : null;
    if (known || !last || Number(last.id) !== Number(msg.id)) {
        renderHistory();
        return;
    }

    const prev = list.length > 1 ? list[list.length - 2] : null;
    if (needsDateSep(prev, msg)) $b24Log.appendChild(buildDateSep(msg));
    $b24Log.appendChild(buildLine(msg));
    if (local.stickBottom) scrollToBottom();
}

/**
 * Подпись сессии над лентой.
 *
 * @param {?object} dialog — DialogDto
 */
export function updateHead(dialog) {
    if (!$b24SessionHead) return;
    if (!dialog) {
        $b24SessionHead.textContent = 'SESSION ▸ —';
        return;
    }
    const parts = [displayName(dialog)];
    if (dialog.kind === 'task' && dialog.task_id) parts.push(`задача #${dialog.task_id}`);
    if (dialog.members) parts.push(`${dialog.members} уч.`);
    if (!dialog.can_write) parts.push('только чтение');
    $b24SessionHead.textContent = `SESSION ▸ ${parts.join(' · ')}`;
}

/** Показывает служебную строку вместо ленты (ошибка, пустота). */
function showNotice(text) {
    if (!$b24Log) return;
    const li = document.createElement('li');
    li.className = 'b24-line b24-line-system b24-line-notice';
    const span = document.createElement('span');
    span.className = 'b24-line-text';
    span.textContent = String(text || '');
    li.appendChild(span);
    $b24Log.appendChild(li);
}

// ---------------------------------------------------------------------------
// Загрузка
// ---------------------------------------------------------------------------

/**
 * Открывает диалог: чистит ленту и грузит хвост истории.
 *
 * Полная очистка обязательна — иначе строки прошлого диалога остались бы под
 * новыми, а `data-msg-id` совпал бы у двух чатов (идентификаторы у портала
 * сквозные, но кэш ленты — свой на каждый диалог).
 *
 * @param {string} dialogId
 * @param {?object} dialog — DialogDto для шапки
 * @returns {Promise<void>}
 */
export async function openDialog(dialogId, dialog) {
    const key = String(dialogId == null ? '' : dialogId);
    if (!key) return;

    // Смена диалога — единственный момент, когда можно безопасно отдать
    // память: блоб живёт, пока жив его objectURL, и без этого вызова двадцать
    // переключений между чатами с картинками остаются во вкладке навсегда.
    // Аватары при этом сохраняются: они общие для всей панели.
    if (local.dialogId && local.dialogId !== key) revokeDialogMedia();
    local.generation += 1;
    const generation = local.generation;
    local.dialogId = key;
    b24State.activeDialogId = key;
    // Новый диалог всегда открывается на конце истории.
    local.stickBottom = true;
    local.hasOlder = false;
    local.loading = false;
    // Подсветка оригинала принадлежала ленте, которой на экране больше нет:
    // её таймер снял бы класс со СТРОКИ НОВОГО диалога, совпавшей по id.
    clearFlash();
    local.jumping = false;
    // Режим ввода принадлежал ПРОШЛОМУ диалогу: цитата чужого сообщения и
    // правка чужой строки в новой ленте бессмысленны и опасны — правка ушла бы
    // по идентификатору из другого чата.
    setMode(null);
    if ($b24Input) $b24Input.value = '';
    updateCharCount();

    updateHead(dialog);
    // Кэш ленты живёт между переключениями: возвращение в диалог показывает
    // прочитанное мгновенно, а свежие строки догружаются следом.
    renderHistory();

    await loadPage({ generation });
}

/**
 * Грузит страницу истории.
 *
 * @param {{generation?: number, beforeId?: number}} [opts]
 * @returns {Promise<void>}
 */
async function loadPage(opts = {}) {
    const generation = opts.generation == null ? local.generation : opts.generation;
    const key = local.dialogId;
    if (!key || local.loading) return;
    local.loading = true;
    try {
        const resp = await b24Api.getMessages(key, {
            limit: PAGE_SIZE,
            beforeId: opts.beforeId,
        });
        if (generation !== local.generation) return;

        const page = Array.isArray(resp && resp.messages) ? resp.messages : [];
        local.hasOlder = !!(resp && resp.has_more);

        if (opts.beforeId) {
            renderOlder(page);
        } else {
            mergeMessages(currentMessages(), page);
            renderHistory();
            if (page.length === 0) showNotice('// история пуста');
            // Открытая лента показывает конец истории — значит она прочитана.
            if (page.length > 0) scheduleRead(key, newestId(currentMessages()));
        }
    } catch (err) {
        if (generation !== local.generation) return;
        // Ошибку показываем В ЛЕНТЕ: индикатор связи в статусбаре объясняет
        // «почему всё замерло», но не «почему пустой этот чат».
        if (currentMessages().length === 0) {
            renderHistory();
            showNotice(`// ${errorLabel(err && err.code)}`);
        }
    } finally {
        if (generation === local.generation) local.loading = false;
    }
}

/**
 * Догружает историю выше загруженной.
 *
 * @returns {Promise<void>}
 */
export async function loadOlder() {
    if (!local.hasOlder || local.loading) return;
    const list = currentMessages();
    const before = oldestId(list);
    if (!before) return;
    await loadPage({ beforeId: before });
}

/**
 * Догружает то, что появилось ниже загруженного (возврат из фона, Ф7).
 *
 * @returns {Promise<void>}
 */
export async function loadNewer() {
    const key = local.dialogId;
    const after = newestId(currentMessages());
    if (!key || !after || local.loading) return;
    local.loading = true;
    const generation = local.generation;
    try {
        const resp = await b24Api.getMessages(key, { limit: PAGE_SIZE, afterId: after });
        if (generation !== local.generation) return;
        const page = Array.isArray(resp && resp.messages) ? resp.messages : [];
        if (page.length === 0) return;
        mergeMessages(currentMessages(), page);
        renderHistory();
    } catch (_) {
        // Молча: это фоновая догрузка, у неё нет пользовательского намерения.
    } finally {
        if (generation === local.generation) local.loading = false;
    }
}

// ---------------------------------------------------------------------------
// Цикл переписки: отправка, правка, удаление, лайк (Ф6)
// ---------------------------------------------------------------------------

/**
 * Состояние счётчика символов — ЧИСТАЯ функция (Р12).
 *
 * Длина считается в code point'ах (`Array.from`), а не в `String.length`:
 * `length` считает единицы UTF-16, и эмодзи весит две, а лимит у портала
 * символьный. Разъезд этих двух счётов означал бы, что фронт разрешает
 * отправку текста, который сервер отвергает.
 *
 * @param {?string} text
 * @returns {{len: number, over: boolean, warn: boolean, left: number}}
 */
export function charCountState(text) {
    const len = Array.from(String(text == null ? '' : text)).length;
    return {
        len,
        over: len > MAX_MESSAGE_CHARS,
        warn: len >= CHAR_WARN_AT,
        left: MAX_MESSAGE_CHARS - len,
    };
}

/** Сообщение ленты по идентификатору. */
function findMessage(id) {
    const key = Number(id);
    return currentMessages().find((m) => Number(m.id) === key) || null;
}

/**
 * Убирает строку из ленты (удаление, отказ от неотправленного).
 *
 * @param {number|string} id
 */
export function removeMessage(id) {
    const list = currentMessages();
    const key = Number(id);
    const i = list.findIndex((m) => Number(m.id) === key);
    if (i < 0) return;
    list.splice(i, 1);
    renderHistory();
}

/**
 * Помечает строку удалённой (кадр `message_delete`, Ф7).
 *
 * НЕ удаляет её из ленты — и это не небрежность. Портал удаление не стирает: в
 * истории остаётся строка с текстом «Это сообщение было удалено» и
 * `params.IS_DELETED`. Убери мы её здесь — она вернулась бы при первой же
 * догрузке истории, и пользователь решил бы, что сообщение воскресло.
 *
 * Строка ЧУЖОГО диалога игнорируется: лента показывает один диалог, а кадр
 * приходит по всем (сервер вещает без фильтрации).
 *
 * @param {string} dialogId
 * @param {Array<number|string>} ids
 * @returns {number} сколько строк помечено
 */
export function markDeleted(dialogId, ids) {
    if (String(dialogId) !== String(local.dialogId)) return 0;
    const list = currentMessages();
    let hit = 0;
    for (const raw of (Array.isArray(ids) ? ids : [ids])) {
        const key = Number(raw);
        const msg = list.find((m) => Number(m.id) === key);
        if (!msg || msg.deleted) continue;
        msg.deleted = true;
        msg.text = DELETED_TEXT;
        msg.segments = [{ kind: 'text', text: DELETED_TEXT, value: null, children: [] }];
        msg.media = [];
        msg.cards = [];
        msg.reply_markup = null;
        msg.likes = [];
        // Сервер в этот же момент стирает свой файловый кэш
        // (`media_cache::invalidate`), а его ответ помечен `immutable` — держать
        // во вкладке блоб удалённого вложения значило бы показывать то, чего на
        // портале больше нет.
        dropMessageMedia(dialogId, key);
        hit += 1;
    }
    if (hit > 0) renderHistory();
    return hit;
}

/**
 * Применяет новый состав лайков (кадр `reaction`, Ф7).
 *
 * Кадр несёт ИТОГОВЫЙ список, а не дельту: `action` в событии портала живьём
 * пуст, и «поставили» от «сняли» отличается только составом `params.LIKE`.
 * Поэтому здесь присваивание, а не инкремент — присваивание идемпотентно, и
 * повторный кадр (дубли у портала реальны) ничего не ломает.
 *
 * @param {string} dialogId
 * @param {number|string} id
 * @param {Array<string>} likes
 * @returns {boolean} строка нашлась и обновилась
 */
export function applyLikes(dialogId, id, likes) {
    if (String(dialogId) !== String(local.dialogId)) return false;
    const msg = findMessage(id);
    if (!msg) return false;
    msg.likes = Array.isArray(likes) ? likes.map(String) : [];
    renderHistory();
    return true;
}

/** Диалог, лента которого сейчас на экране (для обработчиков WS). */
export function currentDialogId() {
    return local.dialogId;
}

/** Следующий временный идентификатор оптимистичной строки. */
function nextTempId() {
    local.tempSeq += 1;
    return TEMP_ID_BASE + local.tempSeq;
}

/**
 * Оптимистичная строка «отправляется».
 *
 * Поле `payload` хранит ровно то, что нужно повтору: текст и цитату. Держать
 * его на самой строке проще, чем во второй карте, — строка и её черновик
 * умирают вместе.
 *
 * @param {object} payload
 * @returns {object} MessageDto-подобная строка
 */
function pendingMessage(payload) {
    const self = b24State.selfUser || {};
    const text = String(payload.text || '');
    return {
        id: nextTempId(),
        dialog_id: local.dialogId,
        out: true,
        date: new Date().toISOString(),
        sender: {
            id: String(self.id == null ? '' : self.id),
            name: [self.name, self.last_name].filter(Boolean).join(' ').trim() || null,
            color: null,
            bot: false,
        },
        system: false,
        text,
        // Сегменты собираются ПЛОСКО: BB-коды разбирает бэкенд, и делать это
        // второй раз во фронте значило бы завести второй парсер, который
        // разъедется с первым.
        segments: [{ kind: 'text', text, value: null, children: [] }],
        media: [],
        cards: [],
        reply_markup: null,
        edited: false,
        likes: [],
        action: null,
        unread: false,
        reply_to: null,
        fwd: null,
        pending: true,
        failed: false,
        error: '',
        payload,
    };
}

/**
 * Отправляет текст: оптимистичная строка → портал → замена настоящей.
 *
 * ПОВТОРА НЕТ (Р9): любая ошибка оставляет строку в ленте с пометкой и кнопкой
 * «⟳», а текст остаётся в `payload` — потерять написанное нельзя.
 *
 * @param {object} payload — `{text, replyTo?, replyAuthor?, replyText?}`
 * @returns {Promise<void>}
 */
async function submitSend(payload) {
    const dialogId = local.dialogId;
    if (!dialogId) return;

    const temp = pendingMessage(payload);
    appendMessage(temp);
    local.sending = true;
    try {
        const resp = await b24Api.sendMessage(dialogId, payload);
        // Диалог могли переключить, пока летел запрос: тогда строка чужая
        // ленте, и её место — в кэше того диалога, а не на экране.
        removeMessage(temp.id);
        const real = resp && resp.message;
        if (real) {
            // Цель ответа известна ЗДЕСЬ и больше нигде: при чтении портал
            // `REPLY_ID` не отражает (Р10), и без этой записи переход по
            // собственной цитате пришлось бы угадывать по её тексту.
            if (payload.replyTo && real.id != null) {
                local.replyById.set(Number(real.id), Number(payload.replyTo));
            }
            appendMessage(real);
        }
    } catch (err) {
        const line = findMessage(temp.id);
        if (line) {
            line.pending = false;
            line.failed = true;
            line.error = (err && err.code) || 'unknown';
            renderHistory();
        }
    } finally {
        local.sending = false;
    }
}

/**
 * Повторяет отправку строки, которая не улетела.
 *
 * Повтор — это НОВАЯ отправка с новым временным идентификатором, а не
 * «дослать ту же»: у портала нет дедуп-идентификатора, и семантики «та же
 * попытка» не существует вовсе.
 *
 * @param {number|string} id
 * @returns {Promise<void>}
 */
export async function retrySend(id) {
    const line = findMessage(id);
    if (!line || !line.failed || !line.payload) return;
    const payload = line.payload;
    removeMessage(id);
    await submitSend(payload);
}

/**
 * Отправляет правку открытого на редактирование сообщения.
 *
 * @param {number} messageId
 * @param {string} text
 * @returns {Promise<void>}
 */
async function submitEdit(messageId, text) {
    const dialogId = local.dialogId;
    if (!dialogId) return;
    local.sending = true;
    try {
        const resp = await b24Api.editMessage(dialogId, messageId, text);
        const real = resp && resp.message;
        // Замена по тому же идентификатору: `appendMessage` перерисовывает
        // ленту целиком, когда строка уже показана.
        if (real) appendMessage(real);
        setMode(null);
        if ($b24Input) $b24Input.value = '';
        updateCharCount();
    } catch (err) {
        showModeError(errorLabel(err && err.code));
    } finally {
        local.sending = false;
    }
}

/**
 * Единственная точка отправки формы: и Enter, и кнопка SEND, и кнопка `SEND`
 * inline-клавиатуры бота приходят сюда.
 *
 * @param {?string} [override] — текст вместо содержимого поля ввода
 * @returns {Promise<void>}
 */
export async function submitPrompt(override) {
    if (local.sending) return;
    const raw = override == null ? ($b24Input ? String($b24Input.value || '') : '') : String(override);
    const text = raw.trim();
    if (!text) return;

    const count = charCountState(text);
    if (count.over) {
        // До сети дело не доходит: сервер ответил бы 400, а пользователь
        // потерял бы секунду на круг до портала и обратно.
        showModeError(`сообщение длиннее ${MAX_MESSAGE_CHARS} символов — сократите на ${count.len - MAX_MESSAGE_CHARS}`);
        return;
    }

    const mode = local.mode;
    if (mode && mode.kind === 'edit') {
        await submitEdit(mode.id, text);
        return;
    }

    const payload = { text };
    if (mode && mode.kind === 'reply') {
        payload.replyTo = mode.id;
        payload.replyAuthor = mode.author;
        payload.replyText = mode.text;
    }
    if ($b24Input) $b24Input.value = '';
    setMode(null);
    updateCharCount();
    await submitSend(payload);
}

/**
 * Включает режим ответа (Р10). Зовётся и кнопкой «↩» строки, и двойным
 * кликом по строке (см. [`classifyLineDblClick`]).
 *
 * Цитата показывается локально, а в портал уезжает и `REPLY_ID`, и та же
 * цитата ТЕКСТОМ: при чтении портал `REPLY_ID` не отражает, и без текстовой
 * копии ответ в ленте неотличим от новой реплики.
 *
 * Системная строка и удалённое сообщение отвергаются: у первой нет автора и
 * содержимого для цитаты (`buildLine` даже не рисует ей кнопку «↩»), у
 * второго текст уже подменён заглушкой «Сообщение удалено» — цитировать
 * нечего у обеих, и кнопка действий их по той же причине не предлагает
 * (см. [`buildActions`]).
 *
 * @param {number|string} id
 */
export function startReply(id) {
    const msg = findMessage(id);
    if (!msg || msg.pending || msg.failed || msg.system || msg.deleted) return;
    setMode({
        kind: 'reply',
        id: Number(msg.id),
        author: senderLabel(msg),
        text: segmentsText(msg.segments) || msg.text || '',
    });
    if ($b24Input) $b24Input.focus();
}

/**
 * Включает режим правки: текст сообщения переезжает в поле ввода.
 *
 * @param {number|string} id
 */
export function startEdit(id) {
    const msg = findMessage(id);
    if (!msg || !msg.out || msg.pending || msg.failed) return;
    const text = segmentsText(msg.segments) || msg.text || '';
    setMode({ kind: 'edit', id: Number(msg.id), text });
    if ($b24Input) {
        $b24Input.value = text;
        $b24Input.focus();
    }
    updateCharCount();
}

/**
 * Удаляет сообщение — с подтверждением.
 *
 * Подтверждение обязательно: отменить удаление у портала нечем, а кнопка «✕»
 * стоит рядом с «✎».
 *
 * @param {number|string} id
 * @returns {Promise<void>}
 */
export async function deleteLine(id) {
    const msg = findMessage(id);
    if (!msg || !msg.out) return;
    const ask = (typeof globalThis.confirm === 'function') ? globalThis.confirm : null;
    if (ask && !ask('Удалить сообщение? Отменить это нельзя.')) return;
    try {
        await b24Api.deleteMessage(msg.id);
        removeMessage(msg.id);
        if (local.mode && local.mode.kind === 'edit' && local.mode.id === Number(msg.id)) {
            setMode(null);
            if ($b24Input) $b24Input.value = '';
            updateCharCount();
        }
    } catch (err) {
        showModeError(errorLabel(err && err.code));
    }
}

/**
 * Переключает «лайк» — единственную доступную вебхуку реакцию (§6).
 *
 * Новое состояние приходит из ПЕРЕЧИТАННОЙ сервером строки: ответ портала на
 * `im.message.like` всегда `true` и о состоянии не говорит ничего.
 *
 * @param {number|string} id
 * @returns {Promise<void>}
 */
export async function toggleLike(id) {
    const msg = findMessage(id);
    if (!msg || msg.pending || msg.failed || !local.dialogId) return;
    try {
        const resp = await b24Api.likeMessage(local.dialogId, msg.id);
        const real = resp && resp.message;
        if (real) appendMessage(real);
    } catch (err) {
        showModeError(errorLabel(err && err.code));
    }
}

// ---------------------------------------------------------------------------
// Поле ввода: режим, счётчик, «печатает…», отметка о прочтении
// ---------------------------------------------------------------------------

/**
 * Ставит режим ввода и перерисовывает строку над полем.
 *
 * @param {?object} mode
 */
function setMode(mode) {
    local.mode = mode || null;
    renderModeNote();
}

/** Показывает ошибку действия там же, где живёт режим ввода. */
function showModeError(text) {
    if (!$b24PromptNote) return;
    $b24PromptNote.hidden = false;
    $b24PromptNote.className = 'b24-prompt-note b24-prompt-note-error';
    $b24PromptNote.textContent = `// ${text}`;
}

/**
 * Строка над полем ввода: цитата ответа либо метка правки.
 *
 * Собирается УЗЛАМИ: в цитате лежит чужой текст.
 */
function renderModeNote() {
    if (!$b24PromptNote) return;
    const mode = local.mode;
    $b24PromptNote.textContent = '';
    $b24PromptNote.className = 'b24-prompt-note';
    if (!mode) {
        $b24PromptNote.hidden = true;
        return;
    }
    $b24PromptNote.hidden = false;

    const doc = document;
    const tag = doc.createElement('span');
    tag.className = 'b24-prompt-note-tag';
    tag.textContent = mode.kind === 'edit' ? '✎ правка' : '↩ ответ';
    $b24PromptNote.appendChild(tag);

    const text = doc.createElement('span');
    text.className = 'b24-prompt-note-text';
    const quote = mode.kind === 'edit' ? mode.text : `${mode.author}: ${mode.text}`;
    text.textContent = String(quote || '').slice(0, 160);
    $b24PromptNote.appendChild(text);

    const cancel = doc.createElement('button');
    cancel.className = 'b24-prompt-note-cancel';
    cancel.type = 'button';
    cancel.title = 'Отменить (Esc)';
    cancel.setAttribute('aria-label', 'Отменить');
    cancel.textContent = '✕';
    cancel.addEventListener('click', () => {
        cancelMode();
    });
    $b24PromptNote.appendChild(cancel);
}

/**
 * Выходит из режима ответа или правки, возвращая поле ввода в исходное.
 *
 * Текст правки СТИРАЕТСЯ (он принадлежит сообщению, а не полю), текст ответа —
 * нет: пользователь мог уже начать печатать.
 */
export function cancelMode() {
    const wasEdit = local.mode && local.mode.kind === 'edit';
    setMode(null);
    if (wasEdit && $b24Input) $b24Input.value = '';
    updateCharCount();
}

/**
 * Текущая цель ответа либо `null` (Ф9).
 *
 * Единственный владелец режима ввода — этот модуль, и вторым он обзаводиться не
 * должен: полоса над полем, реально уходящая цитата и `reply_to` обязаны
 * означать одно и то же. Отправка файла (`upload.js`) ЧИТАЕТ цель здесь и
 * снимает её через [`cancelMode`] — своей копии состояния у неё нет.
 *
 * Режим правки целью ответа НЕ является: править сообщение файлом невозможно
 * (портал такого не умеет), и вернуть его здесь значило бы предложить отправку,
 * которая молча потеряет смысл действия.
 *
 * @returns {?{id: number, author: string, text: string}}
 */
export function replyMode() {
    const mode = local.mode;
    if (!mode || mode.kind !== 'reply') return null;
    return { id: Number(mode.id), author: String(mode.author || ''), text: String(mode.text || '') };
}

/**
 * Вид активного режима ввода: `'reply'` | `'edit'` | `null` (Ф11).
 *
 * Нужен клавиатурной навигации (`keynav.js::onEscape`), и ровно для одного
 * решения: перехватывать ли Escape самой. Слушатель keynav стоит в CAPTURE и
 * опережает собственный обработчик поля ввода, а тот единственный знает, что
 * именно отменять (цитату или правку) и стирать ли при этом текст. Поэтому при
 * активном режиме keynav обязан промолчать и пропустить событие дальше.
 *
 * Отдельная функция, а не расширение [`replyMode`]: та возвращает ЦЕЛЬ ответа и
 * намеренно молчит про правку (файлом сообщение не правят — см. её шапку),
 * и подмешать туда режим правки значило бы сломать `upload.js`.
 *
 * @returns {?string}
 */
export function inputMode() {
    return local.mode ? String(local.mode.kind) : null;
}

/**
 * Обновляет счётчик символов и блокировку кнопки отправки (Р12).
 *
 * Счётчик появляется только на подходе к лимиту: постоянно висящее число
 * ничего не сообщает, а мигающее у 20 000 — сообщает ровно то, что нужно.
 */
export function updateCharCount() {
    const state = charCountState($b24Input ? $b24Input.value : '');
    if ($b24CharCount) {
        $b24CharCount.hidden = !state.warn;
        $b24CharCount.textContent = state.warn ? `${state.len} / ${MAX_MESSAGE_CHARS}` : '';
        $b24CharCount.className = state.over
            ? 'b24-charcount b24-charcount-over'
            : 'b24-charcount';
    }
    if ($b24Send) $b24Send.disabled = state.over;
    return state;
}

/**
 * Шлёт «печатает…» не чаще, чем разрешает окно.
 *
 * Окно держат обе стороны: сервер — потому что вкладок бывает несколько, фронт
 * — чтобы не гонять сеть впустую (набор текста порождает событие на каждую
 * клавишу).
 */
function notifyTyping() {
    const dialogId = local.dialogId;
    if (!dialogId) return;
    // В режиме правки индикатор не нужен: собеседник увидит не новую реплику,
    // а изменение старой, и «печатает…» обещало бы не то.
    if (local.mode && local.mode.kind === 'edit') return;
    const now = Date.now();
    const prev = local.typingAt.get(dialogId) || 0;
    if (now - prev < TYPING_INTERVAL_MS) return;
    local.typingAt.set(dialogId, now);
    b24Api.sendTyping(dialogId).catch(() => {
        // Индикатор — украшение: его отказ не должен ни всплывать, ни мешать
        // набору текста.
    });
}

/**
 * Откладывает отметку о прочтении на окно дебаунса.
 *
 * ЗАДНИЙ фронт (в отличие от серверного окна): пока пользователь листает,
 * отметка не уходит вовсе, а по остановке уезжает САМЫЙ СВЕЖИЙ идентификатор.
 * Так один жест прокрутки стоит одного запроса, а не одного запроса на каждый
 * кадр.
 *
 * @param {string} dialogId
 * @param {number} messageId
 */
export function scheduleRead(dialogId, messageId) {
    if (!dialogId || !Number.isFinite(Number(messageId)) || Number(messageId) <= 0) return;
    local.readPending = { dialogId, messageId: Number(messageId) };
    if (local.readTimer) return;
    const timer = globalThis.setTimeout(() => {
        local.readTimer = null;
        const pending = local.readPending;
        local.readPending = null;
        if (!pending) return;
        b24Api.markRead(pending.dialogId, pending.messageId)
            .then((resp) => {
                // Колбэк зовётся только на ДОШЕДШЕЙ отметке: подавленную
                // сервером (`sent: false`) счётчики переспрашивать незачем.
                if (resp && resp.sent && typeof local.onRead === 'function') {
                    local.onRead(pending.dialogId, pending.messageId);
                }
            })
            .catch(() => {
                // Отметка о прочтении — фоновая: её отказ не ломает ленту.
            });
    }, READ_DEBOUNCE_MS);
    local.readTimer = timer;
}

// ---------------------------------------------------------------------------
// Переход к оригиналу по цитате (forge-92x6, по образцу VK-плагина)
// ---------------------------------------------------------------------------

/**
 * Клик по цитате: прокрутить к оригиналу и подсветить его.
 *
 * Порядок попыток тот же, что у VK (`vk/chat.js::jumpToMessage`), но короче на
 * один шаг:
 *
 * 1. оригинал уже в ленте — показать его;
 * 2. оригинал выше загруженного — тянуть историю вверх, но не более
 *    [`REPLY_JUMP_PAGES`] страниц;
 * 3. не нашли — сказать об этом. Доспросить портал про конкретное сообщение
 *    нечем: идентификатора оригинала у панели НЕТ (Р10 — портал не отражает
 *    `REPLY_ID` при чтении), а цитата — это текст, а не ссылка.
 *
 * @param {number|string} msgId — сообщение-ответ, с цитаты которого пришли
 * @param {{author: string, text: string, raw: string}} quote
 * @returns {Promise<void>}
 */
async function jumpToQuote(msgId, quote) {
    if (local.jumping) return;
    const dialogId = local.dialogId;
    if (!dialogId) return;
    // Переход уезжает ВВЕРХ — прямо против защёлки. Оставь её взведённой, и
    // первое же новое сообщение вернуло бы ленту вниз, а клик по цитате
    // выглядел бы как «ничего не произошло».
    local.stickBottom = false;
    if (revealOrigin(msgId, quote)) return;

    local.jumping = true;
    try {
        for (let i = 0; i < REPLY_JUMP_PAGES && local.hasOlder; i += 1) {
            await loadOlder();
            // Пока грузили, пользователь мог уйти в другой диалог.
            if (String(local.dialogId) !== String(dialogId)) return;
            if (revealOrigin(msgId, quote)) return;
        }
        showModeError('оригинал не найден в загруженной истории');
    } finally {
        local.jumping = false;
    }
}

/**
 * Находит оригинал и показывает его.
 *
 * Сначала — ТОЧНОЕ знание (`local.replyById`): своё сообщение этой сессии
 * помнит цель ответа достоверно. Дальше — сопоставление по тексту цитаты, оно
 * же единственный путь для всего, что приехало из истории.
 *
 * @param {number|string} msgId
 * @param {object} quote
 * @returns {boolean} нашли ли строку В ЛЕНТЕ
 */
function revealOrigin(msgId, quote) {
    const list = currentMessages();
    const known = local.replyById.get(Number(msgId));
    const origin = (known != null && list.some((m) => Number(m.id) === Number(known)))
        ? findMessage(known)
        : findQuoteOrigin(list, msgId, quote);
    if (!origin) return false;
    const row = findRow(origin.id);
    if (!row) return false;
    scrollRowIntoView(row);
    flashRow(origin.id);
    return true;
}

/** Узел строки по идентификатору сообщения (его ставит [`buildLine`]). */
function findRow(id) {
    if (!$b24Log) return null;
    const key = String(id);
    const nodes = $b24Log.childNodes || [];
    for (const node of nodes) {
        if (node && node.dataset && node.dataset.msgId === key) return node;
    }
    return null;
}

/**
 * Прокручивает ленту так, чтобы строка оказалась по центру.
 *
 * Через `getBoundingClientRect`, а НЕ через `offsetTop`: смещения детей ленты
 * считаются от ближайшего позиционированного предка, и прямой
 * `scrollTop = row.offsetTop` промахнулся бы на высоту шапки сессии.
 * `scrollIntoView` — запасной путь для стендов без геометрии.
 */
function scrollRowIntoView(row) {
    if (!$b24Log || !row) return;
    if (typeof row.getBoundingClientRect !== 'function'
        || typeof $b24Log.getBoundingClientRect !== 'function') {
        if (typeof row.scrollIntoView === 'function') row.scrollIntoView();
        return;
    }
    const r = row.getBoundingClientRect();
    const log = $b24Log.getBoundingClientRect();
    const centered = (r.top - log.top) - ((Number($b24Log.clientHeight) || 0) - r.height) / 2;
    $b24Log.scrollTop = (Number($b24Log.scrollTop) || 0) + centered;
}

/**
 * Кратковременно подсвечивает строку оригинала.
 *
 * Подсвеченная строка запоминается ПО ИДЕНТИФИКАТОРУ: `renderHistory` вернёт
 * класс после любой перерисовки (см. [`buildLine`]), а таймер снимет его один
 * раз в конце.
 *
 * @param {number|string} id
 */
function flashRow(id) {
    clearFlash();
    local.flashId = Number(id);
    const row = findRow(id);
    if (row && row.classList) row.classList.add('b24-line-flash');
    local.flashTimer = globalThis.setTimeout(() => {
        local.flashTimer = null;
        clearFlash();
    }, REPLY_FLASH_MS);
}

/** Снимает подсветку и её таймер (идемпотентно). */
function clearFlash() {
    if (local.flashTimer) {
        globalThis.clearTimeout(local.flashTimer);
        local.flashTimer = null;
    }
    if (local.flashId == null) return;
    const row = findRow(local.flashId);
    if (row && row.classList) row.classList.remove('b24-line-flash');
    local.flashId = null;
}

// ---------------------------------------------------------------------------
// Двойной клик по строке → ответ (forge-zw3d, по образцу Telegram-плагина)
// ---------------------------------------------------------------------------

/**
 * Селекторы, внутри которых двойной клик НЕ означает «ответить».
 *
 * Список из селекторов, а не одна склейка: так [`classifyLineDblClick`]
 * проверяется по одному правилу за раз и покрывается тестом без DOM (тот же
 * приём, что у `telegram/reply.js::REPLY_CLICK_IGNORE`).
 *
 * Почему каждый пункт:
 * - `.b24-line-media` — карточка вложения открывается ОДИНАРНЫМ кликом
 *   (см. `media.js::wireCard`); двойной уже запустил лайтбокс/плеер, ловить
 *   сверху ещё и ответ — лишнее действие поверх ожидаемого;
 * - `.b24-kb` — клавиатура бота отправляет команду/текст на каждый клик по
 *   кнопке, перехватывать которую нельзя (forge-zw3d, инлайн-кнопки бота);
 * - `.b24-line-quote` — цитата ведёт к ОРИГИНАЛУ (forge-92x6), и двойной клик
 *   по ней означал бы «перейти и заодно ответить на ответ»;
 * - `button` — любая кнопка строки (♥/↩/✎/✕/⟳) уже несёт своё действие,
 *   включая саму кнопку «↩», у которой обработчик и так есть.
 */
export const REPLY_DBLCLICK_IGNORE = ['.b24-line-media', '.b24-kb', '.b24-line-quote', 'button'];

/**
 * Что означает двойной клик по ленте: `'reply'` или `'ignore'`.
 *
 * От DOM нужен только `closest` — функция чистая и покрыта node-тестом
 * (`bitrix_chat_dblclick_reply.test.mjs`).
 *
 * @param {?EventTarget} target — `e.target`
 * @returns {'reply'|'ignore'}
 */
export function classifyLineDblClick(target) {
    if (!target || typeof target.closest !== 'function') return 'ignore';
    for (const sel of REPLY_DBLCLICK_IGNORE) {
        if (target.closest(sel)) return 'ignore';
    }
    // Двойной клик мимо строк (по распорке, по разделителю дня) — не ответ.
    return target.closest('.b24-line') ? 'reply' : 'ignore';
}

/**
 * Двойной клик по строке ленты — начать ответ на неё.
 *
 * Идентификатор строки читается из `dataset.msgId` (его ставит [`buildLine`])
 * и уходит в уже существующую [`startReply`] — та же проверка
 * pending/failed/system/deleted, что и у кнопки «↩», применяется САМА, без
 * дублирования здесь.
 *
 * @param {Event} e
 */
function onLogDblClick(e) {
    if (classifyLineDblClick(e.target) !== 'reply') return;
    const row = e.target.closest('.b24-line');
    const id = row ? Number(row.dataset.msgId) : NaN;
    if (!Number.isFinite(id)) return;
    startReply(id);
}

// ---------------------------------------------------------------------------
// Инициализация и сброс
// ---------------------------------------------------------------------------

/**
 * Навешивает слушатели ленты. Идемпотентна: вкладку открывают много раз.
 *
 * @param {{onRead?: function}} [handlers]
 */
export function initChat(handlers = {}) {
    local.onRead = typeof handlers.onRead === 'function' ? handlers.onRead : null;

    // Клавиатуры ботов: PUT подставляет текст в поле ввода, SEND отправляет
    // его тем же путём, что и Enter, — через `submitPrompt`. Второй путь
    // отправки означал бы вторую копию проверок лимита и режима ввода.
    initBotKeyboards({
        onPutText: (text) => {
            if (!$b24Input) return;
            $b24Input.value = String(text == null ? '' : text);
            $b24Input.focus();
            updateCharCount();
        },
        onSendText: (text) => {
            submitPrompt(text);
        },
    });

    if (local.inited) return;
    local.inited = true;

    if ($b24Log) {
        $b24Log.addEventListener('scroll', onScroll);
        $b24Log.addEventListener('dblclick', onLogDblClick);
    }
    if ($b24Prompt) {
        $b24Prompt.addEventListener('submit', (ev) => {
            if (ev && typeof ev.preventDefault === 'function') ev.preventDefault();
            submitPrompt();
        });
    }
    if ($b24Input) {
        $b24Input.addEventListener('input', () => {
            updateCharCount();
            notifyTyping();
        });
        // Esc выходит из режима ответа/правки. `keydown`, а не `keyup`: до
        // отпускания клавиши браузер успевает отдать фокус форме.
        //
        // Слоем реестра (`messenger/esc-stack.js`) этот обработчик стать не
        // может: он живёт на ПОЛЕ ВВОДА и срабатывает только при фокусе в нём —
        // Esc из ленты режим не снимает и снимать не должен. Поэтому он не
        // регистрируется, а СПРАШИВАЕТ реестр: пока сверху открыто окно
        // (карточка встречи, попап анализа), нажатие принадлежит ему. Без этой
        // проверки одно нажатие закрывало карточку И снимало цитату, стирая
        // набранный контекст ответа: событие с поля всплывает до документа, и
        // остановить его на стороне окна невозможно (forge-o0sdg).
        $b24Input.addEventListener('keydown', (ev) => {
            if (!ev || ev.key !== 'Escape') return;
            if (!local.mode) return;
            if (topEscLayer()) return;
            if (typeof ev.preventDefault === 'function') ev.preventDefault();
            cancelMode();
        });
    }
    updateCharCount();
}

/**
 * Единственный обработчик прокрутки ленты.
 *
 * Делает ровно две вещи: переключает ЗАЩЁЛКУ и запускает догрузку истории.
 * Обе — по одному и тому же событию, поэтому и живут в одной функции: два
 * слушателя на одном узле рано или поздно разъехались бы по порядку вызова.
 */
function onScroll() {
    local.stickBottom = isAtBottom($b24Log);
    if (local.stickBottom) {
        // Долистал до низа — значит прочитал. Отметка уходит с задержкой и
        // самым свежим идентификатором (см. `scheduleRead`).
        scheduleRead(local.dialogId, newestId(currentMessages()));
    }
    if (shouldLoadOlder($b24Log) && local.hasOlder && !local.loading) {
        loadOlder();
    }
}

/** Полный сброс ленты — при отвязке портала и при смене аккаунта. */
export function resetChat() {
    local.generation += 1;
    local.dialogId = null;
    local.hasOlder = false;
    local.loading = false;
    local.stickBottom = true;
    local.sending = false;
    // Отложенная отметка о прочтении обязана умереть вместе с лентой: она
    // ссылается на диалог, которого в панели уже нет.
    if (local.readTimer) {
        globalThis.clearTimeout(local.readTimer);
        local.readTimer = null;
    }
    local.readPending = null;
    local.typingAt.clear();
    clearFlash();
    local.jumping = false;
    // Карта целей ответа принадлежала АККАУНТУ: после отвязки портала её
    // идентификаторы не значат ничего.
    local.replyById.clear();
    setMode(null);
    if ($b24Input) $b24Input.value = '';
    updateCharCount();
    if ($b24Log) $b24Log.textContent = '';
    resetMediaCards();
    revokeDialogMedia();
    updateHead(null);
}

/** Снимок для e2e и диагностики. */
export function _debugChatState() {
    const list = currentMessages();
    return {
        inited: local.inited,
        dialogId: local.dialogId,
        loading: local.loading,
        hasOlder: local.hasOlder,
        stickBottom: local.stickBottom,
        generation: local.generation,
        messages: list.length,
        oldest: oldestId(list),
        newest: newestId(list),
        // Текст последней строки — по нему e2e отличает «лента отрисовалась»
        // от «лента пуста», не разбирая DOM.
        lastText: list.length > 0 ? segmentsText(list[list.length - 1].segments) : '',
        // Цикл переписки (Ф6): режим ввода, неотправленные строки, счётчик.
        mode: local.mode ? local.mode.kind : null,
        modeId: local.mode ? local.mode.id : null,
        sending: local.sending,
        pending: list.filter((m) => m.pending).length,
        failed: list.filter((m) => m.failed).length,
        // Переход по цитате (forge-92x6): по ним e2e отличает «подсветил
        // оригинал» от «не нашёл».
        flashId: local.flashId,
        jumping: local.jumping,
        chars: charCountState($b24Input ? $b24Input.value : '').len,
    };
}
