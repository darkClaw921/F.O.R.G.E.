// tmux-web — передача файлов в терминальную сессию: вставка из буфера
// (Ctrl/Cmd+V) и перетаскивание из ОС (drag&drop).
//
// Зачем модуль: через WebSocket в PTY уходят только байты ввода, «положить»
// файл в tmux браузер не может. Поэтому передача файла делается в три шага:
//
//   1. перехватываем `paste` (если в буфере есть файл) или `drop` (если в
//      переносе есть файлы) — обычный текст НЕ трогаем, его вставляет сам
//      xterm;
//   2. POST'им байты на `/api/sessions/:name/files?name=<имя>`, бэкенд
//      (`src/session_files.rs`) кладёт файл в `<root>/.forge/pasted/` и
//      возвращает абсолютный путь;
//   3. путь вставляется в терминал как текст (`term.paste`), т.е. попадает в
//      строку ввода Claude Code / любой другой CLI ровно так, как если бы
//      пользователь напечатал его руками.
//
// Шаги 2-3 у обоих жестов общие (`uploadFiles`) — различается только способ
// добыть список файлов.
//
// Слушатель `paste` висит на `document` в фазе capture: xterm обрабатывает
// вставку в своём скрытом textarea и вызывает `preventDefault`, поэтому
// перехватить событие нужно раньше него. Чтобы не ломать вставку в другие
// поля (Telegram/VK/Echo/модалки), обрабатываем только события, чей target
// лежит внутри `#terminal`, и только при активной вкладке терминала.
//
// Слушатели drag&drop висят прямо на `#terminal` — событие и так приходит
// туда, куда бросили, и перехватывать его на document незачем.
//
// Зависимости:
//   - state (term/ws/currentSession/attachWsOrigin/activeTab) из core/state.js
//   - $terminalEl из core/dom.js
//   - apiFetch из core/api.js — origin-aware маршрутизация на remote-серверы

import { state } from '../core/state.js';
import { $terminalEl } from '../core/dom.js';
import { apiFetch } from '../core/api.js';

/** Потолок размера одной вставки — 25 MiB (совпадает с MAX_UPLOAD_BYTES бэка). */
export const MAX_PASTE_BYTES = 25 * 1024 * 1024;

/** Сколько миллисекунд держится тост об успехе. */
const TOAST_OK_MS = 2500;

/** Сколько миллисекунд держится тост об ошибке (читать дольше). */
const TOAST_ERR_MS = 6000;

let toastEl = null;
let toastTimer = null;

let dropzoneEl = null;

/**
 * Глубина вложенности dragenter/dragleave.
 *
 * `dragleave` стреляет и при переходе между дочерними элементами внутри
 * `#terminal` (у xterm их много: canvas'ы, helper-textarea, quick-cmd bar),
 * поэтому по одному событию подсветку гасить нельзя — она бы мигала. Считаем
 * баланс enter/leave и гасим на нуле.
 */
let dragDepth = 0;

/**
 * Вешает обработчик вставки файлов из буфера. Идемпотентна — повторный вызов
 * не дублирует слушатель (флаг на document).
 */
export function initTerminalPasteFiles() {
    if (typeof document === 'undefined') return;
    if (document.__forgePasteFilesBound) return;
    document.__forgePasteFilesBound = true;

    document.addEventListener('paste', onPaste, true);
}

/**
 * Вешает обработчики перетаскивания файлов на `#terminal`. Идемпотентна.
 *
 * `dragover` обязателен и обязан звать `preventDefault`: без него браузер
 * считает зону «не принимающей» и `drop` не приходит вовсе. `dragenter`
 * нужен только для подсветки.
 */
