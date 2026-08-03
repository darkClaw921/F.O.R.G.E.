// tmux-web — общий слой мессенджеров: ядро WebSocket-клиента.
//
// Вынесено из js/telegram/ws.js. Сам telegram/ws.js НЕ РЕДАКТИРУЕТСЯ (см.
// шапку http-envelope.js) — общий слой импортирует только панель VK.
//
// Что делает ядро:
//   - держит ОДНО постоянное соединение и переоткрывает его по лестнице
//     backoff'ов;
//   - отвечает `pong` на серверный `ping` и попутно оценивает задержку по
//     дрейфу интервала между ping'ами;
//   - отдаёт кадры наружу одним колбэком onFrame, не зная ни одного типа
//     кадра, кроме служебного `ping`.
//
// ============================================================================
// ИНВАРИАНТ, РАДИ КОТОРОГО ЯДРО И ВЫДЕЛЕНО
// ============================================================================
//
//   disconnect() снимает onclose/onerror/onmessage/onopen ДО close().
//
// Это не стилистика, а конкретный исправленный дефект js/echo/ws.js: без
// снятия обработчиков `close()` асинхронно вызывает onclose УЖЕ ЗАКРЫТОГО
// сокета, тот планирует реконнект, реконнект открывает новый сокет — и так по
// кругу. Симптом со стороны пользователя: вкладка съедает батарею и сервер
// видит поток апгрейдов. Порядок покрыт тестом
// tmux-web/tests/frontend/messenger_ws_core.test.mjs (фейковый WebSocket
// пишет журнал вызовов); мутация «перенести close() перед снятием» обязана
// красить этот тест.
//
// ============================================================================
//
// Модуль не импортирует ни core/auth.js (у него побочки на верхнем уровне:
// localStorage и подмена window.fetch), ни DOM. Адрес соединения приходит
// функцией `urlFor` — её и подставляет панель, добавляя токен через
// withWsToken. Благодаря этому ядро грузится в Node без стенда.

/** Лестница реконнекта по умолчанию, мс. Последнее значение повторяется. */
export const DEFAULT_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/** Период серверного ping'а по умолчанию, мс (HEARTBEAT_INTERVAL плагинов). */
export const DEFAULT_PING_INTERVAL_MS = 15000;

/**
 * Оценка задержки по дрейфу серверных ping'ов.
 *
 * Сервер не отвечает эхом на `pong`, поэтому классический RTT недоступен.
 * Зато ping'и идут с известным периодом, и отклонение фактического интервала
 * от него — это и есть накопленная задержка доставки.
 *
 * Первый ping (prevAt === 0) опорной точки не имеет ⇒ null. Отрицательный
 * дрейф (сервер прислал раньше) зажимается в 0: «связь быстрее, чем сама
 * себя» — это не измерение, а шум планировщика.
 *
 * @param {number} prevAt — время прошлого ping'а, мс (0 ⇒ не было)
 * @param {number} nowAt — время текущего ping'а, мс
 * @param {number} expectedMs — ожидаемый период
 * @returns {?number} миллисекунды или null
 */
export function latencyFromPingDrift(prevAt, nowAt, expectedMs) {
    if (!prevAt || !Number.isFinite(prevAt) || !Number.isFinite(nowAt)) return null;
    const drift = (nowAt - prevAt) - expectedMs;
    if (!Number.isFinite(drift)) return null;
    return Math.max(0, Math.round(drift));
}

/**
 * Создаёт WS-клиента.
 *
 * @param {object} opts
 * @param {() => string} opts.urlFor — адрес соединения (с токеном, если нужен)
 * @param {Array<number>} [opts.backoffs] — лестница реконнекта, мс
 * @param {number} [opts.pingIntervalMs] — ожидаемый период серверного ping'а
 * @param {(msg: object) => void} [opts.onFrame] — любой кадр, кроме `ping`
 * @param {(latMs: number) => void} [opts.onLatency] — новая оценка задержки
 * @param {(info: {willReconnect: boolean, attempt: number, exhausted: boolean}) => void} [opts.onClose]
 * @param {() => void} [opts.onOpen]
 * @param {string} [opts.label] — префикс сообщений в консоли
 * @param {Function} [opts.socketFactory] — конструктор сокета (для тестов)
 * @returns {{connect: Function, disconnect: Function, isOpen: Function, _debug: Function}}
 */
