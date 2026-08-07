// tmux-web — панель «Битрикс24»: отправка файлов (Ф9).
//
// Три способа выбрать файл, одна очередь, два этапа прогресса:
//
//   ⇪ #b24-attach → скрытый #b24-file-input
//   drag&drop над #bitrix → оверлей #b24-dropzone
//   Ctrl+V в #b24-input (скриншот из буфера)
//        │
//        ▼
//   очередь ФАЙЛОВ (строго последовательная, файл = одно сообщение)
//        │
//   [1] ПРИЁМ %   браузер → devforge   (XMLHttpRequest.upload.onprogress)
//        │        POST /api/bitrix/upload, тело = сырой File
//        ▼        ответ: { upload_id, file_name, size, mime }
//   [2] ПОРТАЛ …  devforge → Битрикс24 (кадр WS upload_progress, indeterminate)
//        │        POST /api/bitrix/dialogs/:id/messages { upload_id, text }
//        ▼
//   ОТПРАВЛЕН → карточка снимается, строку дорисовывает chat.appendMessage
//
// ЧЕМ ОТЛИЧАЕТСЯ ОТ VK-БЛИЗНЕЦА — ровно четыре вещи, и все они следуют из
// платформы, а не из вкуса:
//
// 1. ОДИН ФАЙЛ = ОДНО СООБЩЕНИЕ. `im.v2.File.upload` принимает РОВНО ОДИН
//    файл и сам же отправляет сообщение; альбома, как у VK, у Битрикс24 нет
//    вовсе. Поэтому очередь состоит из файлов, а не из пачек, и выбор пяти
//    файлов даёт пять сообщений.
// 2. ВТОРОЙ ЭТАП БЕЗ ПРОЦЕНТОВ. Заливка одношаговая (base64 в JSON-теле) и
//    обратной связи о ходе не даёт — кадр помечен `indeterminate`, карточка
//    рисует бегущую полосу. Врать процентами здесь запрещено (Р8).
// 3. ОТМЕНА РАБОТАЕТ ТОЛЬКО НА ПЕРВОМ ЭТАПЕ. Как только `upload_id` ушёл в
//    `POST /messages`, сервер его ПОТРЕБИЛ и начал заливку, прервать которую
//    нечем: у портала нет ни отмены, ни отзыва сообщения по нашей команде.
//    Кнопка на этом этапе гаснет — неработающая кнопка хуже отсутствующей.
// 4. ПОВТОРА НЕТ АВТОМАТИЧЕСКОГО (Р9). `upload_id` одноразовый, а у портала
//    нет дедуп-идентификатора: автоповтор означал бы второй файл на Диске и
//    второе сообщение в чате. Повтор — это НОВАЯ загрузка с первого этапа, и
//    начинает её только пользователь кнопкой ⟳.
//
// ГДЕ ЖИВЁТ КАРТОЧКА. В `#b24-uploads` НАД полем ввода, а не в ленте.
// `chat.renderHistory()` пересобирает `#b24-log` целиком (правка, сообщение,
// вставшее не в конец, смена диалога), и карточка отправки исчезала бы вместе
// с прогрессом от любого чужого сообщения. Да и по существу очередь отправки —
// это состояние строки ввода: в переписке файл появляется только после того,
// как портал его принял.
//
// ПОЧЕМУ XHR, А НЕ fetch: у `fetch` нет прогресса ОТПРАВКИ (ReadableStream в
// теле поддерживается не везде и требует half-duplex HTTP/2), а `upload.
// onprogress` у XHR есть из коробки. Следствие, о котором легко забыть: XHR
// идёт МИМО подменённого `window.fetch` из `core/auth.js`, поэтому
// `Authorization: Bearer` ставится ВРУЧНУЮ — без этого отправка работала бы
// только на localhost.
//
// ПОЧЕМУ КАДРЫ ФАЗЫ `client` ПРИХОДЯТ, НО НЕ ПРИМЕНЯЮТСЯ. Сервер вещает их с
// момента, когда начал принимать тело, а `upload_id` вкладка-отправитель
// узнаёт только из ОТВЕТА — то есть когда фаза уже кончилась. Свой прогресс
// приёма она берёт из XHR (он точнее и без сети), а кадры нужны ВТОРОЙ
// открытой вкладке devforge, чтобы та видела занятую панель.

