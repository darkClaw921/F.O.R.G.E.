// tmux-web — VK plugin: нормализация вложений и пересланных сообщений.
//
// НОВЫЙ модуль: аналога в js/telegram/* нет вовсе. У Telegram вложение
// ОДНО на сообщение и живёт полем `msg.media`; у VK их до десяти в массиве
// `msg.attachments`, а сверх того есть рекурсивные `fwd` — и то и другое
// требует чистой логики, которую можно проверить без браузера.
//
// Модуль не трогает DOM и не ходит в сеть: он готовит МОДЕЛЬ для карточек,
// сам рендер приезжает в js/vk/media.js (Ф7). Благодаря этому он грузится в
// Node и покрыт tmux-web/tests/frontend/vk_attachments.test.mjs.
//
// ============================================================================
// ИНВАРИАНТ: url ВСЕГДА null
// ============================================================================
//
// Прямые ссылки НА ФАЙЛЫ VK в DOM запрещены. Они подписанные и протухающие (в
// ленте недельной давности всё равно отдали бы 403), а запрос с Referer
// devforge на серверы VK неуместен для панели с личной перепиской: он светит и
// факт использования плагина, и cookie пользователя в чужих логах.
//
// Ни один `att.url` не должен попасть в `<img src>` или `<a href>` — всё идёт
// через прокси бэкенда (`GET /api/vk/media/:peer/:mid/:idx`). Поле `url`
// оставлено в модели ЯВНО и всегда `null`: так «забыть» правило нельзя —
// рендер, который его нарушит, получит пустую ссылку и сломается заметно, а не
// тихо утечёт. Бэкенд, со своей стороны, адресов файлов вообще не отдаёт (см.
// plugins/vk/src/attach.rs) — это второй, независимый рубеж.
//
// То же правило действует для картинки captcha (js/vk/format.js::captchaFromError).
//
// ЧТО ПОД ЭТО ПРАВИЛО НЕ ПОПАДАЕТ: `playerUrl` и `linkUrl`. Первый ведёт на
// СТРАНИЦУ-плеер VK (файла у вложения video не существует вовсе), второй — на
// адрес, который прислал собеседник. Ни тот, ни другой не адресуют CDN
// вложений, не подписаны и не протухают, а пользователь идёт по ним сознательно.
// Оба проходят белый список схем (`format.js::safeUrl` / `webUrl`).

import { mediaGlyph, truncate } from './format.js';

/**
 * Сколько карточек вложений рисуется в строке ленты подряд.
 *
 * Остальное сворачивается в суффикс «…ещё N». Шесть — это ширина строки лога
 * при типичном размере карточки; десять карточек (потолок VK) превращают одно
 * сообщение в экран.
 */
export const MAX_INLINE_CARDS = 6;

/**
 * Глубина разбора пересланных, которую рисует лента.
 *
 * МЕНЬШЕ серверной (`dto::MAX_FWD_DEPTH = 3`) намеренно: сервер отдаёт на один
 * уровень больше, чем показывает UI, чтобы «…ещё N пересланных» считалось по
 * настоящим данным, а не по обрезанным. Экран из одного сообщения — ровно то,
 * что получается без этого ограничения: у VK цепочка пересылок не ограничена
 * ничем.
 */
export const MAX_FWD_DEPTH = 2;

/**
 * Длина превью текста пересланного/цитаты, символов.
 *
 * Совпадает с серверным `REPLY_PREVIEW_CHARS`: сервер уже обрезал, здесь
 * страховка на случай данных из кадра WS, собранного другой версией плагина.
 */
export const FWD_PREVIEW_CHARS = 140;

/**
 * Вид вложения в словаре UI.
 *
 * Бэкенд уже нормализовал `kind` (см. `attach.rs`), и в 99% случаев здесь
 * просто проверка словаря. Функция всё же существует, потому что через неё
 * проходят и СЫРЫЕ вложения VK — из кадров WS старой версии и из
 * `fwd`/`reply_to`, где вместо полного MediaDto ездит одна строка вида.
 *
 * НЕИЗВЕСТНЫЙ вид даёт `unsupported`, а не выбрасывается: карточка обязана
 * нарисоваться отключённой. Пропавшее вложение читается как потеря данных, а
 * отключённая карточка — как «этот тип мы не умеем».
 *
 * @param {object|string} att — MediaDto, сырое вложение VK либо строка вида
 * @returns {string}
 */
export function attachmentKind(att) {
    if (!att) return 'unsupported';
    const raw = typeof att === 'string' ? att : (att.kind || att.type || '');
    switch (String(raw)) {
        // Словарь MediaDto бэкенда — проходит как есть.
        case 'photo':
        case 'video':
        case 'audio':
        case 'voice':
        case 'document':
        case 'sticker':
        case 'graffiti':
        case 'other':
            return String(raw);
        // Сырые имена типов VK (кадр WS, ответ старого сервера).
        case 'audio_message':
            return 'voice';
        case 'doc':
            return 'document';
        case 'wall':
        case 'wall_reply':
        case 'link':
        case 'market':
        case 'market_album':
        case 'poll':
        case 'gift':
        case 'call':
        case 'story':
        case 'podcast':
        case 'article':
            return 'other';
        default:
            return 'unsupported';
    }
}

