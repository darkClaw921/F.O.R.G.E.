// tmux-web — Telegram plugin: медиа-карточки, загрузка, лайтбокс, плеер.
//
// Карточка (360px, дизайн hablo-taste):
//
//   ┌─ ▣ photo_42.jpg ───────────────────── PHOTO ─┐
//   │ [ превью 184px ]                    ▶  0:07  │
//   │ 1280×720 · 1.2 MB  [====----]  49%  OPEN SAVE│
//   └──────────────────────────────────────────────┘
//
// Ключевые решения:
//
// * КРИТИЧНО — загрузка только через `fetch` → `blob` → `objectURL`.
//   Атрибут `src` у `<img>`/`<video>` НЕ несёт заголовков, а в remote-mode
//   `/api/*` требует `Authorization: Bearer`. Прямой `src="/api/telegram/…"`
//   работал бы локально и молча ломался у пользователя с токеном.
// * Превью грузятся ЛЕНИВО (IntersectionObserver): в истории на сотню медиа
//   иначе улетела бы сотня запросов при первом же рендере. Обратная сторона
//   ленивости — КАРТОЧКА РАСТЁТ ЗАДНИМ ЧИСЛОМ: до декодирования картинки
//   `<img>` занимает по вертикали ноль, тело держится на `min-height: 92px`, а
//   с превью доходит до `max-height: 184px` (THUMB_HEIGHT). Лента к этому
//   моменту уже прокручена, поэтому о каждом таком приросте модуль СООБЩАЕТ
//   наружу (`setMediaGrowthHandler`) — иначе низ переписки уезжает под кромку
//   ровно на сумму догруженных превью (см. шапку js/telegram/chat.js,
//   «прижатие к низу»).
// * Полный файл — ТОЛЬКО по явному действию (OPEN/SAVE/клик по превью).
//   Никаких автозагрузок мегабайтов.
// * objectURL'ы живут в `tgState.mediaCache` и revoke'аются при смене
//   диалога и на teardown: без этого каждый просмотренный файл оставался бы
//   в памяти вкладки до перезагрузки страницы.
// * Всё содержимое — `textContent`/`createElement`. Имена файлов приходят из
//   Telegram, то есть от чужого аккаунта; `innerHTML` здесь означал бы XSS.
// * Прогресс двухногий: `download_progress` из WS — это «сервер тянет из
//   Telegram» (TG), чтение `response.body` — «сервер отдаёт браузеру» (NET).
//   Обе ноги пишут в один и тот же прогрессбар, но с разной подписью, иначе
//   индикатор дважды бежал бы от 0 до 100 без объяснений.
// * Плеер НЕ пересоздаётся при повторном клике по той же карточке: клик
//   переключает play/pause с сохранением позиции. Клики по нативным controls
//   (пауза, ползунок) не считаются кликом по карточке — иначе каждая
//   перемотка перезапускала бы воспроизведение с нуля (см. resolveCardClick).

import { $tgLightbox, $tgLightboxCaption, $tgLightboxClose, $tgLightboxImg } from '../core/dom.js';

import { buildWave, noiseSeed } from '../messenger/waveform-core.js';
import { createVoiceTranscript } from '../messenger/voice-transcript-core.js';
import { fetchMedia, getVoiceTranscript, transcribeVoice } from './api.js';
import {
    canShowThumb, durationLabel, formatBytes, isImageKind, isPlayableKind, mediaFileName,
    mediaGlyph, mediaKey, mediaKindLabel, mediaMetaLine, progressLabel, progressPercent,
    sanitizeDownloadName,
} from './format.js';
import { tgState } from './state.js';

/** Высота превью в карточке (синхронно с CSS `.tg-media-thumb`). */
export const THUMB_HEIGHT = 184;

/** Запас в пикселях, при котором превью считается «почти видимым». */
const OBSERVER_MARGIN = '200px';