export function initTerminalDropFiles() {
    if (!$terminalEl) return;
    if ($terminalEl.__forgeDropFilesBound) return;
    $terminalEl.__forgeDropFilesBound = true;

    // Capture-фаза: xterm вешает свои обработчики на внутренние элементы
    // (screen, helper-textarea), и всплывающее событие досталось бы им первыми.
    $terminalEl.addEventListener('dragenter', onDragEnter, true);
    $terminalEl.addEventListener('dragover', onDragOver, true);
    $terminalEl.addEventListener('dragleave', onDragLeave, true);
    $terminalEl.addEventListener('drop', onDrop, true);
}

/**
 * Есть ли в переносе файлы (а не текст, ссылка или вкладка браузера).
 *
 * Читается именно `types`: сам список файлов в `dragover`/`dragenter`
 * недоступен по соображениям приватности — браузер отдаёт его только в `drop`.
 *
 * @param {DragEvent} e
 * @returns {boolean}
 */
export function hasDraggedFiles(e) {
    const dt = e && e.dataTransfer;
    if (!dt) return false;
    return Array.from(dt.types || []).includes('Files');
}

/** Перетаскивание файла вошло в терминал — считаем глубину и подсвечиваем. */
function onDragEnter(e) {
    if (!hasDraggedFiles(e) || !isDropAllowed()) return;
    e.preventDefault();
    dragDepth += 1;
    showDropzone();
}

/**
 * Без `preventDefault` на `dragover` браузер отменяет перенос и `drop` не
 * произойдёт — это главная причина, по которой обработчик вообще существует.
 */
function onDragOver(e) {
    if (!hasDraggedFiles(e) || !isDropAllowed()) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
}

/** Курсор ушёл — гасим подсветку только когда вышли из терминала целиком. */
function onDragLeave(e) {
    if (!hasDraggedFiles(e)) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) hideDropzone();
}

/**
 * Файлы брошены в терминал: тот же путь, что и у вставки из буфера.
 *
 * `preventDefault` обязателен и здесь — иначе браузер откроет брошенный файл
 * вместо страницы, и сессия просто исчезнет с экрана.
 */
function onDrop(e) {
    if (!hasDraggedFiles(e)) return;
    e.preventDefault();
    dragDepth = 0;
    hideDropzone();

    if (!isDropAllowed()) return;

    if (!state.currentSession) {
        showToast('Нет активной сессии — файл вставлять некуда', true);
        return;
    }

    const files = Array.from((e.dataTransfer && e.dataTransfer.files) || []);
    if (!files.length) {
        // Перенос объявлял файлы, но их нет: так выглядит, например, перенос
        // папки в части браузеров.
        showToast('В переносе нет файлов (папки не поддерживаются)', true);
        return;
    }

    uploadFiles(files);
}

/** Активна ли вкладка терминала (на Tasks/Git бросок игнорируем). */
function isDropAllowed() {
    return !state.activeTab || state.activeTab === 'terminal';
}

/**
 * Обработчик `paste` в фазе capture.
 *
 * Молча выходит (не мешая штатной вставке) во всех случаях, кроме одного:
 * фокус в терминале, вкладка терминала активна, есть подключённая сессия и в
 * буфере лежат файлы.
 *
 * @param {ClipboardEvent} e
 */
function onPaste(e) {
    if (!isTerminalTarget(e.target)) return;
    if (state.activeTab && state.activeTab !== 'terminal') return;

    const files = clipboardFiles(e.clipboardData);
    if (!files.length) return; // только текст — пусть вставляет xterm

    if (!state.currentSession) {
        e.preventDefault();
        showToast('Нет активной сессии — файл вставлять некуда', true);
        return;
    }

    // Файл в буфере есть — забираем событие себе целиком, иначе xterm вставит
    // в командную строку имя файла (или мусор) поверх нашего пути.
    e.preventDefault();
    e.stopPropagation();

    uploadFiles(files);
}

/**
 * Target события внутри контейнера терминала?
 *
 * Реальный target — скрытая `.xterm-helper-textarea` внутри `#terminal`,
 * поэтому проверяем через `contains`, а не по равенству.
 *
 * @param {EventTarget|null} target
 * @returns {boolean}
 */
