// tmux-web — Telegram plugin: отправка файлов.
//
// Три способа выбрать файл, одна очередь, два этапа прогресса:
//
//   ⇪ #tg-attach → скрытый #tg-file-input
//   drag&drop над #telegram → оверлей #tg-dropzone
//   Ctrl+V в #tg-input (скриншот из буфера)
//        │
//        ▼
//   очередь (СТРОГО последовательно)
//        │
//   [1] LOCAL %  браузер → devforge   (XMLHttpRequest.upload.onprogress)
//        │       POST /api/telegram/upload, тело = сырой File
//        ▼       ответ: { upload_id, file_name, size, mime }
//   [2] TG %     devforge → Telegram  (WS upload_progress по upload_id)
//        │       POST /api/telegram/dialogs/:id/messages { upload_id, caption }
//        ▼
//   ✓ SENT → optimistic-карточка снимается, ленту дорисовывает chat.onNewMessage
//
// Почему XHR, а не fetch: у fetch НЕТ прогресса отправки (ReadableStream в
// body поддерживается не всеми браузерами и требует half-duplex HTTP/2).
// XHR даёт `upload.onprogress` из коробки.
//
// ВАЖНО: XHR идёт МИМО подменённого window.fetch из core/auth.js, поэтому
// заголовок Authorization в remote-mode ставится ВРУЧНУЮ (getAuthToken).
// Без этого отправка файлов работала бы только на localhost.
//
// Очередь строго последовательная: два параллельных upload'а делят один
// канал, из-за чего оба идут вдвое медленнее, а события upload_progress
// перемешиваются в статусбаре карточек.

import { $telegram, $tgAttach, $tgDropzone, $tgFileInput, $tgInput, $tgLog } from '../core/dom.js';
import { getAuthToken } from '../core/auth.js';

import { sendMessage } from './api.js';
import * as chat from './chat.js';
// Состояние «на какое сообщение отвечаем» живёт в reply.js и ТОЛЬКО там:
// второй владелец у него появиться не должен, иначе полоса над полем ввода и
// реально уходящий reply_to_id разъедутся. Отсюда — только чтение цели;
// снимает её chat.cancelReply(), потому что у полосы две половины (цель в
// reply.js и DOM-узел в chat.js) и разъединять их нельзя.
import { replyTargetId } from './reply.js';
import {
    MAX_UPLOAD_BYTES, formatBytes, formatTime, mediaGlyph, mediaKindLabel, parseApiError,
    pastedFileName, progressPercent, uploadKind, uploadMime, uploadQueueLabel, uploadRejectReason,
    uploadStageLabel,
} from './format.js';
import { tgState } from './state.js';

/** Путь загрузки (совпадает с исключением csrf_guard в tmux-web/src/auth.rs). */
export const UPLOAD_PATH = '/api/telegram/upload';

/** Заголовок с именем файла: percent-encoded, потому что заголовки — ASCII. */
export const FILE_NAME_HEADER = 'X-Forge-File-Name';

const local = {
    /** Обработчики уже навешены. */
    bound: false,
    /** Очередь: [0] — тот, что заливается прямо сейчас. */
    queue: [],
    /** Идёт ли обработка очереди. */
    pumping: false,
    /** Счётчик локальных id (уникальность без crypto). */
    seq: 0,
    /** upload_id → item: по нему WS-события находят карточку. */
    byUploadId: new Map(),
    /** Вложенность dragenter/dragleave — иначе оверлей мигает. */
    dragDepth: 0,
};

// ---------------------------------------------------------------------------
// Публичный API
// ---------------------------------------------------------------------------

/**
 * Навешивает обработчики выбора файлов. Идемпотентна.
 *
 * Вызывается один раз из main.initTelegram() — вместе с остальными
 * обработчиками вкладки.
 */
export function initUpload() {
    if (local.bound) return;
    local.bound = true;
    bindPicker();
    bindDragAndDrop();
    bindPaste();
}

