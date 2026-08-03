// tmux-web — VK plugin: карточки вложений, загрузка, лайтбокс-галерея, плеер.
//
// Карточка (360px, та же вёрстка, что у Telegram):
//
//   ┌─ ▣ photo_42_0.jpg ─────────────────── PHOTO ─┐
//   │ [ превью 184px ]                    ▶  0:07  │
//   │ 1280×720 · 1.2 MB  [====----]  49%  OPEN SAVE│
//   └──────────────────────────────────────────────┘
//
// ============================================================================
// ЧЕМ ЭТОТ ФАЙЛ ОТЛИЧАЕТСЯ ОТ js/telegram/media.js
// ============================================================================
//
// 1. У сообщения VK ВЛОЖЕНИЙ ДО ДЕСЯТИ, а не одно. Отсюда:
//    * `buildMediaCard(msg, att)` принимает ВЛОЖЕНИЕ, а не сообщение;
//    * `renderAttachmentGroup(msg)` рисует N карточек в одном контейнере,
//      сворачивая хвост в «…ещё N» (потолок — MAX_INLINE_CARDS);
//    * ключ кэша несёт ИНДЕКС: `mediaKey(dialogId, messageId, index, kind)`.
//      Без индекса второе фото затирало бы первое и в `vkState.mediaCache`, и
//      в учёте objectURL — то есть один блоб остался бы неотозванным навсегда.
// 2. ЛАЙТБОКС — ГАЛЕРЕЯ. Стрелки ←/→ ходят по картиночным вложениям ОДНОГО
//    сообщения; у Telegram галереи не было вовсе (там нечего листать).
// 3. ВИДЕО НЕ ПРОКСИРУЕТСЯ. Вложение `video` у VK не содержит воспроизводимого
//    файла: есть только страница-плеер (`player_url`). Карточка показывает
//    превью и открывает плеер во внешней вкладке — делать вид, что мы умеем
//    отдать видеофайл, нельзя.
// 4. Появилась карточка `other` (ссылка, опрос, товар, запись на стене).
//    Оригинала у неё не бывает НИКОГДА, а вот ПРЕВЬЮ и АДРЕС бывают — у вида
//    `link`. Карточка ссылки показывает заголовок, превью (через тот же прокси,
//    что и все вложения), домен и переход по клику.
//
// ============================================================================
// ИНВАРИАНТЫ, ПЕРЕНЕСЁННЫЕ ДОСЛОВНО
// ============================================================================
//
// * КРИТИЧНО — загрузка только через `fetch` → `blob` → `objectURL`. Атрибут
//   `src` у `<img>`/`<audio>` НЕ несёт заголовков, а в remote-mode `/api/*`
//   требует `Authorization: Bearer`. Прямой `src="/api/vk/…"` работал бы
//   локально и молча ломался у пользователя с токеном.
// * ПРЯМЫХ ССЫЛОК НА ФАЙЛЫ VK В DOM НЕ БЫВАЕТ. Ни один адрес CDN вложений не
//   попадает в `<img src>`/`<a href>`: байты идут через прокси бэкенда. Ссылки
//   VK подписанные и протухающие, а запрос с Referer devforge на серверы VK
//   неуместен для панели с личной перепиской.
//   Внешних адресов в DOM ровно два, и оба — не файлы: страница плеера видео
//   (через `safeUrl`) и адрес вложения `link` (через `webUrl`, то есть строго
//   `http`/`https`). Второй — содержимое переписки, а не инфраструктура VK:
//   ссылку набрал собеседник, и пользователь идёт по ней сознательно.
// * Превью грузятся ЛЕНИВО (IntersectionObserver): в истории на сотню медиа
//   иначе улетела бы сотня запросов при первом же рендере. Обратная сторона
//   ленивости — КАРТОЧКА РАСТЁТ ЗАДНИМ ЧИСЛОМ: до декодирования картинки
//   `<img>` занимает по вертикали ноль, тело держится на `min-height: 92px`, а
//   с превью доходит до `max-height: 184px`. Лента к этому моменту уже
//   прокручена, поэтому о каждом таком приросте модуль СООБЩАЕТ наружу
//   (`setMediaGrowthHandler`) — иначе низ переписки уезжает под кромку ровно на
//   сумму догруженных превью (см. шапку js/vk/chat.js, «прижатие к низу»).
// * Полный файл — ТОЛЬКО по явному действию (OPEN/SAVE/клик по превью).
// * objectURL'ы живут в `vkState.mediaCache` и revoke'аются при смене беседы и
//   на teardown: без этого каждый просмотренный файл оставался бы в памяти
//   вкладки до перезагрузки страницы.
// * Всё содержимое — `textContent`/`createElement`. Имена файлов приходят из
//   VK, то есть от чужого аккаунта; `innerHTML` здесь означал бы XSS.
// * Прогресс двухногий: `download_progress` из WS — это «сервер тянет из VK»
//   (VK), чтение `response.body` — «сервер отдаёт браузеру» (NET). Обе ноги
//   пишут в один прогрессбар, но с разной подписью, иначе индикатор дважды
//   бежал бы от 0 до 100 без объяснений.
// * Плеер НЕ пересоздаётся при повторном клике по той же карточке: клик
//   переключает play/pause с сохранением позиции.

import {
    $vkLightbox, $vkLightboxCaption, $vkLightboxClose, $vkLightboxImg, $vkLightboxNext,
    $vkLightboxPrev,
} from '../core/dom.js';

import { fetchMedia } from './api.js';
import { MAX_INLINE_CARDS, normalizeAttachments } from './attachments.js';
import {
    canShowThumb, durationLabel, formatBytes, isImageKind, isPlayableKind, linkDomain,
    mediaFileName, mediaGlyph, mediaKey, mediaKindLabel, mediaMetaLine, progressLabel,
    progressPercent, safeUrl, sanitizeDownloadName, webUrl,
} from './format.js';
import { vkState } from './state.js';

/** Высота превью в карточке (синхронно с CSS `.vk-media-thumb`). */
export const THUMB_HEIGHT = 184;

/**
 * Верхний индекс вложения — столько же, сколько у бэкенда
 * (`media_cache::MAX_ATTACH_INDEX`).
 *
 * Нужен ровно в одном месте: `dropMessageMedia` обходит все возможные ключи
 * сообщения, потому что к моменту правки список вложений уже другой и спросить
 * «сколько их было» не у кого.
 */