import {
    $b24, $b24Attach, $b24Dropzone, $b24FileInput, $b24Input, $b24Uploads,
} from '../core/dom.js';
import { getAuthToken } from '../core/auth.js';

import * as b24Api from './api.js';
import * as chat from './chat.js';
import {
    errorLabel, formatBytes, mediaKindLabel, pastedFileName, progressPercent,
    uploadKind, uploadMime, uploadQueueLabel, uploadRejectReason, uploadStageLabel,
} from './format.js';
import { b24State } from './state.js';

/** Путь приёма файла (он же исключение csrf_guard в tmux-web/src/auth.rs). */
export const UPLOAD_PATH = b24Api.PATHS.upload;

/** Заголовок с именем файла: percent-encoded, потому что заголовки — ASCII. */
export const FILE_NAME_HEADER = 'X-Forge-File-Name';

/**
 * Сколько файлов принимается за один выбор.
 *
 * Не ограничение платформы, а защита от случайного «выделил папку и бросил»:
 * каждый файл станет ОТДЕЛЬНЫМ сообщением, и три десятка картинок превратили
 * бы чат в ленту, которую собеседник не сможет прочитать. Лишние отбрасываются
 * с явным сообщением, а не молча.
 */
export const MAX_FILES_PER_DROP = 10;

const local = {
    /** Обработчики уже навешены. */
    bound: false,
    /** Очередь файлов: [0] — тот, что отправляется прямо сейчас. */
    queue: [],
    /** Идёт ли прокачка очереди. */
    pumping: false,
    /** Счётчик локальных id (уникальность без crypto). */
    seq: 0,
    /** upload_id → item: по нему кадры WS находят карточку. */
    byUploadId: new Map(),
    /** Вложенность dragenter/dragleave — иначе оверлей мигает. */
    dragDepth: 0,
    /**
     * Поколение очереди; растёт на каждом `teardownUpload`.
     *
     * Без него teardown посреди летящего запроса оставлял бы ЗОМБИ-цикл:
     * `pumping` снят, а старый `pump()` всё ещё висит на `await` и, проснувшись,
     * берётся за файлы следующего поколения параллельно с новым циклом. Файл
     * при этом уехал бы дважды.
     */
    epoch: 0,
};

// ---------------------------------------------------------------------------
// Публичный API
// ---------------------------------------------------------------------------

/**
 * Навешивает обработчики выбора файлов. Идемпотентна.
 *
 * Зовётся из `main.initB24()` вместе с остальными обработчиками вкладки.
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
 * Каждый файл становится ОТДЕЛЬНЫМ сообщением: `im.v2.File.upload` принимает
 * один файл за вызов. Подпись из поля ввода достаётся один раз и достаётся
 * ПЕРВОМУ файлу — пользователь, написавший текст и приложивший три картинки,
 * ждёт подпись под одной из них, а не три одинаковых.
 *
 * Цель ответа (Р10) захватывается ЗДЕСЬ, а не в момент реальной отправки:
 * очередь последовательная, и пока дойдёт ход, пользователь успеет и полосу
 * закрыть, и выбрать другое сообщение.
 *
 * @param {Array<File>} files
 * @param {{dialogId?: string, caption?: string}} [opts]
 * @returns {Array<object>} принятые элементы очереди
 */