export function isTerminalTarget(target) {
    if (!$terminalEl || !target) return false;
    if (target === $terminalEl) return true;
    if (typeof target.nodeType !== 'number') return false;
    return $terminalEl.contains(target);
}

/**
 * Файлы из буфера обмена.
 *
 * Скриншот в части браузеров приезжает только через `items` (в `files` его
 * нет), поэтому проверяем оба источника. Текстовые item'ы (`kind === 'string'`)
 * игнорируются — иначе обычная вставка текста считалась бы вставкой файла.
 *
 * @param {DataTransfer|null} dt
 * @returns {Array<File>}
 */
export function clipboardFiles(dt) {
    if (!dt) return [];
    const direct = Array.from(dt.files || []);
    if (direct.length) return direct;

    const out = [];
    for (const it of Array.from(dt.items || [])) {
        if (it.kind !== 'file') continue;
        const f = typeof it.getAsFile === 'function' ? it.getAsFile() : null;
        if (f) out.push(f);
    }
    return out;
}

/**
 * Имя для файла, которого браузер не назвал (или назвал обезличенно).
 *
 * Скриншот из буфера почти везде приходит как `image.png` либо вовсе без
 * имени — в `.forge/pasted/` такие мгновенно сливаются в одну кучу. Дата в
 * имени делает файл узнаваемым, а бэкенд всё равно разведёт коллизии
 * суффиксом `-2`.
 *
 * @param {File} file
 * @param {Date} [now]
 * @returns {string}
 */
export function pasteFileName(file, now) {
    const raw = String((file && file.name) || '');
    const generic = !raw || raw === 'image.png' || raw === 'blob' || raw === 'unknown';
    if (!generic) return raw;

    const d = now || new Date();
    const p = (n) => String(n).padStart(2, '0');
    const stamp =
        d.getFullYear() +
        p(d.getMonth() + 1) +
        p(d.getDate()) +
        '_' +
        p(d.getHours()) +
        p(d.getMinutes()) +
        p(d.getSeconds());
    // Расширение доберёт бэкенд из ?mime= — здесь достаточно основы.
    return 'pasted_' + stamp;
}

/**
 * Загружает файлы по одному и вставляет полученные пути в терминал.
 *
 * Последовательно, а не параллельно: порядок путей в командной строке должен
 * совпадать с порядком файлов в буфере, и одновременная заливка нескольких
 * скриншотов ничего не ускоряет.
 *
 * @param {Array<File>} files
 */
async function uploadFiles(files) {
    const total = files.length;
    let index = 0;

    for (const file of files) {
        index += 1;
        const label = total > 1 ? `(${index}/${total}) ` : '';

        // Брошенная папка приезжает как File нулевого размера (читать её
        // содержимое браузер не даёт) — объясняем это словами, а не 400-й от
        // бэкенда «пустое тело запроса».
        if (!file.size) {
            showToast(
                `${label}${file.name || 'Объект'}: пустой файл или папка — не поддерживается`,
                true,
            );
            continue;
        }

        if (file.size > MAX_PASTE_BYTES) {
            showToast(
                `${label}${file.name || 'файл'} слишком большой (> ${Math.round(
                    MAX_PASTE_BYTES / 1024 / 1024,
                )} МБ)`,
                true,
            );
            continue;
        }

        showToast(`${label}Загрузка ${file.name || 'файла'}…`, false, 0);

        try {
            const inserted = await uploadOne(file);
            insertIntoTerminal(inserted);
            showToast(`${label}Вставлено: ${inserted}`);
        } catch (err) {
            console.warn('[paste-files] upload failed', err);
            showToast(`${label}Не удалось вставить файл: ${err.message || err}`, true);
        }
    }
}

/**
 * Заливает один файл и возвращает строку, готовую к вставке в шелл
 * (абсолютный путь, при необходимости в кавычках).
 *
 * @param {File} file
 * @returns {Promise<string>}
 */