/**
 * Кладёт файлы в очередь отправки.
 *
 * Подпись по умолчанию берётся из #tg-input и поле очищается: пользователь
 * набрал текст и приложил файл — он ждёт ОДНО сообщение с подписью, а не
 * текст отдельно и файл отдельно. Явный `opts.caption` (в том числе пустая
 * строка) отменяет это поведение.
 *
 * Цель ответа снимается ЗДЕСЬ, а не в момент реальной отправки: очередь
 * строго последовательная, и пока до файла дойдёт ход, пользователь успеет и
 * полосу закрыть, и выбрать другое сообщение. Захватываем намерение на
 * момент действия — ровно как подпись.
 *
 * @param {Iterable<File>} files
 * @param {{caption?: string, replyToId?: number}} [opts] — подпись достаётся
 *   ПЕРВОМУ файлу; `replyToId` перекрывает текущую цель ответа из reply.js
 * @returns {number} сколько файлов принято
 */
export function sendFiles(files, opts) {
    const list = Array.from(files || []).filter(Boolean);
    if (!list.length) return 0;

    const dialogId = tgState.activeDialogId ? String(tgState.activeDialogId) : null;
    if (!dialogId) {
        chat.systemLine('no dialog selected — файл отправлять некуда');
        return 0;
    }

    let caption = (opts && typeof opts.caption === 'string')
        ? opts.caption
        : takeCaption();
    // Явный opts.replyToId — не полоса, а вызов из кода: гасить чужую полосу
    // по такому вызову нельзя, поэтому источник запоминаем.
    const fromBar = !(opts && Number.isFinite(opts.replyToId));
    const replyToId = fromBar ? activeReplyToId(dialogId) : opts.replyToId;
    let accepted = 0;
    for (const file of list) {
        const reason = uploadRejectReason(file, MAX_UPLOAD_BYTES);
        if (reason) {
            // Отказ ДО сети: гнать заведомо отвергнутый файл незачем.
            chat.systemLine(`не отправлено (${file.name || 'file'}): ${reason}`);
            continue;
        }
        enqueue(file, dialogId, caption, replyToId);
        // Подпись уходит только с первым файлом: Telegram привязывает её к
        // одному медиа, и дублировать её у каждого файла — спам.
        caption = '';
        // А вот reply_to_id сохраняется у ВСЕХ файлов пачки, в отличие от
        // подписи: подпись, повторённая трижды, — спам, а ответ тремя файлами
        // на одно сообщение и должен выглядеть как три ответа (так же ведут
        // себя официальные клиенты с альбомом).
        accepted += 1;
    }
    if (accepted) {
        // Полоса снимается ТОЛЬКО когда файл реально принят в очередь: если
        // всё отвергнуто до сети (размер, пустой файл), пользователь ничего не
        // отправил, и терять выбранный ответ ему не за что.
        //
        // Снимает её chat.cancelReply(), а не reply.clearReplyTarget(): цель
        // живёт в reply.js, а узел полосы — в chat.js, и «сбросить половину»
        // означает полосу, висящую над пустым состоянием.
        if (fromBar && replyToId !== null && typeof chat.cancelReply === 'function') {
            chat.cancelReply();
        }
        pump();
    }
    return accepted;
}

/**
 * WS `upload_progress` — вторая нога прогресса (devforge → Telegram).
 *
 * События приходят с сервера всем вкладкам без фильтрации, поэтому чужие
 * (или уже завершённые) `upload_id` молча игнорируются.
 *
 * @param {{upload_id: string, sent: number, total: number}} evt
 */
export function onUploadProgress(evt) {
    if (!evt || !evt.upload_id) return;
    const item = local.byUploadId.get(String(evt.upload_id));
    if (!item || item.stage === 'sent' || item.stage === 'failed') return;
    const total = Number.isFinite(evt.total) && evt.total > 0 ? evt.total : item.size;
    setStage(item, 'tg', Number(evt.sent) || 0, total);
}

/**
 * Снимает всё, что осталось от отправки (смена аккаунта, уход со страницы).
 *
 * Активный XHR обрывается: без abort() браузер продолжал бы гнать гигабайт
 * уже никому не нужного файла. Незаконченный второй этап специально НЕ
 * «додавливается» — staging на сервере подберёт GC по TTL.
 */
export function teardownUpload() {
    for (const item of local.queue) {
        abortItem(item);
        if (item.row && item.row.parentNode) item.row.remove();
    }
    local.queue.length = 0;
    local.byUploadId.clear();
    local.pumping = false;
    local.dragDepth = 0;
    hideDropzone();
}

