// ⚠ СТРАЖ ГЕЙТА ПЛАГИНА ВСТРЕЧ (фронтовая Ф2 forge-meet, задача ФФ2.1).
//
// Проверяет РЕАЛЬНЫЕ модули `static/js/meet/api.js`, `static/js/meet/ws.js` и
// `static/js/messenger/meet-store.js`. Сервер не нужен: транспорт — журналируемая
// заглушка `fetch`, сокет — журналируемая заглушка `WebSocket`.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Выключенный плагин — это ОСНОВНОЙ режим: девять
// из десяти запусков devforge идут без `--meet`. Если гейт протечёт, каждая
// такая загрузка страницы получит лишние запросы, открытый сокет и жёлтый
// warning в консоли — у людей, которые про встречи вообще не спрашивали.
// Симптом при этом будет не «ошибка», а «фронт стал тяжелее», то есть его
// никто не заметит. Поэтому здесь проверяется не «работает ли включённый
// плагин», а ЦЕНА ВЫКЛЮЧЕННОГО: ровно один GET, ноль сокетов, ноль строк в
// консоли.
//
// Порядок установки заглушек ЗНАЧИМ: `core/auth.js` на верхнем уровне
// подменяет `window.fetch`, поэтому журнал ставится ДО первого импорта, и
// запросы приходят в него уже с заголовком `Authorization` — тем самым
// проверяется, что кнопка встречи ходит на сервер тем же способом, что и
// остальные панели, а не мимо авторизации.
//
// Запуск: node tmux-web/tests/frontend/meet_api_gate.test.mjs
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

/** Батч уходит через BATCH_DELAY_MS (40 мс); ждём с запасом. */
const BATCH_WAIT = 90;

// ==========================================================================
// Заглушки браузера — минимальный срез, которым пользуются модули
// ==========================================================================

const journal = { requests: [], sockets: [] };

/** Ответ-заглушка в форме, которую понимает `createCaller`. */
function jsonResponse(status, body) {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: { get: (h) => (h.toLowerCase() === 'content-type' ? 'application/json' : null) },
        json: async () => body,
        text: async () => JSON.stringify(body),
    };
}

/** Что сервер отвечает на `/api/meet/status`. Меняется по ходу теста. */
let statusHandler = () => jsonResponse(404, { error: { code: 'not_found', message: 'нет такого роута' } });
/** Что сервер отвечает на `/api/meet/badges`. */
let badgesHandler = () => jsonResponse(200, { badges: [] });

globalThis.fetch = async (input, init) => {
    const url = typeof input === 'string' ? input : String(input && input.url);
    const headers = (init && init.headers) || null;
    let auth = null;
    if (headers && typeof headers.get === 'function') auth = headers.get('Authorization');
    else if (headers && typeof headers === 'object') auth = headers.Authorization || null;
    journal.requests.push({ url, method: (init && init.method) || 'GET', auth });
    if (url.startsWith('/api/meet/status')) return statusHandler();
    if (url.startsWith('/api/meet/badges')) return badgesHandler(url);
    return jsonResponse(404, { error: { code: 'not_found', message: 'нет такого роута' } });
};

globalThis.localStorage = {
    _v: { 'forge.authToken': 'test-token-123' },
    getItem(k) { return Object.prototype.hasOwnProperty.call(this._v, k) ? this._v[k] : null; },
    setItem(k, v) { this._v[k] = String(v); },
    removeItem(k) { delete this._v[k]; },
};
globalThis.location = { protocol: 'http:', host: 'localhost:7331', href: 'http://localhost:7331/', origin: 'http://localhost:7331', hash: '', pathname: '/', search: '' };
globalThis.history = { replaceState() {} };
globalThis.window = globalThis;

/** Сокет-заглушка: считает открытия и не делает ничего. */
class FakeSocket {
    constructor(url) {
        this.url = url;
        this.readyState = 0;
        journal.sockets.push(url);
    }

    send() {}

    close() { this.readyState = 3; }
}

