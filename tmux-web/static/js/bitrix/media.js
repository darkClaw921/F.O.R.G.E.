// tmux-web — Bitrix24 plugin: поведение вложений, аватары, лайтбокс, плеер.
//
// Разметку карточки собирает `attachments.js` (чистые узлы, без единого
// обработчика), а этот модуль находит её по классам и оживляет: ленивая
// загрузка, прогресс, лайтбокс, плеер, SAVE.
//
// ============================================================================
// ПЯТЬ ИНВАРИАНТОВ, КОТОРЫЕ ЛЕГКО ПОТЕРЯТЬ
// ============================================================================
//
// 1. ЗАГРУЗКА ТОЛЬКО ЧЕРЕЗ `fetch` → `blob` → `objectURL`. Атрибут `src` у
//    `<img>`/`<audio>` НЕ несёт заголовков, а в remote-mode `/api/*` требует
//    `Authorization: Bearer`. Прямой `src="/api/bitrix/media/…"` работал бы на
//    локальной машине и молча ломался бы у пользователя с токеном — самый
//    неприятный вид дефекта.
// 2. ВНЕШНИХ АДРЕСОВ В DOM НЕТ ВООБЩЕ. Ни один адрес портала не попадает ни в
//    `src`, ни в `href`: и байты вложений, и аватары идут через прокси
//    плагина. У аватаров CDN-адрес публичен и работал бы напрямую — он всё
//    равно идёт через прокси (Р6), потому что инвариант проверяется одним
//    грепом по разметке, а единственное исключение превращает проверку в
//    разбор каждого поля каждого DTO.
// 3. ЛЕНИВО. Превью запрашивается, только когда карточка подъехала к вьюпорту
//    (`IntersectionObserver`): на истории в сотню медиа иначе улетела бы сотня
//    запросов при первом же рендере, а бюджет портала — два вызова в секунду.
//    Полный файл (документ, аудио, видео) грузится ТОЛЬКО по явному действию.
// 4. `objectURL` ОСВОБОЖДАЮТСЯ ПРИ СМЕНЕ ДИАЛОГА. Браузер не собирает блоб,
//    пока живёт его URL: без `revoke` двадцать переключений между чатами с
//    картинками оставляют в памяти вкладки всё скачанное до перезагрузки
//    страницы. Аватары при этом ПЕРЕЖИВАЮТ смену диалога — они общие для всей
//    панели, и отзывать их значило бы качать те же лица заново на каждый клик
//    по списку.
// 5. ВСЁ СОДЕРЖИМОЕ — `textContent`/`createElement`. Имена файлов пишет
//    собеседник; `innerHTML` здесь означал бы XSS.
//
// ============================================================================
// ПРОГРЕСС ДВУНОГИЙ
// ============================================================================
//
// Кадр WS `download_progress` — это «сервер тянет файл из портала» (подпись
// `B24`), чтение `response.body` — «сервер отдаёт браузеру» (подпись `NET`).
// Обе ноги пишут в одну полосу, но с разной подписью: иначе индикатор дважды
// бежал бы от нуля до ста без всякого объяснения.

import {
    $b24Lightbox, $b24LightboxCaption, $b24LightboxClose, $b24LightboxImg, $b24LightboxNext,
    $b24LightboxPrev,
} from '../core/dom.js';

import { createVoiceTranscript } from '../messenger/voice-transcript-core.js';
import * as b24Api from './api.js';
import { buildMediaList } from './attachments.js';
import {
    avatarKey, isImageKind, isPlayableKind, mediaKey, playerTag, progressLabel, progressPercent,
    sanitizeDownloadName,
} from './format.js';
import { b24State } from './state.js';

/** Запас в пикселях, при котором карточка считается «почти видимой». */
const OBSERVER_MARGIN = '200px';

/**
 * Имя DOM-события, которым панель отдаёт кадры расшифровки наружу.
 *
 * Объявлено ЗДЕСЬ, а не в `main.js`, хотя рассылает кадры именно он: карточку
 * оживляет этот файл, а `main.js` импортирует его — обратный импорт замкнул бы
 * цикл. `main.js` берёт имя отсюда и переэкспортирует.
 */
export const VOICE_FRAME_EVENT = 'bitrix:voice-transcribe';

