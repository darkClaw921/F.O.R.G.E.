// tmux-web — VK plugin: отправка файлов.
//
// Три способа выбрать файл, одна очередь, два этапа прогресса:
//
//   ⇪ #vk-attach → скрытый #vk-file-input
//   drag&drop над #vk → оверлей #vk-dropzone
//   Ctrl+V в #vk-input (скриншот из буфера)
//        │
//        ▼
//   очередь ПАЧЕК (строго последовательно, пачка = одно сообщение)
//        │
//   [1] LOCAL %  браузер → devforge   (XMLHttpRequest.upload.onprogress)
//        │       POST /api/vk/upload, тело = сырой File
//        ▼       ответ: { upload_id, file_name, size, mime }
//   [2] VK %     devforge → VK        (WS upload_progress по upload_id)
//        │       POST /api/vk/dialogs/:id/messages { upload_ids: [...], text }
//        ▼
//   ✓ SENT → карточки снимаются, ленту дорисовывает chat.onNewMessage
//
// ЧЕМ ОТЛИЧАЕТСЯ ОТ ТЕЛЕГРАМНОГО БЛИЗНЕЦА — ровно три вещи:
//
// 1. ОДНО СООБЩЕНИЕ НЕСЁТ НЕСКОЛЬКО ФАЙЛОВ. У Telegram каждый файл — своё
//    сообщение; у VK пять фотографий уезжают ОДНИМ multipart-POST, одним
//    сохранением и одним messages.send — три запроса вместо пятнадцати при
//    лимите 3 rps. Поэтому очередь состоит из ПАЧЕК: пачка собирает
//    `uploadIds[]` и отправляется одним `sendMessage`.
// 2. ЭТАП «VK %» ИДЁТ ВО ВРЕМЯ POST /messages, А НЕ ВО ВРЕМЯ POST /upload.
//    Upload-сервер VK выдаётся под конкретный peer_id, а на шаге /upload
//    адресат ещё неизвестен, поэтому весь трёхшаговый конвейер живёт внутри
//    отправки сообщения. Следствие для этого модуля: слушать кадры
//    `upload_progress` надо ПОСЛЕ нажатия «отправить», и индикатор не имеет
//    права гаснуть, пока сообщение не подтверждено.
// 3. ЛИМИТ РАЗМЕРА ЗАВИСИТ ОТ ВИДА ВЛОЖЕНИЯ (`UPLOAD_LIMITS` в format.js):
//    у фотографии он жёстче, чем у документа, и одно число дало бы либо
//    ложный отказ, либо пропущенный.
//
// Почему XHR, а не fetch: у fetch НЕТ прогресса отправки (ReadableStream в
// body поддерживается не всеми браузерами и требует half-duplex HTTP/2).
// XHR даёт `upload.onprogress` из коробки.
//
// ВАЖНО: XHR идёт МИМО подменённого window.fetch из core/auth.js, поэтому
// заголовок Authorization в remote-mode ставится ВРУЧНУЮ (getAuthToken).
// Без этого отправка файлов работала бы только на localhost. Это конкретный
// исправленный дефект, а не стилистика.
//
// Очередь строго последовательная: две параллельные заливки делят один канал,
// из-за чего обе идут вдвое медленнее, а кадры upload_progress перемешиваются
// в статусбарах карточек.

import { $vk, $vkAttach, $vkDropzone, $vkFileInput, $vkInput, $vkLog } from '../core/dom.js';
import { getAuthToken } from '../core/auth.js';

