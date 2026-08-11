// tmux-web — Bitrix24 plugin: попап «Анализ чата» (#b24-analysis).
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
// разбор ошибок по `err.code`. То же ядро держит панели VK и Telegram — на
// 85 % это был бы копипаст, и первая же правка в одном из мест дала бы
// расхождение вида «в Telegram работает, в Битрикс24 нет».
//
// Здесь остаётся ровно то, что у Битрикс24 СВОЁ:
//
// 1. **Ref'ы.** Разметка статична (index.html, блок `#b24-analysis`), ref'ы
//    взяты из `core/dom.js`: конвенция панели — все узлы резолвятся один раз
//    на верхнем уровне модуля ref'ов, а не querySelector'ом по месту.
// 2. **Транспорт.** Прямые вызовы `js/bitrix/api.js` БЕЗ очереди: у VK есть
//    клиентский лимитер 3 rps на токен, а интенсивность портала душит он сам
//    (состояние `throttled` панели), и второй лимитер ради попапа означал бы
//    ограничение, которого нет на сервере.
// 3. **Блокирующих оверлеев нет.** У VK попап перекрывается captcha (код 14
//    прилетает на любой метод), у портала такого оверлея не существует вовсе:
//    вебхук либо подключён, либо панель закрыта экраном подключения целиком —
//    и тогда попап не открыть. `isBlockingOverlayOpen` всегда `false`, и Esc
//    всегда принадлежит попапу.
// 4. **Имя чата** в шапке и активный чат панели — из `b24State`.
// 5. **ГЛУБИНА ВЫГРУЗКИ ДРУГАЯ: 50 / 100 / 200, по умолчанию 100.** Это не
//    вкус, а платформа: `im.dialog.messages.get` отдаёт максимум 50 сообщений
//    за вызов при бюджете 2 rps, то есть привычные для Telegram и VK 500
//    сообщений — это десять запросов и пять секунд ожидания ПЕРЕД первым
//    словом модели. Тот же список стоит потолком на бэкенде: селект не обещает
//    того, чего сервер не сделает.
// 6. **Четыре подписи ошибок**, в которых стоит имя мессенджера или его
//    особенность: `not_authorized`, `not_connected`, `flood_wait` (именно
//    `flood_wait`, как у Telegram, — это код `plugins/bitrix/src/routes/error.rs`;
//    у VK на том же месте стоит `rate_limited`) и `scope_missing` — своё,
//    которого нет ни у одного соседа: у портала права выдаются ГАЛОЧКАМИ при
//    создании вебхука, и «нет доступа» здесь чинится не повторным входом, а
//    пересозданием вебхука с нужной областью.
//
// ПУБЛИЧНЫЕ ЭКСПОРТЫ — те же имена, что и у `js/telegram/analysis.js` и
// `js/vk/analysis.js`: панель одна и та же, у модуля свой namespace, и разные
// имена в трёх портах одной фичи только мешали бы читать три `main.js` рядом.

import {
    $b24Analysis, $b24AnalysisAsk, $b24AnalysisCancel, $b24AnalysisClear,
    $b24AnalysisClearThread, $b24AnalysisClose, $b24AnalysisDepth,
    $b24AnalysisError, $b24AnalysisOut, $b24AnalysisOutToggle,
    $b24AnalysisPaneCtx, $b24AnalysisPaneRun, $b24AnalysisPrompt,
    $b24AnalysisPromptClear, $b24AnalysisPromptCount, $b24AnalysisPromptSave,
    $b24AnalysisPromptStatus, $b24AnalysisQuestion, $b24AnalysisRefresh,
    $b24AnalysisSend, $b24AnalysisStart, $b24AnalysisStatus, $b24AnalysisTabCtx,
    $b24AnalysisTabRun, $b24AnalysisTarget, $b24AnalysisThread,
    $b24AnalysisUpdated,
} from '../core/dom.js';