/**
 * Текст расшифровки под голосовым — ОБЩИЙ модуль на три панели.
 *
 * Живёт в media.js, а не в `attachments.js`, где строится сама разметка
 * карточки, и это не случайность: `attachments.js` не делает ни одного запроса
 * и не вешает ни одного обработчика — ровно поэтому он проверяется в голом
 * Node. Расшифровка же и запрашивает, и слушает; её место здесь, рядом с
 * остальным «оживлением» карточек.
 */
export const voiceTranscript = createVoiceTranscript({
    plugin: 'bitrix',
    classPrefix: 'b24-voice',
    frameEvent: VOICE_FRAME_EVENT,
    api: { transcribe: b24Api.transcribeVoice, transcript: b24Api.getVoiceTranscript },
});

const local = {
    /** ключ карточки → контроллер. */
    cards: new Map(),
    /** `<dialog>/<message>` → карточки одного сообщения В ПОРЯДКЕ вложений. */
    groups: new Map(),
    /** ключ медиа → Promise<objectURL> НЕЗАВЕРШЁННОЙ загрузки. */
    loads: new Map(),
    /** IntersectionObserver ленивых превью (создаётся при первой карточке). */
    observer: null,
    /** Активный плеер: старт нового гасит предыдущий. */
    player: null,
    /** Ключ карточки, для которой файл плеера ещё грузится. */
    pendingPlayKey: null,
    /** Esc-обработчик остановки плеера навешен. */
    playerEscBound: false,
    /** Обработчики лайтбокса навешены. */
    lightboxBound: false,
    /** Состояние лайтбокса: галерея, позиция и куда вернуть фокус. */
    lightbox: { ctl: null, gallery: [], pos: 0, lastFocus: null },
};

// ---------------------------------------------------------------------------
// Загрузка байтов
// ---------------------------------------------------------------------------

/**
 * Грузит медиа по готовому пути прокси и возвращает objectURL.
 *
 * Дедупликация ДВУХУРОВНЕВАЯ: готовый URL берётся из `b24State.mediaCache`, а
 * незавершённая загрузка — из `local.loads`. Второй уровень важен не меньше
 * первого: без него два клика по OPEN дали бы две параллельные загрузки одного
 * файла (сервер бы их схлопнул своим дедупом, но браузер честно скачал бы
 * дважды).
 *
 * @param {string} key — ключ кэша (`mediaKey` либо `avatarKey`)
 * @param {string} path — путь прокси
 * @param {function(number, ?number): void} [onNetProgress]
 * @returns {Promise<string>} objectURL
 */
export function loadMedia(key, path, onNetProgress) {
    const cached = b24State.mediaCache.get(key);
    if (typeof cached === 'string') return Promise.resolve(cached);
    const inflight = local.loads.get(key);
    if (inflight) return inflight;

    const p = downloadBlobUrl(path, onNetProgress)
        .then((url) => {
            local.loads.delete(key);
            b24State.mediaCache.set(key, url);
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
 * на `response.blob()`: прогресса тогда нет, но файл загрузится.
 */
async function downloadBlobUrl(path, onNetProgress) {
    const res = await b24Api.fetchBytes(path);
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
 * Освобождает objectURL'ы ВЛОЖЕНИЙ (аватары остаются).
 *
 * Зовётся при смене диалога. Аватары не трогаются намеренно: они общие для всей
 * панели и переиспользуются в колонке, в ленте и в шапке — отзывать их на
 * каждом клике по списку значило бы качать те же лица заново.
 *
 * @returns {number} сколько ссылок освобождено
 */
export function revokeDialogMedia() {
    let n = 0;
    for (const [key, url] of Array.from(b24State.mediaCache.entries())) {
        if (key.startsWith('avatar/')) continue;
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); n += 1; } catch (_) { /* уже отозван */ }
        }
        b24State.mediaCache.delete(key);
        local.loads.delete(key);
    }
    return n;
}

/**
 * Сбрасывает objectURL'ы одного сообщения (ВСЕ его вложения).
 *
 * Нужен на `message_update`/`message_delete`: сервер в этот момент
 * инвалидирует свой файловый кэш (`media_cache::invalidate`), и держать во
 * вкладке старую картинку значило бы показывать то, чего на портале больше нет.
 *
 * Обходятся все известные ключи сообщения по РЕЕСТРУ КАРТОЧЕК, а не по
 * «сколько было вложений»: правка могла заменить список, и старую длину
 * спросить уже негде.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @returns {number} сколько ссылок освобождено
 */
export function dropMessageMedia(dialogId, messageId) {
    const prefix = `${dialogId}/${messageId}/`;
    let n = 0;
    for (const [key, url] of Array.from(b24State.mediaCache.entries())) {
        if (!key.startsWith(prefix)) continue;
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); n += 1; } catch (_) { /* уже отозван */ }
        }
        b24State.mediaCache.delete(key);
        local.loads.delete(key);
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
// Аватары (Р6)
// ---------------------------------------------------------------------------