/** Снимок состояния для e2e/диагностики. */
export function _debugUploadState() {
    return {
        queued: local.queue.length,
        pumping: local.pumping,
        tracked: local.byUploadId.size,
        dragDepth: local.dragDepth,
        stages: local.queue.map((i) => i.stage),
        // Цели ответа видны снаружи: иначе «файл ушёл не тем ответом»
        // проверяется только по сетевому трафику.
        replies: local.queue.map((i) => i.replyToId),
    };
}

// ---------------------------------------------------------------------------
// Способы выбрать файл
// ---------------------------------------------------------------------------

/** Кнопка ⇪ открывает скрытый input[type=file]. */
function bindPicker() {
    if (!$tgAttach || !$tgFileInput) return;
    $tgAttach.addEventListener('click', (e) => {
        e.preventDefault();
        $tgFileInput.click();
    });
    $tgFileInput.addEventListener('change', () => {
        const files = Array.from($tgFileInput.files || []);
        // value сбрасывается ДО отправки: иначе повторный выбор того же файла
        // не вызовет `change`, и пользователю покажется, что кнопка сломалась.
        $tgFileInput.value = '';
        if (files.length) sendFiles(files);
    });
}

/**
 * Drag&drop по всей панели.
 *
 * `preventDefault` на dragover ОБЯЗАТЕЛЕН: без него браузер обработает drop
 * сам и откроет файл вместо страницы. Счётчик вложенности нужен потому, что
 * dragenter/dragleave стреляют на каждом дочернем узле — без него оверлей
 * мигал бы при движении курсора над списком диалогов.
 */
function bindDragAndDrop() {
    if (!$telegram) return;
    $telegram.addEventListener('dragenter', (e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        local.dragDepth += 1;
        showDropzone();
    });
    $telegram.addEventListener('dragover', (e) => {
        if (!hasFiles(e)) return;
        // Без этого drop не случится вовсе (спецификация HTML5 DnD).
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
    });
    $telegram.addEventListener('dragleave', (e) => {
        if (!hasFiles(e)) return;
        local.dragDepth = Math.max(0, local.dragDepth - 1);
        if (local.dragDepth === 0) hideDropzone();
    });
    $telegram.addEventListener('drop', (e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        local.dragDepth = 0;
        hideDropzone();
        const files = e.dataTransfer ? Array.from(e.dataTransfer.files || []) : [];
        if (files.length) sendFiles(files);
    });
}

/**
 * Вставка из буфера обмена в #tg-input.
 *
 * Перехватываем ТОЛЬКО когда в буфере есть файл: обычный текст обязан
 * вставляться как текст, иначе Ctrl+V перестанет работать по назначению.
 * Скриншот приезжает без имени — генерируем `pasted_<дата>_<время>.png`.
 */
function bindPaste() {
    if (!$tgInput) return;
    $tgInput.addEventListener('paste', (e) => {
        const dt = e.clipboardData;
        if (!dt) return;
        const files = clipboardFiles(dt);
        if (!files.length) return; // только текст — не мешаем
        e.preventDefault();
        sendFiles(files);
    });
}

/**
 * Файлы из буфера обмена с восстановленными именами.
 *
 * @param {DataTransfer} dt
 * @returns {Array<File>}
 */
export function clipboardFiles(dt) {
    const out = [];
    const files = Array.from((dt && dt.files) || []);
    if (files.length) {
        for (const f of files) out.push(renameIfAnonymous(f));
        return out;
    }
    // Скриншот из буфера в некоторых браузерах приходит только через items.
    const items = Array.from((dt && dt.items) || []);
    for (const it of items) {
        if (it.kind !== 'file') continue;
        const f = typeof it.getAsFile === 'function' ? it.getAsFile() : null;
        if (f) out.push(renameIfAnonymous(f));
    }
    return out;
}

/**
 * Даёт имя файлу из буфера, если браузер его не дал (или дал общее
 * «image.png» — такое имя у каждого скриншота, и в Telegram они сливаются).
 *
 * @param {File} file
 * @returns {File|{name: string, size: number, type: string, blob: File}}
 */