/**
 * Кого известить, что карточка выросла в высоту.
 *
 * Модульная переменная, а не параметр `buildMediaCard`: рост случается НЕ в
 * момент построения карточки, а спустя сотни миллисекунд — когда браузер
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
        console.debug('[tg-media] обработчик роста упал', e);
    }
}

const local = {
    /** `<dialog_id>/<message_id>` → контроллер карточки. */
    cards: new Map(),
    /** ключ медиа → Promise<objectURL> незавершённой загрузки. */
    loads: new Map(),
    /** IntersectionObserver ленивых превью (создаётся при первой карточке). */
    observer: null,
    /** Активный плеер: старт нового останавливает предыдущий. */
    player: null,
    /** Ключ карточки (`dialog/msg`), для которой полный файл плеера ещё грузится. */
    pendingPlayKey: null,
    /** Esc-обработчик остановки плеера навешен (лениво, один раз). */
    playerEscBound: false,
    /** Обработчики лайтбокса навешены. */
    lightboxBound: false,
    /** Состояние лайтбокса: контроллер карточки + куда вернуть фокус. */
    lightbox: { ctl: null, lastFocus: null, url: null },
};

// ---------------------------------------------------------------------------
// Загрузка
// ---------------------------------------------------------------------------

/**
 * Грузит медиа и возвращает objectURL.
 *
 * Дедупликация двухуровневая: готовый URL берётся из `tgState.mediaCache`, а
 * НЕЗАВЕРШЁННАЯ загрузка — из `local.loads`. Второй уровень важен не меньше
 * первого: без него два клика по OPEN дали бы две параллельные загрузки
 * одного файла (сервер бы их схлопнул, но браузер честно скачал бы дважды).
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @param {'thumb'|'full'} kind
 * @param {function(number, ?number): void} [onNetProgress] — принято/всего
 *   на «сетевой ноге» (сервер → браузер)
 * @returns {Promise<string>} objectURL
 */
export function loadMedia(dialogId, messageId, kind, onNetProgress) {
    const key = mediaKey(dialogId, messageId, kind);
    const cached = tgState.mediaCache.get(key);
    if (typeof cached === 'string') return Promise.resolve(cached);
    const inflight = local.loads.get(key);
    if (inflight) return inflight;

    const p = downloadBlobUrl(dialogId, messageId, kind, onNetProgress)
        .then((url) => {
            local.loads.delete(key);
            // Пока грузили, диалог могли переключить — тогда кэш уже очищен, и
            // класть URL обратно нельзя: он утечёт (revoke'ать его будет некому).
            if (String(tgState.activeDialogId) !== String(dialogId)) {
                try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
                return url;
            }
            tgState.mediaCache.set(key, url);
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
 * Если поток недоступен (старый браузер, прокси без streaming), честно
 * падаем на `response.blob()` — тогда прогресса нет, но файл загрузится.
 */
async function downloadBlobUrl(dialogId, messageId, kind, onNetProgress) {
    const res = await fetchMedia(dialogId, messageId, kind);
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
 * Освобождает ВСЕ objectURL'ы медиа.
 *
 * Зовётся при смене диалога и на teardown. Без этого 20 переключений между
 * диалогами с фотографиями оставляли бы в памяти вкладки все скачанные
 * блобы: браузер сам их не собирает, пока живёт URL.
 *
 * @returns {number} сколько ссылок освобождено
 */
export function revokeDialogMedia() {
    let n = 0;
    for (const url of tgState.mediaCache.values()) {
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); n += 1; } catch (_) { /* уже отозван */ }
        }
    }
    tgState.mediaCache.clear();
    local.loads.clear();
    return n;
}

/**
 * Сбрасывает objectURL'ы одного сообщения (оба `kind`).
 *
 * Нужен на `message_edited`/`message_deleted`: сервер в этот момент
 * инвалидирует свой файловый кэш, и держать во вкладке старую картинку —
 * значит показывать пользователю то, чего в Telegram больше нет.
 *
 * @param {string} dialogId
 * @param {number} messageId
 */
export function dropMessageMedia(dialogId, messageId) {
    for (const kind of ['thumb', 'full']) {
        const key = mediaKey(dialogId, messageId, kind);
        const url = tgState.mediaCache.get(key);
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
        }
        tgState.mediaCache.delete(key);
        local.loads.delete(key);
    }
}

/** Забывает карточки (перед перерисовкой ленты). */
export function resetMediaCards() {
    if (local.observer) local.observer.disconnect();
    local.cards.clear();
    stopPlayer();
    // Незавершённый старт плеера относится к старым карточкам — забываем,
    // иначе клик по перерисованной карточке молча игнорировался бы.
    local.pendingPlayKey = null;
}

// ---------------------------------------------------------------------------
// Карточка
// ---------------------------------------------------------------------------