/**
 * Подставляет аватар сотрудника в готовый `<img>` через прокси.
 *
 * `img.src` получает `blob:`-адрес, а не путь `/api/bitrix/media/avatar/…`:
 * прямой путь не пронёс бы `Authorization: Bearer` в remote-mode. Отказ (404 —
 * фото у сотрудника нет) прячет картинку, и остаётся кружок с инициалами на
 * фирменном цвете портала: это штатная ветка, а не ошибка.
 *
 * @param {?object} img — элемент `<img>`
 * @param {number|string} userId
 * @returns {Promise<void>}
 */
export async function attachAvatar(img, userId) {
    if (!img) return;
    const id = String(userId == null ? '' : userId).trim();
    if (!/^\d+$/.test(id)) return;
    try {
        const url = await loadMedia(avatarKey(id), b24Api.avatarUrl(id));
        img.src = url;
        img.hidden = false;
    } catch (_) {
        // Нет фото либо нет связи — инициалы уже нарисованы, добавить нечего.
        img.hidden = true;
    }
}

// ---------------------------------------------------------------------------
// Карточки вложений
// ---------------------------------------------------------------------------

/**
 * Собирает контейнер карточек сообщения и оживляет их.
 *
 * Возвращает готовый узел либо `null`, если вложений нет. Разметку строит
 * `attachments.js`; здесь — реестр контроллеров, ленивые превью и обработчики.
 *
 * @param {object} msg — MessageDto
 * @returns {?HTMLElement}
 */
export function renderMediaGroup(msg) {
    if (!msg) return null;
    const list = Array.isArray(msg.media) ? msg.media : [];
    const box = buildMediaList(list, document, msg);
    if (!box) return null;

    const dialogId = String(msg.dialog_id);
    const messageId = Number(msg.id);
    const group = [];
    const cards = Array.from(box.childNodes || []);
    for (let i = 0; i < cards.length; i += 1) {
        const el = cards[i];
        const media = list[i];
        if (!el || !media) continue;
        wireCard(el, media, dialogId, messageId, group);
        // Блок расшифровки — последним потомком карточки и только у звука:
        // `null` для прочих видов возвращает сам общий модуль.
        const transcript = voiceTranscript.attach({
            dialogId, messageId, index: media.index, kind: media.kind,
        });
        if (transcript) el.appendChild(transcript);
    }
    local.groups.set(`${dialogId}/${messageId}`, group);
    return box;
}

/**
 * Все узлы поддерева в порядке обхода.
 *
 * Обход руками, а не `querySelectorAll`, ровно по одной причине: этот же код
 * обязан работать под крошечным фейком документа во фронт-тестах, где из всего
 * DOM есть `createElement`, `appendChild` и `childNodes`. Настоящий селектор
 * потребовал бы либо jsdom в зависимостях, либо второй ветки «а если это тест»
 * — и то, и другое хуже пятистрочного обхода.
 */
function walkNodes(root, out = []) {
    if (!root) return out;
    out.push(root);
    for (const child of (root.childNodes || [])) walkNodes(child, out);
    return out;
}

/** Есть ли у узла класс. */
function hasClass(node, cls) {
    return typeof node.className === 'string' && node.className.split(' ').includes(cls);
}

/** Первый узел поддерева с указанным классом. */
function pick(root, cls) {
    return walkNodes(root).find((n) => hasClass(n, cls)) || null;
}

/** Все узлы поддерева с указанным классом. */
function pickAll(root, cls) {
    return walkNodes(root).filter((n) => hasClass(n, cls));
}

/** Подписка, переживающая узел без обработчиков (фейковый документ тестов). */
function on(node, type, fn) {
    if (node && typeof node.addEventListener === 'function') node.addEventListener(type, fn);
}

