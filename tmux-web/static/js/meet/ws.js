// tmux-web — forge-meet: WebSocket-клиент `/ws/meet`.
//
// ЧЕТВЁРТЫЙ клиент поверх `messenger/ws-core.js` (после Telegram, VK и
// Битрикса). Ядро держит соединение, лестницу реконнекта, ping/pong и оценку
// задержки; этот модуль знает только адрес, токен и то, КУДА положить кадр.
// Своей копии backoff'а здесь нет и быть не должно — на инвариант «снять
// обработчики ДО close()» есть тест в `messenger_ws_core.test.mjs`.
//
// ============================================================================
// СОБЫТИЯ РАСХОДЯТСЯ ЧЕРЕЗ СТОР, А НЕ ЧЕРЕЗ ПАНЕЛИ
// ============================================================================
//
// Здесь нет ни одного импорта панели и ни одной записи в `window.ForgeApp`.
// Кадр разбирается и отдаётся в `messenger/meet-store.js`, у которого уже есть
// подписчики. Панели импортируют стор ровно в ту же сторону, в какую уже
// импортируют `analysis-core.js`, поэтому направление зависимостей не
// разворачивается: транспорт ничего не знает о том, кто на него смотрит.
//
// ============================================================================
// СОЕДИНЕНИЕ НЕ ПРИВЯЗАНО К АКТИВНОЙ ВКЛАДКЕ
// ============================================================================
//
// Индикатор записи обязан работать, когда человек нажал кнопку в Telegram и
// ушёл в терминал. Поэтому на бэкенде нет фильтрации по встрече, а здесь нет
// teardown'а при смене вкладки: соединение рвётся только при уходе страницы в
// фон (`suspendMeet`) и при выгрузке (`teardownMeet`).
//
// ============================================================================
// ГЕЙТ
// ============================================================================
//
// `connectMeetWs()` ПЕРВОЙ СТРОКОЙ спрашивает `meetEnabled()`. Плагин запущен
// без `--meet` — соединение не открывается вовсе, и это не зависит от того,
// кто и откуда позвал connect: гейт закрыт в самом транспорте, а не только в
// вызывающем коде.

import { withWsToken } from '../core/auth.js';
import { createWsClient } from '../messenger/ws-core.js';
import {
    meetEnabled,
    applyMeetSnapshot,
    applyMeetingUpdate,
    applyMeetDeps,
    applyMeetProgress,
    applyMeetTranscriptDelta,
    applyMeetParticipants,
    applyMeetNeedsHuman,
    applyMeetError,
    applyMeetModelProgress,
    resyncMeet,
    noteMeetConnection,
    stopMeetTicker,
} from '../messenger/meet-store.js';

/** Лестница реконнекта, мс. Последнее значение повторяется. */
export const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/**
 * Ожидаемый период серверного ping'а, мс — `HEARTBEAT_INTERVAL` в
 * `plugins/meet/src/ws/mod.rs`. Только эталон для оценки задержки: расхождение
 * ухудшит точность цифры, но ничего не сломает.
 */
export const PING_INTERVAL_MS = 15000;

/**
 * КАРТА КАДРОВ — контракт с `MeetServerMsg` (`plugins/meet/src/ws/protocol.rs`).
 *
 * Ключи сверяются с вариантами enum'а В ОБЕ СТОРОНЫ контрактным тестом:
 * кадр без обработчика и обработчик без кадра одинаково валят прогон.
 * Порядок здесь ТОТ ЖЕ, что в enum'е, — иначе будущий diff перестанет
 * отличать «добавили кадр» от «переставили строки».
 *
 * `ping` в карте есть, хотя `pong` отправляет ядро: панелям важен сам факт
 * живого соединения, и молчаливое исключение из карты выглядело бы как
 * забытый кадр.
 */
export const FRAME_HANDLERS = {
    snapshot: (msg) => applyMeetSnapshot(msg),
    meeting_update: (msg) => applyMeetingUpdate(msg.meeting),
    deps: (msg) => applyMeetDeps(msg.deps),
    progress: (msg) => applyMeetProgress(msg),
    transcript_delta: (msg) => applyMeetTranscriptDelta(msg),
    participants: (msg) => applyMeetParticipants(msg),
    needs_human: (msg) => applyMeetNeedsHuman(msg),
    error: (msg) => applyMeetError(msg),
    // Скачивание модели whisper. Кадр без `meeting_id` — модель качают из
    // настроек, когда никакой встречи ещё нет.
    model_progress: (msg) => applyMeetModelProgress(msg),
    // Отставший подписчик. Сервер шлёт кадр ВМЕСТО разрыва, и правильная
    // реакция — один добор батчем, а не реконнект и не шквал запросов.
    resync: () => resyncMeet(),
    ping: () => noteMeetConnection(true, { ping: true }),
};