/**
 * Имя DOM-события, которым панель отдаёт кадры расшифровки наружу.
 *
 * Объявлено ЗДЕСЬ, а не в `main.js`, хотя рассылает кадры именно он: карточку
 * рисует этот файл, а `main.js` импортирует его — обратный импорт замкнул бы
 * цикл. `main.js` берёт имя отсюда и переэкспортирует.
 */
export const VOICE_FRAME_EVENT = 'telegram:voice-transcribe';

/**
 * Текст расшифровки под голосовым — ОБЩИЙ модуль на три панели.
 *
 * Панель отдаёт сюда только транспорт, имя события с кадрами и префикс
 * классов. У Telegram вложение в сообщении ОДНО, поэтому индекс всегда 0 —
 * ровно как в соли заглушки волны.
 */
export const voiceTranscript = createVoiceTranscript({
    plugin: 'telegram',
    classPrefix: 'tg-voice',
    frameEvent: VOICE_FRAME_EVENT,
    api: { transcribe: transcribeVoice, transcript: getVoiceTranscript },
});

/** Кнопка карточки в терминальном стиле. */
function button(label, title) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'tg-media-btn';
    b.textContent = label;
    if (title) b.title = title;
    return b;
}

/**
 * Собирает карточку медиа для строки лога.
 *
 * Диспетчеризация по `media.kind`: photo | video | document | sticker | voice
 * | audio | other. Незнакомый вид попадает в ветку `other` (глиф + размер) —
 * проглатывать сообщение нельзя, даже если рисовать нечего.
 *
 * @param {object} msg — MessageDto с непустым `media`
 * @returns {?HTMLElement} null, если у сообщения нет медиа
 */