/**
 * Навешивает поведение на одну карточку.
 *
 * @param {HTMLElement} el
 * @param {object} media — MediaDto
 * @param {string} dialogId
 * @param {number} messageId
 * @param {Array<object>} group — карточки того же сообщения (галерея)
 */
function wireCard(el, media, dialogId, messageId, group) {
    const index = Number.isFinite(Number(media.index)) ? Number(media.index) : 0;
    const kind = String(media.kind || 'file');
    const key = mediaKey(dialogId, messageId, index);
    const ctl = {
        el,
        key,
        dialogId,
        messageId,
        index,
        kind,
        media,
        hasFile: media.has_full === true,
        fileName: String(media.name || '').trim(),
        group,
        els: {
            thumb: pick(el, 'b24-media-thumb'),
            icon: pick(el, 'b24-media-icon'),
            player: pick(el, 'b24-media-player'),
            body: pick(el, 'b24-media-body'),
            progress: pick(el, 'b24-media-progress'),
            bar: pick(el, 'b24-media-progress-bar'),
            status: pick(el, 'b24-media-status'),
        },
        thumbRequested: false,
    };
    ctl.path = b24Api.mediaUrl(dialogId, messageId, index);

    for (const btn of pickAll(el, 'b24-media-btn')) {
        on(btn, 'click', (e) => {
            e.stopPropagation();
            if (btn.disabled) return;
            if (btn.dataset.act === 'save') saveMedia(ctl);
            else openMedia(ctl);
        });
    }
    on(ctl.els.body, 'click', (e) => {
        // Клики изнутри плеера (нативные controls: пауза, ползунок) — НЕ
        // действие карточки. Без этой проверки они всплывали бы до тела, плеер
        // пересоздавался бы, и воспроизведение начиналось бы с нуля.
        if (isPlayerClick(e.target)) return;
        if (!ctl.hasFile) return;
        openMedia(ctl);
    });
    // Двойная защита: `stopPropagation` на слоте отсекает обработчики ВЫШЕ
    // карточки (и `pointerdown`/`mousedown` — иначе drag ползунка перемотки
    // дёргает чужие хендлеры).
    for (const type of ['click', 'pointerdown', 'mousedown']) {
        on(ctl.els.player, type, (e) => e.stopPropagation());
    }

    local.cards.set(key, ctl);
    group.push(ctl);
    if (ctl.els.thumb && ctl.hasFile) observeThumb(ctl);
}

/**
 * Ставит карточку под наблюдение: превью грузится, когда она подъезжает к
 * вьюпорту.
 *
 * Без `IntersectionObserver` (очень старый браузер) грузим сразу: лучше лишние
 * запросы, чем пустые карточки.
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
                    mediaKey(el.dataset.dialogId, el.dataset.msgId, el.dataset.index),
                );
                if (found) ensureThumb(found);
            }
        }, { root: null, rootMargin: OBSERVER_MARGIN, threshold: 0.01 });
    }
    local.observer.observe(ctl.el);
}

/**
 * Грузит превью один раз.
 *
 * Отдельного «маленького файла» у портала нет (`im.v2.File.download` отдаёт
 * оригинал), поэтому превью картинки — это и есть её полный файл. Именно
 * поэтому оно и грузится ЛЕНИВО: на ленте с десятком фотографий разница между
 * «сразу» и «когда видно» — это разница между полусотней мегабайт и одним
 * экраном.
 */
function ensureThumb(ctl) {
    if (ctl.thumbRequested) return;
    ctl.thumbRequested = true;
    loadMedia(ctl.key, ctl.path, (received, total) => setProgress(ctl, 'NET', received, total))
        .then((url) => {
            clearProgress(ctl);
            if (!ctl.els.thumb) return;
            ctl.els.thumb.src = url;
            ctl.els.thumb.hidden = false;
            if (ctl.els.icon) ctl.els.icon.hidden = true;
        })
        .catch((e) => {
            // Превью — не главная функция карточки: рисуем иконку и молчим,
            // пользователь по-прежнему может нажать OPEN.
            markError(ctl, e, () => { ctl.thumbRequested = false; ensureThumb(ctl); });
        });
}

// ---------------------------------------------------------------------------
// Прогресс и статус
// ---------------------------------------------------------------------------

