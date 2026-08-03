// Регресс-тест ядра WebSocket-клиента общего слоя мессенджеров.
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/messenger/ws-core.js через
// `await import()`: он намеренно не импортирует ни DOM, ни core/auth.js
// (у того побочки на верхнем уровне), поэтому работает в Node как есть.
// Адрес соединения и конструктор сокета инжектируются параметрами.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Главный инвариант ядра —
//
//     disconnect() снимает onclose/onerror/onmessage/onopen ДО close()
//
// — это конкретный исправленный дефект js/echo/ws.js, а не стилистика. Без
// снятия обработчиков close() асинхронно дёргает onclose уже закрытого
// сокета, тот планирует реконнект, реконнект открывает новый сокет — и так по
// кругу, пока вкладка открыта. Фейковый WebSocket ниже ведёт ЖУРНАЛ вызовов,
// поэтому порядок проверяется буквально.
//
// Мутационная проверка: перенесите `ws.close()` в disconnect() ВЫШЕ снятия
// обработчиков — блок «порядок снятия» обязан покраснеть.
//
// Запуск: node tmux-web/tests/frontend/messenger_ws_core.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const wsUrl = new URL('../../static/js/messenger/ws-core.js', import.meta.url).href;
const envUrl = new URL('../../static/js/messenger/http-envelope.js', import.meta.url).href;
const W = await import(wsUrl);
const H = await import(envUrl);

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

// ---------------------------------------------------------------------------
// Стенд: фейковый WebSocket с журналом.
// ---------------------------------------------------------------------------

const sockets = [];

class FakeWebSocket {
    constructor(url) {
        this.url = url;
        this.readyState = 0; // CONNECTING
        this.sent = [];
        // Журнал ВСЕХ операций над сокетом: и присваивание обработчиков, и
        // close(). Именно по нему проверяется порядок в disconnect().
        this.log = [];
        this._handlers = {};
        for (const name of ['onopen', 'onmessage', 'onerror', 'onclose']) {
            Object.defineProperty(this, name, {
                get: () => this._handlers[name] || null,
                set: (fn) => {
                    this.log.push(fn === null ? `clear:${name}` : `set:${name}`);
                    this._handlers[name] = fn;
                },
                configurable: true,
            });
        }
        sockets.push(this);
    }

    send(data) {
        if (this.readyState !== 1) throw new Error('socket is not open');
        this.sent.push(data);
    }

    close() {
        this.log.push('close');
        this.readyState = 3; // CLOSED
        // Настоящий сокет вызывает onclose АСИНХРОННО и уже после close():
        // именно поэтому снятые обработчики спасают от реконнект-петли.
        queueMicrotask(() => {
            if (this._handlers.onclose) this._handlers.onclose({});
        });
    }

    // --- помощники стенда ---
    accept() {
        this.readyState = 1;
        if (this._handlers.onopen) this._handlers.onopen({});
    }

    frame(obj) {
        if (this._handlers.onmessage) this._handlers.onmessage({ data: JSON.stringify(obj) });
    }

    raw(data) {
        if (this._handlers.onmessage) this._handlers.onmessage({ data });
    }

    drop() {
        this.readyState = 3;
        if (this._handlers.onclose) this._handlers.onclose({});
    }
}

/** Подменяем таймеры: реконнект-лестницу нельзя ждать по-настоящему. */
const timers = [];
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;
globalThis.setTimeout = (fn, ms) => {
    const id = timers.length + 1;
    timers.push({ id, fn, ms, cancelled: false });
    return id;
};
globalThis.clearTimeout = (id) => {
    const t = timers.find((x) => x.id === id);
    if (t) t.cancelled = true;
};

/** Выполняет последний запланированный таймер. */
function runPendingTimer() {
    for (let i = timers.length - 1; i >= 0; i -= 1) {
        if (!timers[i].cancelled && !timers[i].done) {
            timers[i].done = true;
            timers[i].fn();
            return timers[i].ms;
        }
    }
    return null;
}

function pendingDelays() {
    return timers.filter((t) => !t.cancelled).map((t) => t.ms);
}

function makeClient(extra) {
    return W.createWsClient({
        urlFor: () => 'ws://stub/ws/vk?token=T',
        backoffs: [10, 20, 40],
        pingIntervalMs: 1000,
        socketFactory: FakeWebSocket,
        label: 'test-ws',
        ...(extra || {}),
    });
}

function reset() {
    sockets.length = 0;
    timers.length = 0;
}

