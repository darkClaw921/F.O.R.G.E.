// tmux-web — Bitrix24 plugin: ЦИТАТА ОТВЕТА и поиск оригинала (forge-92x6).
//
// ============================================================================
// ПОЧЕМУ ЗДЕСЬ ВООБЩЕ НУЖЕН ПОИСК, А НЕ `reply_to`
// ============================================================================
//
// У VK и Telegram цитата адресуется идентификатором: сервер отдаёт
// `reply_to`, фронт по нему находит строку и подсвечивает её
// (`vk/chat.js::jumpToMessage`). У Битрикс24 этого пути НЕТ ВОВСЕ: `REPLY_ID`
// при отправке уходит, но при ЧТЕНИИ портал его не отражает — `params` пустые,
// а `MessageDto.reply_to` в плагине ВСЕГДА `null` (Р10, см. `dto.rs`).
//
// Единственный след ответа, доживающий до чтения, — ТЕКСТОВАЯ ЦИТАТА, которую
// приписывает сервер (`routes/dialogs.rs::compose_reply`):
//
// ```text
//   > Игорь Герасимов: текст оригинала, схлопнутый по пробелам и обрезанный
//   сам ответ
// ```
//
// Отсюда весь модуль: цитату надо УЗНАТЬ в тексте, показать отдельной строкой
// и по клику НАЙТИ оригинал среди загруженных сообщений — сопоставлением
// автора и начала текста, потому что сопоставлять больше нечем.
//
// ============================================================================
// ЧИСТОТА
// ============================================================================
//
// Ни одной ссылки на DOM: и разбор цитаты, и поиск оригинала — чистые функции
// над сегментами и списком сообщений. Так они покрываются node-тестом
// (`bitrix_reply.test.mjs`) без фейкового документа, а `chat.js` остаётся
// единственным владельцем ленты — тот же приём, что у `telegram/reply.js`.

import { segmentsText, senderLabel } from './format.js';

/**
 * Потолок длины цитаты в символах.
 *
 * Дословно совпадает с `crate::routes::dialogs::QUOTE_MAX_CHARS`: сервер
 * обрезает превью ровно здесь, и фронт обязан знать ту же границу — иначе
 * сравнение «текст оригинала начинается с цитаты» ломалось бы на длинных
 * сообщениях (см. [`stripEllipsis`]).
 */
export const QUOTE_MAX_CHARS = 200;

/**
 * Максимальная длина имени автора в цитате, символов.
 *
 * Совпадает с `truncate_chars(author, 64)` в `compose_reply`. Имя длиннее
 * приезжает обрезанным с многоточием, поэтому автор сравнивается ПРЕФИКСОМ, а
 * не на равенство.
 */
export const QUOTE_AUTHOR_MAX_CHARS = 64;

/** Начало строки-цитаты: `>` и необязательный пробел за ним. */
const QUOTE_PREFIX_RE = /^\s{0,3}>\s?/;

/**
 * Схлопывает пробелы и переводы строк в один пробел.
 *
 * Обязательно на ОБЕИХ сторонах сравнения: `compose_reply` кладёт в цитату
 * `split_whitespace().join(" ")`, то есть уже схлопнутый текст, а оригинал в
 * ленте живёт как есть — с переводами строк и двойными пробелами.
 *
 * @param {*} value
 * @returns {string}
 */
export function normalizeQuoteText(value) {
    return String(value == null ? '' : value).replace(/\s+/g, ' ').trim();
}

/**
 * Убирает многоточие, которым `truncate_chars` помечает обрезку.
 *
 * Без этого длинная цитата не совпала бы с оригиналом НИКОГДА: у сервера
 * последний символ превью — «…», а в оригинале на его месте обычная буква.
 *
 * @param {string} value
 * @returns {string}
 */
export function stripEllipsis(value) {
    const s = String(value == null ? '' : value);
    return s.endsWith('…') ? s.slice(0, -1).trim() : s;
}

/**
 * Разбирает тело цитаты на автора и текст.
 *
 * Разделитель — ПЕРВОЕ `": "`, ровно так его ставит `compose_reply`. Разбор
 * заведомо приблизителен: цитата «Внимание: смотри вложение» без автора даст
 * автора «Внимание». Это не ломает переход — [`findQuoteOrigin`] при
 * несовпадении автора пробует ещё и целую строку как текст.
 *
 * @param {string} raw — тело цитаты без ведущих `>`
 * @returns {?{author: string, text: string, raw: string}}
 */
export function parseQuoteBody(raw) {
    const flat = normalizeQuoteText(raw);
    if (!flat) return null;
    const sep = flat.indexOf(': ');
    // Автор — только правдоподобной длины: двоеточие посреди длинной фразы
    // означает знак препинания, а не подпись.
    if (sep > 0 && sep <= QUOTE_AUTHOR_MAX_CHARS) {
        return { author: flat.slice(0, sep), text: flat.slice(sep + 2), raw: flat };
    }
    return { author: '', text: flat, raw: flat };
}

/** Текстовый сегмент — служебный конструктор для остатка строки. */
function textSegment(text) {
    return { kind: 'text', text: String(text), value: null, children: [] };
}