/** Показывает полосу с подписью. `leg` — `B24` (портал → сервер) или `NET`. */
function setProgress(ctl, leg, received, total) {
    if (!ctl.els.progress || !ctl.els.bar || !ctl.els.status) return;
    const pct = progressPercent(received, total);
    ctl.els.progress.hidden = false;
    ctl.els.bar.style.width = pct === null ? '100%' : `${pct}%`;
    ctl.els.progress.classList.toggle('b24-media-progress-indeterminate', pct === null);
    ctl.els.status.classList.remove('b24-media-done', 'b24-media-failed');
    ctl.els.status.textContent = `${leg} ${progressLabel(received, total)}`;
}

/** Убирает полосу и ставит «✓». */
function clearProgress(ctl) {
    if (!ctl.els.progress || !ctl.els.status) return;
    ctl.els.progress.hidden = true;
    ctl.els.status.classList.remove('b24-media-failed');
    ctl.els.status.classList.add('b24-media-done');
    ctl.els.status.textContent = '✓';
}

/**
 * Ошибка загрузки.
 *
 * Решение принимается по КОДУ контракта, а не по тексту: `no_media` — повтор
 * бессмыслен (файла нет), `flood_wait` — показываем, сколько ждать, остальное —
 * кнопка повтора.
 */
function markError(ctl, err, retry) {
    if (!ctl.els.status) return;
    if (ctl.els.progress) ctl.els.progress.hidden = true;
    if (ctl.els.icon) ctl.els.icon.hidden = false;
    ctl.els.status.classList.remove('b24-media-done');
    ctl.els.status.classList.add('b24-media-failed');
    const code = err && err.code;
    if (code === 'no_media') {
        ctl.els.status.textContent = '>> НЕТ ФАЙЛА';
        return;
    }
    if (code === 'flood_wait') {
        const secs = (err && err.retryAfterSecs) || 0;
        ctl.els.status.textContent = secs ? `>> ЖДЁМ ${secs}с` : '>> ЖДЁМ';
    } else {
        ctl.els.status.textContent = '>> ОШИБКА';
    }
    if (typeof retry !== 'function') return;
    const again = document.createElement('button');
    again.type = 'button';
    again.className = 'b24-media-btn b24-media-retry';
    again.textContent = 'RETRY';
    again.title = 'Повторить загрузку';
    again.addEventListener('click', (e) => {
        e.stopPropagation();
        again.remove();
        ctl.els.status.textContent = '';
        ctl.els.status.classList.remove('b24-media-failed');
        retry();
    });
    ctl.els.status.after(again);
}

/**
 * Кадр WS `download_progress` — «сервер тянет файл из портала».
 *
 * Карточка ищется по ключу `<dialog>/<message>/<index>`, который сервер
 * присылает целиком (`media_cache::CacheKey::id`). ИНДЕКС обязателен: без него
 * прогресс второго файла сообщения прилетал бы в карточку первого. События о
 * медиа, которого нет на экране (другой диалог, страница истории выше), молча
 * игнорируются.
 *
 * @param {{key: string, received: number, total: ?number}} evt
 */
export function onDownloadProgress(evt) {
    if (!evt || typeof evt.key !== 'string') return;
    const ctl = local.cards.get(evt.key);
    if (!ctl) return;
    const total = Number.isFinite(evt.total) ? evt.total : null;
    setProgress(ctl, 'B24', Number(evt.received) || 0, total);
}

// ---------------------------------------------------------------------------
// Открытие, сохранение, воспроизведение
// ---------------------------------------------------------------------------

/**
 * Грузит файл с прогрессом и передаёт objectURL в колбэк.
 *
 * Единственная точка запуска «тяжёлой» загрузки: и лайтбокс, и плеер, и SAVE
 * ходят через неё, поэтому прогресс и обработка ошибок везде одинаковы.
 *
 * `onFail` нужен вызывающим со СВОИМ индикатором: лайтбокс показывает
 * «загрузка…» в подписи и без этого колбэка остался бы «загружающимся»
 * навсегда, хотя загрузка уже провалилась.
 */
function withFile(ctl, done, onFail) {
    setProgress(ctl, 'NET', 0, ctl.media && ctl.media.size);
    loadMedia(ctl.key, ctl.path, (received, total) => setProgress(ctl, 'NET', received, total))
        .then((url) => {
            clearProgress(ctl);
            done(url);
        })
        .catch((e) => {
            markError(ctl, e, () => withFile(ctl, done, onFail));
            if (typeof onFail === 'function') onFail(e);
        });
}