import {
    analysisErrorText as coreErrorText, collapseStorageKey, createAnalysisPanel,
    depthStorageKey, pickDepthFrom,
} from '../messenger/analysis-core.js';

import * as b24Api from './api.js';
import { displayName, findDialog } from './format.js';
import { b24State } from './state.js';

export {
    BOTTOM_EPS_PX, charCount, clampPrompt, clampThread, clampThreadForView,
    FALLBACK_MAX_CHARS, MAX_HISTORY_CHARS, MAX_HISTORY_TURNS,
    MAX_QUESTION_CHARS, MAX_THREAD_TURNS, updatedLabel,
} from '../messenger/analysis-core.js';

/** Префикс панели: ключи localStorage, классы разметки, имена id. */
const PREFIX = 'b24';

/**
 * Допустимые значения глубины — ровно те, что стоят `<option>`'ами в разметке.
 *
 * СВОИ, а не ядровые `[50, 200, 500]`: потолок страницы портала — 50 сообщений
 * при бюджете 2 rps (см. п.5 в шапке). Бэкенд клампит по этому же списку, то
 * есть селект не обещает того, чего сервер не сделает.
 *
 * Список закрытый: значение приезжает из localStorage, то есть из места, куда
 * писать может кто угодно (другая версия панели, руки в devtools). Чужое число
 * молча игнорируется.
 */
export const DEPTH_CHOICES = [50, 100, 200];

/**
 * Глубина по умолчанию — та же, что `DEFAULT_DEPTH` на стороне плагина.
 *
 * Дублируется намеренно: значение стоит и в `<option selected>` разметки, и
 * здесь как страховка на случай, если селект не резолвился (старый index.html
 * из кэша PWA).
 */
export const DEFAULT_DEPTH = 100;

/**
 * Нормализует глубину: только значение из [`DEPTH_CHOICES`], иначе `null`.
 *
 * Своя, а не переэкспорт ядровой `pickDepth`: та разбирает по списку ядра и
 * приняла бы 500, которого у портала нет ни в разметке, ни на бэкенде.
 *
 * @param {*} value
 * @returns {?number}
 */
export function pickDepth(value) {
    return pickDepthFrom(DEPTH_CHOICES, value);
}

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
 * Подписи ошибок, в которых стоит имя мессенджера или его особенность.
 *
 * Общая формулировка ядра («нет связи с мессенджером») пользователю панели
 * Битрикс24 ничего не объясняет: у него в соседних вкладках открыты Telegram и
 * VK, и вопрос «а с каким именно?» стоит буквально.
 *
 * `not_authorized` говорит про ВЕБХУК, а не про «войдите заново»: у портала нет
 * ни сессии, ни кода из СМС — подключение это один вставленный URL, и чинится
 * оно ровно его повторной вставкой.
 *
 * `scope_missing` (409) — своя, у соседей такого кода нет. Права вебхука
 * выдаются галочками при его создании и дописать их потом нечем: вебхук
 * пересоздают. Отправить пользователя «войти заново» здесь значило бы
 * отправить его по кругу.
 *
 * Остальные коды (`claude_unavailable`, `invalid_state`, `storage_failed`,
 * `invalid_input`) мессенджера не касаются и берутся из ядра.
 */
const B24_ERROR_TEXTS = {
    not_authorized: 'Битрикс24 не подключён: вставьте вебхук заново.',
    not_connected: 'Нет связи с Битрикс24 — историю чата сейчас не выгрузить.',
    flood_wait: 'Слишком часто — Битрикс24 просит подождать.',
    scope_missing: 'У вебхука не хватает прав: пересоздайте его с областями «Чат и уведомления» (im) и «Задачи» (task).',
};

/**
 * Человеческий текст ошибки — СТРОГО по `err.code`.
 *
 * Разбор по тексту здесь был бы особенно вреден: сообщение
 * `claude_unavailable` содержит путь до бинаря, который у каждого свой, а
 * ошибки портала приходят его дословной формулировкой.
 *
 * @param {?Error & {code?: ?string}} err
 * @returns {string}
 */