export function sendFiles(files, opts = {}) {
    const dialogId = String(opts.dialogId || chat.currentDialogId() || '');
    if (!dialogId) {
        showNotice('сначала откройте диалог');
        return [];
    }
    const list = Array.from(files || []).filter(Boolean);
    if (!list.length) return [];

    const accepted = list.slice(0, MAX_FILES_PER_DROP);
    if (list.length > accepted.length) {
        showNotice(`за раз отправляется не больше ${MAX_FILES_PER_DROP} файлов`);
    }

    const caption = opts.caption == null ? takeCaption() : String(opts.caption);
    const reply = chat.replyMode();
    if (reply) chat.cancelMode();

    const items = [];
    let first = true;
    for (const file of accepted) {
        const item = makeItem(file, dialogId, first ? caption : '', first ? reply : null);
        first = false;
        // Отказ по размеру виден СРАЗУ и не занимает очередь: гнать 30 МБ ради
        // 413 — это минута ожидания там, где ответ известен заранее.
        const reason = uploadRejectReason(item.size, b24State.maxUploadBytes);
        if (reason) {
            setStage(item, 'failed');
            setError(item, reason, false);
            continue;
        }
        local.queue.push(item);
        items.push(item);
    }
    refreshQueueLabels();
    pump();
    return items;
}

/**
 * Кадр WS `upload_progress` (фаза `portal`).
 *
 * Кадры фазы `client` приходят тоже, но применять их этой вкладке нечему: свой
 * прогресс приёма она уже знает из XHR, а `upload_id` узнаёт только после
 * конца фазы (см. шапку модуля). Неизвестный идентификатор — штатный случай:
 * это либо чужая вкладка, либо наша же фаза приёма.
 *
 * @param {{upload_id: string, phase: string, sent: number, total: number,
 *   indeterminate: boolean}} evt
 */
export function onUploadProgress(evt) {
    if (!evt || !evt.upload_id) return;
    const item = local.byUploadId.get(String(evt.upload_id));
    if (!item) return;
    if (String(evt.phase) !== 'portal') return;
    setStage(item, 'portal', Number(evt.sent) || 0, Number(evt.total) || item.size);
}

/**
 * Снимает обработчики состояния и очередь (beforeunload, смена аккаунта).
 *
 * Летящие XHR обрываются: страница уходит, и достучаться до их обработчиков
 * будет уже некому. Поколение увеличивается, чтобы проснувшийся `pump()` не
 * взялся за очередь следующей жизни модуля.
 */
export function teardownUpload() {
    local.epoch += 1;
    for (const item of local.queue) abortItem(item);
    for (const item of local.queue) releaseThumb(item);
    local.queue.length = 0;
    local.byUploadId.clear();
    local.pumping = false;
    local.dragDepth = 0;
    hideDropzone();
    if ($b24Uploads) {
        $b24Uploads.textContent = '';
        $b24Uploads.hidden = true;
    }
}

/**
 * Отменяет отправку одного файла.
 *
 * На первом этапе обрывается XHR. На втором отмена НЕВОЗМОЖНА: `upload_id`
 * потреблён сервером, заливка идёт, и прервать её нечем — кнопка к этому
 * моменту уже скрыта.
 *
 * @param {object} item
 */
export function cancelUpload(item) {
    if (!item || item.stage === 'portal') return;
    abortItem(item);
    dropItem(item);
    refreshQueueLabels();
    // Очередь могла стоять на этом файле — двигаем дальше.
    pump();
}

/** Снимок состояния для e2e и отладки. */
export function _debugUploadState() {
    return {
        bound: local.bound,
        queued: local.queue.length,
        pumping: local.pumping,
        epoch: local.epoch,
        tracked: local.byUploadId.size,
        stages: local.queue.map((i) => i.stage),
        names: local.queue.map((i) => i.name),
    };
}

// ---------------------------------------------------------------------------
// Способы выбрать файл
// ---------------------------------------------------------------------------

