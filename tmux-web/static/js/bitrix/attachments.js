// tmux-web — Bitrix24 plugin: вложения и карточки `ATTACH`.
//
// ============================================================================
// ЧТО РИСУЕТ ЭТОТ МОДУЛЬ
// ============================================================================
//
//   • КАРТОЧКУ ФАЙЛА (`msg.media[]`) — имя, размер, вид, место под превью,
//     полосу прогресса и кнопки OPEN/SAVE. Только РАЗМЕТКУ: байты, ленивую
//     загрузку, лайтбокс и плеер добавляет `js/bitrix/media.js` (Ф8), находя
//     эти узлы по классам. Модуль не делает ни одного запроса и не вешает ни
//     одного обработчика — поэтому он проверяется в голом Node.
//   • КАРТОЧКУ `ATTACH` (`msg.cards[]`) — блоки трёх видов:
//       MESSAGE — текстовый блок (BB-коды уже разобраны в сегменты),
//       LINK    — ссылка с заголовком и описанием,
//       GRID    — таблица «поле: значение».
//     Незнакомый вид блока до фронта не доезжает вовсе: его отбрасывает
//     `attach.rs`, и это правильное место — фильтр должен быть один.
//
// ============================================================================
// ДВА ПРАВИЛА, КОТОРЫЕ ЛЕГКО ПОТЕРЯТЬ
// ============================================================================
//
// 1. ТОЛЬКО `createElement`/`textContent`. `innerHTML` в панели запрещён
//    полностью: содержимое карточек пишут боты и роботы портала.
// 2. АДРЕС ССЫЛКИ НЕ СТАНОВИТСЯ `href`. Схему уже проверил бэкенд
//    (`attach.rs::http_url` пропускает только http/https), но рубежа должно
//    быть два, а `href` с чужим адресом — это ещё и утечка referrer'а с
//    localhost-девфоржа на чужой хост. Ссылка рисуется текстом; открытие
//    (если понадобится) появится вместе с медиа в Ф8.
//
// Модуль НЕ импортирует ленту: он только собирает узлы и отдаёт их наверх.

import { buildWave, noiseSeed } from '../messenger/waveform-core.js';
import { formatBytes, mediaKindLabel, renderSegments } from './format.js';

/** Сколько карточек рисуется под одним сообщением. */
export const MAX_CARDS = 10;

/** Сколько строк `GRID` показывается в одной карточке. */
export const MAX_GRID_ROWS = 40;

/** Глиф вида вложения — им подписывается карточка-заглушка. */
export function mediaGlyph(kind) {
    switch (kind) {
        case 'image': return '▣';
        case 'video':
        case 'video_note': return '▶';
        case 'audio':
        case 'voice': return '♪';
        default: return '⛁';
    }
}

/**
 * Строка описания вложения: `forge_probe.txt · 500 Б · файл`.
 *
 * Имя может отсутствовать (дескриптор файла не приехал в ответе) — тогда
 * подписью становится вид: «файл без имени» честнее, чем пустая карточка.
 *
 * @param {?object} media — MediaDto
 * @returns {string}
 */
export function mediaLabel(media) {
    if (!media) return '';
    const parts = [];
    const name = String(media.name == null ? '' : media.name).trim();
    if (name) parts.push(name);
    const size = formatBytes(media.size);
    if (size) parts.push(size);
    parts.push(mediaKindLabel(media.kind));
    return parts.join(' · ');
}

/**
 * Карточка одного вложения — ЧИСТАЯ РАЗМЕТКА, без единого обработчика.
 *
 * # Почему построение и поведение разнесены
 *
 * Здесь только узлы: ни `addEventListener`, ни `fetch`, ни `objectURL`. Всё
 * это живёт в `js/bitrix/media.js`, который находит готовые узлы по классам и
 * навешивает на них поведение. Разделение не косметическое — оно позволяет
 * проверять разметку карточки в голом Node с крошечным фейком документа
 * (`bitrix_bbcode.test.mjs`), где нет ни `addEventListener`, ни `URL`.
 *
 * # Превью строится только у картинок
 *
 * Отдельного «маленького файла» у портала нет: `im.v2.File.download` отдаёт
 * ОРИГИНАЛ, а `mediaUrl.preview["250"]` без сессионной куки не работает.
 * Поэтому `<img>` заводится ровно там, где его действительно есть чем
 * наполнить, — у `kind === 'image'` с доступным файлом. Атрибут `src` при
 * построении НЕ ставится: байты приезжают лениво и только через прокси.
 *
 * @param {object} media — MediaDto
 * @param {object} [doc]
 * @param {?object} [msg] — MessageDto, если известен: его идентификаторы
 *   становятся адресом карточки для прокси и для кадров прогресса
 * @returns {object} узел
 */