export const MAX_ATTACH_INDEX = 9;

/** Запас в пикселях, при котором превью считается «почти видимым». */
const OBSERVER_MARGIN = '200px';

/**
 * Кого известить, что карточка выросла в высоту.
 *
 * Модульная переменная, а не параметр `renderAttachmentGroup`: рост случается
 * НЕ в момент построения карточки, а спустя сотни миллисекунд — когда браузер
 * декодировал картинку. Прокидывать колбэк через каждую карточку означало бы
 * хранить его в контроллере ради одного вызова.
 *
 * Обратной зависимости `media.js → chat.js` при этом не возникает: подписчика
 * ставит сам chat.js в `initChat`, а модуль знает о нём только как о функции.
 */
let onMediaGrowth = null;

/**
 * Подписывает ленту на «карточка доросла».
 *
 * @param {?function(): void} fn — null снимает подписку
 */
export function setMediaGrowthHandler(fn) {
    onMediaGrowth = typeof fn === 'function' ? fn : null;
}

/**
 * Сообщает подписчику о приросте высоты.
 *
 * Исключение подписчика не имеет права уронить загрузку превью: карточка
 * важнее прокрутки.
 */
function noteMediaGrowth() {
    if (!onMediaGrowth) return;
    try {
        onMediaGrowth();
    } catch (e) {
        console.debug('[vk-media] обработчик роста упал', e);
    }
}

const local = {
    /** `<dialog>/<message>/<index>` → контроллер карточки. */
    cards: new Map(),
    /** `<dialog>/<message>` → карточки одного сообщения В ПОРЯДКЕ ВЛОЖЕНИЙ. */
    groups: new Map(),
    /** ключ медиа → Promise<objectURL> незавершённой загрузки. */
    loads: new Map(),
    /** IntersectionObserver ленивых превью (создаётся при первой карточке). */
    observer: null,
    /** Активный плеер: старт нового останавливает предыдущий. */
    player: null,
    /** Ключ карточки, для которой полный файл плеера ещё грузится. */
    pendingPlayKey: null,
    /** Esc-обработчик остановки плеера навешен (лениво, один раз). */
    playerEscBound: false,
    /** Обработчики лайтбокса навешены. */
    lightboxBound: false,
    /** Состояние лайтбокса: галерея, позиция в ней и куда вернуть фокус. */
    lightbox: { ctl: null, gallery: [], pos: 0, lastFocus: null, url: null },
};

// ---------------------------------------------------------------------------
// Загрузка
// ---------------------------------------------------------------------------

/**
 * Грузит вложение и возвращает objectURL.
 *
 * Дедупликация двухуровневая: готовый URL берётся из `vkState.mediaCache`, а
 * НЕЗАВЕРШЁННАЯ загрузка — из `local.loads`. Второй уровень важен не меньше
 * первого: без него два клика по OPEN дали бы две параллельные загрузки одного
 * файла (сервер бы их схлопнул, но браузер честно скачал бы дважды).
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @param {number} index — 0..9
 * @param {'thumb'|'full'} kind
 * @param {function(number, ?number): void} [onNetProgress] — принято/всего на
 *   «сетевой ноге» (сервер → браузер)
 * @returns {Promise<string>} objectURL
 */
export function loadMedia(dialogId, messageId, index, kind, onNetProgress) {
    const key = mediaKey(dialogId, messageId, index, kind);
    const cached = vkState.mediaCache.get(key);
    if (typeof cached === 'string') return Promise.resolve(cached);
    const inflight = local.loads.get(key);
    if (inflight) return inflight;

    const p = downloadBlobUrl(dialogId, messageId, index, kind, onNetProgress)
        .then((url) => {
            local.loads.delete(key);
            // Пока грузили, беседу могли переключить — тогда кэш уже очищен, и
            // класть URL обратно нельзя: он утечёт (revoke'ать его будет некому).
            if (String(vkState.activeDialogId) !== String(dialogId)) {
                try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
                return url;
            }
            vkState.mediaCache.set(key, url);
            return url;
        })
        .catch((e) => {
            local.loads.delete(key);
            throw e;
        });
    local.loads.set(key, p);
    return p;
}

/**
 * `fetch` → чтение тела с прогрессом → `Blob` → `objectURL`.
 *
 * Если поток недоступен (старый браузер, прокси без streaming), честно падаем
 * на `response.blob()` — тогда прогресса нет, но файл загрузится.
 */
async function downloadBlobUrl(dialogId, messageId, index, kind, onNetProgress) {
    const res = await fetchMedia(dialogId, messageId, index, kind);
    const type = res.headers.get('content-type') || 'application/octet-stream';
    const lenHeader = Number(res.headers.get('content-length'));
    const total = Number.isFinite(lenHeader) && lenHeader > 0 ? lenHeader : null;

    if (!res.body || typeof res.body.getReader !== 'function') {
        const blob = await res.blob();
        if (typeof onNetProgress === 'function') onNetProgress(blob.size, blob.size);
        return URL.createObjectURL(blob);
    }

    const reader = res.body.getReader();
    const chunks = [];
    let received = 0;
    if (typeof onNetProgress === 'function') onNetProgress(0, total);
    for (;;) {
        // eslint-disable-next-line no-await-in-loop
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        received += value.byteLength || value.length || 0;
        if (typeof onNetProgress === 'function') onNetProgress(received, total);
    }
    const blob = new Blob(chunks, { type });
    if (typeof onNetProgress === 'function') onNetProgress(blob.size, blob.size);
    return URL.createObjectURL(blob);
}

/**
 * Освобождает ВСЕ objectURL'ы вложений.
 *
 * Зовётся при смене беседы и на teardown. Без этого 20 переключений между
 * беседами с фотографиями оставляли бы в памяти вкладки все скачанные блобы:
 * браузер сам их не собирает, пока живёт URL. У VK цена ошибки выше, чем у
 * Telegram: одно сообщение способно принести десять блобов.
 *
 * @returns {number} сколько ссылок освобождено
 */
export function revokeDialogMedia() {
    let n = 0;
    for (const url of vkState.mediaCache.values()) {
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); n += 1; } catch (_) { /* уже отозван */ }
        }
    }
    vkState.mediaCache.clear();
    local.loads.clear();
    return n;
}