/**
 * Все вложения сообщения в модели для рендера.
 *
 * ПОРЯДОК И ИНДЕКСЫ СОХРАНЯЮТСЯ РОВНО ТАКИМИ, какими их прислал сервер:
 * `index` входит в ключ медиа-кэша и в маршрут `/api/vk/media/:peer/:mid/:idx`,
 * поэтому ни сортировка, ни фильтрация «ненужных» видов здесь невозможны — они
 * сдвинули бы индексы и показали бы пользователю не то вложение.
 *
 * Сообщение старого формата (одно поле `media` без массива) поддерживается:
 * так приезжают кадры WS, собранные предыдущей версией плагина, и терять из-за
 * этого картинку в ленте незачем.
 *
 * @param {object} msg — MessageDto
 * @param {{max?: number}} [opts] — `max` ограничивает ДЛИНУ выдачи (не индексы)
 * @returns {Array<{index: number, kind: string, mime: ?string, size: ?number,
 *   width: ?number, height: ?number, durationSecs: ?number, fileName: ?string,
 *   title: ?string, url: null, playable: boolean, thumbable: boolean,
 *   playerUrl: ?string, linkUrl: ?string, raw: object}>}
 */
export function normalizeAttachments(msg, opts) {
    const src = attachmentSource(msg);
    const max = opts && Number.isFinite(opts.max) && opts.max > 0 ? opts.max : src.length;
    const out = [];
    for (let i = 0; i < src.length && out.length < max; i += 1) {
        const a = src[i];
        if (!a || typeof a !== 'object') continue;
        // Индекс берём из самого вложения: сервер уже проставил его, и он
        // может не совпасть с позицией, если кадр WS нёс подмножество.
        const index = Number.isFinite(Number(a.index)) ? Number(a.index) : i;
        const kind = attachmentKind(a);
        out.push({
            index,
            kind,
            mime: strOrNull(a.mime),
            size: numOrNull(a.size),
            width: numOrNull(a.width),
            height: numOrNull(a.height),
            durationSecs: numOrNull(a.duration_secs),
            fileName: strOrNull(a.file_name),
            title: strOrNull(a.title),
            // ↓↓↓ ИНВАРИАНТ МОДУЛЯ: прямых ссылок VK в DOM не бывает.
            url: null,
            // Воспроизводимо ли ЗДЕСЬ, в панели. У video false всегда:
            // вложение VK не содержит файла, есть только страница-плеер.
            playable: (kind === 'voice' || kind === 'audio') && a.has_full === true,
            thumbable: a.has_thumb === true,
            // Ссылка на СТРАНИЦУ-плеер VK, а не на файл (у вложения video
            // воспроизводимого файла не существует).
            playerUrl: strOrNull(a.player_url),
            // Адрес вложения `link` — тот, что прислал собеседник. Здесь он
            // только нормализуется до строки-или-null; белый список схем
            // применяется на границе с DOM (js/vk/media.js через `webUrl`),
            // как и для playerUrl.
            linkUrl: strOrNull(a.link_url),
            raw: a,
        });
    }
    return out;
}

/**
 * Что показывать в свёрнутой строке ленты.
 *
 * Первое вложение, а не «самое интересное»: у VK порядок задаёт отправитель, и
 * ровно его показывает официальный клиент. Плюс это совпадает с `msg.media`
 * бэкенда — две разные «первые» карточки означали бы, что строка ленты и
 * превью в списке бесед показывают разное.
 *
 * @param {Array<object>} list — результат normalizeAttachments
 * @returns {?object}
 */
export function primaryAttachment(list) {
    return (Array.isArray(list) && list.length) ? list[0] : null;
}

/**
 * Суффикс строки при множественных вложениях: `▣▣▶ 3 вложения`.
 *
 * Глифы — ровно те же, что на карточках (`mediaGlyph`), чтобы свёрнутая строка
 * читалась как оглавление развёрнутой. Пустая строка при одном вложении: там
 * карточка и так одна, и подпись «1 вложение» только шумит.
 *
 * @param {Array<object>} list
 * @returns {string}
 */
export function attachmentsSummary(list) {
    const arr = Array.isArray(list) ? list : [];
    if (arr.length < 2) return '';
    const glyphs = arr.slice(0, MAX_INLINE_CARDS).map((a) => mediaGlyph(a.kind)).join('');
    return `${glyphs} ${arr.length} ${plural(arr.length, 'вложение', 'вложения', 'вложений')}`;
}

/**
 * Есть ли что рисовать карточками.
 *
 * `other`/`unsupported` тоже считаются: у них нет файла, но есть карточка с
 * заголовком — иначе сообщение «поделился опросом» выглядело бы пустым.
 *
 * @param {object} msg — MessageDto
 * @returns {boolean}
 */
