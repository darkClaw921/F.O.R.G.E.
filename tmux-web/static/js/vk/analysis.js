// tmux-web — VK plugin: попап «Анализ чата» (#vk-analysis).
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
// разбор ошибок по `err.code`. Панели Telegram и Bitrix24 берут ровно то же
// ядро — на 85 % это был бы копипаст, и первая же правка в одном из трёх мест
// дала бы расхождение вида «в VK работает, в Telegram нет».
//
// Здесь остаётся ровно то, что у VK СВОЁ:
//
// 1. **Ref'ы.** Разметка статична (index.html, блок `#vk-analysis`), ref'ы
//    взяты из `core/dom.js`: контракт «ref в dom.js ⇔ id в index.html»
//    проверяется `vk_contract.test.mjs`.
// 2. **Транспорт.** Каждый вызов идёт через `vkLimiter` (клиентские 3 rps на
//    токен, `js/vk/ratelimit.js`) и `callC` из `js/vk/api.js` — у Telegram
//    лимитера нет вовсе, и тащить его в ядро значило бы навязать чужую
//    механику. Очередь берётся ПЕРЕД запросом: ядро ждёт `await`, и ему
//    безразлично, что внутри — очередь или сразу `fetch`.
// 3. **Captcha.** Оверлей VK живёт выше попапа (z-index 7 против 6) и может
//    всплыть прямо во время выгрузки истории. Пока он на экране, Esc
//    принадлежит ему — ядро спрашивает об этом `isBlockingOverlayOpen`.
// 4. **Имя беседы** в шапке и активная беседа панели — из `vkState`.
// 5. **Три подписи ошибок**, в которых стоит имя мессенджера: `not_authorized`,
//    `not_connected`, `rate_limited`. Остальные коды общие.
//
// ПУБЛИЧНЫЕ ЭКСПОРТЫ НЕ МЕНЯЛИСЬ при выносе ядра: `js/vk/main.js`, keynav и
// тесты панели зовут те же имена, что и раньше.

import {
    $vkAnalysis, $vkAnalysisAsk, $vkAnalysisCancel, $vkAnalysisClear,
    $vkAnalysisClearThread, $vkAnalysisClose, $vkAnalysisDepth,
    $vkAnalysisError, $vkAnalysisOut, $vkAnalysisOutToggle, $vkAnalysisPaneCtx,
    $vkAnalysisPaneRun, $vkAnalysisPrompt, $vkAnalysisPromptClear,
    $vkAnalysisPromptCount, $vkAnalysisPromptSave, $vkAnalysisPromptStatus,
    $vkAnalysisQuestion, $vkAnalysisRefresh, $vkAnalysisSend, $vkAnalysisStart,
    $vkAnalysisStatus, $vkAnalysisTabCtx, $vkAnalysisTabRun, $vkAnalysisTarget,
    $vkAnalysisThread, $vkAnalysisUpdated,
} from '../core/dom.js';

import {
    analysisErrorText as coreErrorText, collapseStorageKey, createAnalysisPanel,
    depthStorageKey,
} from '../messenger/analysis-core.js';

import * as vkApi from './api.js';
import { isCaptchaOpen } from './captcha.js';
import { displayName } from './format.js';
import { PRIORITY_USER, vkLimiter } from './ratelimit.js';
import { vkState } from './state.js';

export {
    BOTTOM_EPS_PX, charCount, clampPrompt, clampThread, clampThreadForView,
    DEFAULT_DEPTH, DEPTH_CHOICES, FALLBACK_MAX_CHARS, MAX_HISTORY_CHARS,
    MAX_HISTORY_TURNS, MAX_QUESTION_CHARS, MAX_THREAD_TURNS, pickDepth,
    updatedLabel,
} from '../messenger/analysis-core.js';

/** Префикс панели: ключи localStorage, классы разметки, имена id. */
const PREFIX = 'vk';

/** Ключ localStorage для выбранной глубины. Один на все вкладки — так и надо. */
export const DEPTH_STORAGE_KEY = depthStorageKey(PREFIX);

/**
 * Ключ localStorage для свёрнутого состояния разбора.
 *
 * Один на все беседы — так и задумано (решение ТЗ): это привычка смотреть в
 * вопросы, а не свойство конкретного чата.
 */
export const COLLAPSE_STORAGE_KEY = collapseStorageKey(PREFIX);

/**
 * Подписи ошибок, в которых стоит имя мессенджера.
 *
 * Общая формулировка ядра («нет связи с мессенджером») пользователю панели VK
 * ничего не объясняет: у него в соседней вкладке открыт Telegram, и вопрос
 * «а с каким именно?» стоит буквально.
 *
 * Остальные коды (`claude_unavailable`, `invalid_state`, `storage_failed`,
 * `invalid_input`) мессенджера не касаются и берутся из ядра.
 */