/**
 * Сбрасывает objectURL'ы одного сообщения (ВСЕ вложения, оба `kind`).
 *
 * Нужен на `message_edited`/`message_deleted`: сервер в этот момент
 * инвалидирует свой файловый кэш, и держать во вкладке старую картинку —
 * значит показывать пользователю то, чего в VK больше нет.
 *
 * Обходятся все индексы 0..MAX_ATTACH_INDEX, а не «сколько было вложений»:
 * правка могла ЗАМЕНИТЬ список, и старую длину спросить уже негде.
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @returns {number} сколько ссылок освобождено
 */
export function dropMessageMedia(dialogId, messageId) {
    let n = 0;
    for (let index = 0; index <= MAX_ATTACH_INDEX; index += 1) {
        for (const kind of ['thumb', 'full']) {
            const key = mediaKey(dialogId, messageId, index, kind);
            const url = vkState.mediaCache.get(key);
            if (typeof url === 'string' && url.startsWith('blob:')) {
                try { URL.revokeObjectURL(url); n += 1; } catch (_) { /* уже отозван */ }
            }
            vkState.mediaCache.delete(key);
            local.loads.delete(key);
        }
    }
    return n;
}

/** Забывает карточки (перед перерисовкой ленты). */
export function resetMediaCards() {
    if (local.observer) local.observer.disconnect();
    local.cards.clear();
    local.groups.clear();
    stopPlayer();
    // Незавершённый старт плеера относится к старым карточкам — забываем,
    // иначе клик по перерисованной карточке молча игнорировался бы.
    local.pendingPlayKey = null;
}

// ---------------------------------------------------------------------------
// Карточка
// ---------------------------------------------------------------------------

/** Кнопка карточки в терминальном стиле. */
function button(label, title) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'vk-media-btn';
    b.textContent = label;
    if (title) b.title = title;
    return b;
}

/** Ключ карточки в реестре. */
function cardKey(dialogId, messageId, index) {
    return `${dialogId}/${messageId}/${index}`;
}

/**
 * Собирает карточку ОДНОГО вложения.
 *
 * Диспетчеризация по `att.kind`: photo | video | document | sticker | graffiti
 * | voice | audio | other | unsupported. Незнакомый вид попадает в ветку
 * «other» (глиф + заголовок) — проглатывать вложение нельзя, даже если рисовать
 * нечего: пропавшая карточка читается как потеря данных.
 *
 * @param {object} msg — MessageDto
 * @param {object} att — элемент `normalizeAttachments(msg)`
 * @param {Array<object>} [group] — карточки того же сообщения (для галереи)
 * @returns {?HTMLElement} null, если вложение не разобралось
 */