// ==========================================================================
// Импорты — ТОЛЬКО после установки заглушек
// ==========================================================================

const base = (rel) => new URL(rel, import.meta.url).href;

// ws.js тянет core/auth.js, который подменяет window.fetch на верхнем уровне.
// Импортируем его ПЕРВЫМ, чтобы дальнейшие запросы шли уже через подменённый
// fetch — то есть ровно так, как в браузере.
const WS = await import(base('../../static/js/meet/ws.js'));
const API = await import(base('../../static/js/meet/api.js'));
const STORE = await import(base('../../static/js/messenger/meet-store.js'));
const CORE = await import(base('../../static/js/messenger/meet-core.js'));

ok(typeof API.initMeetGate === 'function', 'api.js грузится в голом Node (ни DOM, ни core/auth.js в его импортах)');

// ==========================================================================
// 1. Плагин выключен: 404 стоит ровно один запрос и ноль всего остального
// ==========================================================================

{
    API.resetMeetGate();
    journal.requests.length = 0;
    journal.sockets.length = 0;

    // Консоль обязана молчать ЦЕЛИКОМ: без флага `--meet` запускается
    // большинство инсталляций, и жёлтая строка на каждой загрузке страницы —
    // это шум ни о чём.
    const said = [];
    const orig = { log: console.log, warn: console.warn, error: console.error, debug: console.debug };
    console.log = (...a) => said.push(['log', a]);
    console.warn = (...a) => said.push(['warn', a]);
    console.error = (...a) => said.push(['error', a]);
    console.debug = (...a) => said.push(['debug', a]);

    const res = await API.initMeetGate();

    console.log = orig.log; console.warn = orig.warn;
    console.error = orig.error; console.debug = orig.debug;

    eq(res, null, '404 даёт null, а не исключение');
    eq(API.meetEnabled(), false, 'meetEnabled() = false при выключенном плагине');
    eq(STORE.meetEnabled(), false, 'стор без транспорта — тот же ответ');
    eq(journal.requests.length, 1, 'ровно ОДИН запрос за загрузку страницы');
    eq(journal.requests[0].url, '/api/meet/status', 'и это именно /api/meet/status');
    eq(said, [], 'при 404 в консоль не пишется ничего');

    // Повторные вызовы гейта из других точек (панель, возврат вкладки) не
    // имеют права спросить второй раз.
    await API.initMeetGate();
    await Promise.all([API.initMeetGate(), API.initMeetGate(), API.initMeetGate()]);
    eq(journal.requests.length, 1, 'пять вызовов гейта = один GET (мемоизировано ОБЕЩАНИЕ)');
    eq(API.meetGateRequests(), 1, 'счётчик гейта подтверждает единственный поход на сервер');

    // Чтение состояния строки ленты при выключенном плагине не ставит ничего
    // в очередь и, следовательно, не порождает батчей.
    const reads = [];
    for (let i = 0; i < 50; i += 1) reads.push(STORE.getMeetState(STORE.meetKey('telegram', '7', 100 + i)));
    ok(reads.every((r) => r === null), 'getMeetState отдаёт null при выключенном плагине');
    await sleep(BATCH_WAIT);
    eq(journal.requests.length, 1, '50 промахов при выключенном плагине не дают ни одного батча');

    eq(WS.connectMeetWs({ socketFactory: FakeSocket }), false, 'connectMeetWs отказывается открывать соединение');
    eq(journal.sockets.length, 0, 'сокет не открыт ни разу');
}

// ==========================================================================
// 2. Плагин включён: тот же единственный GET, но всё включается
// ==========================================================================

