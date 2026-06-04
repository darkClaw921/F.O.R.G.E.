// tmux-web — Echo plugin WebSocket client (Phase 5c)
//
// Подключение к /ws/echo?conversation_id=...&token=...
//
// Соблюдает контракт wire-protocol из plugins/echo/src/ws/protocol.rs:
//   - Server → Client: assistant_chunk, assistant_done, action_buttons,
//                       notification, stats_update, autonomous_task_event,
//                       error, ping.
//   - Client → Server: user_message, cancel, action_invoke, pong.
//
// При получении server-side `ping` сразу отвечаем `pong` — без этого сервер
// закроет соединение по idle-timeout (60с).
//
// Reconnect: backoff [1s, 2s, 5s, 10s]. Состояние держится в module-scoped
// объекте — connectEchoWs идемпотентен (повторный вызов с тем же
// conversation_id — no-op, если уже подключены).

import { withWsToken } from '../core/auth.js';
import { fetchNextSteps } from '../sessions/sessions.js';
import { renderSidebar } from '../sidebar/sidebar.js';

const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

const state = {
    ws: null,
    conversationId: null,
    handlers: null,
    closedByUs: false,
    backoffStep: 0,
    reconnectTimer: null,
};

/**
 * Подключиться к /ws/echo для указанной conversation.
 *
 * @param {string} conversationId
 * @param {object} handlers — словарь { msg_type: fn(msg) }. Незаданные типы
 *   просто игнорируются (после warn в консоли).
 */
export function connectEchoWs(conversationId, handlers) {
    if (!conversationId) {
        console.warn('[echo-ws] connectEchoWs called without conversation_id');
        return;
    }
    // Уже подключены к этому же conversation — no-op.
    if (state.ws && state.conversationId === conversationId
        && (state.ws.readyState === WebSocket.OPEN || state.ws.readyState === WebSocket.CONNECTING)) {
        // Обновим handlers — на случай, если caller переподписался с другими.
        state.handlers = handlers || state.handlers;
        return;
    }
    // Другая conversation — закрыть текущее соединение. Отвязываем onclose
    // у старого ws ДО close(), иначе его отложенный onclose-callback обнулит
    // state.ws (который уже указывает на новый WebSocket) и запланирует
    // лишний reconnect — это была причина WS reconnect-loop'а.
    if (state.ws) {
        try { state.ws.onclose = null; state.ws.onerror = null; state.ws.onmessage = null; } catch (_) {}
        try { state.ws.close(); } catch (_) {}
        state.ws = null;
    }

    state.conversationId = conversationId;
    state.handlers = handlers || {};
    state.closedByUs = false;

    if (state.reconnectTimer) {
        clearTimeout(state.reconnectTimer);
        state.reconnectTimer = null;
    }

    open();
}

/**
 * Закрыть соединение. После disconnectEchoWs автоматическая реконнектация
 * не происходит — нужно явно вызвать connectEchoWs.
 */
export function disconnectEchoWs() {
    state.closedByUs = true;
    if (state.reconnectTimer) {
        clearTimeout(state.reconnectTimer);
        state.reconnectTimer = null;
    }
    if (state.ws) {
        try { state.ws.close(); } catch (_) {}
        state.ws = null;
    }
    state.conversationId = null;
    state.handlers = null;
    state.backoffStep = 0;
}

/**
 * Отправить ClientMsg. Возвращает true, если фрейм был отправлен,
 * false — если соединение не открыто.
 */
export function sendClientMsg(msg) {
    if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
        console.warn('[echo-ws] sendClientMsg called with closed ws', msg);
        return false;
    }
    try {
        state.ws.send(JSON.stringify(msg));
        return true;
    } catch (e) {
        console.warn('[echo-ws] send failed', e);
        return false;
    }
}

// -------- helpers --------