const tick = () => new Promise((resolve) => realSetTimeout(resolve, 0));

// ---------------------------------------------------------------------------
// 1. ГЛАВНЫЙ ИНВАРИАНТ: порядок снятия обработчиков.
// ---------------------------------------------------------------------------

{
    reset();
    const client = makeClient();
    client.connect();
    const ws = sockets[0];
    ws.accept();

    client.disconnect();

    const clears = ws.log.filter((e) => e.startsWith('clear:') || e === 'close');
    eq(
        clears,
        ['clear:onclose', 'clear:onerror', 'clear:onmessage', 'clear:onopen', 'close'],
        'disconnect: все четыре обработчика сняты ДО close()',
    );
    ok(
        ws.log.indexOf('close') === ws.log.length - 1,
        'disconnect: close() — последняя операция над сокетом',
    );
}

// Отложенный onclose закрытого сокета НЕ поднимает реконнект.
{
    reset();
    const client = makeClient();
    client.connect();
    sockets[0].accept();
    client.disconnect();
    await tick();

    eq(sockets.length, 1, 'после disconnect новых сокетов не появилось');
    eq(pendingDelays().filter((ms) => ms !== undefined), [], 'реконнект не запланирован');
    eq(client._debug().readyState, null, 'сокет отпущен');
    eq(client._debug().closedByUs, true, 'закрыли сами');
}

// disconnect без соединения и повторный disconnect не бросают.
{
    reset();
    const client = makeClient();
    let threw = false;
    try {
        client.disconnect();
        client.connect();
        sockets[0].accept();
        client.disconnect();
        client.disconnect();
    } catch (_) {
        threw = true;
    }
    ok(!threw, 'disconnect идемпотентен и переживает отсутствие сокета');
}

// ---------------------------------------------------------------------------
// 2. Реконнект: лестница backoff'ов и её сброс.
// ---------------------------------------------------------------------------

{
    reset();
    const closes = [];
    const client = makeClient({ onClose: (info) => closes.push(info) });
    client.connect();
    sockets[0].accept();

    sockets[0].drop();
    eq(closes.length, 1, 'обрыв доехал колбэком');
    eq(closes[0].willReconnect, true, 'обрыв не по нашей воле ⇒ реконнект будет');
    eq(runPendingTimer(), 10, 'первая ступень backoff’а');

    sockets[1].drop();
    eq(runPendingTimer(), 20, 'вторая ступень');
    sockets[2].drop();
    eq(runPendingTimer(), 40, 'третья ступень');
    sockets[3].drop();
    eq(runPendingTimer(), 40, 'последняя ступень повторяется, а не растёт бесконечно');
    ok(closes[closes.length - 1].exhausted === true, 'лестница исчерпана — панель скажет OFFLINE');

    // Успешное открытие сбрасывает ступень: следующий обрыв снова начнёт с 10.
    sockets[4].accept();
    sockets[4].drop();
    eq(runPendingTimer(), 10, 'успешный connect сбрасывает лестницу');
}

// Один реконнект-таймер на все обрывы: два onclose подряд не дают двух сокетов.
{
    reset();
    const client = makeClient();
    client.connect();
    sockets[0].accept();
    sockets[0].drop();
    sockets[0].drop(); // повторный onclose того же сокета
    const scheduled = timers.filter((t) => !t.cancelled).length;
    eq(scheduled, 1, 'второй onclose не планирует второй реконнект');
}

// connect при живом сокете не открывает второго.
{
    reset();
    const client = makeClient();
    client.connect();
    sockets[0].accept();
    client.connect();
    client.connect();
    eq(sockets.length, 1, 'connect идемпотентен при живом сокете');
    eq(client.isOpen(), true, 'isOpen отражает состояние');
}

// connect ПОСЛЕ disconnect открывает ровно один новый сокет.
{
    reset();
    const client = makeClient();
    client.connect();
    sockets[0].accept();
    client.disconnect();
    await tick();
    client.connect();
    await tick();
    eq(sockets.length, 2, 'повторный connect после disconnect даёт ровно один сокет');
    eq(client._debug().closedByUs, false, 'флаг «закрыли сами» снят');
}

