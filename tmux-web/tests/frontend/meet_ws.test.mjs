// ⚠ СТРАЖ WS-КЛИЕНТА ВСТРЕЧ (фронтовая Ф2 forge-meet, задача ФФ2.3).
//
// Грузит РЕАЛЬНЫЕ static/js/meet/ws.js и static/js/messenger/meet-store.js.
// Сокет — заглушка, транспорт батча — заглушка, сервера нет. Заглушки браузера
// (window/location/localStorage) ставятся ДО импортов, потому что `core/auth.js`
// подменяет `window.fetch` на верхнем уровне модуля.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. У транспорта встреч три обязательства, и каждое
// ломается тихо:
//
//   1. КАДР ДОХОДИТ ДО ВСЕХ ПАНЕЛЕЙ ЧЕРЕЗ СТОР. Если кто-то однажды позовёт
//      панель напрямую (или через window.ForgeApp), направление зависимостей
//      развернётся, и вторая панель просто перестанет обновляться — без
//      единой ошибки.
//   2. RESYNC — ОДИН ДОБОР, А НЕ ШКВАЛ. Кадр приходит ВМЕСТО разрыва и именно
//      в момент пиковой нагрузки; реакция «перечитать всё по строке» удвоила
//      бы нагрузку ровно тогда, когда её и так много.
//   3. ГЕЙТ ЗАКРЫТ В ТРАНСПОРТЕ. `connectMeetWs` обязан отказать при
//      выключенном плагине сам, а не полагаться на дисциплину вызывающего.
//
// Запуск: node tmux-web/tests/frontend/meet_ws.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

let failures = 0;
let checks = 0;

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const DELAY = 5;
const WAIT = 40;

// ==========================================================================
// Заглушки браузера — ДО импортов
// ==========================================================================

globalThis.localStorage = {
    _v: { 'forge.authToken': 'tok-42' },
    getItem(k) { return Object.prototype.hasOwnProperty.call(this._v, k) ? this._v[k] : null; },
    setItem(k, v) { this._v[k] = String(v); },
    removeItem(k) { delete this._v[k]; },
};
globalThis.location = {
    protocol: 'http:', host: 'localhost:7331', href: 'http://localhost:7331/',
    origin: 'http://localhost:7331', hash: '', pathname: '/', search: '',
};
globalThis.history = { replaceState() {} };
globalThis.window = globalThis;

/** Сокет-заглушка: журналирует адрес, отданные наружу кадры и порядок закрытия. */
const sockets = [];

class FakeSocket {
    constructor(url) {
        this.url = url;
        this.readyState = 1;
        this.sent = [];
        this.log = [];
        this.onopen = null;
        this.onmessage = null;
        this.onclose = null;
        this.onerror = null;
        sockets.push(this);
    }

    send(data) { this.sent.push(data); }

    close() { this.readyState = 3; this.log.push('close'); }

    /** Прислать серверный кадр. */
    deliver(obj) {
        if (typeof this.onmessage === 'function') this.onmessage({ data: JSON.stringify(obj) });
    }
}

const WS = await import(new URL('../../static/js/meet/ws.js', import.meta.url).href);
const S = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);

// ==========================================================================
// 1. Гейт закрыт в самом транспорте
// ==========================================================================

{
    S.resetMeetStore();
    sockets.length = 0;
    eq(S.meetEnabled(), false, 'плагин выключен (configureMeetStore не звался)');
    eq(WS.connectMeetWs({ socketFactory: FakeSocket }), false, 'connectMeetWs отказывает первой строкой');
    eq(sockets.length, 0, 'сокет не создан');
    eq(WS.resumeMeet(), false, 'возврат вкладки из фона тоже ничего не открывает');
    eq(WS._debugMeetWs().created, false, 'клиента ядра не создано — значит нет и его таймеров');
}

// ==========================================================================
// 2. Включённый плагин: одно соединение, правильный адрес, токен в query
// ==========================================================================

const transport = { calls: [], answer: () => ({ badges: [] }) };
S.configureMeetStore({
    fetchStates: async (q) => { transport.calls.push(q); return transport.answer(q); },
    batchDelayMs: DELAY,
});

{
    eq(WS.connectMeetWs({ socketFactory: FakeSocket }), true, 'при включённом плагине соединение открывается');
    eq(sockets.length, 1, 'ровно один сокет');
    eq(sockets[0].url, 'ws://localhost:7331/ws/meet?token=tok-42', 'адрес /ws/meet с токеном в query (заголовки WebSocket не умеет)');

    // Идемпотентность: connect зовут из гейта и из возврата вкладки, порядок
    // между ними не определён.
    WS.connectMeetWs({ socketFactory: FakeSocket });
    WS.resumeMeet();
    eq(sockets.length, 1, 'повторный connect не плодит сокетов');
}