function open() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const cid = encodeURIComponent(state.conversationId);
    const url = withWsToken(`${proto}://${location.host}/ws/echo?conversation_id=${cid}`);
    let ws;
    try {
        ws = new WebSocket(url);
    } catch (e) {
        console.warn('[echo-ws] WebSocket constructor failed', e);
        scheduleReconnect();
        return;
    }
    state.ws = ws;
    // Capture this ws instance — все handlers ниже игнорируют события,
    // если state.ws уже указывает на другой ws (защита от race при
    // быстром переключении conversation'ов).
    const myWs = ws;

    ws.onopen = () => {
        if (state.ws !== myWs) return;
        state.backoffStep = 0;
        if (state.handlers && typeof state.handlers.open === 'function') {
            try { state.handlers.open(); } catch (e) { console.warn('handlers.open threw', e); }
        }
    };
    ws.onmessage = (ev) => {
        if (state.ws !== myWs) return;
        handleFrame(ev.data);
    };
    ws.onerror = (ev) => {
        if (state.ws !== myWs) return;
        console.debug('[echo-ws] error event', ev);
    };
    ws.onclose = () => {
        if (state.ws !== myWs) return;
        state.ws = null;
        if (state.handlers && typeof state.handlers.close === 'function') {
            try { state.handlers.close(); } catch (e) { console.warn('handlers.close threw', e); }
        }
        if (state.closedByUs) return;
        scheduleReconnect();
    };
}

function scheduleReconnect() {
    if (state.closedByUs) return;
    if (state.reconnectTimer) return;
    const idx = Math.min(state.backoffStep, WS_BACKOFFS_MS.length - 1);
    const delay = WS_BACKOFFS_MS[idx];
    state.backoffStep = Math.min(state.backoffStep + 1, WS_BACKOFFS_MS.length - 1);
    state.reconnectTimer = setTimeout(() => {
        state.reconnectTimer = null;
        if (!state.closedByUs && state.conversationId) {
            open();
        }
    }, delay);
}

function handleFrame(raw) {
    let msg;
    try {
        msg = JSON.parse(raw);
    } catch (e) {
        console.warn('[echo-ws] non-JSON frame', raw);
        return;
    }
    if (!msg || typeof msg !== 'object' || !msg.type) return;

    // NextStepEvent — broadcast (не привязан к conversation): изменилось
    // состояние предложения «следующего шага» для сессии. Обрабатываем прямо
    // здесь (а не через conversation-handlers), чтобы голубое свечение
    // появлялось/исчезало почти мгновенно, не дожидаясь 3с-поллинга
    // fetchSessions. has_suggestion=true → новое предложение; false → снято
    // (send/feedback/dismiss или сессия снова активна). В обоих случаях самый
    // надёжный путь — перефетчить актуальный список и перерисовать сайдбар.
    if (msg.type === 'next_step_event') {
        fetchNextSteps().then(() => renderSidebar());
        return;
    }

    // Серверный ping — сразу шлём pong, чтобы не словить idle-timeout.
    if (msg.type === 'ping') {
        try { state.ws.send(JSON.stringify({ type: 'pong' })); } catch (_) {}
        if (state.handlers && typeof state.handlers.ping === 'function') {
            try { state.handlers.ping(msg); } catch (e) { console.warn(e); }
        }
        return;
    }

    const h = state.handlers && state.handlers[msg.type];
    if (typeof h === 'function') {
        try { h(msg); } catch (e) { console.warn(`[echo-ws] handler ${msg.type} threw`, e); }
    } else {
        // Не падаем — просто debug-лог; UI может частично игнорировать события.
        console.debug('[echo-ws] no handler for', msg.type, msg);
    }
}

// -------- convenience senders --------

/**
 * Отправить user_message в текущую открытую conversation.
 *
 * @param {object} payload — { text, conversation_id?, model?, ctx_opts? }
 *   Если conversation_id опущен — подставляется из state (текущий чат).
 */
export function sendUserMessage(payload) {
    const conv = payload.conversation_id || state.conversationId || '';
    return sendClientMsg({
        type: 'user_message',
        text: payload.text,
        conversation_id: conv,
        model: payload.model || null,
        ctx_opts: payload.ctx_opts || null,
    });
}

export function sendCancel(runId) {
    return sendClientMsg({ type: 'cancel', run_id: runId });
}

export function sendActionInvoke(actionId, params) {
    return sendClientMsg({
        type: 'action_invoke',
        action_id: actionId,
        params: params || {},
    });
}

/**
 * Для тестов / диагностики — возвращает текущий conversation_id и состояние.
 */
export function _debugState() {
    return {
        conversationId: state.conversationId,
        readyState: state.ws ? state.ws.readyState : null,
        backoffStep: state.backoffStep,
    };
}