export function buildMediaCard(media, doc = globalThis.document, msg = null) {
    const kind = String(media.kind || 'file');
    const box = doc.createElement('div');
    box.className = `b24-media-card b24-media-${kind}`;
    box.dataset.kind = kind;
    box.dataset.index = String(media.index == null ? 0 : media.index);
    box.dataset.fileId = String(media.file_id == null ? 0 : media.file_id);
    if (msg) {
        box.dataset.dialogId = String(msg.dialog_id == null ? '' : msg.dialog_id);
        box.dataset.msgId = String(msg.id == null ? '' : msg.id);
    }

    // --- шапка: глиф, имя, вид
    const head = doc.createElement('div');
    head.className = 'b24-media-head';
    const glyph = doc.createElement('span');
    glyph.className = 'b24-media-glyph';
    glyph.textContent = mediaGlyph(kind);
    head.appendChild(glyph);
    const name = doc.createElement('span');
    name.className = 'b24-media-name';
    // Имени может не быть вовсе (дескриптор файла не приехал) — тогда подписью
    // становится вид: «файл без имени» честнее пустой строки.
    name.textContent = String(media.name || '').trim() || mediaKindLabel(kind);
    head.appendChild(name);
    box.appendChild(head);

    // --- тело: превью либо иконка, плюс слот плеера
    const body = doc.createElement('div');
    body.className = 'b24-media-body';
    if (kind === 'image' && media.has_full) {
        const thumb = doc.createElement('img');
        thumb.className = 'b24-media-thumb';
        thumb.alt = '';
        thumb.hidden = true;
        body.appendChild(thumb);
    }
    const icon = doc.createElement('span');
    icon.className = 'b24-media-icon';
    icon.textContent = mediaGlyph(kind);
    body.appendChild(icon);
    if (kind === 'voice' || kind === 'audio') {
        // Звуковая дорожка. Портал огибающую не отдаёт вовсе — её считает
        // бэкенд по скачанному файлу и кладёт в `media.waveform`; пока её нет,
        // общий модуль рисует детерминированную заглушку. Рисовальщик тот же,
        // что у VK и Telegram: волна одной записи обязана выглядеть одинаково
        // во всех трёх панелях.
        body.appendChild(buildWave({
            waveform: media.waveform,
            seed: noiseSeed(msg && msg.id, media.index),
            className: 'b24-media-wave',
            doc,
        }));
        icon.hidden = true;
    }
    const player = doc.createElement('div');
    player.className = 'b24-media-player';
    player.hidden = true;
    body.appendChild(player);
    box.appendChild(body);

    // --- подвал: метаданные, полоса, статус, кнопки
    const foot = doc.createElement('div');
    foot.className = 'b24-media-foot';
    const meta = doc.createElement('span');
    meta.className = 'b24-media-meta';
    meta.textContent = metaLine(media);
    foot.appendChild(meta);
    const progress = doc.createElement('span');
    progress.className = 'b24-media-progress';
    progress.hidden = true;
    const bar = doc.createElement('span');
    bar.className = 'b24-media-progress-bar';
    progress.appendChild(bar);
    foot.appendChild(progress);
    const status = doc.createElement('span');
    status.className = 'b24-media-status';
    foot.appendChild(status);
    for (const [label, act, title] of [
        ['OPEN', 'open', 'Открыть вложение'],
        ['SAVE', 'save', 'Сохранить файл'],
    ]) {
        const btn = doc.createElement('button');
        btn.type = 'button';
        btn.className = 'b24-media-btn';
        btn.dataset.act = act;
        btn.textContent = label;
        btn.title = media.has_full ? title : 'Файла нет на портале';
        btn.disabled = !media.has_full;
        foot.appendChild(btn);
    }
    box.appendChild(foot);
    return box;
}

/**
 * Строка метаданных карточки: `500 Б · файл`.
 *
 * Отдельно от [`mediaLabel`], потому что имя файла уехало в свою строку: у
 * длинного имени и у метаданных разное поведение при переносе.
 *
 * @param {object} media
 * @returns {string}
 */
function metaLine(media) {
    const parts = [];
    const size = formatBytes(media.size);
    if (size) parts.push(size);
    parts.push(mediaKindLabel(media.kind));
    return parts.join(' · ');
}