/**
 * OPEN: смысл зависит от вида вложения.
 *
 * - изображение → лайтбокс (с галереей по вложениям сообщения);
 * - аудио/голосовое/видео/кружок → плеер внутри карточки;
 * - документ и всё прочее → blob в новой вкладке; заблокировали popup — падаем
 *   на SAVE.
 *
 * Внешних адресов ни в одной ветке нет: страницы-плеера у портала не
 * существует вовсе (`player_url` в DTO всегда `null`), поэтому и открывать
 * нечего, кроме собственного blob'а.
 *
 * @param {object} ctl
 */
export function openMedia(ctl) {
    if (!ctl.hasFile) {
        markError(ctl, { code: 'no_media' });
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
    withFile(ctl, (url) => {
        const w = window.open(url, '_blank', 'noopener');
        if (!w) saveMedia(ctl);
    });
}

/**
 * SAVE: файл → `<a download>` → click.
 *
 * objectURL НЕ revoke'ается: он лежит в `b24State.mediaCache` и будет
 * переиспользован (например, лайтбоксом), а освободится при смене диалога.
 * Имя санитизируется — `download` браузеры трактуют как подсказку ПУТИ.
 *
 * @param {object} ctl
 */
export function saveMedia(ctl) {
    if (!ctl.hasFile) {
        markError(ctl, { code: 'no_media' });
        return;
    }
    withFile(ctl, (url) => {
        const a = document.createElement('a');
        a.href = url;
        a.download = sanitizeDownloadName(ctl.fileName || `b24_${ctl.messageId}_${ctl.index}`);
        a.rel = 'noopener';
        document.body.appendChild(a);
        a.click();
        a.remove();
    });
}

/** Клик пришёл изнутри плеера (нативные controls)? */
export function isPlayerClick(target) {
    return !!(target && typeof target.closest === 'function'
        && target.closest('.b24-media-player'));
}

/**
 * Что делать при клике, запускающем воспроизведение.
 *
 * Чистая функция — единственная точка истины для «пересоздавать ли плеер»:
 * `ignore` — клик из controls активного плеера; `toggle` — плеер этой карточки
 * уже открыт, значит play/pause без пересоздания и с сохранением позиции;
 * `start` — плеера нет или он на другой карточке.
 *
 * @param {string} key
 * @param {?string} activeKey
 * @param {boolean} fromPlayer
 * @returns {'ignore'|'toggle'|'start'}
 */
export function resolveCardClick(key, activeKey, fromPlayer) {
    if (fromPlayer) return 'ignore';
    if (activeKey != null && String(activeKey) === String(key)) return 'toggle';
    return 'start';
}

/** Останавливает и снимает активный плеер. */
export function stopPlayer() {
    if (!local.player) return;
    try {
        local.player.el.pause();
    } catch (_) { /* элемент мог уехать из DOM */ }
    local.player.slot.textContent = '';
    local.player.slot.hidden = true;
    if (local.player.card) local.player.card.classList.remove('b24-media-playing');
    if (local.player.restore) local.player.restore();
    local.player = null;
}

/** Esc останавливает активный плеер (навешивается лениво, один раз). */
function ensurePlayerEsc() {
    if (local.playerEscBound) return;
    local.playerEscBound = true;
    document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape' || !local.player) return;
        // Лайтбокс перекрывает панель и обрабатывает Esc сам: одно нажатие не
        // должно закрывать оба.
        if ($b24Lightbox && !$b24Lightbox.hidden) return;
        e.preventDefault();
        stopPlayer();
    });
}

/**
 * Плеер прямо в карточке (аудио, голосовое, видео, кружок).
 *
 * Одновременно играет РОВНО один: старт на другой карточке гасит предыдущий —
 * иначе два голосовых из одного сообщения перекрикивают друг друга. Повторный
 * клик по ТОЙ ЖЕ карточке не пересоздаёт элемент, а переключает play/pause с
 * сохранением позиции.
 *
 * @param {object} ctl
 */
