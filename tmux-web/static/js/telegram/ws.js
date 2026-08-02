// tmux-web — Telegram plugin: WebSocket-клиент `/ws/telegram`.
//
// Порт js/echo/ws.js с тремя отличиями, каждое из которых вынужденное:
//
// 1. Соединение ПОСТОЯННОЕ и не привязано к вкладке. Сервер вещает события
//    без фильтрации по диалогу (plugins/telegram/src/state.rs), потому что
//    unread-бейдж на кнопке ✈ и тосты обязаны работать, пока пользователь
//    сидит в terminal/tasks. Поэтому здесь нет ни conversation_id, ни
//    teardown'а при смене вкладки — только visibilitychange и beforeunload.
// 2. Замер задержки по дрейфу серверных ping'ов (см. latencyFromPingDrift):
//    сервер не отвечает эхом на pong, поэтому классический RTT недоступен.
// 3. `resync` — отдельный колбэк: клиент отстал (broadcast Lagged), и
//    состояние надо перечитать по REST целиком.
//
// Контракт кадров — plugins/telegram/src/ws/protocol.rs:
//   server → client: login_state, new_message, message_edited,
//     message_deleted, typing, online, read, upload_progress,
//     download_progress, resync, ping;
//   client → server: pong (единственный).

import { withWsToken } from '../core/auth.js';
import { latencyFromPingDrift } from './format.js';
import { tgState } from './state.js';

/** Лестница реконнекта, мс. Последнее значение повторяется. */
export const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/**
 * Период серверного ping'а, мс — HEARTBEAT_SECS в plugins/telegram/src/ws/mod.rs.
 * Используется только как эталон для оценки задержки; расхождение с сервером
 * ухудшит точность LAT, но ничего не сломает.
 */
export const PING_INTERVAL_MS = 15000;

const local = {
    /** Активный сокет или null. */
    ws: null,
    /** Карта type → обработчик (из buildHandlers в main.js). */
    handlers: null,
    /** Закрыли сами (disconnect) — реконнект не нужен. */
    closedByUs: false,
    /** Текущая ступень backoff'а. */
    backoffStep: 0,
    /** Таймер отложенного реконнекта. */
    reconnectTimer: null,
    /** Когда пришёл прошлый ping (мс, 0 ⇒ ещё не приходил). */
    lastPingAt: 0,
};

/**
 * Открыть (или переиспользовать) соединение.
 *
 * Идемпотентен: при живом сокете только обновляет handlers — второго сокета
 * не появится. Это важно, потому что connect зовут из нескольких точек
 * (гейт при загрузке страницы, applyLoginState после входа, возврат вкладки
 * из фона), и каждая из них может сработать в любом порядке.
 *
 * @param {object} handlers — { [msgType]: fn(msg), open?, close?, resync?, lat? }
 */
export function connectTelegramWs(handlers) {
    local.handlers = handlers || local.handlers || {};
    local.closedByUs = false;

    if (local.ws
        && (local.ws.readyState === WebSocket.OPEN || local.ws.readyState === WebSocket.CONNECTING)) {
        return;
    }
    if (local.reconnectTimer) {
        clearTimeout(local.reconnectTimer);
        local.reconnectTimer = null;
    }
    open();
}

/**
 * Закрыть соединение без реконнекта.
 *
 * Вызывается на visibilitychange:hidden (скрытую вкладку держать сокетом
 * незачем — бейджа всё равно не видно) и на beforeunload.
 */
export function disconnectTelegramWs() {
    local.closedByUs = true;
    if (local.reconnectTimer) {
        clearTimeout(local.reconnectTimer);
        local.reconnectTimer = null;
    }
    if (local.ws) {
        // Обработчики снимаются ДО close(): иначе отложенный onclose уже
        // закрытого сокета обнулил бы состояние нового (это была причина
        // reconnect-loop'а в echo/ws.js).
        try {
            local.ws.onclose = null;
            local.ws.onerror = null;
            local.ws.onmessage = null;
            local.ws.onopen = null;
        } catch (_) { /* сокет уже мёртв */ }
        try { local.ws.close(); } catch (_) { /* уже закрыт */ }
    }
    local.ws = null;
    tgState.ws = null;
    local.backoffStep = 0;
    local.lastPingAt = 0;
}

/** Открыто ли соединение прямо сейчас. */
export function isTelegramWsOpen() {
    return !!local.ws && local.ws.readyState === WebSocket.OPEN;
}