/** Кнопка ⇪ открывает скрытый input[type=file]. */
function bindPicker() {
    if (!$b24Attach || !$b24FileInput) return;
    $b24Attach.addEventListener('click', (e) => {
        if (e && typeof e.preventDefault === 'function') e.preventDefault();
        $b24FileInput.click();
    });
    $b24FileInput.addEventListener('change', () => {
        const files = Array.from($b24FileInput.files || []);
        // `value` сбрасывается ДО отправки: иначе повторный выбор того же файла
        // не вызовет `change`, и пользователю покажется, что кнопка сломалась.
        $b24FileInput.value = '';
        if (files.length) sendFiles(files);
    });
}

/**
 * Drag&drop по всей панели.
 *
 * `preventDefault` на `dragover` ОБЯЗАТЕЛЕН: без него браузер обработает drop
 * сам и откроет файл вместо страницы. Счётчик вложенности нужен потому, что
 * `dragenter`/`dragleave` стреляют на каждом дочернем узле — без него оверлей
 * мигал бы при движении курсора над списком диалогов.
 *
 * Оверлей поднимается ТОЛЬКО когда тянут файлы: выделение текста мышью не
 * должно закрывать панель полупрозрачной заглушкой.
 */
function bindDragAndDrop() {
    if (!$b24) return;
    $b24.addEventListener('dragenter', (e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        local.dragDepth += 1;
        showDropzone();
    });
    $b24.addEventListener('dragover', (e) => {
        if (!hasFiles(e)) return;
        // Без этого drop не случится вовсе (спецификация HTML5 DnD).
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
    });
    $b24.addEventListener('dragleave', (e) => {
        if (!hasFiles(e)) return;
        local.dragDepth = Math.max(0, local.dragDepth - 1);
        if (local.dragDepth === 0) hideDropzone();
    });
    $b24.addEventListener('drop', (e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        local.dragDepth = 0;
        hideDropzone();
        const files = e.dataTransfer ? Array.from(e.dataTransfer.files || []) : [];
        if (files.length) sendFiles(files);
    });
}

/**
 * Вставка из буфера обмена в `#b24-input`.
 *
 * Перехватываем ТОЛЬКО когда в буфере есть файл: обычный текст обязан
 * вставляться как текст, иначе Ctrl+V перестанет работать по назначению.
 */