export function hasRenderableMedia(msg) {
    return attachmentSource(msg).length > 0;
}

/**
 * Уплощает пересланные сообщения в список строк с глубиной.
 *
 * Сервер уже отдал их плоскими (`msg.fwd` с полем `depth`) и уже обрезал по
 * своим границам; здесь применяется ВТОРАЯ, более жёсткая граница — та, что
 * помещается на экран (см. [`MAX_FWD_DEPTH`]). Функция при этом принимает и
 * СЫРУЮ рекурсивную форму (`fwd_messages` с вложенными массивами): в этой
 * форме пересылки приходят внутри `reply_to` и в кадрах WS старой версии.
 *
 * @param {object} msg — MessageDto
 * @param {number} [maxDepth=MAX_FWD_DEPTH]
 * @returns {{rows: Array<{depth: number, msg: object}>, truncatedCount: number}}
 *   `truncatedCount` — сколько пересланных НЕ показано (глубина + серверный
 *   остаток `omitted`); ровно это число уходит в хвост «…ещё N пересланных».
 */
export function flattenForwards(msg, maxDepth) {
    const cap = Number.isFinite(maxDepth) && maxDepth > 0 ? maxDepth : MAX_FWD_DEPTH;
    const rows = [];
    let truncated = 0;

    const flat = (msg && Array.isArray(msg.fwd)) ? msg.fwd : null;
    if (flat) {
        for (const f of flat) {
            if (!f || typeof f !== 'object') continue;
            const depth = Number(f.depth) || 0;
            // Серверный остаток уровня доезжает числом даже с уровня, который
            // мы не рисуем: пользователь обязан видеть, что там что-то есть.
            truncated += Math.max(0, Number(f.omitted) || 0);
            if (depth >= cap) {
                truncated += 1;
                continue;
            }
            rows.push({ depth, msg: f });
        }
        return { rows, truncatedCount: truncated };
    }

    // Сырая рекурсивная форма VK.
    const walk = (list, depth) => {
        for (const m of Array.isArray(list) ? list : []) {
            if (!m || typeof m !== 'object') continue;
            if (depth >= cap) {
                truncated += 1;
                continue;
            }
            rows.push({ depth, msg: m });
            walk(m.fwd_messages, depth + 1);
        }
    };
    walk(msg && msg.fwd_messages, 0);
    return { rows, truncatedCount: truncated };
}

/**
 * Подпись хвоста: `…ещё 3 пересланных`.
 *
 * @param {number} count
 * @returns {string} пустая строка, если скрывать нечего
 */
export function forwardsTail(count) {
    const n = Math.max(0, Number(count) || 0);
    if (n === 0) return '';
    return `…ещё ${n} ${plural(n, 'пересланное', 'пересланных', 'пересланных')}`;
}

/**
 * Однострочное превью пересланного: `имя: текст` либо `имя: ▣ PHOTO`.
 *
 * Пустой текст при непустых вложениях — обычное дело у VK, и превью обязано
 * это пережить: строка «имя:» без продолжения читается как поломка.
 *
 * @param {object} fwd — элемент rows[].msg
 * @returns {string}
 */
export function forwardPreview(fwd) {
    const f = fwd || {};
    const who = (f.sender && (f.sender.screen_name || f.sender.first_name || f.sender.id)) || '';
    const text = typeof f.text === 'string' ? f.text.trim() : '';
    if (text) return who ? `${who}: ${truncate(text, FWD_PREVIEW_CHARS)}` : truncate(text, FWD_PREVIEW_CHARS);
    const kind = f.media_kind ? attachmentKind(f.media_kind) : null;
    const mark = kind ? `${mediaGlyph(kind)} ${kind}` : '—';
    return who ? `${who}: ${mark}` : mark;
}

// ---------------------------------------------------------------------------
// Вспомогательное.
// ---------------------------------------------------------------------------

/**
 * Массив вложений сообщения в любой из поддерживаемых форм.
 *
 * `attachments` — источник истины; одиночный `media` поддерживается ради
 * кадров WS, собранных предыдущей версией плагина.
 */
function attachmentSource(msg) {
    if (!msg || typeof msg !== 'object') return [];
    if (Array.isArray(msg.attachments) && msg.attachments.length) return msg.attachments;
    return msg.media ? [msg.media] : [];
}

function strOrNull(v) {
    if (typeof v !== 'string') return null;
    const t = v.trim();
    return t ? t : null;
}

function numOrNull(v) {
    // `Number(null)` — это 0, поэтому отсутствие проверяется ДО приведения:
    // иначе «размер не пришёл» рисовалось бы честным «0 B».
    if (v === null || v === undefined || v === '') return null;
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
}

/** Русское склонение по числу. */
function plural(n, one, few, many) {
    const abs = Math.abs(Number(n) || 0) % 100;
    const last = abs % 10;
    if (abs > 10 && abs < 20) return many;
    if (last > 1 && last < 5) return few;
    return last === 1 ? one : many;
}