// Ошибка конструктора не роняет клиента, а планирует повтор.
{
    reset();
    let first = true;
    const client = W.createWsClient({
        urlFor: () => 'ws://stub/ws/vk',
        backoffs: [7],
        socketFactory: function Boom(url) {
            if (first) {
                first = false;
                throw new Error('constructor failed');
            }
            return new FakeWebSocket(url);
        },
    });
    client.connect();
    eq(sockets.length, 0, 'сокет не создан');
    eq(runPendingTimer(), 7, 'повтор запланирован, исключение не вылетело наружу');
    eq(sockets.length, 1, 'вторая попытка удалась');
}

// ---------------------------------------------------------------------------
// 3. ping/pong и оценка задержки.
// ---------------------------------------------------------------------------

{
    reset();
    const frames = [];
    const lats = [];
    const client = makeClient({
        onFrame: (m) => frames.push(m.type),
        onLatency: (l) => lats.push(l),
    });
    client.connect();
    const ws = sockets[0];
    ws.accept();

    const realNow = Date.now;
    let now = 1_000_000;
    Date.now = () => now;

    ws.frame({ type: 'ping' });
    eq(ws.sent, ['{"type":"pong"}'], 'на ping отвечаем pong немедленно');
    eq(lats, [], 'первый ping опорной точки не имеет — задержку не выдумываем');

    now += 1120; // период 1000 мс + 120 мс дрейфа
    ws.frame({ type: 'ping' });
    eq(lats, [120], 'задержка = дрейф интервала между ping’ами');

    now += 800; // сервер прислал раньше срока
    ws.frame({ type: 'ping' });
    eq(lats, [120, 0], 'отрицательный дрейф зажимается в 0');

    Date.now = realNow;
    eq(ws.sent.length, 3, 'на каждый ping — ровно один pong');
    eq(frames, ['ping', 'ping', 'ping'], 'ping тоже доезжает до панели');
}

// Кадры приложения уходят наружу как есть; мусор не роняет соединение.
{
    reset();
    const frames = [];
    const client = makeClient({ onFrame: (m) => frames.push(m) });
    client.connect();
    const ws = sockets[0];
    ws.accept();

    ws.frame({ type: 'login_state', state: 'authorized' });
    ws.raw('not json at all');
    ws.raw('null');
    ws.frame({ nope: 1 }); // кадр без type
    ws.frame({ type: 'resync' });

    eq(frames.map((f) => f.type), ['login_state', 'resync'], 'битые кадры отброшены, соединение живо');
    eq(frames[0].state, 'authorized', 'полезная нагрузка не теряется');
    eq(client.isOpen(), true, 'мусорный кадр не рвёт соединение');
}

// Исключение в обработчике панели не рвёт соединение и не глотает следующие
// кадры: панель может упасть на одном сообщении, но realtime обязан жить.
{
    reset();
    const seen = [];
    const client = makeClient({
        onFrame: (m) => {
            seen.push(m.type);
            if (m.type === 'boom') throw new Error('handler exploded');
        },
    });
    client.connect();
    const ws = sockets[0];
    ws.accept();
    ws.frame({ type: 'boom' });
    ws.frame({ type: 'new_message' });
    eq(seen, ['boom', 'new_message'], 'бросивший обработчик не отменяет следующие кадры');
    eq(client.isOpen(), true, 'соединение живо');
}

// pong в закрытый сокет не бросает наружу.
{
    reset();
    const client = makeClient();
    client.connect();
    const ws = sockets[0];
    ws.accept();
    ws.readyState = 3; // закрылся между кадром и ответом
    let threw = false;
    try {
        ws.frame({ type: 'ping' });
    } catch (_) {
        threw = true;
    }
    ok(!threw, 'неудачная отправка pong проглатывается');
}

// ---------------------------------------------------------------------------
// 4. latencyFromPingDrift как чистая функция.
// ---------------------------------------------------------------------------

eq(W.latencyFromPingDrift(0, 1000, 1000), null, 'lat: нет опорной точки');
eq(W.latencyFromPingDrift(1000, 2000, 1000), 0, 'lat: точный период');
eq(W.latencyFromPingDrift(1000, 2250, 1000), 250, 'lat: дрейф 250 мс');
eq(W.latencyFromPingDrift(1000, 1500, 1000), 0, 'lat: отрицательный дрейф зажат');
eq(W.latencyFromPingDrift(null, 1000, 1000), null, 'lat: null');
eq(W.latencyFromPingDrift(NaN, 1000, 1000), null, 'lat: NaN');

// urlFor обязателен: без него клиент молча ходил бы в никуда.
{
    let threw = false;
    try { W.createWsClient({}); } catch (_) { threw = true; }
    ok(threw, 'createWsClient без urlFor бросает сразу, а не при первом connect');
}