/** Плоский список кадров — для сверки глазами и для контрактного теста. */
export const SERVER_FRAMES = Object.keys(FRAME_HANDLERS);

const local = {
    /** Клиент ядра (создаётся при первом connect). */
    client: null,
    /** Фабрика сокета, переданная в connect (тесты подставляют свою). */
    socketFactory: null,
    /** Кадры, тип которых мы не знаем: считаем, но не шумим на каждый. */
    unknown: new Map(),
};

/** Адрес соединения с токеном в query (WebSocket не умеет заголовки). */
function urlFor() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return withWsToken(`${proto}://${location.host}/ws/meet`);
}

/**
 * Разложить кадр по обработчикам.
 *
 * Незнакомый тип — НЕ ошибка: сервер может начать присылать новый кадр раньше,
 * чем обновится закэшированный PWA фронт. Но и молчать про него нельзя, иначе
 * расхождение версий выглядит как «событие потерялось». Компромисс: `debug`
 * один раз на тип.
 */
export function dispatchMeetFrame(msg) {
    if (!msg || typeof msg !== 'object' || typeof msg.type !== 'string') return false;
    const handler = FRAME_HANDLERS[msg.type];
    if (!handler) {
        const seen = local.unknown.get(msg.type) || 0;
        local.unknown.set(msg.type, seen + 1);
        if (seen === 0) console.debug(`[meet-ws] неизвестный кадр ${msg.type}`);
        return false;
    }
    try {
        handler(msg);
    } catch (e) {
        // Исключение обработчика не имеет права порвать соединение: следующий
        // кадр должен дойти.
        console.warn(`[meet-ws] handler ${msg.type} threw`, e);
    }
    return true;
}

/**
 * Открыть постоянное соединение.
 *
 * @param {object} [opts]
 * @param {Function} [opts.socketFactory] — конструктор сокета (тесты)
 * @param {Array<number>} [opts.backoffs]
 * @returns {boolean} открыли ли (false = плагин выключен)
 */
export function connectMeetWs(opts) {
    // ГЕЙТ. Первая строка, до создания клиента и до любых таймеров.
    if (!meetEnabled()) return false;
    const o = opts || {};
    if (!local.client) {
        local.socketFactory = typeof o.socketFactory === 'function' ? o.socketFactory : null;
        local.client = createWsClient({
            urlFor,
            backoffs: Array.isArray(o.backoffs) && o.backoffs.length ? o.backoffs : WS_BACKOFFS_MS,
            pingIntervalMs: PING_INTERVAL_MS,
            label: 'meet-ws',
            socketFactory: local.socketFactory || undefined,
            onFrame: dispatchMeetFrame,
            onOpen: () => noteMeetConnection(true, null),
            onClose: (info) => noteMeetConnection(false, info),
        });
    }
    local.client.connect();
    return true;
}

/** Закрыть соединение без реконнекта (порядок снятия обработчиков — в ядре). */
export function disconnectMeetWs() {
    if (!local.client) return;
    local.client.disconnect();
}

/** Открыто ли соединение прямо сейчас. */
export function isMeetWsOpen() {
    return !!local.client && local.client.isOpen();
}

/**
 * Страница ушла в фон.
 *
 * Соединение рвётся: держать сокет ради индикатора, которого не видно, незачем.
 * Пропущенные за время фона события догоняются на `resume` — снапшот приходит
 * первым кадром нового соединения, а состояния строк добираются батчем при
 * первой же перерисовке ленты.
 */
export function suspendMeet() {
    disconnectMeetWs();
}

/** Страница вернулась из фона. */
export function resumeMeet() {
    if (!meetEnabled()) return false;
    return connectMeetWs();
}

/**
 * Выгрузка страницы.
 *
 * Живой таймер записи гасится ЗДЕСЬ: интервал переживает уход документа в
 * bfcache и потом стреляет по мёртвому DOM.
 */
export function teardownMeet() {
    disconnectMeetWs();
    stopMeetTicker();
    local.client = null;
    local.socketFactory = null;
    local.unknown.clear();
}

/** Снимок состояния для тестов и e2e. */
export function _debugMeetWs() {
    return {
        created: !!local.client,
        open: isMeetWsOpen(),
        core: local.client ? local.client._debug() : null,
        unknownFrames: Object.fromEntries(local.unknown),
    };
}