function renameIfAnonymous(file) {
    const name = String(file.name || '');
    const generic = !name || name === 'image.png' || name === 'blob';
    if (!generic) return file;
    const nice = pastedFileName(file.type || 'image/png');
    // Пересобрать File можно только через конструктор; там, где его нет
    // (очень старый браузер), кладём обёртку с нужным именем и исходным
    // blob'ом — XHR всё равно отправляет `item.file`.
    try {
        return new File([file], nice, { type: file.type || 'image/png' });
    } catch (_) {
        return { name: nice, size: file.size, type: file.type, blob: file };
    }
}

/** Есть ли в переносе файлы (а не текст/ссылка). */
function hasFiles(e) {
    const dt = e && e.dataTransfer;
    if (!dt) return false;
    const types = Array.from(dt.types || []);
    return types.includes('Files');
}

function showDropzone() {
    if ($tgDropzone) $tgDropzone.hidden = false;
}

function hideDropzone() {
    if ($tgDropzone) $tgDropzone.hidden = true;
}

/**
 * Забирает текст из prompt'а как подпись к файлу и очищает поле.
 *
 * Пользователь набрал текст и приложил файл — он ждёт ОДНО сообщение с
 * подписью, а не текст отдельно и файл отдельно.
 */
function takeCaption() {
    if (!$tgInput) return '';
    const text = String($tgInput.value || '');
    if (!text.trim()) return '';
    $tgInput.value = '';
    // Набор текста мог зажечь индикатор «печатает…» — гасим его, как это
    // делает обычная отправка (chat.js).
    if (typeof chat.cancelTyping === 'function') chat.cancelTyping();
    return text;
}

/**
 * Текущая цель ответа для диалога — то, что показывает полоса над prompt'ом.
 *
 * Проверку «цель из ЭТОГО диалога» делает сам reply.js: пока файл стоял в
 * очереди, пользователь мог уйти в другой чат, и ответ туда уходить не должен.
 *
 * Ошибка здесь не имеет права ронять отправку: ответ — необязательная деталь
 * сообщения, и «файл не отправился, потому что не прочиталась полоса» —
 * несоразмерная плата.
 *
 * @param {string} dialogId
 * @returns {?number} message_id оригинала либо null
 */
function activeReplyToId(dialogId) {
    if (typeof replyTargetId !== 'function') return null;
    let id = null;
    try {
        id = replyTargetId(dialogId);
    } catch (_) {
        return null;
    }
    // id <= 0 — «Telegram не подтвердил номер» (optimistic-строка): ответ на
    // несуществующий message_id сервер отвергнет с 400.
    return Number.isFinite(id) && id > 0 ? id : null;
}

// ---------------------------------------------------------------------------
// Очередь
// ---------------------------------------------------------------------------

/**
 * Добавляет файл в очередь и рисует его карточку.
 *
 * `replyToId` фиксируется в самом item'е, а не читается перед отправкой:
 * очередь последовательная, и к моменту, когда до файла дойдёт ход, полоса
 * ответа давно может показывать другое сообщение. Побочная выгода — retry
 * (showRetry) переиспользует тот же item и отвечает туда же, куда собирался
 * пользователь, а не туда, куда указывает полоса сейчас.
 */
function enqueue(file, dialogId, caption, replyToId) {
    local.seq += 1;
    const mime = uploadMime(file.name, file.type);
    const item = {
        localId: `up-${Date.now()}-${local.seq}`,
        file: file.blob || file,
        name: String(file.name || 'file'),
        size: Number(file.size) || 0,
        mime,
        kind: uploadKind(mime),
        dialogId: String(dialogId),
        caption: String(caption || ''),
        replyToId: Number.isFinite(replyToId) && replyToId > 0 ? replyToId : null,
        stage: 'queued',
        uploadId: null,
        xhr: null,
        aborted: false,
        row: null,
        els: null,
    };
    buildRow(item);
    local.queue.push(item);
    refreshQueueLabels();
    return item;
}

/**
 * Прокачивает очередь по одному файлу.
 *
 * Реентерабельность защищена флагом `pumping`: и `sendFiles`, и retry могут
 * позвать pump() в любой момент.
 */
