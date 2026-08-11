// tmux-web — Telegram plugin: попап «Анализ чата» (#tg-analysis).
//
// Разбор переписки локальным `claude` CLI: кнопка ✦ в титлбаре открывает
// попап, кнопка ANALYZE запускает разбор, поле снизу задаёт вопросы по той же
// истории, вкладка «Контекст чата» хранит пер-чатовую заметку пользователя.
//
// ============================================================================
// ЭТО ТОНКАЯ ОБЁРТКА, А НЕ РЕАЛИЗАЦИЯ
// ============================================================================
//
// Всё поведение попапа живёт в общем ядре `js/messenger/analysis-core.js`:
// вкладки, ленивый GET заметки и сохранённого разбора, тред Q&A, сворачивание,
// rAF-буфер дельт, фильтрация кадров по паре (dialog_id, run_id), усыновление
// `run_id` из `analysis_started`, перерисовка Markdown по `analysis_done`,
// разбор ошибок по `err.code`. То же ядро держит панель VK — на 85 % это был
// бы копипаст, и первая же правка в одном из мест дала бы расхождение вида
// «в VK работает, в Telegram нет».
//
// Здесь остаётся ровно то, что у Telegram СВОЁ:
//
// 1. **Ref'ы.** Разметка статична (index.html, блок `#tg-analysis`), ref'ы
//    взяты из `core/dom.js`: конвенция панели — все узлы резолвятся один раз
//    на верхнем уровне модуля ref'ов, а не querySelector'ом по месту.
// 2. **Транспорт.** Прямые вызовы `js/telegram/api.js` БЕЗ очереди: у VK есть
//    клиентский лимитер 3 rps на токен, у Telegram лимитера нет вовсе, и
//    заводить его ради попапа значило бы придумать ограничение, которого нет
//    на сервере.
// 3. **Блокирующих оверлеев нет.** У VK попап перекрывается captcha (код 14
//    прилетает на любой метод), у Telegram такого оверлея не существует —
//    `isBlockingOverlayOpen` всегда `false`, и Esc всегда принадлежит попапу.
// 4. **Имя чата** в шапке и активный чат панели — из `tgState`.
// 5. **Три подписи ошибок**, в которых стоит имя мессенджера:
//    `not_authorized`, `not_connected`, `flood_wait` (именно `flood_wait` —
//    это код, который отдаёт `plugins/telegram/src/routes/error.rs`, у VK на
//    том же месте стоит `rate_limited`). Остальные коды общие.
//
// ПУБЛИЧНЫЕ ЭКСПОРТЫ — те же имена, что и у `js/vk/analysis.js`: панель одна и
// та же, у модуля свой namespace, и разные имена в двух портах одной фичи
// только мешали бы читать `main.js` рядом.

import {
    $tgAnalysis, $tgAnalysisAsk, $tgAnalysisCancel, $tgAnalysisClear,
    $tgAnalysisClearThread, $tgAnalysisClose, $tgAnalysisDepth,
    $tgAnalysisError, $tgAnalysisOut, $tgAnalysisOutToggle, $tgAnalysisPaneCtx,
    $tgAnalysisPaneRun, $tgAnalysisPrompt, $tgAnalysisPromptClear,
    $tgAnalysisPromptCount, $tgAnalysisPromptSave, $tgAnalysisPromptStatus,
    $tgAnalysisQuestion, $tgAnalysisRefresh, $tgAnalysisSend, $tgAnalysisStart,
    $tgAnalysisStatus, $tgAnalysisTabCtx, $tgAnalysisTabRun, $tgAnalysisTarget,
    $tgAnalysisThread, $tgAnalysisUpdated,
} from '../core/dom.js';

import {
    analysisErrorText as coreErrorText, collapseStorageKey, createAnalysisPanel,
    depthStorageKey,
} from '../messenger/analysis-core.js';

import * as tgApi from './api.js';
import { displayName } from './format.js';
import { tgState } from './state.js';

export {
    BOTTOM_EPS_PX, charCount, clampPrompt, clampThread, clampThreadForView,
    DEFAULT_DEPTH, DEPTH_CHOICES, FALLBACK_MAX_CHARS, MAX_HISTORY_CHARS,
    MAX_HISTORY_TURNS, MAX_QUESTION_CHARS, MAX_THREAD_TURNS, pickDepth,
    updatedLabel,
} from '../messenger/analysis-core.js';