export function createWsClient(opts) {
    const o = opts || {};
    const urlFor = typeof o.urlFor === 'function' ? o.urlFor : null;
    if (!urlFor) throw new TypeError('createWsClient: urlFor must be a function');

    const backoffs = (Array.isArray(o.backoffs) && o.backoffs.length)
        ? o.backoffs.slice()
        : DEFAULT_BACKOFFS_MS.slice();
    const pingIntervalMs = Number.isFinite(o.pingIntervalMs) && o.pingIntervalMs > 0
        ? o.pingIntervalMs
        : DEFAULT_PING_INTERVAL_MS;
    const label = o.label || 'ws';

    const local = {
        /** Активный сокет или null. */
        ws: null,
        /** Закрыли сами — реконнект не нужен. */
        closedByUs: false,
        /** Текущая ступень backoff'а. */
        backoffStep: 0,
        /** Таймер отложенного реконнекта. */
        reconnectTimer: null,
        /** Когда пришёл прошлый ping (мс, 0 ⇒ ещё не приходил). */
        lastPingAt: 0,
        /** Сколько сокетов было открыто — счётчик для теста «не плодим». */
        opened: 0,
    };

    /** Конструктор сокета: подменяемый ради тестов без браузера. */
    function makeSocket(url) {
        const Factory = typeof o.socketFactory === 'function'
            ? o.socketFactory
            : (typeof WebSocket !== 'undefined' ? WebSocket : null);
        if (!Factory) throw new TypeError('WebSocket is not available');
        return new Factory(url);
    }

    function safe(fn, arg) {
        if (typeof fn !== 'function') return;
        try {
            fn(arg);
        } catch (e) {
            console.warn(`[${label}] handler threw`, e);
        }
    }

    function open() {
        let ws;
        try {
            ws = makeSocket(urlFor());
        } catch (e) {
            console.warn(`[${label}] WebSocket constructor failed`, e);
            scheduleReconnect();
            return;
        }
        local.ws = ws;
        local.opened += 1;
        // Захватываем экземпляр: все колбэки ниже игнорируют события, если
        // local.ws уже указывает на другой сокет (защита от гонки реконнекта).
        const myWs = ws;

        ws.onopen = () => {
            if (local.ws !== myWs) return;
            local.backoffStep = 0;
            local.lastPingAt = 0;
            safe(o.onOpen);
        };
        ws.onmessage = (ev) => {
            if (local.ws !== myWs) return;
            handleFrame(ev && ev.data, myWs);
        };
        ws.onerror = (ev) => {
            if (local.ws !== myWs) return;
            console.debug(`[${label}] error event`, ev);
        };
        ws.onclose = () => {
            if (local.ws !== myWs) return;
            local.ws = null;
            local.lastPingAt = 0;
            // Колбэк получает подробности, потому что «связь оборвалась» и
            // «связь не возвращается уже минуту» — разные сообщения
            // пользователю (RECONNECTING против OFFLINE). Считать это в
            // панели по своим таймерам значило бы завести второй источник
            // правды о backoff'е.
            safe(o.onClose, {
                willReconnect: !local.closedByUs,
                attempt: local.backoffStep,
                exhausted: local.backoffStep >= backoffs.length - 1,
            });
            if (local.closedByUs) return;
            scheduleReconnect();
        };
    }

    function scheduleReconnect() {
        if (local.closedByUs) return;
        if (local.reconnectTimer) return;
        const idx = Math.min(local.backoffStep, backoffs.length - 1);
        const delay = backoffs[idx];
        local.backoffStep = Math.min(local.backoffStep + 1, backoffs.length - 1);
        local.reconnectTimer = setTimeout(() => {
            local.reconnectTimer = null;
            if (!local.closedByUs) open();
        }, delay);
    }

    function handleFrame(raw, myWs) {
        let msg;
        try {
            msg = JSON.parse(raw);
        } catch (_) {
            console.warn(`[${label}] non-JSON frame`, raw);
            return;
        }
        if (!msg || typeof msg !== 'object' || !msg.type) return;

        if (msg.type === 'ping') {
            // Pong СРАЗУ: сервер закрывает соединение по idle 60с, а ping'и
            // идут раз в 15с — молчание в три такта означает разрыв.
            try {
                myWs.send(JSON.stringify({ type: 'pong' }));
            } catch (_) { /* закрылся между кадром и ответом */ }
            const now = Date.now();
            const lat = latencyFromPingDrift(local.lastPingAt, now, pingIntervalMs);
            local.lastPingAt = now;
            if (lat !== null) safe(o.onLatency, lat);
            safe(o.onFrame, msg);
            return;
        }
        safe(o.onFrame, msg);
    }

    return {
        /**
         * Открыть (или переиспользовать) соединение.
         *
         * Идемпотентен: при живом сокете не делает ничего. Это важно, потому
         * что connect зовут из нескольких точек (гейт при загрузке страницы,
         * успешный логин, возврат вкладки из фона), и порядок между ними не
         * определён.
         */
        connect() {
            local.closedByUs = false;
            const alive = local.ws
                && (local.ws.readyState === 1 /* OPEN */ || local.ws.readyState === 0 /* CONNECTING */);
            if (alive) return;
            if (local.reconnectTimer) {
                clearTimeout(local.reconnectTimer);
                local.reconnectTimer = null;
            }
            open();
        },

        /**
         * Закрыть соединение без реконнекта.
         *
         * ПОРЯДОК ДЕЙСТВИЙ ЗДЕСЬ — ИНВАРИАНТ (см. шапку модуля): сначала
         * снимаются все четыре обработчика, только потом close().
         */
        disconnect() {
            local.closedByUs = true;
            if (local.reconnectTimer) {
                clearTimeout(local.reconnectTimer);
                local.reconnectTimer = null;
            }
            const ws = local.ws;
            if (ws) {
                try {
                    ws.onclose = null;
                    ws.onerror = null;
                    ws.onmessage = null;
                    ws.onopen = null;
                } catch (_) { /* сокет уже мёртв */ }
                try { ws.close(); } catch (_) { /* уже закрыт */ }
            }
            local.ws = null;
            local.backoffStep = 0;
            local.lastPingAt = 0;
        },

        /** Открыто ли соединение прямо сейчас. */
        isOpen() {
            return !!local.ws && local.ws.readyState === 1;
        },

        /** Активный сокет (панель кладёт его в своё состояние для индикации). */
        socket() {
            return local.ws;
        },

        /** Снимок состояния для e2e/диагностики. */
        _debug() {
            return {
                readyState: local.ws ? local.ws.readyState : null,
                backoffStep: local.backoffStep,
                closedByUs: local.closedByUs,
                reconnectPending: !!local.reconnectTimer,
                opened: local.opened,
                lastPingAt: local.lastPingAt,
            };
        },
    };
}