async function pump() {
    if (local.pumping) return;
    local.pumping = true;
    try {
        while (local.queue.length) {
            const item = local.queue[0];
            if (item.stage === 'failed') {
                // Ждём решения пользователя (retry / просто оставить).
                break;
            }
            await runItem(item);
            // Успех и отмена убирают элемент; провал оставляет его первым,
            // чтобы порядок сообщений не перемешался.
            if (local.queue[0] === item && item.stage !== 'failed') {
                local.queue.shift();
                refreshQueueLabels();
            } else if (local.queue[0] === item) {
                break;
            }
        }
    } finally {
        local.pumping = false;
    }
}

/** Полный цикл одного файла: LOCAL → TG → SENT. */
async function runItem(item) {
    setStage(item, 'local', 0, item.size);
    try {
        const staged = await uploadToServer(item);
        if (item.aborted) return;
        item.uploadId = String(staged.upload_id || '');
        if (!item.uploadId) throw new Error('сервер не вернул upload_id');
        local.byUploadId.set(item.uploadId, item);
        // Вторая нога: до первого WS-события показываем «TG 0%», иначе
        // карточка выглядела бы зависшей на «LOCAL 100%».
        setStage(item, 'tg', 0, item.size);

        const resp = await sendMessage(item.dialogId, {
            uploadId: item.uploadId,
            caption: item.caption,
            // null сюда попасть может, и это норма: api.js кладёт поле в тело
            // только для конечного числа, поэтому «ответа нет» = поля нет.
            replyToId: item.replyToId,
        });
        if (item.aborted) return;
        finishItem(item, resp && resp.message);
    } catch (e) {
        if (item.aborted) {
            cleanupItem(item);
            return;
        }
        failItem(item, e);
    }
}

/** Заливка на devforge через XHR (единственный источник LOCAL-прогресса). */
function uploadToServer(item) {
    return new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        item.xhr = xhr;
        xhr.open('POST', UPLOAD_PATH, true);
        // Content-Length браузер выставит сам по размеру File — вручную его
        // ставить запрещено (forbidden header), а сервер без него ответит 411.
        xhr.setRequestHeader('Content-Type', 'application/octet-stream');
        xhr.setRequestHeader(FILE_NAME_HEADER, encodeURIComponent(item.name));
        // Bearer вручную: XHR не проходит через подменённый fetch (core/auth.js).
        const token = getAuthToken();
        if (token) xhr.setRequestHeader('Authorization', 'Bearer ' + token);

        xhr.upload.onprogress = (e) => {
            const total = e.lengthComputable && e.total > 0 ? e.total : item.size;
            setStage(item, 'local', e.loaded, total);
        };
        xhr.onload = () => {
            item.xhr = null;
            if (xhr.status >= 200 && xhr.status < 300) {
                try {
                    resolve(JSON.parse(xhr.responseText || '{}'));
                } catch (_) {
                    reject(new Error('сервер вернул неразбираемый ответ'));
                }
                return;
            }
            reject(errorFromXhr(xhr));
        };
        xhr.onerror = () => {
            item.xhr = null;
            reject(new Error('обрыв соединения при заливке файла'));
        };
        xhr.onabort = () => {
            item.xhr = null;
            const err = new Error('отправка отменена');
            err.aborted = true;
            reject(err);
        };
        xhr.send(item.file);
    });
}

/** Ошибка XHR в едином виде контракта плагина. */
function errorFromXhr(xhr) {
    const parsed = parseApiError(xhr.status, xhr.responseText);
    const err = new Error(parsed.message);
    err.status = parsed.status;
    err.code = parsed.code;
    err.retryAfterSecs = parsed.retryAfterSecs;
    return err;
}

/** Успешная отправка: карточка снимается, ленту дорисовывает chat.js. */
function finishItem(item, dto) {
    setStage(item, 'sent', item.size, item.size);
    cleanupItem(item);
    if (item.row && item.row.parentNode) item.row.remove();
    if (dto && Number.isFinite(dto.id) && dto.id > 0) {
        // Тот же путь, что у входящих: insertMessage дедуплицирует, если
        // WS-апдейт про наше сообщение опередил ответ POST.
        chat.onNewMessage(dto);
    }
}