// ==========================================================================
// 3. Кадры расходятся ЧЕРЕЗ СТОР — до всех трёх панелей сразу
// ==========================================================================

{
    const ws = sockets[0];
    // Три подписчика = три панели. Ни одна из них не импортируется
    // транспортом: они пришли к стору сами.
    const telegram = [];
    const vk = [];
    const bitrix = [];
    const offs = [
        S.subscribeMeet((ev) => telegram.push(ev.type)),
        S.subscribeMeet((ev) => vk.push(ev.type)),
        S.subscribeMeet((ev) => bitrix.push(ev.type)),
    ];

    // Строка ленты уже знает свою встречу (её дал батч ленты).
    transport.answer = () => ({
        badges: [{
            chat_id: '7', message_id: 100, meeting_id: 'm-1', status: 'joining',
            summary_status: 'none', platform: 'telemost', started_at: null, ended_at: null, duration_ms: null,
        }],
    });
    S.getMeetState(S.meetKey('telegram', '7', 100));
    await sleep(WAIT);
    eq(S.getMeetState(S.meetKey('telegram', '7', 100)).status, 'joining', 'состояние строки из батча');

    ws.deliver({
        type: 'meeting_update',
        meeting: { id: 'm-1', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 5000, ended_at: null, duration_ms: null },
    });

    eq(S.getMeetState(S.meetKey('telegram', '7', 100)).status, 'recording', 'кадр обновления доехал до строки ленты');
    ok(telegram.includes('meeting') && vk.includes('meeting') && bitrix.includes('meeting'),
        'кадр дошёл ДО ВСЕХ ТРЁХ панелей — через шину стора, а не прямым вызовом');
    ok(telegram.includes('states') && vk.includes('states') && bitrix.includes('states'),
        'и событие изменившихся строк тоже');
    for (const off of offs) off();
}

// ==========================================================================
// 4. Resync — ОДИН добор батчем, а не шквал
// ==========================================================================

{
    const ws = sockets[0];
    // Прогреваем полсотни строк: половина терминальных, половина живых.
    transport.answer = (q) => ({
        badges: Array.from({ length: q.max_id - q.min_id + 1 }, (_, i) => {
            const id = q.min_id + i;
            return {
                chat_id: q.chat_id, message_id: id, meeting_id: `m-${id}`,
                status: id % 2 === 0 ? 'ready' : 'recording',
                summary_status: 'none', platform: 'telemost',
                started_at: 1000, ended_at: null, duration_ms: null,
            };
        }),
    });
    for (let i = 0; i < 50; i += 1) S.getMeetState(S.meetKey('telegram', '7', 200 + i));
    await sleep(WAIT);
    const before = transport.calls.length;
    eq(S.getMeetState(S.meetKey('telegram', '7', 200)).status, 'ready', 'чётные строки терминальны');
    eq(S.getMeetState(S.meetKey('telegram', '7', 201)).status, 'recording', 'нечётные — живые');

    ws.deliver({ type: 'resync' });
    await sleep(WAIT);

    eq(transport.calls.length - before, 1, 'resync дал РОВНО ОДИН запрос, а не по одному на строку');
    const last = transport.calls[transport.calls.length - 1];
    // Нижняя граница — 100, а не 201: строка из секции 3 тоже живая
    // (`recording`) и тоже подлежит добору. Это и есть смысл диапазона —
    // одним запросом накрыть все нетерминальные строки чата, где бы они ни
    // лежали, вместо запроса на каждую.
    eq([last.min_id, last.max_id], [100, 249], 'добор идёт ОДНИМ диапазоном на весь чат');
    eq(S.getMeetState(S.meetKey('telegram', '7', 200)).status, 'ready', 'терминальная строка не перезапрашивалась и не изменилась');
}

// ==========================================================================
// 5. ping/pong обслуживает ядро, а свои копии логики транспорт не заводит
// ==========================================================================