export function buildMediaCard(msg, att, group) {
    if (!msg || !att) return null;

    const dialogId = String(msg.dialog_id);
    const messageId = Number(msg.id);
    const index = Number.isFinite(Number(att.index)) ? Number(att.index) : 0;
    const kind = String(att.kind || 'other');
    // Форматтеры работают с СЫРЫМ DTO (snake_case) — он же уехал бы в лог
    // сервера, поэтому лексика одна на обе стороны.
    const media = att.raw || {};
    const fileName = mediaFileName(msg, media);

    const card = document.createElement('div');
    card.className = `vk-media-card vk-media-${kind}`;
    card.dataset.dialogId = dialogId;
    card.dataset.msgId = String(messageId);
    card.dataset.index = String(index);
    card.dataset.mediaKind = kind;

    // --- head: глиф + имя файла + вид вложения
    const head = document.createElement('div');
    head.className = 'vk-media-head';
    const glyph = document.createElement('span');
    glyph.className = 'vk-media-glyph';
    glyph.textContent = mediaGlyph(kind);
    const name = document.createElement('span');
    name.className = 'vk-media-name';
    // У ссылки/опроса имени файла нет — там осмысленнее заголовок.
    const label = att.title && !att.fileName ? att.title : fileName;
    name.textContent = label;
    name.title = label;
    const kindLabel = document.createElement('span');
    kindLabel.className = 'vk-media-kind';
    kindLabel.textContent = mediaKindLabel(kind);
    head.appendChild(glyph);
    head.appendChild(name);
    head.appendChild(kindLabel);

    // --- body: превью / иконка / псевдо-волна
    const body = document.createElement('div');
    body.className = 'vk-media-body';

    const thumb = document.createElement('img');
    thumb.className = 'vk-media-thumb';
    thumb.alt = '';
    thumb.hidden = true;

    const icon = document.createElement('div');
    icon.className = 'vk-media-icon';
    icon.textContent = mediaGlyph(kind);

    body.appendChild(thumb);
    body.appendChild(icon);

    if (kind === 'voice' || kind === 'audio') {
        body.appendChild(buildWave(media, messageId, index));
        icon.hidden = true;
    }

    const play = document.createElement('span');
    play.className = 'vk-media-play';
    play.textContent = '▶';
    play.hidden = kind !== 'video';
    body.appendChild(play);

    const dur = durationLabel(att.durationSecs);
    const durEl = document.createElement('span');
    durEl.className = 'vk-media-duration';
    durEl.textContent = dur;
    durEl.hidden = !dur;
    body.appendChild(durEl);

    const playerSlot = document.createElement('div');
    playerSlot.className = 'vk-media-player';
    playerSlot.hidden = true;
    body.appendChild(playerSlot);

    // --- foot: метаданные + прогресс + статус + кнопки
    const foot = document.createElement('div');
    foot.className = 'vk-media-foot';
    const metaEl = document.createElement('span');
    metaEl.className = 'vk-media-meta';
    metaEl.textContent = mediaMetaLine(media) || formatBytes(att.size) || '—';
    const progress = document.createElement('span');
    progress.className = 'vk-media-progress';
    progress.hidden = true;
    const bar = document.createElement('span');
    bar.className = 'vk-media-progress-bar';
    progress.appendChild(bar);
    const status = document.createElement('span');
    status.className = 'vk-media-status';
    const player = safeUrl(att.playerUrl);
    // Адрес вложения `link`. `webUrl`, а не `safeUrl`: схему тут выбирал
    // СОБЕСЕДНИК, и `vk://` из чужого сообщения — не то, что стоит скармливать
    // установленному клиенту VK. Бэкенд уже отфильтровал (`attach::http_url`),
    // здесь второй рубеж.
    const link = webUrl(att.linkUrl);
    const domain = linkDomain(link);
    // Что умеет карточка: файл (OPEN/SAVE), страница плеера или ссылка
    // (только OPEN), либо ничего (опрос, товар — там нечего открывать).
    const hasFile = media.has_full === true;
    const external = player || link;
    const openBtn = button(
        'OPEN',
        (() => {
            if (hasFile) return 'Открыть вложение';
            if (player) return 'Открыть плеер VK во вкладке';
            if (link) return `Перейти на ${domain}`;
            return 'Открыть вложение';
        })()
    );
    const saveBtn = button('SAVE', 'Сохранить файл');
    if (!hasFile && !external) {
        openBtn.disabled = true;
        openBtn.title = 'у этого вложения нет файла';
    }
    if (!hasFile) {
        saveBtn.disabled = true;
        if (player) saveBtn.title = 'видео VK не отдаёт файлом — только страницей плеера';
        else if (link) saveBtn.title = 'это ссылка, а не файл';
        else saveBtn.title = 'у этого вложения нет файла';
    }
    // Домен ссылки — отдельным <a>, а не текстом в метаданных: пользователю
    // нужен и ответ «куда это ведёт», и сам переход, а кнопка OPEN рядом с
    // безымянным заголовком на этот вопрос не отвечает. Полный адрес НЕ
    // показывается — он ломает раскладку карточки (см. format.js::linkDomain).
    const linkEl = document.createElement('a');
    linkEl.className = 'vk-media-link';
    linkEl.hidden = !link;
    if (link) {
        // setAttribute, а не присваивание свойств: у `<a>` в DOM свойство href
        // нормализуется браузером (относительно base), и в разметке оказалось
        // бы не то, что проверяет ревьюер глазами. Плюс так значение читается
        // getAttribute одинаково везде.
        linkEl.setAttribute('href', link);
        linkEl.textContent = domain;
        linkEl.setAttribute('title', link);
        linkEl.setAttribute('target', '_blank');
        // Обязательны оба: `noopener` не даёт открытой странице дотянуться до
        // нашей вкладки через window.opener, `noreferrer` не отдаёт ей Referer
        // с адресом панели.
        linkEl.setAttribute('rel', 'noopener noreferrer');
    }
    foot.appendChild(linkEl);
    foot.appendChild(metaEl);
    foot.appendChild(progress);
    foot.appendChild(status);
    foot.appendChild(openBtn);
    foot.appendChild(saveBtn);

    card.appendChild(head);
    card.appendChild(body);
    card.appendChild(foot);

    const ctl = {
        el: card,
        dialogId,
        messageId,
        index,
        att,
        media,
        msg,
        fileName,
        kind,
        playerUrl: player,
        linkUrl: link,
        hasFile,
        group: Array.isArray(group) ? group : [],
        els: {
            thumb, icon, play, durEl, progress, bar, status, openBtn, saveBtn, playerSlot, body,
            link: linkEl,
        },
        /** Что делает OPEN — зависит от вида вложения. */
        open: () => openMedia(ctl),
        save: () => saveMedia(ctl),
    };

    openBtn.addEventListener('click', () => { if (!openBtn.disabled) ctl.open(); });
    saveBtn.addEventListener('click', () => { if (!saveBtn.disabled) ctl.save(); });
    body.addEventListener('click', (e) => {
        // Клики изнутри плеера (нативные controls: пауза, ползунок, громкость)
        // — НЕ действие карточки. Без этой проверки они всплывали до body,
        // playInCard пересоздавал элемент, и воспроизведение начиналось с нуля.
        if (isPlayerClick(e.target)) return;
        // Клик по превью = OPEN, но только когда там действительно превью или
        // плеер: по generic-иконке случайно попадают чаще, чем осознанно.
        if (!thumb.hidden || isPlayableKind(kind) || kind === 'video') ctl.open();
    });
    // Двойная защита: stopPropagation на слоте отсекает обработчики ВЫШЕ
    // карточки (и pointerdown/mousedown — иначе drag ползунка перемотки
    // дёргает чужие хендлеры), а isPlayerClick выше страхует сам обработчик
    // body на случай, если событие пришло минуя bubbling через слот.
    for (const type of ['click', 'pointerdown', 'mousedown']) {
        playerSlot.addEventListener(type, (e) => e.stopPropagation());
    }

    local.cards.set(cardKey(dialogId, messageId, index), ctl);
    if (Array.isArray(group)) group.push(ctl);
    if (canShowThumb(media)) {
        observeThumb(ctl);
    } else {
        // Нет превью — сразу честная иконка, без запроса, который вернёт 404.
        icon.hidden = kind === 'voice' || kind === 'audio';
    }
    return card;
}

/**
 * Псевдо-волна голосового.
 *
 * Если VK прислал огибающую (`waveform`), рисуем ЕЁ — это настоящая форма
 * записи. Иначе детерминированный шум по `(message_id, index)`, чтобы карточка
 * не «дышала» при каждой перерисовке ленты и чтобы два голосовых одного
 * сообщения выглядели по-разному.
 */
function buildWave(media, messageId, index) {
    const wave = document.createElement('div');
    wave.className = 'vk-media-wave';
    wave.setAttribute('aria-hidden', 'true');
    const real = Array.isArray(media.waveform) ? media.waveform : [];
    if (real.length) {
        const peak = real.reduce((acc, v) => Math.max(acc, Number(v) || 0), 0) || 1;
        const step = Math.max(1, Math.ceil(real.length / 40));
        for (let i = 0; i < real.length; i += step) {
            const bar = document.createElement('span');
            bar.style.height = `${8 + Math.round(((Number(real[i]) || 0) / peak) * 24)}px`;
            wave.appendChild(bar);
        }
        return wave;
    }
    let seed = ((messageId || 1) * 2654435761) ^ ((index + 1) * 40503);
    for (let i = 0; i < 40; i += 1) {
        seed = (seed ^ (seed << 13)) >>> 0;
        seed = (seed ^ (seed >>> 17)) >>> 0;
        const bar = document.createElement('span');
        bar.style.height = `${8 + (seed % 24)}px`;
        wave.appendChild(bar);
    }
    return wave;
}

/**
 * Контейнер с карточками ВСЕХ вложений сообщения.
 *
 * Верхняя граница — `MAX_INLINE_CARDS`: десять карточек (потолок VK)
 * превращают одно сообщение в экран, поэтому хвост сворачивается в «…ещё N».
 *
 * @param {object} msg — MessageDto
 * @returns {?HTMLElement} null, если вложений нет
 */
