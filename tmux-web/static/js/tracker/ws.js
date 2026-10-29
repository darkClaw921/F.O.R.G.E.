// tmux-web — плагин forge-tracker: WebSocket-клиент `/ws/tracker`.
//
// Образец — js/ws/todos-ws.js: та же лестница реконнекта и тот же
// polling-фолбэк. Общее ядро js/messenger/ws-core.js здесь НЕ используется
// намеренно: оно построено вокруг JSON-кадра `ping` и оценки задержки, а
// трекер шлёт heartbeat протокольным `Ping` фрейма WebSocket, на который
// браузер отвечает сам. Тащить ради этого чужой контракт значило бы завести
// в трекере мёртвый код и второй смысл у поля `kind`.
//
// КОНТРАКТ КАДРОВ — plugins/tracker/src/ws.rs:
//
//   {"kind":"snapshot", …StateDto…}   при подключении и после Lagged
//   {"kind":"task_upsert","task":{…}}
//   {"kind":"task_removed","id":"…"}
//   {"kind":"entry_upsert","entry":{…}}
//   {"kind":"entry_removed","id":"…"}
//   {"kind":"reload"}                 изменилось то, для чего точечного кадра
//                                     нет (список, пункт, веха, перестановка)
//
// ПОЧЕМУ СНИМОК ПРИХОДИТ ПЕРВЫМ КАДРОМ, А НЕ ЗАПРАШИВАЕТСЯ ОТДЕЛЬНО. Между
// ответом `GET /api/tracker/state` и открытием сокета есть окно, в котором
// события теряются молча. Сервер закрывает его сам: снимок и поток идут одним
// каналом, поэтому и после разрыва панель не дублирует и не теряет строк —
// новый снимок ЗАМЕЩАЕТ состояние целиком, а не доливается в него.
//
// ПОЧЕМУ СОКЕТ ЗАКРЫВАЕТСЯ ПРИ УХОДЕ С ВКЛАДКИ (в отличие от мессенджеров).
// У Telegram/VK/Битрикса соединение постоянное, потому что бейдж
// непрочитанного обязан работать с любой вкладки. Трекеру считать нечего:
// задачи не приходят извне, их заводит сам пользователь. Тикающие часы
// идущих таймеров (фаза 5) считаются от `started_at` и не нуждаются в потоке
// событий. Поэтому здесь `disconnectTrackerWs()` при уходе — и ни одного
// висящего таймера реконнекта после него (это отдельный критерий приёмки).

import { withWsToken } from '../core/auth.js';
import * as api from './api.js';

/** Лестница реконнекта, мс. Последнее значение повторяется. */
export const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/** Период polling-фолбэка, пока сокет не поднялся, мс. */
export const POLL_INTERVAL_MS = 15000;

/** Все кадры сервера — для сверки с ws.rs глазами и для диагностики. */
export const SERVER_FRAMES = [
    'snapshot', 'task_upsert', 'task_removed', 'entry_upsert', 'entry_removed', 'reload',
];

const local = {
    ws: null,
    handlers: {},
    backoffStep: 0,
    reconnectTimer: null,
    pollTimer: null,
    closedByUs: false,
    /** Счётчик открытий — для e2e и диагностики. */
    opened: 0,
};

function url() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return withWsToken(`${proto}://${location.host}/ws/tracker`);
}

/** Зовёт обработчик панели, не давая его исключению порвать соединение. */
function callHandler(name, arg) {
    const h = local.handlers && local.handlers[name];
    if (typeof h !== 'function') return;
    try {
        h(arg);
    } catch (e) {
        console.warn(`[tracker-ws] handler ${name} threw`, e);
    }
}

/**
 * Разбор кадра — ЧИСТАЯ функция, покрывается тестом без сокета.
 *
 * @param {string} raw — тело кадра
 * @returns {{name: string, arg: any}|null} имя обработчика и его аргумент;
 *          `null` — кадр не наш (не JSON, не объект, неизвестный `kind`).
 */
export function frameToHandler(raw) {
    let msg;
    try {
        msg = JSON.parse(raw);
    } catch (_) {
        return null;
    }
    if (!msg || typeof msg !== 'object') return null;
    switch (msg.kind) {
        case 'snapshot': return { name: 'snapshot', arg: msg };
        case 'task_upsert':
            // Кадр без задачи — расхождение фронта и сервера. Точечное
            // применение такого кадра невозможно, но и терять его нельзя:
            // отдаём `reload`, панель перечитает состояние целиком.
            return msg.task ? { name: 'taskUpsert', arg: msg.task } : { name: 'reload', arg: msg };
        case 'task_removed':
            return msg.id ? { name: 'taskRemoved', arg: msg.id } : { name: 'reload', arg: msg };
        case 'entry_upsert':
            return msg.entry ? { name: 'entryUpsert', arg: msg.entry } : { name: 'reload', arg: msg };
        case 'entry_removed':
            return msg.id ? { name: 'entryRemoved', arg: msg.id } : { name: 'reload', arg: msg };
        case 'reload': return { name: 'reload', arg: msg };
        default: return null;
    }
}