const STATUS_BODY = {
    enabled: true,
    db_ready: true,
    deps: { chrome: { ok: true } },
    limits: { recordings: 1, whisper: 1, summaries: 1 },
    providers: [
        {
            platform: 'telemost',
            title: 'Яндекс Телемост',
            hosts: ['telemost.yandex.ru'],
            host_suffixes: [],
            paths: ['/j/(\\d{4,32})'],
            keep_params: [],
            key_case: 'preserve',
            link_regex: 'https?://telemost',
            audio: true,
        },
        {
            platform: 'jitsi',
            title: 'Jitsi',
            hosts: ['meet.jit.si'],
            host_suffixes: [],
            paths: ['/([A-Za-z0-9]{4,64})'],
            keep_params: ['jwt'],
            key_case: 'preserve',
            link_regex: 'https?://meet\\.jit\\.si',
            audio: true,
        },
    ],
    active: [],
    config: { bot_name: 'F.O.R.G.E.' },
};

{
    API.resetMeetGate();
    journal.requests.length = 0;
    journal.sockets.length = 0;
    statusHandler = () => jsonResponse(200, STATUS_BODY);

    const res = await Promise.all([API.initMeetGate(), API.initMeetGate()]).then((r) => r[0]);
    ok(res && res.enabled === true, 'при 200 гейт возвращает тело статуса');
    eq(journal.requests.length, 1, 'два параллельных вызова гейта = один GET');
    eq(API.meetEnabled(), true, 'meetEnabled() = true');
    eq(API.meetStatusSnapshot().db_ready, true, 'снимок статуса доступен панелям');

    // Реестр платформ подменён ответом сервера — в форме, которую отдаёт
    // providers.rs (`platform`/`host_suffixes`/`keep_params`/`key_case`).
    eq(CORE.meetPlatforms().map((p) => p.platform), ['telemost', 'jitsi'], 'реестр платформ пришёл с бэкенда в проводной форме');
    const hit = CORE.detectMeetUrl('https://meet.jit.si/ForgeStandup?jwt=a&utm=b');
    ok(hit !== null, 'платформа, которой нет во встроенном наборе, распознаётся после гейта');
    eq(hit && hit.url, 'https://meet.jit.si/ForgeStandup?jwt=a', 'белый список параметров приехал вместе с платформой');
}

// ==========================================================================
// 3. Транспорт батча инъектирован гейтом (и только им)
// ==========================================================================

{
    journal.requests.length = 0;
    badgesHandler = () => jsonResponse(200, {
        plugin: 'telegram',
        chat_id: '7',
        min_id: 100,
        max_id: 149,
        badges: [
            { chat_id: '7', message_id: 120, meeting_id: 'm-1', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 1000, ended_at: null, duration_ms: null },
        ],
    });

    for (let i = 0; i < 50; i += 1) STORE.getMeetState(STORE.meetKey('telegram', '7', 100 + i));
    await sleep(BATCH_WAIT);

    eq(journal.requests.length, 1, '50 промахов дают ОДИН запрос');
    const url = journal.requests[0].url;
    ok(url.startsWith('/api/meet/badges?'), 'и это /api/meet/badges');
    const q = new URLSearchParams(url.slice(url.indexOf('?') + 1));
    eq([q.get('plugin'), q.get('chat_id'), q.get('min_id'), q.get('max_id')], ['telegram', '7', '100', '149'], 'запрос идёт ДИАПАЗОНОМ, а не списком id');

    const state = STORE.getMeetState(STORE.meetKey('telegram', '7', 120));
    eq(state && state.status, 'recording', 'состояние из ответа читается синхронно');
    const none = STORE.getMeetState(STORE.meetKey('telegram', '7', 121));
    eq(none && none.status, 'none', 'сообщение без встречи закэшировано как «можно записать», а не как промах');
}

// ==========================================================================
// 4. Bearer-токен — тем же способом, что и у остальных панелей
// ==========================================================================

{
    ok(journal.requests.every((r) => r.auth === 'Bearer test-token-123'),
        'каждый запрос несёт Authorization: Bearer <токен> (подменённый window.fetch из core/auth.js)');
    eq(journal.requests.length > 0, true, 'запросы вообще были — иначе проверка выше зеленела бы впустую');
}

// ==========================================================================
// 5. Таблица роутов: сама по себе непротиворечива
// ==========================================================================