/**
 * Все вложения сообщения одним контейнером.
 *
 * @param {?Array<object>} list — `msg.media`
 * @param {object} [doc]
 * @param {?object} [msg] — MessageDto (адрес карточек для прокси)
 * @returns {?object} узел либо `null`, если вложений нет
 */
export function buildMediaList(list, doc = globalThis.document, msg = null) {
    const items = Array.isArray(list) ? list : [];
    if (items.length === 0) return null;
    const box = doc.createElement('div');
    box.className = 'b24-line-media';
    for (const media of items) box.appendChild(buildMediaCard(media, doc, msg));
    return box;
}

/**
 * Блок карточки `ATTACH`.
 *
 * @param {object} block — CardBlockDto
 * @param {object} doc
 * @returns {?object}
 */
function buildBlock(block, doc) {
    if (!block || typeof block !== 'object') return null;

    if (block.kind === 'message') {
        const box = doc.createElement('div');
        box.className = 'b24-card-message';
        // Сегменты, а не строка: текст блока писал бот.
        box.appendChild(renderSegments(block.segments, doc));
        return box;
    }

    if (block.kind === 'link') {
        const links = Array.isArray(block.links) ? block.links : [];
        if (links.length === 0) return null;
        const box = doc.createElement('div');
        box.className = 'b24-card-links';
        for (const link of links) {
            const row = doc.createElement('div');
            row.className = 'b24-card-link';
            const name = doc.createElement('span');
            name.className = 'b24-card-link-name';
            name.textContent = String(link.name || link.url || '');
            row.appendChild(name);
            if (link.url) {
                const url = doc.createElement('span');
                // НЕ `<a href>`: адрес прислал собеседник (см. шапку модуля).
                url.className = 'b24-card-link-url';
                url.textContent = String(link.url);
                url.title = String(link.url);
                row.appendChild(url);
            }
            if (link.desc) {
                const desc = doc.createElement('span');
                desc.className = 'b24-card-link-desc';
                desc.textContent = String(link.desc);
                row.appendChild(desc);
            }
            box.appendChild(row);
        }
        return box;
    }

    if (block.kind === 'grid') {
        const rows = (Array.isArray(block.rows) ? block.rows : []).slice(0, MAX_GRID_ROWS);
        if (rows.length === 0) return null;
        const box = doc.createElement('div');
        box.className = 'b24-card-grid';
        for (const row of rows) {
            const line = doc.createElement('div');
            line.className = `b24-card-grid-row b24-card-grid-${String(row.display || 'line')}`;
            const name = doc.createElement('span');
            name.className = 'b24-card-grid-name';
            name.textContent = String(row.name == null ? '' : row.name);
            line.appendChild(name);
            const value = doc.createElement('span');
            value.className = 'b24-card-grid-value';
            value.textContent = String(row.value == null ? '' : row.value);
            // Цвет уже проверен бэкендом (`hex_color`), но повторная проверка
            // здесь стоит одного регэкспа — рубежа должно быть два.
            if (typeof row.color === 'string' && /^#[0-9a-f]{3}([0-9a-f]{3})?$/i.test(row.color)) {
                value.style.color = row.color;
            }
            line.appendChild(value);
            box.appendChild(line);
        }
        return box;
    }

    // Незнакомый вид блока (бэкенд такие не отдаёт) — молча пропускаем.
    return null;
}

/**
 * Карточки `params.ATTACH` одного сообщения.
 *
 * @param {?Array<object>} list — `msg.cards`
 * @param {object} [doc]
 * @returns {?object} узел либо `null`
 */
export function buildCards(list, doc = globalThis.document) {
    const cards = (Array.isArray(list) ? list : []).slice(0, MAX_CARDS);
    if (cards.length === 0) return null;

    const box = doc.createElement('div');
    box.className = 'b24-cards';
    let drawn = 0;
    for (const card of cards) {
        const node = doc.createElement('div');
        node.className = 'b24-card';
        if (typeof card.color === 'string' && /^#[0-9a-f]{3}([0-9a-f]{3})?$/i.test(card.color)) {
            node.style.borderLeftColor = card.color;
        }
        if (card.description) {
            const desc = doc.createElement('div');
            desc.className = 'b24-card-desc';
            desc.textContent = String(card.description);
            node.appendChild(desc);
        }
        let blocks = 0;
        for (const block of (Array.isArray(card.blocks) ? card.blocks : [])) {
            const built = buildBlock(block, doc);
            if (built) {
                node.appendChild(built);
                blocks += 1;
            }
        }
        if (blocks > 0 || card.description) {
            box.appendChild(node);
            drawn += 1;
        }
    }
    return drawn > 0 ? box : null;
}