export function renderAttachmentGroup(msg) {
    const list = normalizeAttachments(msg);
    if (!list.length) return null;

    const box = document.createElement('div');
    box.className = 'vk-media-group';
    const group = [];
    const shown = list.slice(0, MAX_INLINE_CARDS);
    for (const att of shown) {
        const card = buildMediaCard(msg, att, group);
        if (card) box.appendChild(card);
    }
    const hidden = list.length - shown.length;
    if (hidden > 0) {
        const more = document.createElement('div');
        more.className = 'vk-media-more';
        more.textContent = `…ещё ${hidden}`;
        box.appendChild(more);
    }
    local.groups.set(`${msg.dialog_id}/${msg.id}`, group);
    return box;
}

/**
 * Ставит карточку под наблюдение: превью грузится, когда она подъезжает к
 * вьюпорту. Без IntersectionObserver (очень старый браузер) грузим сразу —
 * лучше лишние запросы, чем пустые карточки.
 */
function observeThumb(ctl) {
    if (typeof IntersectionObserver !== 'function') {
        ensureThumb(ctl);
        return;
    }
    if (!local.observer) {
        local.observer = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                if (!entry.isIntersecting) continue;
                const el = entry.target;
                local.observer.unobserve(el);
                const found = local.cards.get(
                    cardKey(el.dataset.dialogId, el.dataset.msgId, el.dataset.index)
                );
                if (found) ensureThumb(found);
            }
        }, { root: null, rootMargin: OBSERVER_MARGIN, threshold: 0.01 });
    }
    local.observer.observe(ctl.el);
}

/** Грузит превью один раз. */
function ensureThumb(ctl) {
    if (ctl.thumbRequested) return;
    ctl.thumbRequested = true;
    loadMedia(ctl.dialogId, ctl.messageId, ctl.index, 'thumb')
        .then((url) => showThumb(ctl, url))
        .catch((e) => {
            // Превью — не главная функция карточки: `no_media` значит «рисуй
            // иконку», всё остальное просто логируем и не пугаем пользователя
            // красной строкой (полный файл по-прежнему можно открыть).
            if (e && e.code !== 'no_media') {
                console.debug('[vk-media] thumb failed', ctl.dialogId, ctl.messageId, ctl.index, e);
            }
            ctl.els.icon.hidden = false;
        });
}

/**
 * Подставляет превью в карточку.
 *
 * О приросте высоты сообщается ИЗ СОБЫТИЯ `load`, а не отсюда. Здесь картинка
 * ещё не декодирована: `<img>` занимает по вертикали ноль, тело карточки стоит
 * на `min-height`, и доскролл в этот момент пришёлся бы на старую раскладку —
 * то есть ровно на ту, из-за которой дефект и существовал. К моменту `load`
 * размеры известны и высота уже пересчитана.
 *
 * Слушатель одноразовый (снимает сам себя): у одной карточки превью
 * подставляется единожды, а забытый слушатель держал бы в памяти контроллер
 * вместе со всей строкой ленты.
 */
function showThumb(ctl, url) {
    const img = ctl.els.thumb;
    if (img && typeof img.addEventListener === 'function') {
        const grown = () => {
            img.removeEventListener('load', grown);
            noteMediaGrowth();
        };
        img.addEventListener('load', grown);
    }
    img.src = url;
    img.hidden = false;
    ctl.els.icon.hidden = true;
}

// ---------------------------------------------------------------------------
// Прогресс
// ---------------------------------------------------------------------------

/** Показывает прогрессбар с подписью. `leg` — 'VK' или 'NET'. */
function setProgress(ctl, leg, received, total) {
    const pct = progressPercent(received, total);
    ctl.els.progress.hidden = false;
    ctl.els.bar.style.width = pct === null ? '100%' : `${pct}%`;
    ctl.els.progress.classList.toggle('vk-media-progress-indeterminate', pct === null);
    ctl.els.status.classList.remove('vk-media-done', 'vk-media-failed');
    ctl.els.status.textContent = `${leg} ${progressLabel(received, total)}`;
}

/** Финальное состояние: «✓ RECEIVED». */
function markReceived(ctl) {
    ctl.els.progress.hidden = true;
    ctl.els.bar.style.width = '100%';
    ctl.els.status.classList.remove('vk-media-failed');
    ctl.els.status.classList.add('vk-media-done');
    ctl.els.status.textContent = '✓ RECEIVED';
}

/**
 * Ошибка загрузки.
 *
 * Решение принимается по КОДУ контракта, а не по тексту: `no_media` — ретрай
 * бессмыслен (файла нет), `flood_wait` — показываем, сколько ждать, остальное —
 * кнопка повтора.
 */
function markError(ctl, err, retry) {
    ctl.els.progress.hidden = true;
    ctl.els.status.classList.remove('vk-media-done');
    ctl.els.status.classList.add('vk-media-failed');
    const code = err && err.code;
    if (code === 'no_media') {
        ctl.els.status.textContent = '>> NO MEDIA';
        return;
    }
    if (code === 'flood_wait') {
        const secs = (err && err.retryAfterSecs) || 0;
        ctl.els.status.textContent = secs ? `>> WAIT ${secs}s` : '>> WAIT';
    } else {
        ctl.els.status.textContent = '>> ERR';
    }
    if (typeof retry !== 'function') return;
    const again = button('RETRY', 'Повторить загрузку');
    again.classList.add('vk-media-retry');
    again.addEventListener('click', () => {
        again.remove();
        ctl.els.status.textContent = '';
        ctl.els.status.classList.remove('vk-media-failed');
        retry();
    });
    ctl.els.status.after(again);
}

/**
 * WS `download_progress` — «сервер тянет файл из VK».
 *
 * Карточка ищется по `(dialog_id, message_id, index)`. ИНДЕКС ОБЯЗАТЕЛЕН: без
 * него прогресс второго фото сообщения прилетал бы в карточку первого.
 * События о медиа, которого нет на экране (другая беседа, страница истории
 * выше), молча игнорируются.
 *
 * @param {{dialog_id: string, message_id: number, index: number, kind: string,
 *   received: number, total: ?number}} evt
 */