/** Снимок состояния для e2e/диагностики. */
export function _debugWsState() {
    return {
        readyState: local.ws ? local.ws.readyState : null,
        backoffStep: local.backoffStep,
        closedByUs: local.closedByUs,
        reconnectPending: !!local.reconnectTimer,
        lat: tgState.lat,
    };
}

// -------- внутреннее --------

function open() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    // Токен подставляется в query: WebSocket-конструктор не умеет заголовки,
    // а remote-mode требует авторизации (withWsToken — тот же путь, что у Echo).
    const url = withWsToken(`${proto}://${location.host}/ws/telegram`);
    let ws;
    try {
        ws = new WebSocket(url);
    } catch (e) {
        console.warn('[tg-ws] WebSocket constructor failed', e);
        scheduleReconnect();
        return;
    }
    local.ws = ws;
    tgState.ws = ws;
    // Захватываем экземпляр: все колбэки ниже игнорируют события, если
    // local.ws уже указывает на другой сокет (защита от гонки реконнекта).
    const myWs = ws;

    ws.onopen = () => {
        if (local.ws !== myWs) return;
        local.backoffStep = 0;
        local.lastPingAt = 0;
        callHandler('open');
    };
    ws.onmessage = (ev) => {
        if (local.ws !== myWs) return;
        handleFrame(ev.data);
    };
    ws.onerror = (ev) => {
        if (local.ws !== myWs) return;
        console.debug('[tg-ws] error event', ev);
    };
    ws.onclose = () => {
        if (local.ws !== myWs) return;
        local.ws = null;
        tgState.ws = null;
        local.lastPingAt = 0;
        // Колбэк получает подробности, потому что «связь оборвалась» и «связь
        // не возвращается уже минуту» — разные сообщения пользователю
        // (RECONNECTING против OFFLINE). Считать это в main.js по своим
        // таймерам значило бы завести второй источник правды о backoff'е.
        callHandler('close', {
            willReconnect: !local.closedByUs,
            attempt: local.backoffStep,
            exhausted: local.backoffStep >= WS_BACKOFFS_MS.length - 1,
        });
        if (local.closedByUs) return;
        scheduleReconnect();
    };
}

function scheduleReconnect() {
    if (local.closedByUs) return;
    if (local.reconnectTimer) return;
    const idx = Math.min(local.backoffStep, WS_BACKOFFS_MS.length - 1);
    const delay = WS_BACKOFFS_MS[idx];
    local.backoffStep = Math.min(local.backoffStep + 1, WS_BACKOFFS_MS.length - 1);
    local.reconnectTimer = setTimeout(() => {
        local.reconnectTimer = null;
        if (!local.closedByUs) open();
    }, delay);
}

function callHandler(name, arg) {
    const h = local.handlers && local.handlers[name];
    if (typeof h !== 'function') return;
    try {
        h(arg);
    } catch (e) {
        console.warn(`[tg-ws] handler ${name} threw`, e);
    }
}

function handleFrame(raw) {
    let msg;
    try {
        msg = JSON.parse(raw);
    } catch (_) {
        console.warn('[tg-ws] non-JSON frame', raw);
        return;
    }
    if (!msg || typeof msg !== 'object' || !msg.type) return;

    if (msg.type === 'ping') {
        // Pong СРАЗУ: сервер закрывает соединение по idle 60с, а ping'и идут
        // раз в 15с — молчание в три такта означает разрыв.
        try { local.ws.send(JSON.stringify({ type: 'pong' })); } catch (_) { /* закрылся */ }
        const now = Date.now();
        const lat = latencyFromPingDrift(local.lastPingAt, now, PING_INTERVAL_MS);
        local.lastPingAt = now;
        if (lat !== null) {
            tgState.lat = lat;
            callHandler('lat', lat);
        }
        callHandler('ping', msg);
        return;
    }

    if (msg.type === 'resync') {
        callHandler('resync', msg);
        return;
    }

    const h = local.handlers && local.handlers[msg.type];
    if (typeof h === 'function') {
        try {
            h(msg);
        } catch (e) {
            console.warn(`[tg-ws] handler ${msg.type} threw`, e);
        }
    } else {
        // Неизвестный тип — не ошибка: сервер может начать присылать новые
        // события раньше, чем обновится закешированный фронт.
        console.debug('[tg-ws] no handler for', msg.type, msg);
    }
}