const VK_ERROR_TEXTS = {
    not_authorized: 'Токен VK не привязан: войдите заново.',
    not_connected: 'Нет связи с VK — историю беседы сейчас не выгрузить.',
    rate_limited: 'Слишком часто — VK просит подождать.',
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
    return coreErrorText(err, VK_ERROR_TEXTS);
}

/** Имя беседы для шапки попапа; неизвестная беседа — просто её id. */
function targetLabel(dialogId) {
    const id = String(dialogId == null ? '' : dialogId);
    const list = Array.isArray(vkState.dialogs) ? vkState.dialogs : [];
    const found = list.find((d) => d && String(d.id) === id);
    return found ? displayName(found) : id;
}

/**
 * Транспорт панели: очередь 3 rps, потом запрос.
 *
 * Очередь берётся ПЕРЕД каждым обращением — ровно там же, где она бралась до
 * выноса ядра. Ядро видит только `Promise`, а порядок «сначала слот, потом
 * сеть» остаётся VK-специфичным: у Telegram и Bitrix24 своих лимитеров нет.
 */
const api = {
    async getPrompt(dialogId) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.getChatPrompt(dialogId);
    },
    async putPrompt(dialogId, text) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.putChatPrompt(dialogId, text);
    },
    async getSavedAnalysis(dialogId) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.getSavedAnalysis(dialogId);
    },
    async clearAnalysis(dialogId) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.clearSavedAnalysis(dialogId);
    },
    async clearThread(dialogId) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.clearThread(dialogId);
    },
    async start(dialogId, opts) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.startAnalysis(dialogId, opts);
    },
    async cancel(dialogId, runId) {
        await vkLimiter.acquire(PRIORITY_USER);
        return vkApi.cancelAnalysis(dialogId, runId);
    },
};

/** Единственный попап панели: разметка в index.html тоже одна. */
const panel = createAnalysisPanel({
    prefix: PREFIX,
    refs: {
        root: $vkAnalysis,
        close: $vkAnalysisClose,
        target: $vkAnalysisTarget,
        tabRun: $vkAnalysisTabRun,
        tabCtx: $vkAnalysisTabCtx,
        paneRun: $vkAnalysisPaneRun,
        paneCtx: $vkAnalysisPaneCtx,
        depth: $vkAnalysisDepth,
        start: $vkAnalysisStart,
        refresh: $vkAnalysisRefresh,
        cancel: $vkAnalysisCancel,
        clear: $vkAnalysisClear,
        clearThread: $vkAnalysisClearThread,
        status: $vkAnalysisStatus,
        updated: $vkAnalysisUpdated,
        outToggle: $vkAnalysisOutToggle,
        out: $vkAnalysisOut,
        thread: $vkAnalysisThread,
        ask: $vkAnalysisAsk,
        question: $vkAnalysisQuestion,
        send: $vkAnalysisSend,
        error: $vkAnalysisError,
        prompt: $vkAnalysisPrompt,
        promptCount: $vkAnalysisPromptCount,
        promptSave: $vkAnalysisPromptSave,
        promptClear: $vkAnalysisPromptClear,
        promptStatus: $vkAnalysisPromptStatus,
    },
    api,
    // Captcha живёт ВЫШЕ попапа: пока она на экране, Esc принадлежит ей.
    isBlockingOverlayOpen: isCaptchaOpen,
    dialogLabel: targetLabel,
    activeDialogId: () => vkState.activeDialogId,
    errorTexts: VK_ERROR_TEXTS,
});

// ---------------------------------------------------------------------------
// Публичный интерфейс модуля — те же имена, что и до выноса ядра
// ---------------------------------------------------------------------------

/** Навешивает обработчики попапа. Идемпотентна (как `initCaptcha`). */
export function initAnalysis(opts) {
    panel.init(opts);
}

/** Снимает слушатели и очищает состояние (beforeunload, закрытие вкладки). */
export function teardownAnalysis() {
    panel.teardown();
}

/** Открывает попап. Без аргумента берётся активная беседа панели. */
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

/** Читает заметку беседы, если она ещё не прочитана. */
export async function ensurePrompt() {
    return panel.ensurePrompt();
}

/** Сохраняет заметку (CLEAR шлёт пустую строку). */
export async function savePrompt(override) {
    return panel.savePrompt(override);
}

/** Читает сохранённый разбор беседы, если он ещё не читался. */
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

// Кадры /ws/vk. Раскладка по типам — в `js/vk/main.js::buildHandlers`; попап
// сам решает, его ли это run: сервер вещает без фильтрации по беседе.

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