export function onDownloadProgress(evt) {
    if (!evt || !Number.isFinite(evt.message_id)) return;
    const index = Number.isFinite(evt.index) ? evt.index : 0;
    const ctl = local.cards.get(cardKey(evt.dialog_id, evt.message_id, index));
    if (!ctl) return;
    // Превью грузятся мгновенно и не должны мигать прогрессбаром на всю
    // карточку: показываем только прогресс полного файла.
    if (evt.kind !== 'full') return;
    const total = Number.isFinite(evt.total) ? evt.total : null;
    if (Number.isFinite(total) && evt.received >= total && total > 0) {
        // VK отдал всё; впереди ещё сетевая нога, поэтому не «RECEIVED».
        setProgress(ctl, 'VK', total, total);
        return;
    }
    setProgress(ctl, 'VK', evt.received, total);
}

// ---------------------------------------------------------------------------
// Открытие, сохранение, воспроизведение
// ---------------------------------------------------------------------------

/**
 * Открывает внешнюю ссылку в новой вкладке.
 *
 * Единственный адрес, который сюда попадает, — страница плеера VK
 * (`player_url`). Файлы вложений так не открываются НИКОГДА: они идут через
 * прокси бэкенда и `blob:`.
 *
 * @param {*} rawUrl
 * @returns {boolean} удалось ли открыть
 */
export function openExternal(rawUrl) {
    const url = safeUrl(rawUrl);
    if (!url) return false;
    // noopener,noreferrer обязательны: открытая страница не должна получить ни
    // ссылку на нашу вкладку (window.opener), ни Referer с токеном в пути.
    const w = window.open(url, '_blank', 'noopener,noreferrer');
    if (w) {
        try { w.opener = null; } catch (_) { /* браузер уже обнулил сам */ }
    }
    return true;
}

/**
 * OPEN: смысл зависит от вида вложения.
 *
 * - фото/стикер/граффити → лайтбокс (с галереей по вложениям сообщения);
 * - видео → страница плеера VK во внешней вкладке (файла у нас нет и не будет);
 * - голосовое/аудио → плеер внутри карточки;
 * - ссылка → адрес собеседника во внешней вкладке (файла у неё не бывает);
 * - документ/прочее → blob в новой вкладке (браузер сам решит, показать или
 *   предложить сохранить); заблокировали popup — падаем на SAVE.
 */
export function openMedia(ctl) {
    if (ctl.kind === 'video') {
        // ↓ ЧЕСТНОЕ ОГРАНИЧЕНИЕ: вложение video у VK не содержит файла.
        if (!openExternal(ctl.playerUrl)) {
            markError(ctl, { code: 'no_media' });
        }
        return;
    }
    if (isImageKind(ctl.kind)) {
        openLightbox(ctl);
        return;
    }
    if (isPlayableKind(ctl.kind)) {
        playInCard(ctl);
        return;
    }
    // Ссылка: файла нет и не будет, зато есть адрес, ради которого её и
    // прислали. Проверка стоит ПОСЛЕ файловых видов — у документа с превью
    // `linkUrl` не бывает, но порядок не должен зависеть от этого допущения.
    if (!ctl.hasFile && ctl.linkUrl) {
        if (!openExternal(ctl.linkUrl)) markError(ctl, { code: 'no_media' });
        return;
    }
    if (!ctl.hasFile) {
        markError(ctl, { code: 'no_media' });
        return;
    }
    withFull(ctl, (url) => {
        const w = window.open(url, '_blank', 'noopener');
        if (!w) saveMedia(ctl);
    });
}

/**
 * Грузит полный файл с прогрессом и передаёт URL в колбэк.
 *
 * Единственная точка запуска «тяжёлой» загрузки: и лайтбокс, и плеер, и SAVE
 * ходят через неё, поэтому прогресс и обработка ошибок везде одинаковы.
 *
 * `onFail` нужен вызывающим, у которых есть СВОЙ индикатор: лайтбокс показывает
 * «loading…» в подписи, и без этого колбэка он остался бы «загружающимся»
 * навсегда, хотя загрузка уже провалилась.
 */
function withFull(ctl, done, onFail) {
    setProgress(ctl, 'NET', 0, ctl.att && ctl.att.size);
    loadMedia(ctl.dialogId, ctl.messageId, ctl.index, 'full', (received, total) => {
        setProgress(ctl, 'NET', received, total);
    })
        .then((url) => {
            markReceived(ctl);
            done(url);
        })
        .catch((e) => {
            markError(ctl, e, () => withFull(ctl, done, onFail));
            if (typeof onFail === 'function') onFail(e);
        });
}

/**
 * SAVE: полный файл → `<a download>` → click.
 *
 * objectURL НЕ revoke'ается: он лежит в `vkState.mediaCache` и будет
 * переиспользован (например, лайтбоксом), а освободится при смене беседы.
 * Имя санитизируется — `download` браузеры трактуют как подсказку пути.
 */
export function saveMedia(ctl) {
    if (!ctl.hasFile) {
        markError(ctl, { code: 'no_media' });
        return;
    }
    withFull(ctl, (url) => {
        const a = document.createElement('a');
        a.href = url;
        // САНИТИЗАЦИЯ обязательна: имя пришло из VK, а `download` браузеры
        // трактуют как подсказку пути («../../.bashrc» недопустим).
        a.download = sanitizeDownloadName(ctl.fileName);
        a.rel = 'noopener';
        document.body.appendChild(a);
        a.click();
        a.remove();
    });
}

/** Останавливает и снимает активный плеер, возвращая карточку в исходный вид. */
export function stopPlayer() {
    if (!local.player) return;
    try {
        local.player.el.pause();
    } catch (_) { /* элемент мог уже уехать из DOM */ }
    // textContent = '' снимает разом и сам элемент, и кнопку ✕.
    local.player.slot.textContent = '';
    local.player.slot.hidden = true;
    if (local.player.card) local.player.card.classList.remove('vk-media-playing');
    if (local.player.restore) local.player.restore();
    local.player = null;
}

/**
 * Клик пришёл изнутри плеера (нативные controls)?
 *
 * Shadow DOM браузерных controls ретаргетит события на сам `<audio>`, поэтому
 * `closest('.vk-media-player')` надёжно ловит любой клик по плееру, включая
 * перемотку и паузу.
 *
 * @param {?EventTarget} target — `e.target` клика
 * @returns {boolean}
 */
export function isPlayerClick(target) {
    return !!(target && typeof target.closest === 'function'
        && target.closest('.vk-media-player'));
}