/** Провал любого этапа: «!! FAILED» + кнопка повтора. */
function failItem(item, err) {
    setStage(item, 'failed');
    cleanupItem(item);
    const code = err && err.code;
    const extra = code === 'flood_wait' && err.retryAfterSecs
        ? ` (подождите ${err.retryAfterSecs}с)`
        : '';
    if (item.els) {
        item.els.status.title = `${(err && err.message) || err}${extra}`;
    }
    chat.systemLine(`не отправлено (${item.name}): ${(err && err.message) || err}${extra}`);
    showRetry(item);
}

/** Снимает временные ссылки (xhr, реестр upload_id). */
function cleanupItem(item) {
    if (item.uploadId) local.byUploadId.delete(item.uploadId);
    item.uploadId = null;
    item.xhr = null;
}

/** Обрывает активную заливку. */
function abortItem(item) {
    item.aborted = true;
    if (item.xhr && typeof item.xhr.abort === 'function') {
        try {
            item.xhr.abort();
        } catch (_) {
            /* уже завершён */
        }
    }
    item.xhr = null;
}

/** Убирает элемент из очереди (отмена или удавшийся повтор). */
function dropItem(item) {
    const idx = local.queue.indexOf(item);
    if (idx >= 0) local.queue.splice(idx, 1);
    refreshQueueLabels();
}

/** Обновляет подписи «QUEUED n/m» у ожидающих файлов. */
function refreshQueueLabels() {
    const total = local.queue.length;
    local.queue.forEach((item, i) => {
        if (item.stage !== 'queued' || !item.els) return;
        item.els.status.textContent = uploadQueueLabel(i + 1, total);
    });
}

// ---------------------------------------------------------------------------
// Карточка отправки
// ---------------------------------------------------------------------------

/**
 * Строка лога с карточкой отправляемого файла.
 *
 * Формат совпадает с обычным своим сообщением (`[HH:MM:SS] you@local $ …`),
 * поэтому очередь отправки читается как продолжение переписки, а не как
 * отдельная панель. Внутри — карточка с теми же классами, что у принятых
 * медиа (`.tg-media-*`), плюс `.tg-upload-card` для состояний отправки.
 *
 * Всё через textContent: имя файла приходит из ФС пользователя, но правило
 * «никакого innerHTML» в этом плагине действует без исключений.
 */
function buildRow(item) {
    const row = document.createElement('div');
    row.className = 'tg-line tg-line-own tg-msg-pending';
    row.dataset.uploadLocalId = item.localId;
    // Цель ответа помечена прямо на узле: цитату в optimistic-строке рисует
    // владелец полосы (reply.js) — ему нужен способ узнать, к чему эта строка
    // относится, не заглядывая в приватную очередь upload.js.
    if (item.replyToId) row.dataset.replyToId = String(item.replyToId);

    const time = document.createElement('span');
    time.className = 'tg-line-time';
    time.textContent = `[${formatTime(Math.floor(Date.now() / 1000))}] `;

    const nick = document.createElement('span');
    nick.className = 'tg-line-nick-own';
    nick.textContent = 'you@local';

    const sep = document.createElement('span');
    sep.className = 'tg-line-time';
    sep.textContent = ' $ ';

    const text = document.createElement('span');
    text.className = 'tg-line-text';
    const tag = `[${item.kind} ${item.name}]`;
    text.textContent = item.caption ? `${tag} ${item.caption}` : tag;

    row.appendChild(time);
    row.appendChild(nick);
    row.appendChild(sep);
    row.appendChild(text);

    const card = document.createElement('div');
    card.className = `tg-media-card tg-upload-card tg-upload-${item.kind}`;
    card.dataset.uploadLocalId = item.localId;

    const head = document.createElement('div');
    head.className = 'tg-media-head';
    const glyph = document.createElement('span');
    glyph.className = 'tg-media-glyph';
    glyph.textContent = mediaGlyph(item.kind);
    const name = document.createElement('span');
    name.className = 'tg-media-name';
    name.textContent = item.name;
    name.title = item.name;
    const kindLabel = document.createElement('span');
    kindLabel.className = 'tg-media-kind';
    kindLabel.textContent = mediaKindLabel(item.kind);
    head.appendChild(glyph);
    head.appendChild(name);
    head.appendChild(kindLabel);

    const foot = document.createElement('div');
    foot.className = 'tg-media-foot';
    const meta = document.createElement('span');
    meta.className = 'tg-media-meta';
    meta.textContent = formatBytes(item.size) || '—';
    const progress = document.createElement('span');
    progress.className = 'tg-media-progress';
    const bar = document.createElement('span');
    bar.className = 'tg-media-progress-bar';
    bar.style.width = '0%';
    progress.appendChild(bar);
    const status = document.createElement('span');
    status.className = 'tg-media-status';
    status.textContent = uploadQueueLabel(1, 1);
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'tg-media-btn tg-upload-cancel';
    cancel.textContent = 'CANCEL';
    cancel.title = 'Отменить отправку';
    cancel.addEventListener('click', () => cancelUpload(item));

    foot.appendChild(meta);
    foot.appendChild(progress);
    foot.appendChild(status);
    foot.appendChild(cancel);

    card.appendChild(head);
    card.appendChild(foot);
    row.appendChild(card);

    item.row = row;
    item.els = { card, progress, bar, status, cancel, meta };
    if ($tgLog) {
        $tgLog.appendChild(row);
        $tgLog.scrollTop = $tgLog.scrollHeight;
    }
    return row;
}

