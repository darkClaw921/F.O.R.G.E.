// tmux-web — VK plugin: WebSocket-клиент `/ws/vk`.
//
// Тонкая надстройка над js/messenger/ws-core.js: ядро держит соединение,
// лестницу реконнекта, ping/pong и оценку задержки, а этот модуль знает про
// адрес, про токен и про то, куда класть результат.
//
// Соединение ПОСТОЯННОЕ и НЕ привязано к вкладке devforge. Сервер вещает
// события без фильтрации по беседе (plugins/vk/src/state.rs), потому что
// бейдж непрочитанного на кнопке Ⓥ и тосты обязаны работать, пока
// пользователь сидит в terminal/tasks. Поэтому здесь нет teardown'а при смене
// вкладки — только visibilitychange и beforeunload (см. core/bootstrap.js).
//
// Контракт кадров — plugins/vk/src/ws/protocol.rs:
//   server → client: login_state, new_message, message_edited,
//     message_deleted, typing, online, read, upload_progress,
//     download_progress, notify_settings, unread_total, history_cleared,
//     dialog_changed, analysis_started, analysis_delta, analysis_done,
//     analysis_error, resync, ping;
//   client → server: pong (единственный — вся мутация идёт по REST).
//
// Инвариант «снять обработчики ДО close()» живёт в ядре и покрыт тестом там
// (tmux-web/tests/frontend/messenger_ws_core.test.mjs). Дублировать его здесь
// нечем и незачем: disconnectVkWs делегирует.

import { withWsToken } from '../core/auth.js';
import { createWsClient } from '../messenger/ws-core.js';
import { vkState } from './state.js';

/** Лестница реконнекта, мс. Последнее значение повторяется. */
export const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/**
 * Ожидаемый период серверного ping'а, мс — HEARTBEAT_INTERVAL в
 * plugins/vk/src/ws/mod.rs. Используется только как эталон для оценки
 * задержки: расхождение с сервером ухудшит точность LAT, но ничего не сломает.
 */
export const PING_INTERVAL_MS = 15000;

/**
 * Полный набор кадров сервера — для сверки с protocol.rs глазами и для
 * диагностики: неизвестный тип не ошибка (сервер может начать присылать новое
 * событие раньше, чем обновится закешированный фронт), но в консоли о нём
 * стоит сказать.
 *
 * ПОРЯДОК ЗДЕСЬ — ЧАСТЬ КОНТРАКТА: список сверяется с вариантами `VkServerMsg`
 * (plugins/vk/src/ws/protocol.rs) построчно контрактным тестом. Четвёрка
 * `analysis_*` стоит РОВНО между `dialog_changed` и `resync`, как в enum'е.
 */
export const SERVER_FRAMES = [
    'login_state', 'new_message', 'message_edited', 'message_deleted',
    'typing', 'online', 'read', 'upload_progress', 'download_progress',
    'notify_settings', 'unread_total', 'history_cleared', 'dialog_changed',
    'analysis_started', 'analysis_delta', 'analysis_done', 'analysis_error',
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
    return withWsToken(`${proto}://${location.host}/ws/vk`);
}

/** Зовёт обработчик панели, не давая его исключению порвать соединение. */
function callHandler(name, arg) {
    const h = local.handlers && local.handlers[name];
    if (typeof h !== 'function') return;
    try {
        h(arg);
    } catch (e) {
        console.warn(`[vk-ws] handler ${name} threw`, e);
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
        // Клиент отстал (broadcast Lagged) — состояние перечитывается по REST
        // целиком, поэтому у resync отдельный колбэк, а не общий обработчик.
        callHandler('resync', msg);
        return;
    }
    if (typeof (local.handlers && local.handlers[msg.type]) === 'function') {
        callHandler(msg.type, msg);
        return;
    }
    console.debug('[vk-ws] no handler for', msg.type, msg);
}

function ensureClient() {
    if (local.client) return local.client;
    local.client = createWsClient({
        urlFor,
        backoffs: WS_BACKOFFS_MS,
        pingIntervalMs: PING_INTERVAL_MS,
        label: 'vk-ws',
        onFrame,
        onLatency: (lat) => {
            vkState.lat = lat;
            callHandler('lat', lat);
        },
        onOpen: () => {
            vkState.ws = local.client ? local.client.socket() : null;
            callHandler('open');
        },
        onClose: (info) => {
            vkState.ws = null;
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
 * applyLoginState после успешного логина, возврат вкладки из фона), и порядок
 * между ними не определён.
 *
 * @param {object} handlers — { [frameType]: fn(msg), open?, close?, resync?, lat?, ping? }
 */
export function connectVkWs(handlers) {
    local.handlers = handlers || local.handlers || {};
    const client = ensureClient();
    client.connect();
    vkState.ws = client.socket();
}

/**
 * Закрыть соединение без реконнекта.
 *
 * Вызывается на visibilitychange:hidden (скрытую вкладку держать сокетом
 * незачем — бейджа всё равно не видно) и на beforeunload.
 */
export function disconnectVkWs() {
    if (local.client) local.client.disconnect();
    vkState.ws = null;
    vkState.lat = null;
}

/** Открыто ли соединение прямо сейчас. */
export function isVkWsOpen() {
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
    return { ...base, lat: vkState.lat, handlers: Object.keys(local.handlers || {}).length };
}