/**
 * Решает, что делать при клике, запускающем воспроизведение в карточке.
 *
 * Чистая функция — единственная точка истины для «пересоздавать ли плеер»:
 * - `ignore` — клик из controls активного плеера, карточка его не трогает;
 * - `toggle` — плеер этой карточки уже открыт: play/pause без пересоздания,
 *   позиция сохраняется;
 * - `start`  — плеера нет или он на другой карточке: создать новый (старый
 *   гасится — инвариант «играет ровно один»).
 *
 * @param {string} key — `<dialog>/<message>/<index>` карточки
 * @param {?string} activeKey — ключ карточки активного плеера или null
 * @param {boolean} fromPlayer — клик пришёл изнутри плеера
 * @returns {'ignore'|'toggle'|'start'}
 */
export function resolveCardClick(key, activeKey, fromPlayer) {
    if (fromPlayer) return 'ignore';
    if (activeKey != null && String(activeKey) === String(key)) return 'toggle';
    return 'start';
}

/** play/pause существующего элемента: позиция воспроизведения сохраняется. */
function togglePlayback(el) {
    if (el.paused) {
        const played = el.play();
        if (played && typeof played.catch === 'function') played.catch(() => {});
    } else {
        el.pause();
    }
}

/**
 * Esc останавливает активный плеер (навешивается лениво, один раз).
 *
 * Лайтбокс перекрывает панель и обрабатывает Esc сам (см. initMedia) — при
 * открытом лайтбоксе не вмешиваемся, чтобы одно нажатие не закрывало оба.
 */
function ensurePlayerEsc() {
    if (local.playerEscBound) return;
    local.playerEscBound = true;
    document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape' || !local.player) return;
        if ($vkLightbox && !$vkLightbox.hidden) return;
        e.preventDefault();
        stopPlayer();
    });
}

/**
 * Плеер прямо в карточке (голосовое и аудиозапись).
 *
 * Одновременно играет РОВНО один: старт на ДРУГОЙ карточке гасит предыдущий
 * (иначе два голосовых из одного сообщения перекрикивают друг друга —
 * у VK это штатная ситуация, а не экзотика). Повторный вызов для ТОЙ ЖЕ
 * карточки НЕ пересоздаёт элемент: переключает play/pause, сохраняя позицию.
 */
export function playInCard(ctl) {
    const key = cardKey(ctl.dialogId, ctl.messageId, ctl.index);
    const action = resolveCardClick(key, local.player ? local.player.key : null, false);
    if (action === 'toggle') {
        togglePlayback(local.player.el);
        return;
    }
    // Полный файл уже грузится для этой карточки — второй клик не должен
    // порождать второй плеер (loadMedia схлопнет запрос, но callback'и
    // сработали бы дважды).
    if (local.pendingPlayKey === key) return;
    stopPlayer();
    local.pendingPlayKey = key;
    withFull(ctl, (url) => {
        if (local.pendingPlayKey === key) local.pendingPlayKey = null;
        stopPlayer(); // пока грузили, плеер могли запустить на другой карточке
        const el = document.createElement('audio');
        el.controls = true;
        // preload=metadata: длительность известна сразу, ползунок перемотки
        // активен с первого кадра (для blob-URL это бесплатно — данные уже
        // в памяти вкладки).
        el.preload = 'metadata';
        el.src = url;
        // Класса плееру НЕ даём: `.vk-media-audio`/`.vk-media-voice` — это
        // классы самой карточки (`vk-media-${kind}`), и стилизовать плеер по
        // ним значит попасть правилом в карточку. CSS цепляется за вложенность
        // (`.vk-media-player > audio`).
        const hidden = [];
        for (const node of [ctl.els.thumb, ctl.els.icon, ctl.els.play, ctl.els.durEl]) {
            if (node && !node.hidden) {
                node.hidden = true;
                hidden.push(node);
            }
        }
        const wave = ctl.els.body.querySelector('.vk-media-wave');
        if (wave && !wave.hidden) {
            wave.hidden = true;
            hidden.push(wave);
        }
        // Кнопка ✕ — явный способ остановить воспроизведение и вернуть
        // карточку в исходный вид (плюс Esc, см. ensurePlayerEsc).
        const close = button('✕', 'Остановить и закрыть плеер');
        close.classList.add('vk-media-player-close');
        close.addEventListener('click', () => stopPlayer());
        ctl.els.playerSlot.textContent = '';
        ctl.els.playerSlot.appendChild(el);
        ctl.els.playerSlot.appendChild(close);
        ctl.els.playerSlot.hidden = false;
        // Пока открыт плеер, карточке нельзя резать высоту тела (см. CSS
        // `.vk-media-playing`).
        ctl.el.classList.add('vk-media-playing');
        local.player = {
            key,
            el,
            slot: ctl.els.playerSlot,
            card: ctl.el,
            restore: () => hidden.forEach((n) => { n.hidden = false; }),
        };
        ensurePlayerEsc();
        const played = el.play();
        if (played && typeof played.catch === 'function') {
            // Автовоспроизведение может быть запрещено политикой браузера —
            // это не ошибка: у элемента есть controls, пользователь нажмёт сам.
            played.catch(() => {});
        }
    }, () => {
        // Ошибка загрузки: снять «pending», иначе карточка навсегда глохнет
        // к кликам (RETRY идёт через markError и снова позовёт playInCard).
        if (local.pendingPlayKey === key) local.pendingPlayKey = null;
    });
}

// ---------------------------------------------------------------------------
// Лайтбокс-галерея
// ---------------------------------------------------------------------------

/** Навешивает обработчики лайтбокса (идемпотентно). */
export function initMedia() {
    if (local.lightboxBound || !$vkLightbox) return;
    local.lightboxBound = true;
    if ($vkLightboxClose) {
        $vkLightboxClose.addEventListener('click', () => closeLightbox());
    }
    if ($vkLightboxPrev) {
        $vkLightboxPrev.addEventListener('click', (e) => {
            e.stopPropagation();
            stepLightbox(-1);
        });
    }
    if ($vkLightboxNext) {
        $vkLightboxNext.addEventListener('click', (e) => {
            e.stopPropagation();
            stepLightbox(1);
        });
    }
    $vkLightbox.addEventListener('click', (e) => {
        // Клик по фону закрывает; по самой картинке и по стрелкам — нет.
        if (e.target === $vkLightbox) closeLightbox();
    });
    // keydown на документе: лайтбокс перекрывает панель целиком, и ловить Esc
    // только на нём означало бы «работает, лишь пока фокус внутри».
    document.addEventListener('keydown', (e) => {
        if ($vkLightbox.hidden) return;
        if (e.key === 'Escape') {
            e.preventDefault();
            closeLightbox();
            return;
        }
        if (e.key === 'ArrowLeft') {
            e.preventDefault();
            stepLightbox(-1);
            return;
        }
        if (e.key === 'ArrowRight') {
            e.preventDefault();
            stepLightbox(1);
            return;
        }
        if (e.key === 'Tab') trapFocus(e);
    });
}