import { sendMessage } from './api.js';
import * as chat from './chat.js';
// Состояние «на какое сообщение отвечаем» живёт в reply.js и ТОЛЬКО там:
// второй владелец у него появиться не должен, иначе полоса над полем ввода и
// реально уходящий reply_to_cmid разъедутся. Отсюда — только чтение цели;
// снимает её chat.cancelReply(), потому что у полосы две половины (цель в
// reply.js и DOM-узел в chat.js) и разъединять их нельзя.
import { replyTargetCmid } from './reply.js';
import {
    formatBytes, formatTime, mediaGlyph, mediaKindLabel, parseApiError, pastedFileName,
    progressPercent, uploadCardKind, uploadKind, uploadMime, uploadQueueLabel, uploadRejectReason,
    uploadStageLabel,
} from './format.js';
import { PRIORITY_USER, vkLimiter } from './ratelimit.js';
import { vkState } from './state.js';

/** Путь загрузки (совпадает с исключением csrf_guard в tmux-web/src/auth.rs). */
export const UPLOAD_PATH = '/api/vk/upload';

/** Заголовок с именем файла: percent-encoded, потому что заголовки — ASCII. */
export const FILE_NAME_HEADER = 'X-Forge-File-Name';

/**
 * Сколько вложений VK принимает в ОДНОМ сообщении.
 *
 * Совпадает с серверным `MAX_ATTACHMENTS`: больше десяти сервер отвергает
 * `400 invalid_input`, и гнать их по сети незачем — лишнее просто уезжает
 * следующей пачкой, то есть следующим сообщением.
 */
export const MAX_PER_MESSAGE = 10;

const local = {
    /** Обработчики уже навешены. */
    bound: false,
    /** Очередь ПАЧЕК: [0] — та, что отправляется прямо сейчас. */
    queue: [],
    /** Идёт ли обработка очереди. */
    pumping: false,
    /** Счётчик локальных id (уникальность без crypto). */
    seq: 0,
    /** upload_id → item: по нему кадры WS находят карточку. */
    byUploadId: new Map(),
    /** Вложенность dragenter/dragleave — иначе оверлей мигает. */
    dragDepth: 0,
    /**
     * Поколение очереди; растёт на каждом [`teardownUpload`].
     *
     * Без него `teardown` посреди летящего запроса оставлял бы ЗОМБИ-цикл:
     * `pumping` снят, а старый `pump()` всё ещё висит на `await` и, проснувшись,
     * берётся за пачки следующего поколения — параллельно с новым циклом. Файл
     * при этом заливается дважды.
     */
    epoch: 0,
};

// ---------------------------------------------------------------------------
// Публичный API
// ---------------------------------------------------------------------------

/**
 * Навешивает обработчики выбора файлов. Идемпотентна.
 *
 * Вызывается один раз из main.initVk() — вместе с остальными обработчиками
 * вкладки.
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
 * Все принятые файлы одного вызова становятся ОДНИМ сообщением (пачкой) — так
 * работает альбом VK. Если файлов больше [`MAX_PER_MESSAGE`], пачек будет
 * несколько, и каждая уедет своим сообщением.
 *
 * Подпись по умолчанию берётся из #vk-input и поле очищается: пользователь
 * набрал текст и приложил файл — он ждёт ОДНО сообщение с подписью, а не текст
 * отдельно и файл отдельно. Явный `opts.caption` (в том числе пустая строка)
 * отменяет это поведение.
 *
 * Цель ответа снимается ЗДЕСЬ, а не в момент реальной отправки: очередь строго
 * последовательная, и пока до пачки дойдёт ход, пользователь успеет и полосу
 * закрыть, и выбрать другое сообщение. Захватываем намерение на момент
 * действия — ровно как подпись.
 *
 * @param {Iterable<File>} files
 * @param {{caption?: string, replyToCmid?: number}} [opts]
 * @returns {number} сколько файлов принято
 */