{
    const ws = sockets[0];
    ws.sent.length = 0;
    ws.deliver({ type: 'ping' });
    eq(ws.sent, ['{"type":"pong"}'], 'на ping ядро отвечает pong (сервер рвёт связь по idle 60 с)');

    const src = (await import('node:fs')).readFileSync(new URL('../../static/js/meet/ws.js', import.meta.url), 'utf8');
    const code = src.split('\n').filter((l) => !l.trimStart().startsWith('//') && !l.trimStart().startsWith('*')).join('\n');
    ok(!/setTimeout\s*\(/.test(code), 'своей лестницы реконнекта в модуле нет — она в ws-core.js');
    ok(!/new\s+WebSocket/.test(code), 'сокет создаёт ядро, а не этот модуль');
    ok(/createWsClient/.test(code), 'соединение строится поверх messenger/ws-core.js');
    ok(!/ForgeApp/.test(code), 'window.ForgeApp не используется');
    ok(!/from '\.\.\/(telegram|vk|bitrix)\//.test(code), 'ни одного импорта панели — направление зависимостей не разворачивается');
}

// ==========================================================================
// 6. Кадры, которых мы не знаем, не ломают соединение
// ==========================================================================

{
    const ws = sockets[0];
    const said = [];
    const origDebug = console.debug;
    console.debug = (...a) => said.push(a);
    ws.deliver({ type: 'какой_то_новый_кадр', payload: 1 });
    ws.deliver({ type: 'какой_то_новый_кадр', payload: 2 });
    ws.deliver({ type: 'какой_то_новый_кадр', payload: 3 });
    console.debug = origDebug;
    eq(said.length, 1, 'про незнакомый тип сказано ОДИН раз, а не на каждый кадр');
    eq(WS._debugMeetWs().unknownFrames['какой_то_новый_кадр'], 3, 'но все три посчитаны');

    // Кадр после незнакомого обязан дойти: соединение живо.
    ws.deliver({ type: 'deps', deps: { ffmpeg: { ok: false } } });
    eq(S.getMeetDeps().ffmpeg.ok, false, 'следующий кадр обработан как ни в чём не бывало');

    // Исключение обработчика тоже не рвёт поток.
    const origWarn = console.warn;
    const warned = [];
    console.warn = (...a) => warned.push(a);
    S.subscribeMeet((ev) => { if (ev.type === 'needs_human') throw new Error('панель упала'); });
    ws.deliver({ type: 'needs_human', meeting_id: 'm-1', kind: 'captcha' });
    ws.deliver({ type: 'deps', deps: { ffmpeg: { ok: true } } });
    console.warn = origWarn;
    eq(S.getMeetDeps().ffmpeg.ok, true, 'кадр после упавшего обработчика обработан');
    ok(warned.length >= 1, 'падение обработчика попало в консоль');
}

// ==========================================================================
// 7. Все девять содержательных кадров разложены по стору
// ==========================================================================

{
    const ws = sockets[0];
    S.resetMeetStore();
    S.configureMeetStore({ fetchStates: async () => ({ badges: [] }), batchDelayMs: DELAY });
    const seen = [];
    S.subscribeMeet((ev) => seen.push(ev.type));

    ws.deliver({
        type: 'snapshot',
        meetings: [{ id: 'm-9', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 1, ended_at: null, duration_ms: null }],
        deps: { chrome: { ok: true } },
        limits: { recordings: 1, whisper: 1, summaries: 1 },
    });
    eq(S.liveMeetings().length, 1, 'snapshot наполнил реестр активных встреч');
    eq(S.getMeetLimits().whisper, 1, 'лимиты из снапшота');

    ws.deliver({ type: 'progress', meeting_id: 'm-9', phase: 'transcribing', queued: true, queue_position: 3 });
    eq(S.getMeetProgress('m-9').queuePosition, 3, 'progress с очередью whisper');

    ws.deliver({ type: 'transcript_delta', meeting_id: 'm-9', segments: [{ text: 'привет' }] });
    ws.deliver({ type: 'participants', meeting_id: 'm-9', participants: [{ name: 'Игорь' }] });
    ws.deliver({ type: 'error', meeting_id: 'm-9', code: 'whisper_unavailable', message: 'нет whisper' });

    eq(seen.filter((t) => t === 'transcript_delta').length, 1, 'дельта расшифровки разошлась подписчикам');
    eq(seen.filter((t) => t === 'participants').length, 1, 'список участников разошёлся');
    eq(seen.filter((t) => t === 'error').length, 1, 'ошибка воркера разошлась');
}

// ==========================================================================
// 8. Уход в фон и выгрузка: соединение и таймер гасятся
// ==========================================================================

{
    S.applyMeetingUpdate({ id: 'm-live', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 1, ended_at: null, duration_ms: null });
    eq(S._debugMeetStore().ticking, true, 'живая встреча завела таймер записи');

    WS.suspendMeet();
    eq(WS.isMeetWsOpen(), false, 'уход страницы в фон закрыл соединение');
    eq(S._debugMeetStore().ticking, true, 'но таймер записи в фоне продолжает считать — цифра не должна отстать');

    const opened = sockets.length;
    eq(WS.resumeMeet(), true, 'возврат из фона поднимает соединение');
    eq(sockets.length, opened + 1, 'и это новый сокет');

    WS.teardownMeet();
    eq(WS.isMeetWsOpen(), false, 'выгрузка закрыла соединение');
    eq(S._debugMeetStore().ticking, false, 'и погасила интервал — иначе он стрелял бы по мёртвому DOM из bfcache');
    eq(WS._debugMeetWs().created, false, 'клиент ядра отпущен');
}

// ==========================================================================

S.resetMeetStore();
if (failures === 0) console.log(`[OK] meet_ws: ${checks} проверок`);
else console.error(`[FAIL] meet_ws: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