export function buildMediaCard(msg) {
    const media = msg && msg.media;
    if (!media) return null;

    const dialogId = String(msg.dialog_id);
    const messageId = Number(msg.id);
    const kind = String(media.kind || 'other');
    const fileName = mediaFileName(msg);

    const card = document.createElement('div');
    card.className = `tg-media-card tg-media-${kind}`;
    card.dataset.dialogId = dialogId;
    card.dataset.msgId = String(messageId);
    card.dataset.mediaKind = kind;

    // --- head: глиф + имя файла + вид вложения
    const head = document.createElement('div');
    head.className = 'tg-media-head';
    const glyph = document.createElement('span');
    glyph.className = 'tg-media-glyph';
    glyph.textContent = mediaGlyph(kind);
    const name = document.createElement('span');
    name.className = 'tg-media-name';
    name.textContent = fileName;
    name.title = fileName;
    const kindLabel = document.createElement('span');
    kindLabel.className = 'tg-media-kind';
    kindLabel.textContent = mediaKindLabel(kind);
    head.appendChild(glyph);
    head.appendChild(name);
    head.appendChild(kindLabel);

    // --- body: превью / иконка / псевдо-волна
    const body = document.createElement('div');
    body.className = 'tg-media-body';

    const thumb = document.createElement('img');
    thumb.className = 'tg-media-thumb';
    thumb.alt = '';
    thumb.hidden = true;

    const icon = document.createElement('div');
    icon.className = 'tg-media-icon';
    icon.textContent = mediaGlyph(kind);

    body.appendChild(thumb);
    body.appendChild(icon);

    if (kind === 'voice' || kind === 'audio') {
        // Звуковая дорожка. У голосовых огибающая приезжает от Telegram
        // (`DocumentAttributeAudio.waveform`), у обычных аудиофайлов её считает
        // бэкенд по скачанному файлу; пока её нет — детерминированная по
        // message_id заглушка, чтобы карточка не «дышала» при перерисовке
        // ленты. Рисовальщик общий на три панели.
        body.appendChild(buildWave({
            waveform: media.waveform,
            seed: noiseSeed(messageId),
            className: 'tg-media-wave',
        }));
        icon.hidden = true;
    }

    const play = document.createElement('span');
    play.className = 'tg-media-play';
    play.textContent = '▶';
    play.hidden = !(kind === 'video');
    body.appendChild(play);

    const dur = durationLabel(media.duration_secs);
    const durEl = document.createElement('span');
    durEl.className = 'tg-media-duration';
    durEl.textContent = dur;
    durEl.hidden = !dur;
    body.appendChild(durEl);

    const playerSlot = document.createElement('div');
    playerSlot.className = 'tg-media-player';
    playerSlot.hidden = true;
    body.appendChild(playerSlot);

    // --- foot: метаданные + прогресс + статус + кнопки
    const foot = document.createElement('div');
    foot.className = 'tg-media-foot';
    const metaEl = document.createElement('span');
    metaEl.className = 'tg-media-meta';
    metaEl.textContent = mediaMetaLine(media) || formatBytes(media.size) || '—';
    const progress = document.createElement('span');
    progress.className = 'tg-media-progress';
    progress.hidden = true;
    const bar = document.createElement('span');
    bar.className = 'tg-media-progress-bar';
    progress.appendChild(bar);
    const status = document.createElement('span');
    status.className = 'tg-media-status';
    const openBtn = button('OPEN', 'Открыть вложение');
    const saveBtn = button('SAVE', 'Сохранить файл');
    foot.appendChild(metaEl);
    foot.appendChild(progress);
    foot.appendChild(status);
    foot.appendChild(openBtn);
    foot.appendChild(saveBtn);

    card.appendChild(head);
    card.appendChild(body);
    card.appendChild(foot);

    // Блок расшифровки — ПОД подвалом карточки и только у звука. `null` для
    // прочих видов возвращает сам общий модуль; обычное аудио блок получает —
    // это то же решение, что у авторазбора (Ф4).
    const transcript = voiceTranscript.attach({ dialogId, messageId, index: 0, kind });
    if (transcript) card.appendChild(transcript);

    const ctl = {
        el: card,
        dialogId,
        messageId,
        media,
        msg,
        fileName,
        kind,
        els: { thumb, icon, play, durEl, progress, bar, status, openBtn, saveBtn, playerSlot, body },
        /** Что делает OPEN — зависит от вида вложения. */
        open: () => openMedia(ctl),
        save: () => saveMedia(ctl),
    };

    openBtn.addEventListener('click', () => ctl.open());
    saveBtn.addEventListener('click', () => ctl.save());
    body.addEventListener('click', (e) => {
        // Клики изнутри плеера (нативные controls: пауза, ползунок, громкость)
        // — НЕ действие карточки. Без этой проверки они всплывали до body,
        // playInCard пересоздавал элемент и воспроизведение начиналось с нуля,
        // а перемотка/пауза были невозможны в принципе.
        if (isPlayerClick(e.target)) return;
        // Клик по превью = OPEN, но только когда там действительно превью или
        // плеер: по generic-иконке случайно попадают чаще, чем осознанно.
        if (!thumb.hidden || isPlayableKind(kind)) ctl.open();
    });
    // Двойная защита: stopPropagation на слоте отсекает обработчики ВЫШЕ
    // карточки (и pointerdown/mousedown — иначе drag ползунка перемотки
    // дёргает чужие хендлеры), а isPlayerClick выше страхует сам обработчик
    // body на случай, если событие пришло минуя bubbling через слот.
    for (const type of ['click', 'pointerdown', 'mousedown']) {
        playerSlot.addEventListener(type, (e) => e.stopPropagation());
    }

    local.cards.set(`${dialogId}/${messageId}`, ctl);
    if (canShowThumb(media)) {
        observeThumb(ctl);
    } else {
        // Нет превью — сразу честная иконка, без запроса, который вернёт 404.
        icon.hidden = kind === 'voice' || kind === 'audio';
    }
    return card;
}