// ---------------------------------------------------------------------------
// 5. Конверт ошибок (js/messenger/http-envelope.js).
// ---------------------------------------------------------------------------

{
    // 409 captcha_required — форма VK: два дополнительных поля в конверте.
    const err = H.parseErrorEnvelope(409, {
        error: {
            code: 'captcha_required',
            message: 'VK requires a captcha',
            captcha_sid: '239123',
            captcha_img: 'https://api.vk.com/captcha.php?sid=239123',
        },
    });
    eq(err.status, 409, 'envelope: статус');
    eq(err.code, 'captcha_required', 'envelope: код — то, по чему принимаются решения');
    eq(err.retryAfterSecs, null, 'envelope: чужого поля не выдумываем');
    eq(err.message, 'VK requires a captcha', 'envelope: текст для пользователя');
    eq(err.body.error.captcha_sid, '239123', 'envelope: тело доступно целиком');
    ok(err instanceof Error, 'envelope: результат — настоящий Error');
}

{
    const err = H.parseErrorEnvelope(429, {
        error: { code: 'flood_wait', message: 'too many', retry_after_secs: 60 },
    });
    eq(err.code, 'flood_wait', 'envelope: 429');
    eq(err.retryAfterSecs, 60, 'envelope: обратный отсчёт');
}

{
    const err = H.parseErrorEnvelope(400, {
        error: { code: 'invalid_input', message: 'VK rejected the parameters' },
    });
    eq(err.code, 'invalid_input', 'envelope: 400');
    eq(err.retryAfterSecs, null, 'envelope: без retry_after_secs');
}

{
    // Тело не наше: страница 502 от прокси, пустой ответ, чужой JSON.
    const html = H.parseErrorEnvelope(502, '<html>bad gateway</html>');
    eq(html.code, null, 'envelope: не-JSON ⇒ code null');
    eq(html.message, '<html>bad gateway</html>', 'envelope: текст как есть');

    const empty = H.parseErrorEnvelope(500, null);
    eq(empty.code, null, 'envelope: пустое тело');
    eq(empty.message, 'HTTP 500', 'envelope: фолбэк по статусу');

    const alien = H.parseErrorEnvelope(400, { detail: 'not our shape' });
    eq(alien.code, null, 'envelope: чужая форма JSON');
    eq(alien.message, 'HTTP 400', 'envelope: фолбэк по статусу');

    const weird = H.parseErrorEnvelope(400, { error: 'строка вместо объекта' });
    eq(weird.code, null, 'envelope: error не объект');
}

{
    // Успешный путь createCaller + разбор ошибки в нём.
    const calls = [];
    const call = H.createCaller(async (path, init) => {
        calls.push([path, init && init.method]);
        if (path === '/api/vk/status') {
            return {
                ok: true,
                status: 200,
                headers: { get: () => 'application/json' },
                json: async () => ({ enabled: true }),
            };
        }
        return {
            ok: false,
            status: 409,
            headers: { get: () => 'application/json' },
            json: async () => ({ error: { code: 'not_authorized', message: 'nope' } }),
        };
    });

    const body = await call('/api/vk/status');
    eq(body, { enabled: true }, 'caller: успешное тело возвращается разобранным');

    let caught = null;
    try {
        await call('/api/vk/dialogs');
    } catch (e) {
        caught = e;
    }
    ok(caught !== null, 'caller: не-2xx бросает');
    eq(caught.code, 'not_authorized', 'caller: код доехал');
    eq(calls.length, 2, 'caller: ровно два обращения, без автоповторов');
}

{
    // 204 без тела — штатный ответ, а не повод бросать.
    const call = H.createCaller(async () => ({
        ok: true,
        status: 204,
        headers: { get: () => '' },
        text: async () => { throw new Error('no body'); },
    }));
    let value = 'unset';
    let threw = false;
    try { value = await call('/api/vk/logout'); } catch (_) { threw = true; }
    ok(!threw, 'caller: пустое тело не бросает');
    eq(value, null, 'caller: пустое тело ⇒ null');
}

{
    let threw = false;
    try { H.createCaller(null); } catch (_) { threw = true; }
    ok(threw, 'createCaller без fetch бросает сразу');
}

// ---------------------------------------------------------------------------

globalThis.setTimeout = realSetTimeout;
globalThis.clearTimeout = realClearTimeout;

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED`);
    process.exit(1);
}
console.log(`OK — ${checks} checks passed`);