/**
 * Отделяет ведущую цитату от самого ответа.
 *
 * Разбираются ОБЕ формы, которые доживают до ленты:
 *
 * | форма | откуда | как выглядит |
 * |---|---|---|
 * | `> Автор: текст\nответ` | своя отправка (`compose_reply`) | ведущие строки текстового сегмента |
 * | `[QUOTE]…[/QUOTE]` | веб-клиент портала | первый сегмент `quote` |
 *
 * Перевод строки живёт ВНУТРИ текстового сегмента, а не отдельным `br`:
 * `bbcode.rs` превращает в `br` только тег `[BR]`, а обычный `\n` остаётся
 * символом. Поэтому первая форма режется по `\n` внутри `segments[0].text`.
 *
 * Сообщение, состоящее ИЗ ОДНОЙ цитаты, цитатой не считается: это просто
 * реплика, начатая с `>`, и прятать её текст в узкую серую строку значило бы
 * потерять содержимое.
 *
 * @param {Array<object>} segments — bbcode::Segment[]
 * @returns {?{quote: {author: string, text: string, raw: string}, body: Array<object>}}
 */
export function splitQuote(segments) {
    const list = Array.isArray(segments) ? segments : [];
    const head = list.length > 0 ? list[0] : null;
    if (!head || typeof head !== 'object') return null;

    if (head.kind === 'quote') {
        const quote = parseQuoteBody(segmentsText(head.children));
        if (!quote) return null;
        let body = list.slice(1);
        // Перевод строки сразу за цитатой принадлежал ЕЙ: оставленный в теле,
        // он дал бы пустую первую строку ответа.
        while (body.length > 0 && body[0] && body[0].kind === 'br') body = body.slice(1);
        if (body.length === 0) return null;
        return { quote, body };
    }

    if (head.kind !== 'text') return null;
    const lines = String(head.text == null ? '' : head.text).split('\n');
    const quoted = [];
    let taken = 0;
    while (taken < lines.length && QUOTE_PREFIX_RE.test(lines[taken])) {
        quoted.push(lines[taken].replace(QUOTE_PREFIX_RE, ''));
        taken += 1;
    }
    if (quoted.length === 0) return null;

    const rest = lines.slice(taken).join('\n');
    const tail = list.slice(1);
    if (!rest.trim() && tail.length === 0) return null;

    const quote = parseQuoteBody(quoted.join(' '));
    if (!quote) return null;

    const body = [];
    // Ведущий перевод строки отброшен вместе с цитатой — он был её концом.
    if (rest !== '') body.push(textSegment(rest.replace(/^\n+/, '')));
    body.push(...tail);
    return { quote, body };
}

/** Плоский текст сообщения — тем же способом, что и цитата при отправке. */
function messageText(msg) {
    if (!msg) return '';
    return segmentsText(msg.segments) || String(msg.text == null ? '' : msg.text);
}

/**
 * Ищет оригинал, на который отвечает сообщение с `msgId`.
 *
 * Порядок обхода — от БЛИЖАЙШЕГО к старому: отвечают почти всегда на недавнее,
 * а одинаковых реплик («ок», «+») в переписке сколько угодно, и первая же
 * найденная снизу — самая правдоподобная.
 *
 * Совпадением считается ПРЕФИКС: сервер обрезал превью до
 * [`QUOTE_MAX_CHARS`], поэтому равенства не бывает у длинных сообщений.
 * Автор сравнивается тоже префиксом ([`QUOTE_AUTHOR_MAX_CHARS`]).
 *
 * Запасной путь — совпадение по ЦЕЛОЙ строке цитаты: он спасает случай, когда
 * двоеточие в тексте было принято за подпись автора ([`parseQuoteBody`]).
 *
 * @param {Array<object>} list — сообщения ленты по возрастанию id
 * @param {number|string} msgId — сообщение-ОТВЕТ (ищем строго старше него)
 * @param {{author: string, text: string, raw: string}} quote
 * @returns {?object} сообщение-оригинал
 */
export function findQuoteOrigin(list, msgId, quote) {
    const arr = Array.isArray(list) ? list : [];
    if (!quote) return null;
    const target = Number(msgId);
    const wantAuthor = stripEllipsis(normalizeQuoteText(quote.author));
    const wantText = stripEllipsis(normalizeQuoteText(quote.text));
    const wantRaw = stripEllipsis(normalizeQuoteText(quote.raw));
    if (!wantText && !wantRaw) return null;

    let fallback = null;
    for (let i = arr.length - 1; i >= 0; i -= 1) {
        const msg = arr[i];
        if (!msg || msg.pending || msg.failed) continue;
        // Оригинал СТАРШЕ ответа. Без этой проверки цитата нашла бы саму себя:
        // текст ответа начинается ровно с той же строки цитаты.
        if (Number.isFinite(target) && !(Number(msg.id) < target)) continue;
        const plain = normalizeQuoteText(messageText(msg));
        if (!plain) continue;
        if (wantText && plain.startsWith(wantText)) {
            if (!wantAuthor) return msg;
            if (normalizeQuoteText(senderLabel(msg)).startsWith(wantAuthor)) return msg;
        }
        if (!fallback && wantRaw && plain.startsWith(wantRaw)) fallback = msg;
    }
    return fallback;
}