function bindPaste() {
    if (!$b24Input) return;
    $b24Input.addEventListener('paste', (e) => {
        const dt = e.clipboardData;
        if (!dt) return;
        const files = clipboardFiles(dt);
        if (!files.length) return; // только текст — не мешаем
        if (typeof e.preventDefault === 'function') e.preventDefault();
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
    // Скриншот из буфера в части браузеров приходит только через `items`.
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
 * `image.png` — такое имя у КАЖДОГО скриншота, и в переписке они сливаются).
 *
 * @param {File} file
 * @returns {File|{name: string, size: number, type: string, blob: File}}
 */
function renameIfAnonymous(file) {
    const name = String(file.name || '');
    const generic = !name || name === 'image.png' || name === 'blob';
    if (!generic) return file;
    const nice = pastedFileName(file.type || 'image/png');
    // Пересобрать File можно только конструктором; там, где его нет (очень
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
    if ($b24Dropzone) $b24Dropzone.hidden = false;
}

function hideDropzone() {
    if ($b24Dropzone) $b24Dropzone.hidden = true;
}

/**
 * Забирает текст из поля ввода как подпись и очищает поле.
 *
 * Пользователь набрал текст и приложил файл — он ждёт ОДНО сообщение с
 * подписью, а не текст отдельно и файл отдельно.
 */
function takeCaption() {
    if (!$b24Input) return '';
    const text = String($b24Input.value || '');
    if (!text.trim()) return '';
    $b24Input.value = '';
    if (typeof chat.updateCharCount === 'function') chat.updateCharCount();
    return text;
}

// ---------------------------------------------------------------------------
// Очередь
// ---------------------------------------------------------------------------

/** Один файл очереди. */
function makeItem(file, dialogId, caption, reply) {
    local.seq += 1;
    const mime = uploadMime(file.name, file.type);
    const item = {
        localId: `b24-up-${Date.now()}-${local.seq}`,
        dialogId: String(dialogId),
        caption: String(caption || ''),
        reply: reply || null,
        file: file.blob || file,
        name: String(file.name || 'file'),
        size: Number(file.size) || 0,
        mime,
        kind: uploadKind(mime),
        stage: 'queued',
        uploadId: null,
        xhr: null,
        aborted: false,
        thumb: null,
        row: null,
        els: null,
    };
    buildRow(item);
    return item;
}

/**
 * Прокачивает очередь по ОДНОМУ файлу.
 *
 * Последовательность здесь не про вежливость к серверу, а про память: тело
 * заливки — это base64 файла целиком (~1.33 размера), и две параллельные
 * отправки по 25 МиБ дали бы 66 МБ в куче devforge разом.
 *
 * Реентерабельность защищена флагом `pumping`: `sendFiles`, отмена и повтор
 * могут позвать `pump()` в любой момент.
 */
async function pump() {
    if (local.pumping) return;
    local.pumping = true;
    // Поколение фиксируется на входе: проснувшись после teardown, цикл обязан
    // выйти, а не взяться за чужую очередь.
    const epoch = local.epoch;
    try {
        while (local.queue.length && epoch === local.epoch) {
            const item = local.queue[0];
            if (item.stage === 'failed') break; // ждём решения пользователя
            await runItem(item);
            if (epoch !== local.epoch) return;
            if (local.queue[0] !== item) continue; // отменили, пока летело
            if (item.stage === 'failed') break;
            local.queue.shift();
            releaseThumb(item);
            refreshQueueLabels();
        }
    } finally {
        // Флаг снимает только «свой» цикл: чужой (нового поколения) уже
        // работает, и обнулить его флаг означало бы пустить третий.
        if (epoch === local.epoch) local.pumping = false;
    }
}

/** Полный цикл одного файла: ПРИЁМ → ПОРТАЛ → строка в ленте. */
async function runItem(item) {
    try {
        setStage(item, 'client', 0, item.size);
        const staged = await uploadToServer(item);
        if (item.aborted) return;

        const uploadId = String((staged && staged.upload_id) || '');
        if (!uploadId) throw new Error('сервер не вернул upload_id');
        item.uploadId = uploadId;
        local.byUploadId.set(uploadId, item);

        // До первого кадра WS карточка обязана показать смену этапа сама:
        // иначе она замирает на «ПРИЁМ 100%» ровно в тот момент, когда
        // начинается самая долгая часть работы.
        setStage(item, 'portal', item.size, item.size);

        const payload = { text: item.caption, uploadId };
        if (item.reply && item.reply.id > 0) {
            payload.replyTo = item.reply.id;
            payload.replyAuthor = item.reply.author;
            payload.replyText = item.reply.text;
        }
        const resp = await b24Api.sendMessage(item.dialogId, payload);
        if (item.aborted) return;

        setStage(item, 'sent');
        // Строка появляется сразу; тот же кадр придёт и по WS, но `appendMessage`
        // сливает по `id` — задвоения не будет.
        const msg = resp && resp.message;
        if (msg && String(chat.currentDialogId()) === item.dialogId) chat.appendMessage(msg);
        removeRow(item);
    } catch (e) {
        if (item.aborted) return;
        setStage(item, 'failed');
        setError(item, errorText(e), true);
    } finally {
        if (item.uploadId) {
            local.byUploadId.delete(item.uploadId);
            // Идентификатор ПОТРЕБЛЁН сервером в любом исходе — повтор обязан
            // начинаться с первого этапа, иначе он получил бы 409.
            item.uploadId = null;
        }
    }
}

/** Заливка на devforge через XHR — единственный источник прогресса приёма. */
function uploadToServer(item) {
    return new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        item.xhr = xhr;
        xhr.open('POST', UPLOAD_PATH, true);
        // Content-Length браузер выставит сам по размеру File — ставить его
        // вручную запрещено (forbidden header), а сервер без него отвечает 411.
        xhr.setRequestHeader('Content-Type', 'application/octet-stream');
        xhr.setRequestHeader(FILE_NAME_HEADER, encodeURIComponent(item.name));
        // Bearer вручную: XHR не проходит через подменённый fetch (core/auth.js).
        const token = getAuthToken();
        if (token) xhr.setRequestHeader('Authorization', `Bearer ${token}`);

        if (xhr.upload) {
            xhr.upload.onprogress = (e) => {
                const total = e.lengthComputable && e.total > 0 ? e.total : item.size;
                setStage(item, 'client', e.loaded, total);
            };
        }
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

/** Ошибка XHR в виде конверта плагина (`{error:{code,message}}`). */
function errorFromXhr(xhr) {
    let code = '';
    let message = '';
    try {
        const parsed = JSON.parse(xhr.responseText || '{}');
        code = String((parsed.error && parsed.error.code) || '');
        message = String((parsed.error && parsed.error.message) || '');
    } catch (_) {
        // Не JSON — значит, отказал не плагин (прокси, шлюз): останется статус.
    }
    const err = new Error(message || `HTTP ${xhr.status}`);
    err.status = xhr.status;
    err.code = code;
    return err;
}

/** Человеческий текст ошибки: код контракта важнее текста портала. */
function errorText(e) {
    if (!e) return 'не удалось отправить файл';
    if (e.code) {
        const label = errorLabel(e.code);
        return e.message ? `${label}: ${e.message}` : label;
    }
    if (e.status === 413) return 'файл больше лимита сервера';
    if (e.status === 411) return 'браузер не сообщил размер файла';
    return String(e.message || 'не удалось отправить файл');
}

/** Обрывает летящий запрос. */
function abortItem(item) {
    item.aborted = true;
    if (item.xhr) {
        try {
            item.xhr.abort();
        } catch (_) {
            // Уже завершился — обрывать нечего.
        }
        item.xhr = null;
    }
}

/** Убирает файл из очереди и снимает его карточку. */
function dropItem(item) {
    const i = local.queue.indexOf(item);
    if (i >= 0) local.queue.splice(i, 1);
    if (item.uploadId) local.byUploadId.delete(item.uploadId);
    item.uploadId = null;
    releaseThumb(item);
    removeRow(item);
}

/** Обновляет подписи «В ОЧЕРЕДИ n/m» у ожидающих файлов. */
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
 * Строит карточку файла в `#b24-uploads`.
 *
 * ВСЁ через `textContent`: имя файла приходит из файловой системы
 * пользователя, а правило «никакого innerHTML» в этой панели действует без
 * исключений.
 */
function buildRow(item) {
    if (!$b24Uploads) return null;
    const doc = document;
    const row = doc.createElement('div');
    row.className = `b24-upload b24-upload-${item.kind}`;
    row.dataset.uploadLocalId = item.localId;

    const head = doc.createElement('div');
    head.className = 'b24-upload-head';

    const glyph = doc.createElement('span');
    glyph.className = 'b24-upload-glyph';
    glyph.textContent = kindGlyph(item.kind);

    const name = doc.createElement('span');
    name.className = 'b24-upload-name';
    name.textContent = item.name;
    name.title = item.name;

    const kindLabel = doc.createElement('span');
    kindLabel.className = 'b24-upload-kind';
    kindLabel.textContent = mediaKindLabel(item.kind);

    const meta = doc.createElement('span');
    meta.className = 'b24-upload-meta';
    meta.textContent = formatBytes(item.size) || '—';

    head.appendChild(glyph);
    head.appendChild(name);
    head.appendChild(kindLabel);
    head.appendChild(meta);

    const foot = doc.createElement('div');
    foot.className = 'b24-upload-foot';

    const progress = doc.createElement('span');
    progress.className = 'b24-upload-progress';
    const bar = doc.createElement('span');
    bar.className = 'b24-upload-bar';
    bar.style.width = '0%';
    progress.appendChild(bar);

    const status = doc.createElement('span');
    status.className = 'b24-upload-status';
    status.textContent = uploadQueueLabel(1, 1);

    const cancel = doc.createElement('button');
    cancel.type = 'button';
    cancel.className = 'b24-upload-btn b24-upload-cancel';
    cancel.textContent = '✕';
    cancel.title = 'Отменить отправку';
    cancel.addEventListener('click', () => cancelUpload(item));

    foot.appendChild(progress);
    foot.appendChild(status);
    foot.appendChild(cancel);

    row.appendChild(head);
    // Миниатюра только у изображений и только из локального blob'а: сеть здесь
    // не нужна вовсе, файл уже в браузере.
    attachThumb(item, row, doc);
    row.appendChild(foot);

    item.row = row;
    item.els = { progress, bar, status, cancel, meta, error: null, retry: null };
    $b24Uploads.appendChild(row);
    $b24Uploads.hidden = false;
    return row;
}

/** Значок вида файла (тот же словарь, что у карточек принятых вложений). */
function kindGlyph(kind) {
    switch (kind) {
        case 'image': return '🖼';
        case 'video': return '🎬';
        case 'audio': return '🎧';
        default: return '📎';
    }
}

/** Локальная миниатюра изображения через objectURL. */
function attachThumb(item, row, doc) {
    if (item.kind !== 'image') return;
    if (!globalThis.URL || typeof globalThis.URL.createObjectURL !== 'function') return;
    try {
        item.thumb = globalThis.URL.createObjectURL(item.file);
    } catch (_) {
        item.thumb = null;
        return;
    }
    const img = doc.createElement('img');
    img.className = 'b24-upload-thumb';
    img.alt = '';
    img.src = item.thumb;
    row.appendChild(img);
}

/**
 * Отпускает objectURL миниатюры.
 *
 * Без этого каждая отправленная картинка держала бы свой blob в памяти вкладки
 * до перезагрузки страницы — ровно та же утечка, которую в приёме медиа
 * закрывает `revokeDialogMedia`.
 */
function releaseThumb(item) {
    if (!item.thumb) return;
    try {
        globalThis.URL.revokeObjectURL(item.thumb);
    } catch (_) {
        // В Node и очень старых браузерах отзывать нечего.
    }
    item.thumb = null;
}

/** Снимает карточку и прячет контейнер, когда очередь опустела. */
function removeRow(item) {
    if (item.row && item.row.parentNode) item.row.parentNode.removeChild(item.row);
    item.row = null;
    item.els = null;
    if ($b24Uploads && (!$b24Uploads.childNodes || $b24Uploads.childNodes.length === 0)) {
        $b24Uploads.hidden = true;
    }
}

/**
 * ЕДИНСТВЕННАЯ точка изменения визуального состояния карточки.
 *
 * `queued|client|portal` показывают полосу, `sent|failed` её гасят. Фаза
 * `portal` рисует полосу БЕГУЩЕЙ: процентов у одношаговой заливки не
 * существует, и показать здесь число значило бы его выдумать (Р8).
 */
function setStage(item, stage, sent, total) {
    item.stage = stage;
    if (!item.els) return;
    const { progress, bar, status, cancel } = item.els;
    status.classList.remove('b24-upload-done', 'b24-upload-failed');

    if (stage === 'sent' || stage === 'failed') {
        progress.hidden = true;
        if (cancel) cancel.hidden = true;
        status.classList.add(stage === 'sent' ? 'b24-upload-done' : 'b24-upload-failed');
        status.textContent = uploadStageLabel(stage);
        if (item.row) item.row.classList.add(`b24-upload-${stage}`);
        return;
    }

    progress.hidden = false;
    // Отмена возможна только пока файл ещё не потреблён сервером.
    if (cancel) cancel.hidden = stage === 'portal';
    const pct = stage === 'portal' ? null : progressPercent(sent, total);
    bar.style.width = pct === null ? '100%' : `${pct}%`;
    progress.classList.toggle('b24-upload-indeterminate', pct === null);
    status.textContent = stage === 'queued'
        ? uploadQueueLabel(Math.max(1, local.queue.indexOf(item) + 1), Math.max(1, local.queue.length))
        : uploadStageLabel(stage, sent, total);
}

/**
 * Показывает причину отказа и, если повтор осмыслен, кнопку ⟳.
 *
 * Автоповтора нет и быть не может (Р9): у портала нет дедуп-идентификатора,
 * поэтому «на всякий случай ещё раз» — это второй файл в чате собеседника.
 * Повтор начинается с ПЕРВОГО этапа: `upload_id` сервер уже потребил.
 *
 * @param {object} item
 * @param {string} reason
 * @param {boolean} retryable — `false` у отказов, которые повтор не починит
 *   (слишком большой или пустой файл)
 */
function setError(item, reason, retryable) {
    if (!item.els || !item.row) return;
    const doc = document;
    if (!item.els.error) {
        const err = doc.createElement('div');
        err.className = 'b24-upload-error';
        item.row.appendChild(err);
        item.els.error = err;
    }
    item.els.error.textContent = reason;

    if (item.els.retry) {
        item.els.retry.parentNode.removeChild(item.els.retry);
        item.els.retry = null;
    }
    const foot = item.els.status.parentNode;
    if (retryable) {
        const retry = doc.createElement('button');
        retry.type = 'button';
        retry.className = 'b24-upload-btn b24-upload-retry';
        retry.textContent = '⟳';
        retry.title = 'Отправить заново';
        retry.addEventListener('click', () => retryItem(item));
        foot.appendChild(retry);
        item.els.retry = retry;
    }
    const dismiss = doc.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'b24-upload-btn b24-upload-dismiss';
    dismiss.textContent = '✕';
    dismiss.title = 'Убрать из очереди';
    dismiss.addEventListener('click', () => {
        dropItem(item);
        refreshQueueLabels();
        pump();
    });
    foot.appendChild(dismiss);
}

/** Повтор — это НОВАЯ загрузка того же файла с первого этапа. */
function retryItem(item) {
    if (item.els && item.els.error) {
        item.els.error.parentNode.removeChild(item.els.error);
        item.els.error = null;
    }
    if (item.row) item.row.classList.remove('b24-upload-failed');
    item.aborted = false;
    // Карточка пересобирается: у неё уже нет ни полосы, ни кнопки отмены.
    removeRow(item);
    buildRow(item);
    setStage(item, 'queued');
    if (local.queue.indexOf(item) < 0) local.queue.push(item);
    refreshQueueLabels();
    pump();
}

/**
 * Сообщение, у которого нет своей карточки (нет диалога, слишком много
 * файлов). Живёт в том же контейнере и снимается по клику.
 */
function showNotice(text) {
    if (!$b24Uploads) return;
    const doc = document;
    const note = doc.createElement('div');
    note.className = 'b24-upload b24-upload-notice';
    note.textContent = text;
    note.title = 'Убрать';
    note.addEventListener('click', () => {
        if (note.parentNode) note.parentNode.removeChild(note);
        if ($b24Uploads && (!$b24Uploads.childNodes || $b24Uploads.childNodes.length === 0)) {
            $b24Uploads.hidden = true;
        }
    });
    $b24Uploads.appendChild(note);
    $b24Uploads.hidden = false;
}