/** Держит фокус внутри лайтбокса (доступность). */
function trapFocus(e) {
    const focusables = [$vkLightboxClose, $vkLightboxPrev, $vkLightboxNext, $vkLightboxImg]
        .filter((n) => n && !n.hidden);
    if (!focusables.length) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
    }
}

/**
 * Картиночные вложения ОДНОГО сообщения — это и есть галерея.
 *
 * Ходить по всей ленте нельзя: «следующее фото» означает следующее в ЭТОМ
 * сообщении, а не в переписке. Непросматриваемые виды (документ, голосовое)
 * из галереи выпадают — листать их лайтбоксом нечем.
 *
 * @param {object} ctl — контроллер карточки
 * @returns {Array<object>} контроллеры, включая сам `ctl`
 */
export function galleryOf(ctl) {
    const group = Array.isArray(ctl.group) && ctl.group.length ? ctl.group : [ctl];
    const images = group.filter((c) => isImageKind(c.kind));
    return images.length ? images : [ctl];
}

/**
 * Открывает полноразмерное изображение поверх панели.
 *
 * Пока грузится full — в подписи «loading…», картинка остаётся пустой:
 * подставлять превью растянутым было бы обманом («уже загрузилось»).
 *
 * @param {object} ctl — контроллер карточки
 */
export function openLightbox(ctl) {
    if (!$vkLightbox || !$vkLightboxImg) return;
    initMedia();
    const gallery = galleryOf(ctl);
    const pos = Math.max(0, gallery.indexOf(ctl));
    local.lightbox.gallery = gallery;
    local.lightbox.lastFocus = document.activeElement;
    $vkLightbox.hidden = false;
    if ($vkLightboxClose) $vkLightboxClose.focus();
    showAt(pos);
}

/**
 * Сдвигает галерею на `delta` позиций.
 *
 * Границы НЕ зациклены намеренно: «следующее» за последним фото — это ничего,
 * а прыжок в начало читается как сбой. Кнопки на границах отключаются
 * (`disabled`), поэтому мышью в упор не упрёшься вовсе.
 *
 * @param {number} delta
 * @returns {boolean} сместились ли
 */
export function stepLightbox(delta) {
    const g = local.lightbox.gallery;
    if (!Array.isArray(g) || g.length < 2) return false;
    const next = local.lightbox.pos + (delta < 0 ? -1 : 1);
    if (next < 0 || next >= g.length) return false;
    showAt(next);
    return true;
}

/** Показывает вложение галереи по позиции. */
function showAt(pos) {
    const g = local.lightbox.gallery;
    const ctl = g[pos];
    if (!ctl) return;
    local.lightbox.pos = pos;
    local.lightbox.ctl = ctl;
    local.lightbox.url = null;
    if ($vkLightboxImg) $vkLightboxImg.removeAttribute('src');
    const counter = g.length > 1 ? ` · ${pos + 1}/${g.length}` : '';
    if ($vkLightboxCaption) {
        $vkLightboxCaption.textContent = `${ctl.fileName}${counter} · loading…`;
    }
    // Стрелки видны только когда есть куда идти: одинокая картинка не должна
    // предлагать листать саму себя.
    if ($vkLightboxPrev) {
        $vkLightboxPrev.hidden = g.length < 2;
        $vkLightboxPrev.disabled = pos === 0;
    }
    if ($vkLightboxNext) {
        $vkLightboxNext.hidden = g.length < 2;
        $vkLightboxNext.disabled = pos === g.length - 1;
    }

    withFull(
        ctl,
        (url) => {
            // Пока грузили, лайтбокс могли закрыть или пролистать дальше.
            if (local.lightbox.ctl !== ctl || $vkLightbox.hidden) return;
            local.lightbox.url = url;
            $vkLightboxImg.src = url;
            $vkLightboxImg.alt = ctl.fileName;
            if ($vkLightboxCaption) {
                const meta = mediaMetaLine(ctl.media);
                $vkLightboxCaption.textContent = meta
                    ? `${ctl.fileName}${counter} · ${meta}`
                    : `${ctl.fileName}${counter}`;
            }
        },
        (err) => {
            if (local.lightbox.ctl !== ctl || !$vkLightboxCaption) return;
            const reason = err && err.code === 'no_media'
                ? '>> NO MEDIA'
                : `>> ${(err && err.message) || 'ERR'}`;
            $vkLightboxCaption.textContent = `${ctl.fileName}${counter} · ${reason}`;
        }
    );
}

/** Закрывает лайтбокс и возвращает фокус на карточку. */
export function closeLightbox() {
    if (!$vkLightbox) return;
    $vkLightbox.hidden = true;
    // Узла картинки может не быть у пользователя со старым index.html в кэше
    // PWA: панель обязана продолжать работать, а не падать на teardown.
    if ($vkLightboxImg) $vkLightboxImg.removeAttribute('src');
    if ($vkLightboxCaption) $vkLightboxCaption.textContent = '';
    if ($vkLightboxPrev) $vkLightboxPrev.hidden = true;
    if ($vkLightboxNext) $vkLightboxNext.hidden = true;
    const back = local.lightbox.lastFocus;
    local.lightbox = { ctl: null, gallery: [], pos: 0, lastFocus: null, url: null };
    if (back && typeof back.focus === 'function') {
        try { back.focus(); } catch (_) { /* элемент уехал из DOM */ }
    }
}

/** Полная очистка (logout, teardown вкладки). */
export function teardownMedia() {
    closeLightbox();
    resetMediaCards();
    revokeDialogMedia();
}

/** Снимок состояния для e2e/диагностики. */
export function _debugMediaState() {
    return {
        cards: local.cards.size,
        groups: local.groups.size,
        loading: local.loads.size,
        cached: vkState.mediaCache.size,
        lightboxOpen: !!($vkLightbox && !$vkLightbox.hidden),
        lightboxPos: local.lightbox.pos,
        gallery: local.lightbox.gallery.length,
        player: local.player ? local.player.el.tagName.toLowerCase() : null,
        playerKey: local.player ? local.player.key : null,
    };
}
