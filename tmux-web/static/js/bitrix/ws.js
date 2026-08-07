// tmux-web — Bitrix24 plugin: WebSocket-клиент `/ws/bitrix`.
//
// Тонкая надстройка над js/messenger/ws-core.js: ядро держит соединение,
// лестницу реконнекта, ping/pong и оценку задержки, а этот модуль знает про
// адрес, про токен и про то, куда класть результат. Копировать ядро ради
// третьей панели нечего — оно уже покрыто Node-тестом
// (tmux-web/tests/frontend/messenger_ws_core.test.mjs).
//
// Соединение ПОСТОЯННОЕ и НЕ привязано к вкладке devforge. Сервер вещает
// события без фильтрации по диалогу (plugins/bitrix/src/ws/mod.rs), потому
// что бейдж непрочитанного на кнопке Ⓑ и тосты обязаны работать, пока
// пользователь сидит в terminal/tasks. Поэтому здесь нет teardown'а при смене
// вкладки — только visibilitychange и beforeunload (см. core/bootstrap.js).
//
// Контракт кадров — plugins/bitrix/src/ws/protocol.rs. Константа
// SERVER_FRAMES ниже обязана совпадать с тестом
// `server_frame_tags_are_frozen` в том же файле.

import { withWsToken } from '../core/auth.js';
import { createWsClient } from '../messenger/ws-core.js';
import { b24State } from './state.js';

/** Лестница реконнекта, мс. Последнее значение повторяется. */
export const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/**
 * Ожидаемый период серверного ping'а, мс — HEARTBEAT_INTERVAL в
 * plugins/bitrix/src/ws/mod.rs. Используется только как эталон для оценки
 * задержки: расхождение с сервером ухудшит точность LAT, но ничего не сломает.
 */
export const PING_INTERVAL_MS = 15000;

/**
 * Полный набор кадров сервера — для сверки с protocol.rs глазами и для
 * диагностики: неизвестный тип не ошибка (сервер может начать присылать новое
 * событие раньше, чем обновится закешированный фронт), но в консоли о нём
 * стоит сказать.
 *
 * Часть кадров платформа не отдаёт никогда (`typing`, `presence`, `read` —
 * см. §6 плана и шапку protocol.rs). Они перечислены здесь ровно потому, что
 * обработчики под них всё равно объявляются: иначе появление такого кадра в
 * будущем потребовало бы правки фронта, а сегодня даёт шум «no handler».
 */
export const SERVER_FRAMES = [
    'login_state', 'dialogs_patch', 'message_new', 'message_update',
    'message_delete', 'reaction', 'typing', 'read', 'counters', 'presence',
    'task_update', 'upload_progress', 'download_progress', 'error',
    'resync', 'ping',
];

const local = {
    /** Клиент ядра (создаётся при первом connect). */
    client: null,
    /** Карта type → обработчик (из buildHandlers в main.js). */
    handlers: null,
};

/** Адрес соединения с токеном в query (WebSocket не умеет заголовки). */
function urlFor() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return withWsToken(`${proto}://${location.host}/ws/bitrix`);
}

/** Зовёт обработчик панели, не давая его исключению порвать соединение. */
function callHandler(name, arg) {
    const h = local.handlers && local.handlers[name];
    if (typeof h !== 'function') return;
    try {
        h(arg);
    } catch (e) {
        console.warn(`[b24-ws] handler ${name} threw`, e);
    }
}

/** Раскладывает кадр по обработчикам панели. */
function onFrame(msg) {
    if (msg.type === 'ping') {
        // Ответ `pong` уже отправлен ядром; панели интересен лишь факт
        // живого соединения.
        callHandler('ping', msg);
        return;
    }
    if (msg.type === 'resync') {
        // Клиент отстал (broadcast Lagged) либо курсор событий признан
        // устаревшим (Р18) — состояние перечитывается по REST целиком,
        // поэтому у resync отдельный колбэк, а не общий обработчик.
        callHandler('resync', msg);
        return;
    }
    if (typeof (local.handlers && local.handlers[msg.type]) === 'function') {
        callHandler(msg.type, msg);
        return;
    }
    console.debug('[b24-ws] no handler for', msg.type, msg);
}

function ensureClient() {
    if (local.client) return local.client;
    local.client = createWsClient({
        urlFor,
        backoffs: WS_BACKOFFS_MS,
        pingIntervalMs: PING_INTERVAL_MS,
        label: 'b24-ws',
        onFrame,
        onLatency: (lat) => {
            b24State.lat = lat;
            callHandler('lat', lat);
        },
        onOpen: () => {
            b24State.ws = local.client ? local.client.socket() : null;
            callHandler('open');
        },
        onClose: (info) => {
            b24State.ws = null;
            callHandler('close', info);
        },
    });
    return local.client;
}

/**
 * Открыть (или переиспользовать) соединение.
 *
 * Идемпотентен: при живом сокете только обновляет handlers. Это важно, потому
 * что connect зовут из нескольких точек (гейт при загрузке страницы,
 * applyLoginState после успешного подключения, возврат вкладки из фона), и
 * порядок между ними не определён.
 *
 * @param {object} handlers — { [frameType]: fn(msg), open?, close?, resync?, lat?, ping? }
 */
export function connectB24Ws(handlers) {
    local.handlers = handlers || local.handlers || {};
    const client = ensureClient();
    client.connect();
    b24State.ws = client.socket();
}

/**
 * Закрыть соединение без реконнекта.
 *
 * Вызывается на visibilitychange:hidden (скрытую вкладку держать сокетом
 * незачем — бейджа всё равно не видно; заодно сервер уводит опрос портала в
 * idle-каденс, Р4) и на beforeunload.
 */
export function disconnectB24Ws() {
    if (local.client) local.client.disconnect();
    b24State.ws = null;
    b24State.lat = null;
}

/** Открыто ли соединение прямо сейчас. */
export function isB24WsOpen() {
    return !!local.client && local.client.isOpen();
}

/** Снимок состояния для e2e/диагностики. */
export function _debugWsState() {
    const base = local.client ? local.client._debug() : {
        readyState: null,
        backoffStep: 0,
        closedByUs: false,
        reconnectPending: false,
        opened: 0,
    };
    return { ...base, lat: b24State.lat, handlers: Object.keys(local.handlers || {}).length };
}