export function sendFiles(files, opts) {
    const list = Array.from(files || []).filter(Boolean);
    if (!list.length) return 0;

    const dialogId = vkState.activeDialogId ? String(vkState.activeDialogId) : null;
    if (!dialogId) {
        chat.systemLine('no dialog selected — файл отправлять некуда');
        return 0;
    }

    const caption = (opts && typeof opts.caption === 'string')
        ? opts.caption
        : takeCaption();
    // Явный opts.replyToCmid — не полоса, а вызов из кода: гасить чужую полосу
    // по такому вызову нельзя, поэтому источник запоминаем.
    const fromBar = !(opts && Number.isFinite(opts.replyToCmid));
    const replyToCmid = fromBar ? activeReplyToCmid(dialogId) : opts.replyToCmid;

    const accepted = [];
    for (const file of list) {
        // Лимит выбирается ПО ВИДУ вложения: у фотографии он жёстче, чем у
        // документа (см. UPLOAD_LIMITS).
        const reason = uploadRejectReason(file);
        if (reason) {
            // Отказ ДО сети: гнать заведомо отвергнутый файл незачем.
            chat.systemLine(`не отправлено (${file.name || 'file'}): ${reason}`);
            continue;
        }
        accepted.push(file);
    }
    if (!accepted.length) return 0;

    // Подпись уходит только с ПЕРВОЙ пачкой: повторённая у каждого сообщения,
    // она выглядит спамом. А вот цель ответа сохраняется у всех пачек — так же
    // ведут себя официальные клиенты, отвечая альбомом.
    let text = caption;
    for (let i = 0; i < accepted.length; i += MAX_PER_MESSAGE) {
        enqueueBatch(accepted.slice(i, i + MAX_PER_MESSAGE), dialogId, text, replyToCmid);
        text = '';
    }

    // Полоса снимается ТОЛЬКО когда файл реально принят в очередь: если всё
    // отвергнуто до сети (размер, пустой файл), пользователь ничего не
    // отправил, и терять выбранный ответ ему не за что.
    //
    // Снимает её chat.cancelReply(), а не reply.clearReplyTarget(): цель живёт
    // в reply.js, а узел полосы — в chat.js, и «сбросить половину» означает
    // полосу, висящую над пустым состоянием.
    if (fromBar && replyToCmid !== null && typeof chat.cancelReply === 'function') {
        chat.cancelReply();
    }
    pump();
    return accepted.length;
}

/**
 * WS `upload_progress` — вторая нога прогресса (devforge → VK).
 *
 * ВАЖНО: у VK эти кадры приходят ВО ВРЕМЯ `POST /messages`, а не во время
 * `POST /upload` — конвейер VK трёхшаговый и живёт внутри отправки. Поэтому
 * слушать их надо и после нажатия «отправить».
 *
 * События приходят с сервера всем вкладкам без фильтрации, поэтому чужие (или
 * уже завершённые) `upload_id` молча игнорируются.
 *
 * @param {{upload_id: string, sent: number, total: number}} evt
 */
export function onUploadProgress(evt) {
    if (!evt || !evt.upload_id) return;
    const item = local.byUploadId.get(String(evt.upload_id));
    if (!item || item.stage === 'sent' || item.stage === 'failed') return;
    const total = Number.isFinite(evt.total) && evt.total > 0 ? evt.total : item.size;
    setStage(item, 'vk', Number(evt.sent) || 0, total);
}

/**
 * Снимает всё, что осталось от отправки (смена аккаунта, уход со страницы).
 *
 * Активный XHR обрывается: без abort() браузер продолжал бы гнать гигабайт уже
 * никому не нужного файла. Незаконченный второй этап специально НЕ
 * «додавливается» — staging на сервере подберёт GC по TTL.
 */
export function teardownUpload() {
    for (const batch of local.queue) {
        for (const item of batch.items) {
            abortItem(item);
            if (item.row && item.row.parentNode) item.row.remove();
        }
    }
    local.queue.length = 0;
    local.byUploadId.clear();
    local.pumping = false;
    // Поколение сменилось: летящий `pump()` увидит это и выйдет, не тронув
    // пачки, поставленные после teardown.
    local.epoch += 1;
    local.dragDepth = 0;
    hideDropzone();
}