export function analysisErrorText(err) {
    return coreErrorText(err, B24_ERROR_TEXTS);
}

/**
 * Имя чата для шапки попапа; неизвестный чат — просто его id.
 *
 * Через общие `findDialog`/`displayName` панели, а не по полю `title` строки:
 * у чата задачи заголовок собирается тем же форматтером, и второй способ
 * назвать чат разъехался бы с колонкой при первой же его правке.
 */
function targetLabel(dialogId) {
    const found = findDialog(b24State.dialogs, dialogId);
    return found ? displayName(found) : String(dialogId == null ? '' : dialogId);
}

/**
 * Транспорт панели: прямые вызовы REST-клиента.
 *
 * Без очереди и без обёрток: клиентского лимитера у панели нет (в отличие от
 * VK с его 3 rps на токен) — интенсивность душит сам портал, и промежуточный
 * слой здесь был бы пустым.
 */
const api = {
    getPrompt: (dialogId) => b24Api.getChatPrompt(dialogId),
    putPrompt: (dialogId, text) => b24Api.putChatPrompt(dialogId, text),
    getSavedAnalysis: (dialogId) => b24Api.getSavedAnalysis(dialogId),
    clearAnalysis: (dialogId) => b24Api.clearSavedAnalysis(dialogId),
    clearThread: (dialogId) => b24Api.clearThread(dialogId),
    start: (dialogId, opts) => b24Api.startAnalysis(dialogId, opts),
    cancel: (dialogId, runId) => b24Api.cancelAnalysis(dialogId, runId),
};

/** Единственный попап панели: разметка в index.html тоже одна. */
const panel = createAnalysisPanel({
    prefix: PREFIX,
    refs: {
        root: $b24Analysis,
        close: $b24AnalysisClose,
        target: $b24AnalysisTarget,
        tabRun: $b24AnalysisTabRun,
        tabCtx: $b24AnalysisTabCtx,
        paneRun: $b24AnalysisPaneRun,
        paneCtx: $b24AnalysisPaneCtx,
        depth: $b24AnalysisDepth,
        start: $b24AnalysisStart,
        refresh: $b24AnalysisRefresh,
        cancel: $b24AnalysisCancel,
        clear: $b24AnalysisClear,
        clearThread: $b24AnalysisClearThread,
        status: $b24AnalysisStatus,
        updated: $b24AnalysisUpdated,
        outToggle: $b24AnalysisOutToggle,
        out: $b24AnalysisOut,
        thread: $b24AnalysisThread,
        ask: $b24AnalysisAsk,
        question: $b24AnalysisQuestion,
        send: $b24AnalysisSend,
        error: $b24AnalysisError,
        prompt: $b24AnalysisPrompt,
        promptCount: $b24AnalysisPromptCount,
        promptSave: $b24AnalysisPromptSave,
        promptClear: $b24AnalysisPromptClear,
        promptStatus: $b24AnalysisPromptStatus,
    },
    api,
    // Оверлея, который перекрывал бы попап, у панели нет: экран подключения
    // закрывает её ЦЕЛИКОМ, и попап из-под него не открыть. Esc всегда его.
    isBlockingOverlayOpen: () => false,
    dialogLabel: targetLabel,
    activeDialogId: () => b24State.activeDialogId,
    errorTexts: B24_ERROR_TEXTS,
    // Свой потолок платформы (см. п.5 в шапке). Ядро без этих двух полей
    // осталось бы на 50/200/500 и предложило бы глубину, которой у портала нет.
    depthChoices: DEPTH_CHOICES,
    defaultDepth: DEFAULT_DEPTH,
});

// ---------------------------------------------------------------------------
// Публичный интерфейс модуля — те же имена, что и у панелей VK и Telegram
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

// Кадры /ws/bitrix. Раскладка по типам — в `js/bitrix/main.js::buildHandlers`;
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