/** Префикс панели: ключи localStorage, классы разметки, имена id. */
const PREFIX = 'tg';

/** Ключ localStorage для выбранной глубины. Один на все вкладки — так и надо. */
export const DEPTH_STORAGE_KEY = depthStorageKey(PREFIX);

/**
 * Ключ localStorage для свёрнутого состояния разбора.
 *
 * Один на все чаты — так и задумано: это привычка смотреть в вопросы, а не
 * свойство конкретного чата.
 */
export const COLLAPSE_STORAGE_KEY = collapseStorageKey(PREFIX);

/**
 * Подписи ошибок, в которых стоит имя мессенджера.
 *
 * Общая формулировка ядра («нет связи с мессенджером») пользователю панели
 * Telegram ничего не объясняет: у него в соседней вкладке открыт VK, и вопрос
 * «а с каким именно?» стоит буквально.
 *
 * Остальные коды (`claude_unavailable`, `invalid_state`, `storage_failed`,
 * `invalid_input`) мессенджера не касаются и берутся из ядра.
 */
const TG_ERROR_TEXTS = {
    not_authorized: 'Аккаунт Telegram не подключён: войдите заново.',
    not_connected: 'Нет связи с Telegram — историю чата сейчас не выгрузить.',
    flood_wait: 'Слишком часто — Telegram просит подождать.',
};

/**
 * Человеческий текст ошибки — СТРОГО по `err.code`.
 *
 * Разбор по тексту здесь был бы особенно вреден: сообщение
 * `claude_unavailable` содержит путь до бинаря, который у каждого свой.
 *
 * @param {?Error & {code?: ?string}} err
 * @returns {string}
 */
export function analysisErrorText(err) {
    return coreErrorText(err, TG_ERROR_TEXTS);
}

/** Имя чата для шапки попапа; неизвестный чат — просто его id. */
function targetLabel(dialogId) {
    const id = String(dialogId == null ? '' : dialogId);
    const list = Array.isArray(tgState.dialogs) ? tgState.dialogs : [];
    const found = list.find((d) => d && String(d.id) === id);
    return found ? displayName(found) : id;
}

/**
 * Транспорт панели: прямые вызовы REST-клиента.
 *
 * Без очереди и без обёрток: клиентского лимитера у Telegram нет (в отличие от
 * VK с его 3 rps на токен), и промежуточный слой здесь был бы пустым.
 */
const api = {
    getPrompt: (dialogId) => tgApi.getChatPrompt(dialogId),
    putPrompt: (dialogId, text) => tgApi.putChatPrompt(dialogId, text),
    getSavedAnalysis: (dialogId) => tgApi.getSavedAnalysis(dialogId),
    clearAnalysis: (dialogId) => tgApi.clearSavedAnalysis(dialogId),
    clearThread: (dialogId) => tgApi.clearThread(dialogId),
    start: (dialogId, opts) => tgApi.startAnalysis(dialogId, opts),
    cancel: (dialogId, runId) => tgApi.cancelAnalysis(dialogId, runId),
};

/** Единственный попап панели: разметка в index.html тоже одна. */
const panel = createAnalysisPanel({
    prefix: PREFIX,
    refs: {
        root: $tgAnalysis,
        close: $tgAnalysisClose,
        target: $tgAnalysisTarget,
        tabRun: $tgAnalysisTabRun,
        tabCtx: $tgAnalysisTabCtx,
        paneRun: $tgAnalysisPaneRun,
        paneCtx: $tgAnalysisPaneCtx,
        depth: $tgAnalysisDepth,
        start: $tgAnalysisStart,
        refresh: $tgAnalysisRefresh,
        cancel: $tgAnalysisCancel,
        clear: $tgAnalysisClear,
        clearThread: $tgAnalysisClearThread,
        status: $tgAnalysisStatus,
        updated: $tgAnalysisUpdated,
        outToggle: $tgAnalysisOutToggle,
        out: $tgAnalysisOut,
        thread: $tgAnalysisThread,
        ask: $tgAnalysisAsk,
        question: $tgAnalysisQuestion,
        send: $tgAnalysisSend,
        error: $tgAnalysisError,
        prompt: $tgAnalysisPrompt,
        promptCount: $tgAnalysisPromptCount,
        promptSave: $tgAnalysisPromptSave,
        promptClear: $tgAnalysisPromptClear,
        promptStatus: $tgAnalysisPromptStatus,
    },
    api,
    // Оверлея, который перекрывал бы попап, у Telegram нет: Esc всегда его.
    isBlockingOverlayOpen: () => false,
    dialogLabel: targetLabel,
    activeDialogId: () => tgState.activeDialogId,
    errorTexts: TG_ERROR_TEXTS,
});