{
    // Число зафиксировано намеренно: роут, добавленный на бэкенде и НЕ
    // добавленный сюда, обязан ломать тест, а не проходить молча. Сверку с
    // самим бэкендом делает meet_contract, здесь — только целостность таблицы.
    // Обновлено в Ф3: `POST /:id/confirm` (ассистированный вход);
    // в Ф5: `POST /deps/recheck`; в Ф6: `POST /:id/summary` (пересчёт саммари);
    // в Ф8: `DELETE /profile` (сброс постоянного профиля браузера).
    eq(API.MEET_ROUTES.length, 23, 'в таблице двадцать три роута');
    const seen = new Set();
    for (const r of API.MEET_ROUTES) {
        const sig = `${r.method} ${r.path}`;
        ok(!seen.has(sig), `роут ${sig} объявлен один раз`);
        seen.add(sig);
        ok(/^\/(api\/meet|ws\/meet)/.test(r.path), `путь ${r.path} лежит в пространстве плагина`);
    }
    for (const r of API.MEET_ROUTES) {
        if (r.path === '/ws/meet') {
            ok(typeof WS[r.fn] === 'function', `${r.fn} экспортирована из js/meet/ws.js`);
            continue;
        }
        ok(typeof API[r.fn] === 'function', `${r.fn} экспортирована из js/meet/api.js`);
    }
}

// ==========================================================================
// 6. Карточка встречи ходит в сеть ТОЛЬКО через объявленный клиент
// ==========================================================================
//
// Попап (`js/messenger/meet-popup.js`) получает клиента инъекцией и зовёт его
// как `api.<имя>`. Опечатка в имени НЕ ПАДАЕТ: `typeof api.getSummarry !==
// 'function'` — это законная ветка «роут ещё не приехал», и карточка молча
// покажет пустую вкладку. Поэтому имена сверяются с таблицей роутов здесь.

{
    const { readFileSync } = await import('node:fs');
    const src = readFileSync(new URL('../../static/js/messenger/meet-popup.js', import.meta.url), 'utf8')
        // Порядок вырезания: сначала строчные комментарии, потом блочные — в
        // шапке есть `/api/*`, и блочный стрипер, запущенный первым, съел бы
        // всё до ближайшего конца комментария вместе с половиной кода.
        .replace(/(^|[^:])\/\/.*$/gm, '$1')
        .replace(/\/\*[\s\S]*?\*\//g, '');
    const used = new Set(Array.from(src.matchAll(/\bapi\.(\w+)\b/g)).map((m) => m[1]));
    ok(used.size > 0, 'карточка вообще пользуется клиентом api');
    const declared = new Set(API.MEET_ROUTES.map((r) => r.fn));
    for (const fn of used) {
        ok(typeof API[fn] === 'function', `карточка зовёт api.${fn} — и такая функция экспортирована`);
        ok(declared.has(fn), `api.${fn} объявлена в таблице роутов`);
    }
    // Роуты карточки (Ф5–Ф7) обязаны быть В ДЕЛЕ, а не «на будущее».
    for (const fn of ['getMeeting', 'getTranscript', 'getSummary', 'fetchMeetingAudio', 'retryMeeting']) {
        ok(used.has(fn), `карточка пользуется ${fn} — иначе роут в таблице мёртв`);
    }
}

// ==========================================================================
// 7. Сброс гейта — только тестовая дверь, и она действительно закрывает всё
// ==========================================================================

{
    API.resetMeetGate();
    eq(API.meetEnabled(), false, 'после сброса плагин снова выключен');
    eq(STORE._debugMeetStore().cached, 0, 'кэш стора очищен');
    eq(CORE.meetPlatforms().map((p) => p.platform), ['telemost', 'meet', 'zoom', 'teams'], 'реестр платформ вернулся к встроенному');
}

// ==========================================================================

if (failures === 0) console.log(`[OK] meet_api_gate: ${checks} проверок`);
else console.error(`[FAIL] meet_api_gate: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