/** Алиас под имя из плана. */
export const renderMediaCard = buildMediaCard;

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
                const key = `${el.dataset.dialogId}/${el.dataset.msgId}`;
                const found = local.cards.get(key);
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
    loadMedia(ctl.dialogId, ctl.messageId, 'thumb')
        .then((url) => showThumb(ctl, url))
        .catch((e) => {
            // Превью — не главная функция карточки: `no_media` значит «рисуй
            // иконку», всё остальное просто логируем и не пугаем пользователя
            // красной строкой (полный файл по-прежнему можно открыть).
            if (e && e.code !== 'no_media') {
                console.debug('[tg-media] thumb failed', ctl.dialogId, ctl.messageId, e);
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
    ctl.els.thumb.src = url;
    ctl.els.thumb.hidden = false;
    ctl.els.icon.hidden = true;
}

// ---------------------------------------------------------------------------
// Прогресс
// ---------------------------------------------------------------------------

/** Показывает прогрессбар с подписью. `leg` — 'TG' или 'NET'. */
function setProgress(ctl, leg, received, total) {
    const pct = progressPercent(received, total);
    ctl.els.progress.hidden = false;
    ctl.els.bar.style.width = pct === null ? '100%' : `${pct}%`;
    ctl.els.progress.classList.toggle('tg-media-progress-indeterminate', pct === null);
    ctl.els.status.classList.remove('tg-media-done', 'tg-media-failed');
    ctl.els.status.textContent = `${leg} ${progressLabel(received, total)}`;
}

/** Финальное состояние: «✓ RECEIVED». */
function markReceived(ctl) {
    ctl.els.progress.hidden = true;
    ctl.els.bar.style.width = '100%';
    ctl.els.status.classList.remove('tg-media-failed');
    ctl.els.status.classList.add('tg-media-done');
    ctl.els.status.textContent = '✓ RECEIVED';
}

/**
 * Ошибка загрузки.
 *
 * Решение принимается по КОДУ контракта, а не по тексту:
 * `no_media` — ретрай бессмыслен (медиа нет), `flood_wait` — показываем,
 * сколько ждать, остальное — кнопка повтора.
 */
function markError(ctl, err, retry) {
    ctl.els.progress.hidden = true;
    ctl.els.status.classList.remove('tg-media-done');
    ctl.els.status.classList.add('tg-media-failed');
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
    again.classList.add('tg-media-retry');
    again.addEventListener('click', () => {
        again.remove();
        ctl.els.status.textContent = '';
        ctl.els.status.classList.remove('tg-media-failed');
        retry();
    });
    ctl.els.status.after(again);
}

/**
 * WS `download_progress` — «сервер тянет файл из Telegram».
 *
 * Карточка ищется по `(dialog_id, message_id)`; события о медиа, которого нет
 * на экране (другой диалог, страница истории выше), молча игнорируются.
 * `total: null` ⇒ индетерминированный индикатор (размер Telegram не сообщил).
 *
 * @param {{dialog_id: string, message_id: number, kind: string,
 *   received: number, total: ?number}} evt
 */
export function onDownloadProgress(evt) {
    if (!evt || !Number.isFinite(evt.message_id)) return;
    const ctl = local.cards.get(`${evt.dialog_id}/${evt.message_id}`);
    if (!ctl) return;
    // Превью грузятся мгновенно и не должны мигать прогрессбаром на всю
    // карточку: показываем только прогресс полного файла.
    if (evt.kind !== 'full') return;
    const total = Number.isFinite(evt.total) ? evt.total : null;
    if (Number.isFinite(total) && evt.received >= total && total > 0) {
        // Telegram отдал всё; впереди ещё сетевая нога, поэтому не «RECEIVED».
        setProgress(ctl, 'TG', total, total);
        return;
    }
    setProgress(ctl, 'TG', evt.received, total);
}

// ---------------------------------------------------------------------------
// Открытие, сохранение, воспроизведение
// ---------------------------------------------------------------------------

/**
 * OPEN: смысл зависит от вида вложения.
 *
 * - фото/стикер → лайтбокс;
 * - видео/голосовое/аудио → плеер внутри карточки;
 * - документ/прочее → blob в новой вкладке (браузер сам решит, показать
 *   или предложить сохранить); заблокировали popup — падаем на SAVE.
 */
export function openMedia(ctl) {
    if (isImageKind(ctl.kind)) {
        openLightbox(ctl);
        return;
    }
    if (isPlayableKind(ctl.kind)) {
        playInCard(ctl);
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
 * Единственная точка запуска «тяжёлой» загрузки: и лайтбокс, и плеер, и
 * SAVE ходят через неё, поэтому прогресс и обработка ошибок везде одинаковы.
 *
 * `onFail` нужен вызывающим, у которых есть СВОЙ индикатор: лайтбокс
 * показывает «loading…» в подписи, и без этого колбэка он остался бы
 * «загружающимся» навсегда, хотя загрузка уже провалилась.
 */
function withFull(ctl, done, onFail) {
    setProgress(ctl, 'NET', 0, ctl.media && ctl.media.size);
    loadMedia(ctl.dialogId, ctl.messageId, 'full', (received, total) => {
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
 * objectURL НЕ revoke'ается: он лежит в `tgState.mediaCache` и будет
 * переиспользован (например, лайтбоксом), а освободится при смене диалога.
 * Имя санитизируется — `download` браузеры трактуют как подсказку пути.
 */
export function saveMedia(ctl) {
    withFull(ctl, (url) => {
        const a = document.createElement('a');
        a.href = url;
        // САНИТИЗАЦИЯ обязательна: имя пришло из Telegram, а `download`
        // браузеры трактуют как подсказку пути («../../.bashrc» недопустим).
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
    if (local.player.card) local.player.card.classList.remove('tg-media-playing');
    if (local.player.restore) local.player.restore();
    local.player = null;
}

/**
 * Клик пришёл изнутри плеера (нативные controls)?
 *
 * Shadow DOM браузерных controls ретаргетит события на сам `<video>`/
 * `<audio>`, поэтому `closest('.tg-media-player')` надёжно ловит любой клик
 * по плееру, включая перемотку и паузу. Проверка `closest` выбрана как
 * основная (в паре со stopPropagation на слоте): она работает и когда
 * событие пришло в обход слота (например, синтетическое в тестах), и не
 * зависит от порядка навешивания обработчиков.
 *
 * От DOM нужен только метод `closest` — покрыто node-тестами.
 *
 * @param {?EventTarget} target — `e.target` клика
 * @returns {boolean}
 */
export function isPlayerClick(target) {
    return !!(target && typeof target.closest === 'function'
        && target.closest('.tg-media-player'));
}

/**
 * Решает, что делать при клике, запускающем воспроизведение в карточке.
 *
 * Чистая функция (покрыта node-тестами) — единственная точка истины для
 * «пересоздавать ли плеер»:
 * - `ignore` — клик из controls активного плеера, карточка его не трогает;
 * - `toggle` — плеер этой карточки уже открыт: play/pause без пересоздания,
 *   позиция сохраняется;
 * - `start`  — плеера нет или он на другой карточке: создать новый (старый
 *   гасится — инвариант «играет ровно один»).
 *
 * @param {string} cardKey — `<dialog_id>/<message_id>` карточки
 * @param {?string} activeKey — ключ карточки активного плеера или null
 * @param {boolean} fromPlayer — клик пришёл изнутри плеера
 * @returns {'ignore'|'toggle'|'start'}
 */
export function resolveCardClick(cardKey, activeKey, fromPlayer) {
    if (fromPlayer) return 'ignore';
    if (activeKey != null && String(activeKey) === String(cardKey)) return 'toggle';
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
        if ($tgLightbox && !$tgLightbox.hidden) return;
        e.preventDefault();
        stopPlayer();
    });
}

/**
 * Плеер прямо в карточке.
 *
 * Одновременно играет РОВНО один: старт на ДРУГОЙ карточке гасит предыдущий
 * (иначе голосовое и видео накладываются друг на друга и перекрикивают).
 * Повторный вызов для ТОЙ ЖЕ карточки НЕ пересоздаёт элемент — переключает
 * play/pause, сохраняя позицию (пересоздание сбрасывало currentTime в 0 и
 * делало перемотку бессмысленной).
 */
export function playInCard(ctl) {
    const key = `${ctl.dialogId}/${ctl.messageId}`;
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
        const isVideo = ctl.kind === 'video';
        const el = document.createElement(isVideo ? 'video' : 'audio');
        el.controls = true;
        // preload=metadata: длительность известна сразу, ползунок перемотки
        // активен с первого кадра (для blob-URL это бесплатно — данные уже
        // в памяти вкладки).
        el.preload = 'metadata';
        el.src = url;
        // Класса плееру НЕ даём: `.tg-media-video`/`.tg-media-audio` — это
        // классы самой карточки (`tg-media-${kind}`), и стилизовать плеер по
        // ним значит попасть правилом в карточку. CSS цепляется за вложенность
        // (`.tg-media-player > video`).
        if (isVideo) el.style.maxHeight = `${THUMB_HEIGHT * 2}px`;
        const hidden = [];
        for (const node of [ctl.els.thumb, ctl.els.icon, ctl.els.play, ctl.els.durEl]) {
            if (node && !node.hidden) {
                node.hidden = true;
                hidden.push(node);
            }
        }
        const wave = ctl.els.body.querySelector('.tg-media-wave');
        if (wave && !wave.hidden) {
            wave.hidden = true;
            hidden.push(wave);
        }
        // Кнопка ✕ — явный способ остановить воспроизведение и вернуть
        // карточку в исходный вид (плюс Esc, см. ensurePlayerEsc).
        const close = button('✕', 'Остановить и закрыть плеер');
        close.classList.add('tg-media-player-close');
        close.addEventListener('click', () => stopPlayer());
        ctl.els.playerSlot.textContent = '';
        ctl.els.playerSlot.appendChild(el);
        ctl.els.playerSlot.appendChild(close);
        ctl.els.playerSlot.hidden = false;
        // Пока открыт плеер, карточке нельзя резать высоту тела (см. CSS
        // `.tg-media-playing`): иначе у видео обрезается полоса controls.
        ctl.el.classList.add('tg-media-playing');
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
// Лайтбокс
// ---------------------------------------------------------------------------

/** Навешивает обработчики лайтбокса (идемпотентно). */
export function initMedia() {
    if (local.lightboxBound || !$tgLightbox) return;
    local.lightboxBound = true;
    if ($tgLightboxClose) {
        $tgLightboxClose.addEventListener('click', () => closeLightbox());
    }
    $tgLightbox.addEventListener('click', (e) => {
        // Клик по фону закрывает; по самой картинке — нет.
        if (e.target === $tgLightbox) closeLightbox();
    });
    // keydown на документе: лайтбокс перекрывает панель целиком, и ловить
    // Esc только на нём означало бы «работает, лишь пока фокус внутри».
    document.addEventListener('keydown', (e) => {
        if ($tgLightbox.hidden) return;
        if (e.key === 'Escape') {
            e.preventDefault();
            closeLightbox();
            return;
        }
        if (e.key === 'Tab') trapFocus(e);
    });
}

/** Держит фокус внутри лайтбокса (доступность). */
function trapFocus(e) {
    const focusables = [$tgLightboxClose, $tgLightboxImg].filter((n) => n && !n.hidden);
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
 * Открывает полноразмерное изображение поверх панели.
 *
 * Пока грузится full — в подписи «loading…», картинка остаётся пустой:
 * подставлять превью растянутым было бы обманом («уже загрузилось»).
 *
 * @param {object} ctl — контроллер карточки
 */
export function openLightbox(ctl) {
    if (!$tgLightbox || !$tgLightboxImg) return;
    initMedia();
    local.lightbox.ctl = ctl;
    local.lightbox.lastFocus = document.activeElement;
    $tgLightboxImg.removeAttribute('src');
    $tgLightbox.hidden = false;
    if ($tgLightboxCaption) {
        $tgLightboxCaption.textContent = `${ctl.fileName} · loading…`;
    }
    if ($tgLightboxClose) $tgLightboxClose.focus();

    withFull(
        ctl,
        (url) => {
            // Пока грузили, лайтбокс могли закрыть или открыть другой файл.
            if (local.lightbox.ctl !== ctl || $tgLightbox.hidden) return;
            local.lightbox.url = url;
            $tgLightboxImg.src = url;
            $tgLightboxImg.alt = ctl.fileName;
            if ($tgLightboxCaption) {
                const meta = mediaMetaLine(ctl.media);
                $tgLightboxCaption.textContent = meta
                    ? `${ctl.fileName} · ${meta}`
                    : ctl.fileName;
            }
        },
        (err) => {
            if (local.lightbox.ctl !== ctl || !$tgLightboxCaption) return;
            const reason = err && err.code === 'no_media'
                ? '>> NO MEDIA'
                : `>> ${(err && err.message) || 'ERR'}`;
            $tgLightboxCaption.textContent = `${ctl.fileName} · ${reason}`;
        }
    );
}

/** Закрывает лайтбокс и возвращает фокус на карточку. */
export function closeLightbox() {
    if (!$tgLightbox) return;
    $tgLightbox.hidden = true;
    $tgLightboxImg.removeAttribute('src');
    if ($tgLightboxCaption) $tgLightboxCaption.textContent = '';
    const back = local.lightbox.lastFocus;
    local.lightbox = { ctl: null, lastFocus: null, url: null };
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
        loading: local.loads.size,
        cached: tgState.mediaCache.size,
        lightboxOpen: !!($tgLightbox && !$tgLightbox.hidden),
        player: local.player ? local.player.el.tagName.toLowerCase() : null,
        playerKey: local.player ? local.player.key : null,
    };
}