// ---------------------------------------------------------------------------
// Публичный интерфейс модуля — те же имена, что и у панели VK
// ---------------------------------------------------------------------------

/** Навешивает обработчики попапа. Идемпотентна. */
export function initAnalysis(opts) {
    panel.init(opts);
}

/** Снимает слушатели и очищает состояние (beforeunload, закрытие вкладки). */
export function teardownAnalysis() {
    panel.teardown();
}

/** Открывает попап. Без аргумента берётся активный чат панели. */
export function openAnalysis(dialogId) {
    panel.open(dialogId);
}

/** Закрывает попап. Идущий run НЕ отменяется — для этого есть STOP. */
export function closeAnalysis() {
    panel.close();
}

/** Открыт ли попап прямо сейчас. Читается keynav.js::panelVisible. */
export function isAnalysisOpen() {
    return panel.isOpen();
}

/** Переключает вкладку попапа (`run` | `ctx`). */
export function setAnalysisTab(tab) {
    panel.setTab(tab);
}

/** Читает заметку чата, если она ещё не прочитана. */
export async function ensurePrompt() {
    return panel.ensurePrompt();
}

/** Сохраняет заметку (CLEAR шлёт пустую строку). */
export async function savePrompt(override) {
    return panel.savePrompt(override);
}

/** Читает сохранённый разбор чата, если он ещё не читался. */
export async function ensureSavedAnalysis() {
    return panel.ensureSavedAnalysis();
}

/** CLEAR ✕ — стереть сохранённый разбор на сервере. */
export async function clearAnalysis() {
    return panel.clearAnalysis();
}

/** CLEAR Q&A — стереть тред вопросов на сервере (свой адрес, не разбора). */
export async function clearThread() {
    return panel.clearThread();
}

/** Ставит состояние сворачивания разбора и запоминает его. */
export function setOutCollapsed(on) {
    panel.setOutCollapsed(on);
}

/** Клик по шапке разбора. */
export function toggleOutCollapsed() {
    panel.toggleOutCollapsed();
}

/** ANALYZE: разбор всей выгруженной переписки. */
export async function startAnalysis() {
    return panel.startAnalysis();
}

/** REFRESH: дочитать только новое и заменить разбор обновлённым цельным. */
export async function refreshAnalysis() {
    return panel.refreshAnalysis();
}

/** ASK: вопрос по той же переписке. Пустой вопрос запроса не порождает. */
export async function askQuestion(text) {
    return panel.askQuestion(text);
}

/** STOP: гасит идущий run. */
export async function cancelRun() {
    return panel.cancelRun();
}

// Кадры /ws/telegram. Раскладка по типам — в `js/telegram/main.js::buildHandlers`;
// попап сам решает, его ли это run: сервер вещает без фильтрации по чату.

/** `analysis_started` — разбор начался: сервер сообщил объём выгрузки. */
export function onAnalysisStarted(evt) {
    panel.onStarted(evt);
}

/** `analysis_delta` — кусок ответа. Копится в буфере до кадра анимации. */
export function onAnalysisDelta(evt) {
    panel.onDelta(evt);
}

/** `analysis_done` — run закончился (успехом, отменой или таймаутом). */
export function onAnalysisDone(evt) {
    panel.onDone(evt);
}

/** `analysis_error` — процесс не запустился или упал. */
export function onAnalysisError(evt) {
    panel.onError(evt);
}

/** Снимок состояния для e2e и `_debugState()` панели. */
export function _debugAnalysisState() {
    return panel.debugState();
}