async function uploadOne(file) {
    const session = state.currentSession;
    const origin = state.attachWsOrigin || 'local';
    const name = pasteFileName(file);
    // Настоящий MIME уходит в query, а Content-Type всегда octet-stream:
    // CSRF-guard бэкенда (auth::is_allowed_raw_body) пропускает не-JSON тело
    // только с этим Content-Type — он не «простой» для CORS, поэтому
    // кросс-доменный запрос браузер обязан предварить preflight'ом.
    const path =
        '/api/sessions/' +
        encodeURIComponent(session) +
        '/files?name=' +
        encodeURIComponent(name) +
        '&mime=' +
        encodeURIComponent(file.type || '');

    const resp = await apiFetch(
        path,
        {
            method: 'POST',
            headers: { 'Content-Type': 'application/octet-stream' },
            body: file,
        },
        origin,
    );

    if (!resp.ok) {
        const text = await resp.text().catch(() => '');
        throw new Error(text || `HTTP ${resp.status}`);
    }

    const dto = await resp.json();
    return dto.insert || dto.path || '';
}

/**
 * Вставляет текст в терминал так же, как обычная вставка из буфера:
 * `term.paste` эмитит onData → байты уходят в PTY (с bracketed paste, если
 * приложение его включило).
 *
 * После пути добавляется пробел — обычно за ним сразу печатают вопрос
 * («посмотри на этот скриншот»), и без пробела слова слипаются.
 *
 * @param {string} text
 */
function insertIntoTerminal(text) {
    if (!text) return;
    if (!state.term || typeof state.term.paste !== 'function') return;
    if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
        showToast('Сессия не подключена — путь не вставлен', true);
        return;
    }
    state.term.paste(text + ' ');
    try {
        state.term.focus();
    } catch (_) {
        /* focus не критичен */
    }
}

/**
 * Показывает рамку-подсказку поверх терминала на время переноса.
 *
 * Элемент создаётся лениво при первом переносе: у большинства пользователей
 * drag&drop не случится ни разу, и держать лишний узел в DOM незачем.
 */
function showDropzone() {
    if (!$terminalEl) return;
    if (!dropzoneEl) {
        dropzoneEl = document.createElement('div');
        dropzoneEl.className = 'terminal-dropzone';
        dropzoneEl.textContent = 'Отпустите файл — путь к нему вставится в терминал';
        $terminalEl.appendChild(dropzoneEl);
    }
    dropzoneEl.hidden = false;
}

/** Прячет рамку-подсказку. */
function hideDropzone() {
    if (dropzoneEl) dropzoneEl.hidden = true;
}

/**
 * Небольшой тост в правом нижнем углу терминала.
 *
 * Терминал нельзя «замусорить» служебными сообщениями (они попадут в вывод
 * tmux и испортят историю), поэтому статус показываем поверх DOM-оверлеем.
 *
 * @param {string} text
 * @param {boolean} [isError]
 * @param {number} [ms] — 0 ⇒ висит до следующего тоста (прогресс загрузки).
 */
function showToast(text, isError, ms) {
    if (!$terminalEl) return;
    if (!toastEl) {
        toastEl = document.createElement('div');
        toastEl.className = 'paste-toast';
        $terminalEl.appendChild(toastEl);
    }
    toastEl.textContent = text;
    toastEl.classList.toggle('paste-toast-error', !!isError);
    toastEl.hidden = false;

    if (toastTimer) {
        clearTimeout(toastTimer);
        toastTimer = null;
    }
    const timeout = ms === 0 ? 0 : ms || (isError ? TOAST_ERR_MS : TOAST_OK_MS);
    if (timeout > 0) {
        toastTimer = setTimeout(() => {
            if (toastEl) toastEl.hidden = true;
            toastTimer = null;
        }, timeout);
    }
}