/** Снимок состояния для e2e/диагностики. */
export function _debugUploadState() {
    return {
        batches: local.queue.length,
        queued: local.queue.reduce((n, b) => n + b.items.length, 0),
        pumping: local.pumping,
        tracked: local.byUploadId.size,
        dragDepth: local.dragDepth,
        stages: local.queue.map((b) => b.items.map((i) => i.stage)),
        // Цели ответа видны снаружи: иначе «файл ушёл не тем ответом»
        // проверяется только по сетевому трафику.
        replies: local.queue.map((b) => b.replyToCmid),
    };
}

// ---------------------------------------------------------------------------
// Способы выбрать файл
// ---------------------------------------------------------------------------

/** Кнопка ⇪ открывает скрытый input[type=file]. */
function bindPicker() {
    if (!$vkAttach || !$vkFileInput) return;
    $vkAttach.addEventListener('click', (e) => {
        e.preventDefault();
        $vkFileInput.click();
    });
    $vkFileInput.addEventListener('change', () => {
        const files = Array.from($vkFileInput.files || []);
        // value сбрасывается ДО отправки: иначе повторный выбор того же файла
        // не вызовет `change`, и пользователю покажется, что кнопка сломалась.
        $vkFileInput.value = '';
        if (files.length) sendFiles(files);
    });
}

/**
 * Drag&drop по всей панели.
 *
 * `preventDefault` на dragover ОБЯЗАТЕЛЕН: без него браузер обработает drop сам
 * и откроет файл вместо страницы. Счётчик вложенности нужен потому, что
 * dragenter/dragleave стреляют на каждом дочернем узле — без него оверлей мигал
 * бы при движении курсора над списком бесед.
 */
function bindDragAndDrop() {
    if (!$vk) return;
    $vk.addEventListener('dragenter', (e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        local.dragDepth += 1;
        showDropzone();
    });
    $vk.addEventListener('dragover', (e) => {
        if (!hasFiles(e)) return;
        // Без этого drop не случится вовсе (спецификация HTML5 DnD).
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
    });
    $vk.addEventListener('dragleave', (e) => {
        if (!hasFiles(e)) return;
        local.dragDepth = Math.max(0, local.dragDepth - 1);
        if (local.dragDepth === 0) hideDropzone();
    });
    $vk.addEventListener('drop', (e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        local.dragDepth = 0;
        hideDropzone();
        const files = e.dataTransfer ? Array.from(e.dataTransfer.files || []) : [];
        if (files.length) sendFiles(files);
    });
}

/**
 * Вставка из буфера обмена в #vk-input.
 *
 * Перехватываем ТОЛЬКО когда в буфере есть файл: обычный текст обязан
 * вставляться как текст, иначе Ctrl+V перестанет работать по назначению.
 * Скриншот приезжает без имени — генерируем `pasted_<дата>_<время>.png`.
 */