/**
 * Отмена пользователем.
 *
 * На первом этапе обрывается XHR. На втором (файл уже у devforge) сообщение
 * просто не отправляется: staging на сервере подберёт GC по TTL, а прерывать
 * заливку в Telegram посередине нам нечем — и не нужно.
 */
export function cancelUpload(item) {
    abortItem(item);
    cleanupItem(item);
    dropItem(item);
    if (item.row && item.row.parentNode) item.row.remove();
    chat.systemLine(`отправка отменена: ${item.name}`);
    // Очередь могла остановиться на этом элементе — двигаем дальше.
    pump();
}

/**
 * Кнопка повтора.
 *
 * Повтор начинается с ПЕРВОГО этапа: `upload_id` мог протухнуть по TTL,
 * а staging — быть уже подчищен, поэтому «догнать» вторую ногу нельзя.
 */
function showRetry(item) {
    if (!item.els) return;
    if (item.els.retry) item.els.retry.remove();
    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'tg-media-btn tg-media-retry';
    retry.textContent = 'RETRY';
    retry.title = 'Отправить файл заново';
    retry.addEventListener('click', () => {
        retry.remove();
        item.els.retry = null;
        item.aborted = false;
        item.stage = 'queued';
        setStage(item, 'queued');
        pump();
    });
    item.els.status.after(retry);
    item.els.retry = retry;
}

/**
 * Единственная точка изменения визуального состояния карточки.
 *
 * `queued|local|tg` показывают полосу, `sent|failed` её гасят. Проценты
 * считает `progressPercent` из format.js — тот же расчёт, что у приёма медиа,
 * поэтому «99 %» до финального события ведёт себя одинаково в обе стороны.
 */
function setStage(item, stage, sent, total) {
    item.stage = stage;
    if (!item.els) return;
    const { progress, bar, status, cancel } = item.els;
    status.classList.remove('tg-media-done', 'tg-media-failed');
    status.title = '';

    if (stage === 'sent' || stage === 'failed') {
        progress.hidden = true;
        if (cancel) cancel.hidden = true;
        status.classList.add(stage === 'sent' ? 'tg-media-done' : 'tg-media-failed');
        status.textContent = uploadStageLabel(stage);
        if (item.row) {
            item.row.classList.toggle('tg-msg-pending', false);
            item.row.classList.toggle('tg-upload-failed', stage === 'failed');
        }
        return;
    }

    progress.hidden = false;
    if (cancel) cancel.hidden = false;
    const pct = progressPercent(sent, total);
    bar.style.width = pct === null ? '100%' : `${pct}%`;
    progress.classList.toggle('tg-media-progress-indeterminate', pct === null);
    status.textContent = stage === 'queued'
        ? uploadQueueLabel(1, Math.max(1, local.queue.length))
        : uploadStageLabel(stage, sent, total);
}