export function playInCard(ctl) {
    const action = resolveCardClick(ctl.key, local.player ? local.player.key : null, false);
    if (action === 'toggle') {
        togglePlayback(local.player.el);
        return;
    }
    // Файл уже грузится для этой карточки — второй клик не должен порождать
    // второй плеер (`loadMedia` схлопнет запрос, но колбэки сработали бы дважды).
    if (local.pendingPlayKey === ctl.key) return;
    stopPlayer();
    local.pendingPlayKey = ctl.key;
    withFile(ctl, (url) => {
        if (local.pendingPlayKey === ctl.key) local.pendingPlayKey = null;
        stopPlayer(); // пока грузили, плеер могли запустить на другой карточке
        const el = document.createElement(playerTag(ctl.kind));
        el.controls = true;
        // preload=metadata: для blob-URL это бесплатно (данные уже в памяти
        // вкладки), зато ползунок перемотки активен с первого кадра.
        el.preload = 'metadata';
        el.src = url;
        const hidden = [];
        for (const node of [ctl.els.thumb, ctl.els.icon]) {
            if (node && !node.hidden) {
                node.hidden = true;
                hidden.push(node);
            }
        }
        const close = document.createElement('button');
        close.type = 'button';
        close.className = 'b24-media-btn b24-media-player-close';
        close.textContent = '✕';
        close.title = 'Остановить и закрыть плеер';
        close.addEventListener('click', (e) => { e.stopPropagation(); stopPlayer(); });
        ctl.els.player.textContent = '';
        ctl.els.player.appendChild(el);
        ctl.els.player.appendChild(close);
        ctl.els.player.hidden = false;
        ctl.el.classList.add('b24-media-playing');
        local.player = {
            key: ctl.key,
            el,
            slot: ctl.els.player,
            card: ctl.el,
            restore: () => hidden.forEach((n) => { n.hidden = false; }),
        };
        ensurePlayerEsc();
        const played = el.play();
        if (played && typeof played.catch === 'function') {
            // Автовоспроизведение может быть запрещено политикой браузера — это
            // не ошибка: у элемента есть controls, пользователь нажмёт сам.
            played.catch(() => {});
        }
    }, () => {
        // Ошибка загрузки: снять «pending», иначе карточка навсегда глохнет к
        // кликам (RETRY идёт через markError и снова позовёт playInCard).
        if (local.pendingPlayKey === ctl.key) local.pendingPlayKey = null;
    });
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

// ---------------------------------------------------------------------------
// Лайтбокс-галерея
// ---------------------------------------------------------------------------

/** Навешивает обработчики лайтбокса (идемпотентно). */
export function initMedia() {
    if (local.lightboxBound || !$b24Lightbox) return;
    local.lightboxBound = true;
    if ($b24LightboxClose) {
        $b24LightboxClose.addEventListener('click', () => closeLightbox());
    }
    if ($b24LightboxPrev) {
        $b24LightboxPrev.addEventListener('click', (e) => { e.stopPropagation(); stepLightbox(-1); });
    }
    if ($b24LightboxNext) {
        $b24LightboxNext.addEventListener('click', (e) => { e.stopPropagation(); stepLightbox(1); });
    }
    $b24Lightbox.addEventListener('click', (e) => {
        // Клик по фону закрывает; по самой картинке и по стрелкам — нет.
        if (e.target === $b24Lightbox) closeLightbox();
    });
    // keydown на документе: лайтбокс перекрывает панель целиком, и ловить Esc
    // только на нём означало бы «работает, лишь пока фокус внутри».
    document.addEventListener('keydown', (e) => {
        if ($b24Lightbox.hidden) return;
        if (e.key === 'Escape') { e.preventDefault(); closeLightbox(); return; }
        if (e.key === 'ArrowLeft') { e.preventDefault(); stepLightbox(-1); return; }
        if (e.key === 'ArrowRight') { e.preventDefault(); stepLightbox(1); return; }
        if (e.key === 'Tab') trapFocus(e);
    });
}

/** Держит фокус внутри лайтбокса (доступность). */
function trapFocus(e) {
    const focusables = [$b24LightboxClose, $b24LightboxPrev, $b24LightboxNext]
        .filter((n) => n && !n.hidden && !n.disabled);
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
 * сообщении, а не в переписке. Непросматриваемые виды (документ, голосовое) из
 * галереи выпадают — листать их лайтбоксом нечем.
 *
 * @param {object} ctl
 * @returns {Array<object>}
 */
export function galleryOf(ctl) {
    const group = Array.isArray(ctl.group) && ctl.group.length ? ctl.group : [ctl];
    const images = group.filter((c) => isImageKind(c.kind) && c.hasFile);
    return images.length ? images : [ctl];
}

/**
 * Открывает изображение поверх панели.
 *
 * @param {object} ctl
 */
export function openLightbox(ctl) {
    if (!$b24Lightbox || !$b24LightboxImg) return;
    initMedia();
    const gallery = galleryOf(ctl);
    const pos = Math.max(0, gallery.indexOf(ctl));
    local.lightbox.gallery = gallery;
    local.lightbox.lastFocus = document.activeElement;
    $b24Lightbox.hidden = false;
    if ($b24LightboxClose) $b24LightboxClose.focus();
    showAt(pos);
}

/**
 * Сдвигает галерею на `delta` позиций.
 *
 * Границы НЕ зациклены намеренно: «следующее» за последним фото — это ничего, а
 * прыжок в начало читается как сбой. Кнопки на границах выключаются, поэтому
 * мышью в упор не упрёшься вовсе.
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
    if ($b24LightboxImg) $b24LightboxImg.removeAttribute('src');
    const counter = g.length > 1 ? ` · ${pos + 1}/${g.length}` : '';
    const title = ctl.fileName || `вложение ${ctl.index + 1}`;
    if ($b24LightboxCaption) $b24LightboxCaption.textContent = `${title}${counter} · загрузка…`;
    if ($b24LightboxPrev) {
        $b24LightboxPrev.hidden = g.length < 2;
        $b24LightboxPrev.disabled = pos === 0;
    }
    if ($b24LightboxNext) {
        $b24LightboxNext.hidden = g.length < 2;
        $b24LightboxNext.disabled = pos === g.length - 1;
    }

    withFile(
        ctl,
        (url) => {
            // Пока грузили, лайтбокс могли закрыть или пролистать дальше.
            if (local.lightbox.ctl !== ctl || $b24Lightbox.hidden) return;
            $b24LightboxImg.src = url;
            $b24LightboxImg.alt = title;
            if ($b24LightboxCaption) $b24LightboxCaption.textContent = `${title}${counter}`;
        },
        (err) => {
            if (local.lightbox.ctl !== ctl || !$b24LightboxCaption) return;
            const reason = err && err.code === 'no_media' ? '>> НЕТ ФАЙЛА' : '>> ОШИБКА';
            $b24LightboxCaption.textContent = `${title}${counter} · ${reason}`;
        },
    );
}

/** Закрывает лайтбокс и возвращает фокус на карточку. */
export function closeLightbox() {
    if (!$b24Lightbox) return;
    $b24Lightbox.hidden = true;
    // Узла картинки может не быть у пользователя со старым index.html в кэше
    // PWA: панель обязана продолжать работать, а не падать на teardown.
    if ($b24LightboxImg) $b24LightboxImg.removeAttribute('src');
    if ($b24LightboxCaption) $b24LightboxCaption.textContent = '';
    if ($b24LightboxPrev) $b24LightboxPrev.hidden = true;
    if ($b24LightboxNext) $b24LightboxNext.hidden = true;
    const back = local.lightbox.lastFocus;
    local.lightbox = { ctl: null, gallery: [], pos: 0, lastFocus: null };
    if (back && typeof back.focus === 'function') {
        try { back.focus(); } catch (_) { /* элемент уехал из DOM */ }
    }
}

/** Полная очистка (logout, teardown вкладки) — включая аватары. */
export function teardownMedia() {
    closeLightbox();
    resetMediaCards();
    revokeDialogMedia();
    for (const [key, url] of Array.from(b24State.mediaCache.entries())) {
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
        }
        b24State.mediaCache.delete(key);
    }
    local.loads.clear();
}

/** Снимок состояния для e2e и диагностики. */
export function _debugMediaState() {
    return {
        cards: local.cards.size,
        groups: local.groups.size,
        loading: local.loads.size,
        cached: b24State.mediaCache.size,
        avatars: Array.from(b24State.mediaCache.keys()).filter((k) => k.startsWith('avatar/')).length,
        lightboxOpen: !!($b24Lightbox && !$b24Lightbox.hidden),
        lightboxPos: local.lightbox.pos,
        gallery: local.lightbox.gallery.length,
        player: local.player ? local.player.el.tagName.toLowerCase() : null,
        playerKey: local.player ? local.player.key : null,
    };
}