function onMessage(raw) {
    const route = frameToHandler(raw);
    if (!route) {
        // Не молча: неизвестный кадр — расхождение с сервером, и молчание
        // здесь дало бы «панель иногда не обновляется» без единого следа.
        console.debug('[tracker-ws] unknown frame', raw);
        return;
    }
    callHandler(route.name, route.arg);
}

// -------------------------------------------------------------- polling-фолбэк
//
// Пока сокета нет, панель обязана оставаться живой: работает REST, а снимок
// подтягивается опросом. Опрос ВСЕГДА выключается при открытии сокета —
// иначе два источника снимка перезатирали бы друг друга.

function startPolling() {
    if (local.pollTimer) return;
    local.pollTimer = setInterval(() => {
        api.getState()
            .then((snap) => { if (snap) callHandler('snapshot', snap); })
            .catch(() => { /* сервер лежит — следующая попытка через интервал */ });
    }, POLL_INTERVAL_MS);
}

function stopPolling() {
    if (local.pollTimer) {
        clearInterval(local.pollTimer);
        local.pollTimer = null;
    }
}

function scheduleReconnect() {
    if (local.closedByUs) return;
    if (local.reconnectTimer) return;
    const idx = Math.min(local.backoffStep, WS_BACKOFFS_MS.length - 1);
    const delay = WS_BACKOFFS_MS[idx];
    local.backoffStep = Math.min(local.backoffStep + 1, WS_BACKOFFS_MS.length - 1);
    local.reconnectTimer = setTimeout(() => {
        local.reconnectTimer = null;
        connectTrackerWs();
    }, delay);
}

/**
 * Открыть (или переиспользовать) соединение.
 *
 * Идемпотентен: при живом сокете только обновляет handlers. Это важно, потому
 * что connect зовут из нескольких точек (открытие вкладки, возврат страницы
 * из фона), и порядок между ними не определён.
 *
 * @param {object} [handlers] — { snapshot, taskUpsert, taskRemoved,
 *        entryUpsert, entryRemoved, reload, open, close }
 */
export function connectTrackerWs(handlers) {
    if (handlers) local.handlers = handlers;
    if (local.ws && (
        local.ws.readyState === WebSocket.OPEN
        || local.ws.readyState === WebSocket.CONNECTING
    )) {
        return;
    }
    local.closedByUs = false;
    if (local.reconnectTimer) {
        clearTimeout(local.reconnectTimer);
        local.reconnectTimer = null;
    }

    let ws;
    try {
        ws = new WebSocket(url());
    } catch (e) {
        console.warn('[tracker-ws] constructor failed', e);
        startPolling();
        scheduleReconnect();
        return;
    }
    local.ws = ws;

    ws.onopen = () => {
        local.backoffStep = 0;
        local.opened += 1;
        // Снимок придёт первым кадром — опрос больше не нужен.
        stopPolling();
        callHandler('open');
    };
    ws.onmessage = (ev) => { onMessage(ev.data); };
    ws.onerror = (ev) => { console.debug('[tracker-ws] error', ev); };
    ws.onclose = () => {
        // Сокет мог смениться (быстрый реконнект) — гасим только свой.
        if (local.ws === ws) local.ws = null;
        callHandler('close');
        if (local.closedByUs) return;
        startPolling();
        scheduleReconnect();
    };
}

/**
 * Закрыть соединение без реконнекта и без опроса.
 *
 * После вызова не остаётся НИ ОДНОГО таймера — ни реконнекта, ни polling'а.
 * Это критерий приёмки, а не аккуратность: уход с вкладки не должен оставлять
 * фоновой активности.
 */
export function disconnectTrackerWs() {
    local.closedByUs = true;
    if (local.reconnectTimer) {
        clearTimeout(local.reconnectTimer);
        local.reconnectTimer = null;
    }
    stopPolling();
    if (local.ws) {
        try { local.ws.close(); } catch (_) { /* уже мёртв */ }
        local.ws = null;
    }
    local.backoffStep = 0;
}

/** Открыто ли соединение прямо сейчас. */
export function isTrackerWsOpen() {
    return !!local.ws && local.ws.readyState === WebSocket.OPEN;
}

/** Снимок состояния транспорта для e2e и диагностики. */
export function _debugWsState() {
    return {
        readyState: local.ws ? local.ws.readyState : null,
        backoffStep: local.backoffStep,
        closedByUs: local.closedByUs,
        reconnectPending: !!local.reconnectTimer,
        polling: !!local.pollTimer,
        opened: local.opened,
    };
}