function bindPaste() {
    if (!$vkInput) return;
    $vkInput.addEventListener('paste', (e) => {
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
 * «image.png» — такое имя у каждого скриншота, и в переписке они сливаются).
 *
 * @param {File} file
 * @returns {File|{name: string, size: number, type: string, blob: File}}
 */
function renameIfAnonymous(file) {
    const name = String(file.name || '');
    const generic = !name || name === 'image.png' || name === 'blob';
    if (!generic) return file;
    const nice = pastedFileName(file.type || 'image/png');
    // Пересобрать File можно только через конструктор; там, где его нет (очень
    // старый браузер), кладём обёртку с нужным именем и исходным blob'ом — XHR
    // всё равно отправляет `item.file`.
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
    if ($vkDropzone) $vkDropzone.hidden = false;
}

function hideDropzone() {
    if ($vkDropzone) $vkDropzone.hidden = true;
}

/**
 * Забирает текст из prompt'а как подпись к файлу и очищает поле.
 *
 * Пользователь набрал текст и приложил файл — он ждёт ОДНО сообщение с
 * подписью, а не текст отдельно и файл отдельно.
 */
function takeCaption() {
    if (!$vkInput) return '';
    const text = String($vkInput.value || '');
    if (!text.trim()) return '';
    $vkInput.value = '';
    // Набор текста мог зажечь индикатор «печатает…» — гасим его, как это делает
    // обычная отправка (chat.js).
    if (typeof chat.cancelTyping === 'function') chat.cancelTyping();
    return text;
}

/**
 * Текущая цель ответа для беседы — то, что показывает полоса над prompt'ом.
 *
 * Адрес ответа у VK — `conversation_message_id`: именно он одинаков у всех
 * участников беседы и именно его несёт каждая цитата.
 *
 * Ошибка здесь не имеет права ронять отправку: ответ — необязательная деталь
 * сообщения, и «файл не отправился, потому что не прочиталась полоса» —
 * несоразмерная плата.
 *
 * @param {string} dialogId
 * @returns {?number} cmid оригинала либо null
 */
function activeReplyToCmid(dialogId) {
    if (typeof replyTargetCmid !== 'function') return null;
    let cmid = null;
    try {
        cmid = replyTargetCmid(dialogId);
    } catch (_) {
        return null;
    }
    // cmid <= 0 — «VK не подтвердил номер» (optimistic-строка): ответ на
    // несуществующий cmid сервер отвергнет.
    return Number.isFinite(cmid) && cmid > 0 ? cmid : null;
}

// ---------------------------------------------------------------------------
// Очередь пачек
// ---------------------------------------------------------------------------

/**
 * Добавляет пачку файлов (= одно будущее сообщение) в очередь и рисует
 * карточки.
 *
 * `replyToCmid` и подпись фиксируются в самой пачке, а не читаются перед
 * отправкой: очередь последовательная, и к моменту, когда до неё дойдёт ход,
 * полоса ответа давно может показывать другое сообщение. Побочная выгода —
 * повтор (showRetry) переиспользует ту же пачку и отвечает туда же, куда
 * собирался пользователь.
 */
function enqueueBatch(files, dialogId, caption, replyToCmid) {
    local.seq += 1;
    const batch = {
        batchId: `up-${Date.now()}-${local.seq}`,
        dialogId: String(dialogId),
        caption: String(caption || ''),
        replyToCmid: Number.isFinite(replyToCmid) && replyToCmid > 0 ? replyToCmid : null,
        items: [],
    };
    for (const file of files) {
        batch.items.push(makeItem(batch, file));
    }
    local.queue.push(batch);
    refreshQueueLabels();
    return batch;
}

/** Один файл внутри пачки. */
function makeItem(batch, file) {
    local.seq += 1;
    const mime = uploadMime(file.name, file.type);
    const item = {
        localId: `${batch.batchId}-${local.seq}`,
        batch,
        file: file.blob || file,
        name: String(file.name || 'file'),
        size: Number(file.size) || 0,
        mime,
        kind: uploadCardKind(uploadKind(mime)),
        stage: 'queued',
        uploadId: null,
        xhr: null,
        aborted: false,
        row: null,
        els: null,
    };
    buildRow(item);
    return item;
}

/**
 * Прокачивает очередь по одной ПАЧКЕ.
 *
 * Реентерабельность защищена флагом `pumping`: и `sendFiles`, и повтор могут
 * позвать pump() в любой момент.
 */
async function pump() {
    if (local.pumping) return;
    local.pumping = true;
    // Поколение фиксируется на входе: проснувшись после teardown, цикл обязан
    // выйти, а не взяться за чужие пачки (см. `local.epoch`).
    const epoch = local.epoch;
    try {
        while (local.queue.length && epoch === local.epoch) {
            const batch = local.queue[0];
            if (batch.items.some((i) => i.stage === 'failed')) {
                // Ждём решения пользователя (повтор / просто оставить).
                break;
            }
            if (!batch.items.length) {
                local.queue.shift();
                refreshQueueLabels();
                continue;
            }
            await runBatch(batch);
            // Успех и отмена убирают пачку; провал оставляет её первой, чтобы
            // порядок сообщений не перемешался.
            if (local.queue[0] !== batch) continue;
            if (batch.items.some((i) => i.stage === 'failed')) break;
            local.queue.shift();
            refreshQueueLabels();
        }
    } finally {
        // Флаг снимает только «свой» цикл: чужой (нового поколения) уже
        // работает, и обнулить его флаг означало бы пустить третий.
        if (epoch === local.epoch) local.pumping = false;
    }
}

/**
 * Полный цикл одной пачки: LOCAL по каждому файлу → один sendMessage → SENT.
 *
 * Файлы заливаются последовательно (общий канал), а отправляются ОДНИМ
 * сообщением: у VK альбом из пяти фотографий стоит трёх запросов вместо
 * пятнадцати.
 */
async function runBatch(batch) {
    const uploadIds = [];
    try {
        for (const item of batch.items) {
            if (item.aborted) continue;
            setStage(item, 'local', 0, item.size);
            const staged = await uploadToServer(item);
            if (item.aborted) continue;
            item.uploadId = String((staged && staged.upload_id) || '');
            if (!item.uploadId) throw new Error('сервер не вернул upload_id');
            local.byUploadId.set(item.uploadId, item);
            uploadIds.push(item.uploadId);
            // До первого кадра WS показываем «VK 0%», иначе карточка выглядела
            // бы зависшей на «LOCAL 100%». Кадры придут во время POST
            // /messages — конвейер VK живёт именно там.
            setStage(item, 'vk', 0, item.size);
        }
        if (!uploadIds.length) {
            // Всё отменили, пока лилось: отправлять нечего.
            dropBatch(batch);
            return;
        }

        // Отправка — действие пользователя: обгоняет фоновые запросы, но
        // проходит через общий бюджет 3 rps.
        await vkLimiter.acquire(PRIORITY_USER);
        const body = { uploadIds, text: batch.caption };
        if (batch.replyToCmid !== null) body.replyToCmid = batch.replyToCmid;
        const resp = await sendMessage(batch.dialogId, body);
        if (batch.items.every((i) => i.aborted)) return;
        finishBatch(batch, resp && resp.message);
    } catch (e) {
        if (batch.items.every((i) => i.aborted)) {
            cleanupBatch(batch);
            return;
        }
        failBatch(batch, e);
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

/** Успешная отправка: карточки снимаются, ленту дорисовывает chat.js. */
function finishBatch(batch, dto) {
    for (const item of batch.items) {
        setStage(item, 'sent', item.size, item.size);
        if (item.row && item.row.parentNode) item.row.remove();
    }
    cleanupBatch(batch);
    dropBatch(batch);
    if (dto && Number.isFinite(dto.id) && dto.id > 0) {
        // Тот же путь, что у входящих: insertMessage дедуплицирует, если
        // кадр LongPoll про наше сообщение опередил ответ POST.
        chat.onNewMessage(dto);
    }
}

/** Провал любого этапа: «!! FAILED» + кнопка повтора у первой карточки. */
function failBatch(batch, err) {
    for (const item of batch.items) setStage(item, 'failed');
    cleanupBatch(batch);
    const code = err && err.code;
    const extra = code === 'flood_wait' && err.retryAfterSecs
        ? ` (подождите ${err.retryAfterSecs}с)`
        : '';
    const message = `${(err && err.message) || err}${extra}`;
    for (const item of batch.items) {
        if (item.els) item.els.status.title = message;
    }
    const names = batch.items.map((i) => i.name).join(', ');
    chat.systemLine(`не отправлено (${names}): ${message}`);
    showRetry(batch);
}

/**
 * Снимает временные ссылки (xhr, реестр upload_id).
 *
 * `upload_id` забывается ВСЕГДА: на сервере он потреблён первой же попыткой
 * отправки, и повтор обязан начинаться с новой заливки.
 */
function cleanupBatch(batch) {
    for (const item of batch.items) {
        if (item.uploadId) local.byUploadId.delete(item.uploadId);
        item.uploadId = null;
        item.xhr = null;
    }
}

/** Обрывает активную заливку одного файла. */
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

/** Убирает пачку из очереди (отмена или удавшаяся отправка). */
function dropBatch(batch) {
    const idx = local.queue.indexOf(batch);
    if (idx >= 0) local.queue.splice(idx, 1);
    refreshQueueLabels();
}

/** Обновляет подписи «QUEUED n/m» у ожидающих пачек. */
function refreshQueueLabels() {
    const total = local.queue.length;
    local.queue.forEach((batch, i) => {
        for (const item of batch.items) {
            if (item.stage !== 'queued' || !item.els) continue;
            item.els.status.textContent = uploadQueueLabel(i + 1, total);
        }
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
 * отдельная панель. Внутри — карточка с теми же классами, что у принятых медиа
 * (`.vk-media-*`), плюс `.vk-upload-card` для состояний отправки.
 *
 * Класс вида НЕ `vk-media-${kind}`, а `vk-upload-${kind}`: первый несёт стили
 * плеера и раскладку принятого вложения, и карточка отправки от них поехала бы.
 *
 * Всё через textContent: имя файла приходит из ФС пользователя, но правило
 * «никакого innerHTML» в этом плагине действует без исключений.
 */
function buildRow(item) {
    const row = document.createElement('div');
    row.className = 'vk-line vk-line-own vk-msg-pending';
    row.dataset.uploadLocalId = item.localId;
    // Цель ответа помечена прямо на узле: цитату в optimistic-строке рисует
    // владелец полосы (reply.js) — ему нужен способ узнать, к чему эта строка
    // относится, не заглядывая в приватную очередь upload.js.
    if (item.batch.replyToCmid) row.dataset.replyToCmid = String(item.batch.replyToCmid);

    const time = document.createElement('span');
    time.className = 'vk-line-time';
    time.textContent = `[${formatTime(Math.floor(Date.now() / 1000))}] `;

    const nick = document.createElement('span');
    nick.className = 'vk-line-nick-own';
    nick.textContent = 'you@local';

    const sep = document.createElement('span');
    sep.className = 'vk-line-time';
    sep.textContent = ' $ ';

    const text = document.createElement('span');
    text.className = 'vk-line-text';
    const tag = `[${item.kind} ${item.name}]`;
    text.textContent = item.batch.caption ? `${tag} ${item.batch.caption}` : tag;

    row.appendChild(time);
    row.appendChild(nick);
    row.appendChild(sep);
    row.appendChild(text);

    const card = document.createElement('div');
    card.className = `vk-media-card vk-upload-card vk-upload-${item.kind}`;
    card.dataset.uploadLocalId = item.localId;

    const head = document.createElement('div');
    head.className = 'vk-media-head';
    const glyph = document.createElement('span');
    glyph.className = 'vk-media-glyph';
    glyph.textContent = mediaGlyph(item.kind);
    const name = document.createElement('span');
    name.className = 'vk-media-name';
    name.textContent = item.name;
    name.title = item.name;
    const kindLabel = document.createElement('span');
    kindLabel.className = 'vk-media-kind';
    kindLabel.textContent = mediaKindLabel(item.kind);
    head.appendChild(glyph);
    head.appendChild(name);
    head.appendChild(kindLabel);

    const foot = document.createElement('div');
    foot.className = 'vk-media-foot';
    const meta = document.createElement('span');
    meta.className = 'vk-media-meta';
    meta.textContent = formatBytes(item.size) || '—';
    const progress = document.createElement('span');
    progress.className = 'vk-media-progress';
    const bar = document.createElement('span');
    bar.className = 'vk-media-progress-bar';
    bar.style.width = '0%';
    progress.appendChild(bar);
    const status = document.createElement('span');
    status.className = 'vk-media-status';
    status.textContent = uploadQueueLabel(1, 1);
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'vk-media-btn vk-upload-cancel';
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
    if ($vkLog) {
        $vkLog.appendChild(row);
        $vkLog.scrollTop = $vkLog.scrollHeight;
    }
    return row;
}

/**
 * Отмена пользователем — ОДНОГО файла, а не всей пачки.
 *
 * На первом этапе обрывается XHR. На втором (файл уже у devforge) он просто не
 * попадёт в `uploadIds`: staging на сервере подберёт GC по TTL, а прерывать
 * конвейер VK посередине нам нечем — и не нужно. Если отменили последний файл
 * пачки, отправлять нечего, и пачка снимается целиком.
 *
 * @param {object} item
 */
export function cancelUpload(item) {
    if (!item || !item.batch) return;
    abortItem(item);
    if (item.uploadId) local.byUploadId.delete(item.uploadId);
    item.uploadId = null;
    const batch = item.batch;
    const idx = batch.items.indexOf(item);
    if (idx >= 0) batch.items.splice(idx, 1);
    if (item.row && item.row.parentNode) item.row.remove();
    chat.systemLine(`отправка отменена: ${item.name}`);
    if (!batch.items.length) dropBatch(batch);
    else refreshQueueLabels();
    // Очередь могла остановиться на этой пачке — двигаем дальше.
    pump();
}

/**
 * Кнопка повтора (одна на пачку).
 *
 * Повтор начинается с ПЕРВОГО этапа: `upload_id` расходуется сервером при
 * первой же попытке отправки (иначе двойной клик слал бы файл дважды), а
 * staging мог быть уже подчищен. «Догнать» вторую ногу нельзя.
 */
function showRetry(batch) {
    const first = batch.items[0];
    if (!first || !first.els) return;
    if (first.els.retry) first.els.retry.remove();
    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'vk-media-btn vk-media-retry';
    retry.textContent = 'RETRY';
    retry.title = 'Отправить заново';
    retry.addEventListener('click', () => {
        retry.remove();
        first.els.retry = null;
        for (const item of batch.items) {
            item.aborted = false;
            item.stage = 'queued';
            setStage(item, 'queued');
        }
        pump();
    });
    first.els.status.after(retry);
    first.els.retry = retry;
}

/**
 * Единственная точка изменения визуального состояния карточки.
 *
 * `queued|local|vk` показывают полосу, `sent|failed` её гасят. Проценты считает
 * `progressPercent` из format.js — тот же расчёт, что у приёма медиа, поэтому
 * «99 %» до финального события ведёт себя одинаково в обе стороны.
 */
function setStage(item, stage, sent, total) {
    item.stage = stage;
    if (!item.els) return;
    const { progress, bar, status, cancel } = item.els;
    status.classList.remove('vk-media-done', 'vk-media-failed');
    status.title = '';

    if (stage === 'sent' || stage === 'failed') {
        progress.hidden = true;
        if (cancel) cancel.hidden = true;
        status.classList.add(stage === 'sent' ? 'vk-media-done' : 'vk-media-failed');
        status.textContent = uploadStageLabel(stage);
        if (item.row) {
            item.row.classList.toggle('vk-msg-pending', false);
            item.row.classList.toggle('vk-upload-failed', stage === 'failed');
        }
        return;
    }

    progress.hidden = false;
    if (cancel) cancel.hidden = false;
    const pct = progressPercent(sent, total);
    bar.style.width = pct === null ? '100%' : `${pct}%`;
    progress.classList.toggle('vk-media-progress-indeterminate', pct === null);
    status.textContent = stage === 'queued'
        ? uploadQueueLabel(1, Math.max(1, local.queue.length))
        : uploadStageLabel(stage, sent, total);
}
