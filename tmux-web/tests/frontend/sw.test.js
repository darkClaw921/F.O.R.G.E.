// tmux-web/tests/frontend/sw.test.js
//
// PWA Service Worker — регресс-тесты краевых ситуаций fetch-роутинга,
// install/activate lifecycle, message SKIP_WAITING, push, notificationclick.
//
// ПОДХОД: грузим РЕАЛЬНЫЙ static/sw.js в node:vm-песочнице с моками
// self/caches/fetch/clients/Request/Response, перехватываем top-level
// addEventListener-хендлеры и вызываем их в тестах. Это проверяет НАСТОЯЩИЙ
// код, а не реплику — изменение sw.js, ломающее контракт, валит этот файл.
//
// Запуск: node tmux-web/tests/frontend/sw.test.js
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.
//
// sw.js — классический worker ('use strict', top-level self.addEventListener),
// НЕ ES-модуль → vm.runInContext подходит напрямую (import не нужен).

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// =============================================================================
// Tiny assertion runner (стиль sidebar_grouping.test.js)
// =============================================================================

let passed = 0;
let failed = 0;
const failures = [];

function assert(label, cond, details) {
    if (cond) {
        passed += 1;
        console.log('  ok   ' + label);
    } else {
        failed += 1;
        failures.push({ label, details: details || '' });
        console.log('  FAIL ' + label + (details ? '  — ' + details : ''));
    }
}

function eq(label, actual, expected) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    assert(label, a === e, 'expected ' + e + ', got ' + a);
}

// group awaits async callbacks: каждый тест с await внутри отрабатывает
// полностью перед следующим. Возвращает промис — вызывающий должен await.
function group(name, fn) {
    console.log('\n[' + name + ']');
    return Promise.resolve().then(fn);
}

// =============================================================================
// Лёгкие стабы Request/Response (контролируем method/mode/headers/type/ok)
// =============================================================================

class FakeHeaders {
    constructor(init) {
        this._m = new Map();
        if (init) {
            for (const k of Object.keys(init)) {
                this._m.set(String(k).toLowerCase(), init[k]);
            }
        }
    }
    get(name) {
        const v = this._m.get(String(name).toLowerCase());
        return v === undefined ? null : v;
    }
}

let _reqSeq = 0;
class FakeRequest {
    constructor(url, init) {
        init = init || {};
        // Request(url) или Request(anotherRequest) — берём .url если объект.
        this.url = typeof url === 'string' ? url : (url && url.url) || String(url);
        this.method = init.method || (typeof url === 'object' && url && url.method) || 'GET';
        this.mode = init.mode || (typeof url === 'object' && url && url.mode) || undefined;
        this.cache = init.cache;
        this.headers = init.headers instanceof FakeHeaders
            ? init.headers
            : new FakeHeaders(init.headers || {});
        this._id = ++_reqSeq; // уникальный — чтобы Request-ключи кэша были разными объектами
    }
}

let _resSeq = 0;
class FakeResponse {
    constructor(body, init) {
        init = init || {};
        this.body = body;
        this.status = init.status !== undefined ? init.status : 200;
        this.statusText = init.statusText || '';
        this.ok = init.ok !== undefined ? init.ok : (this.status >= 200 && this.status < 300);
        this.type = init.type || 'basic';
        this._id = ++_resSeq;
        this._cloned = false;
    }
    clone() {
        const c = new FakeResponse(this.body, {
            status: this.status,
            statusText: this.statusText,
            ok: this.ok,
            type: this.type,
        });
        c._id = this._id; // клон логически тот же ответ (для проверок «это сетевой»)
        c._cloned = true;
        return c;
    }
}

// =============================================================================
// In-memory caches mock
// =============================================================================

function makeCaches() {
    const store = new Map(); // cacheName -> Map(keyString -> Response)
    const log = { put: [], match: [], deletes: [], opens: [] };

    function keyOf(req) {
        if (typeof req === 'string') return req;
        if (req && req.url) return req.url;
        return String(req);
    }

    function openSync(name) {
        if (!store.has(name)) store.set(name, new Map());
        const m = store.get(name);
        return {
            _name: name,
            put(req, res) {
                log.put.push({ cache: name, key: keyOf(req) });
                m.set(keyOf(req), res);
                return Promise.resolve();
            },
            match(req) {
                log.match.push({ cache: name, key: keyOf(req) });
                return Promise.resolve(m.get(keyOf(req)));
            },
            addAll(reqs) {
                // имитируем браузер: дёргаем fetch на каждый Request, кладём ответ.
                // Если любой fetch падает — addAll реджектится (атомарность).
                return Promise.all(
                    reqs.map((r) =>
                        api._fetch(r).then((res) => {
                            m.set(keyOf(r), res);
                            return res;
                        })
                    )
                ).then(() => undefined);
            },
            keys() {
                return Promise.resolve([...m.keys()]);
            },
        };
    }

    const api = {
        _store: store,
        _log: log,
        _fetch: null, // подставляется тестом (для addAll)
        open(name) {
            log.opens.push(name);
            return Promise.resolve(openSync(name));
        },
        keys() {
            return Promise.resolve([...store.keys()]);
        },
        delete(name) {
            log.deletes.push(name);
            const had = store.has(name);
            store.delete(name);
            return Promise.resolve(had);
        },
        has(name) {
            return Promise.resolve(store.has(name));
        },
        // helper для тестов: положить ответ в кэш напрямую
        _seed(name, key, res) {
            if (!store.has(name)) store.set(name, new Map());
            store.get(name).set(key, res);
        },
        _get(name, key) {
            return store.has(name) ? store.get(name).get(key) : undefined;
        },
        _names() {
            return [...store.keys()];
        },
    };
    return api;
}

// =============================================================================
// Программируемый fetch
// =============================================================================

function makeFetch() {
    const calls = [];
    let router = null;
    function fetchFn(req) {
        const url = typeof req === 'string' ? req : (req && req.url) || String(req);
        calls.push(url);
        if (!router) return Promise.reject(new Error('no router'));
        try {
            const r = router(url, req);
            if (r && typeof r.then === 'function') return r;
            return Promise.resolve(r);
        } catch (e) {
            return Promise.reject(e);
        }
    }
    fetchFn._calls = calls;
    fetchFn._setRouter = (fn) => { router = fn; };
    return fetchFn;
}

// =============================================================================
// clients mock
// =============================================================================

function makeClients() {
    const rec = { matchAllOpts: null, openWindow: [], claimCount: 0 };
    const api = {
        _rec: rec,
        _clientList: [],
        _hasOpenWindow: true,
        matchAll(opts) {
            rec.matchAllOpts = opts;
            return Promise.resolve(api._clientList);
        },
        claim() {
            rec.claimCount += 1;
            return Promise.resolve();
        },
    };
    // openWindow определяем как свойство, которое можно «убрать» (set undefined)
    Object.defineProperty(api, 'openWindow', {
        configurable: true,
        get() {
            if (!api._hasOpenWindow) return undefined;
            return (url) => {
                rec.openWindow.push(url);
                return Promise.resolve({});
            };
        },
    });
    return api;
}

function makeClient(url, opts) {
    opts = opts || {};
    const rec = { focused: 0, navigated: null };
    const c = {
        url,
        _rec: rec,
        focus() {
            rec.focused += 1;
            return Promise.resolve(c);
        },
    };
    if (opts.navigate !== false) {
        c.navigate = (u) => {
            rec.navigated = u;
            return Promise.resolve(c);
        };
    }
    return c;
}

// =============================================================================
// Загрузка реального sw.js в vm-песочницу
// =============================================================================

const SW_SRC = fs.readFileSync(
    path.join(__dirname, '..', '..', 'static', 'sw.js'),
    'utf8'
);

/**
 * Создаёт свежий SW-инстанс: исполняет sw.js в новом контексте и возвращает
 * { handlers, caches, fetch, clients, self, recordedNotifications, skipWaitingCount }.
 */
function loadSW() {
    const handlers = {};
    const cachesMock = makeCaches();
    const fetchMock = makeFetch();
    cachesMock._fetch = fetchMock; // addAll использует тот же fetch
    const clientsMock = makeClients();
    const recordedNotifications = [];
    const counters = { skipWaiting: 0 };
    const registeredTypes = []; // журнал всех addEventListener

    const self = {
        location: { origin: 'https://app.test' },
        addEventListener(type, handler) {
            registeredTypes.push(type);
            // храним последний хендлер каждого типа (для инварианта дублей —
            // отдельно считаем сколько раз тип регистрировался).
            handlers[type] = handler;
        },
        skipWaiting() {
            counters.skipWaiting += 1;
        },
        registration: {
            showNotification(title, options) {
                recordedNotifications.push({ title, options });
                return Promise.resolve();
            },
        },
        clients: clientsMock,
    };

    const ctx = {
        self,
        caches: cachesMock,
        fetch: fetchMock,
        clients: clientsMock,
        // глобалы, которые sw.js дёргает голыми именами
        URL: globalThis.URL,
        Request: FakeRequest,
        Response: FakeResponse,
        Headers: FakeHeaders,
        Promise: globalThis.Promise,
        Set: globalThis.Set,
        Map: globalThis.Map,
        console: console,
    };
    vm.createContext(ctx);
    vm.runInContext(SW_SRC, ctx, { filename: 'sw.js' });

    return {
        handlers,
        registeredTypes,
        caches: cachesMock,
        fetch: fetchMock,
        clients: clientsMock,
        self,
        recordedNotifications,
        counters,
    };
}

// =============================================================================
// Event factories
// =============================================================================

function makeFetchEvent(opts) {
    const req = new FakeRequest(opts.url, {
        method: opts.method || 'GET',
        mode: opts.mode,
        headers: opts.headers || {},
    });
    const evt = {
        request: req,
        _responded: undefined,
        respondWith(p) { this._responded = p; },
        waitUntil(p) { this._waited = p; return p; },
    };
    return evt;
}

function makeLifecycleEvent() {
    return {
        _waited: undefined,
        waitUntil(p) { this._waited = p; return p; },
    };
}

// Версии кэшей (должны совпадать с sw.js CACHE_VERSION='forge-pwa-v1').
const SHELL_CACHE = 'forge-shell-forge-pwa-v1';
const RUNTIME_CACHE = 'forge-runtime-forge-pwa-v1';
const DATA_CACHE = 'forge-data-forge-pwa-v1';

// helper: дождаться микротасков
function flush() { return Promise.resolve().then(() => Promise.resolve()); }

// =============================================================================
// Async test orchestration — собираем все группы в промис-цепочку
// =============================================================================

async function run() {

    // ───────────────────────── Загрузка / инвариант ─────────────────────
    await group('загрузка sw.js: хендлеры зарегистрированы единожды', () => {
        const sw = loadSW();
        const expectTypes = ['install', 'activate', 'fetch', 'message', 'push', 'notificationclick'];
        for (const t of expectTypes) {
            assert('зарегистрирован handler ' + t, typeof sw.handlers[t] === 'function');
        }
        eq('всего 6 addEventListener-вызовов', sw.registeredTypes.length, 6);
        // каждый тип ровно один раз
        for (const t of expectTypes) {
            eq('тип ' + t + ' зарегистрирован 1 раз',
                sw.registeredTypes.filter((x) => x === t).length, 1);
        }
    });

    // ─────────────────────────── fetch: не-GET ──────────────────────────
    await group('fetch: POST не перехватывается', () => {
        const sw = loadSW();
        const evt = makeFetchEvent({ url: 'https://app.test/api/echo/conversations', method: 'POST' });
        sw.handlers.fetch(evt);
        assert('respondWith НЕ вызван', evt._responded === undefined);
        eq('fetch-мок не дёрнут', sw.fetch._calls.length, 0);
    });

    await group('fetch: PATCH/DELETE/HEAD/OPTIONS сквозные', () => {
        const sw = loadSW();
        const cases = [
            { method: 'PATCH', url: 'https://app.test/js/main.js' },
            { method: 'DELETE', url: 'https://app.test/api/sessions' },
            { method: 'HEAD', url: 'https://app.test/js/main.js' },
            { method: 'OPTIONS', url: 'https://app.test/api/sessions' },
        ];
        for (const c of cases) {
            const evt = makeFetchEvent(c);
            sw.handlers.fetch(evt);
            assert(c.method + ' ' + c.url + ': respondWith НЕ вызван', evt._responded === undefined);
        }
        eq('fetch не дёрнут ни разу', sw.fetch._calls.length, 0);
    });

    // ─────────────────────────── fetch: /ws/* ───────────────────────────
    await group('fetch: /ws/* и /ws никогда не перехватываются', () => {
        const sw = loadSW();
        const e1 = makeFetchEvent({ url: 'https://app.test/ws/term/123' });
        const e2 = makeFetchEvent({ url: 'https://app.test/ws' });
        sw.handlers.fetch(e1);
        sw.handlers.fetch(e2);
        assert('/ws/term/123 не перехвачен', e1._responded === undefined);
        assert('/ws не перехвачен', e2._responded === undefined);
    });

    await group('fetch: upgrade=websocket header не перехватывается', () => {
        const sw = loadSW();
        const evt = makeFetchEvent({ url: 'https://app.test/anything', headers: { upgrade: 'websocket' } });
        sw.handlers.fetch(evt);
        assert('respondWith НЕ вызван', evt._responded === undefined);
    });

    await group('fetch: cross-origin GET не перехватывается', () => {
        const sw = loadSW();
        const evt = makeFetchEvent({ url: 'https://cdn.other.test/lib.js' });
        sw.handlers.fetch(evt);
        assert('cross-origin: respondWith НЕ вызван', evt._responded === undefined);
        eq('fetch не дёрнут', sw.fetch._calls.length, 0);
    });

    // ───────────────────────── fetch: navigate ──────────────────────────
    await group('fetch: navigate online → network-first + кэш под /', async () => {
        const sw = loadSW();
        const netRes = new FakeResponse('<html>fresh</html>', { ok: true, status: 200 });
        sw.fetch._setRouter(() => netRes);
        const evt = makeFetchEvent({ url: 'https://app.test/', mode: 'navigate' });
        sw.handlers.fetch(evt);
        assert('respondWith вызван', evt._responded !== undefined);
        const out = await evt._responded;
        eq('отдан сетевой ответ (тот же _id)', out._id, netRes._id);
        await flush();
        const cached = sw.caches._get(SHELL_CACHE, '/');
        assert('в SHELL_CACHE под "/" лежит clone ответа', !!cached);
        eq('кэширован тот же логический ответ', cached && cached._id, netRes._id);
    });

    await group('fetch: navigate offline → fallback на кэш /', async () => {
        const sw = loadSW();
        const cachedRes = new FakeResponse('<html>cached</html>', { ok: true });
        sw.caches._seed(SHELL_CACHE, '/', cachedRes);
        sw.fetch._setRouter(() => { throw new Error('offline'); });
        const evt = makeFetchEvent({ url: 'https://app.test/', mode: 'navigate' });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('отдан закэшированный "/"', out._id, cachedRes._id);
    });

    await group('fetch: navigate offline и нет кэша / → rethrow', async () => {
        const sw = loadSW();
        sw.fetch._setRouter(() => { throw new Error('boom-offline'); });
        const evt = makeFetchEvent({ url: 'https://app.test/page', mode: 'navigate' });
        sw.handlers.fetch(evt);
        let threw = false;
        try {
            await evt._responded;
        } catch (e) {
            threw = true;
            eq('исходная ошибка проброшена', e.message, 'boom-offline');
        }
        assert('promise реджектится', threw);
    });

    await group('fetch: navigate с не-ok ответом не кэшируется', async () => {
        const sw = loadSW();
        const res500 = new FakeResponse('err', { ok: false, status: 500 });
        sw.fetch._setRouter(() => res500);
        const evt = makeFetchEvent({ url: 'https://app.test/', mode: 'navigate' });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('500 возвращён как есть', out._id, res500._id);
        await flush();
        assert('cache.put("/") НЕ вызван', sw.caches._get(SHELL_CACHE, '/') === undefined);
    });

    await group('fetch: navigate имеет приоритет над path-роутингом', async () => {
        // mode='navigate' при pathname='/api/sessions' (формально allowlist)
        const sw = loadSW();
        const netRes = new FakeResponse('<html>', { ok: true });
        sw.fetch._setRouter(() => netRes);
        const evt = makeFetchEvent({ url: 'https://app.test/api/sessions', mode: 'navigate' });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('обработан как navigate (отдан сетевой)', out._id, netRes._id);
        await flush();
        // navigationStrategy кладёт под '/' в SHELL_CACHE, не в DATA_CACHE
        assert('положен в SHELL_CACHE под "/"', !!sw.caches._get(SHELL_CACHE, '/'));
        assert('DATA_CACHE не тронут', sw.caches._get(DATA_CACHE, 'https://app.test/api/sessions') === undefined);
    });

    // ───────────────────────── fetch: статика SWR ───────────────────────
    await group('fetch: статика SWR — есть кэш → мгновенно старый + фон-обновление', async () => {
        const sw = loadSW();
        const oldRes = new FakeResponse('old', { ok: true, type: 'basic' });
        const newRes = new FakeResponse('new', { ok: true, type: 'basic' });
        const key = 'https://app.test/js/main.js';
        sw.caches._seed(RUNTIME_CACHE, key, oldRes);
        sw.fetch._setRouter(() => newRes);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('отдан СТАРЫЙ кэш немедленно', out._id, oldRes._id);
        eq('fetch всё равно вызван (revalidate)', sw.fetch._calls.length, 1);
        await flush();
        eq('RUNTIME_CACHE обновлён новым', sw.caches._get(RUNTIME_CACHE, key)._id, newRes._id);
    });

    await group('fetch: статика SWR — нет кэша → ждём сеть и кэшируем', async () => {
        const sw = loadSW();
        const netRes = new FakeResponse('css', { ok: true, type: 'basic' });
        const key = 'https://app.test/css/app.css';
        sw.fetch._setRouter(() => netRes);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('отдан сетевой', out._id, netRes._id);
        await flush();
        eq('положен в RUNTIME_CACHE', sw.caches._get(RUNTIME_CACHE, key)._id, netRes._id);
    });

    await group('fetch: статика SWR — нет кэша и сеть упала → синтетический 504', async () => {
        const sw = loadSW();
        const key = 'https://app.test/vendor/xterm/xterm.min.js';
        sw.fetch._setRouter(() => { throw new Error('offline'); });
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('status 504', out.status, 504);
        eq('statusText Offline', out.statusText, 'Offline');
        assert('кэш не тронут', sw.caches._get(RUNTIME_CACHE, key) === undefined);
    });

    await group('fetch: статика SWR — opaque/cors ответ не кэшируется', async () => {
        const sw = loadSW();
        const key = 'https://app.test/icons/icon.png';
        const opaque = new FakeResponse('img', { ok: true, type: 'opaque' });
        sw.fetch._setRouter(() => opaque);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('ответ возвращён', out._id, opaque._id);
        await flush();
        assert('cache.put НЕ вызван (type!=basic)', sw.caches._get(RUNTIME_CACHE, key) === undefined);
    });

    await group('fetch: статика SWR — не-ok ответ не кэшируется но возвращается', async () => {
        const sw = loadSW();
        const key = 'https://app.test/js/x.js';
        const res404 = new FakeResponse('nf', { ok: false, status: 404, type: 'basic' });
        sw.fetch._setRouter(() => res404);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('404 возвращён', out._id, res404._id);
        await flush();
        assert('cache.put НЕ вызван', sw.caches._get(RUNTIME_CACHE, key) === undefined);
    });

    await group('fetch: isStaticAsset покрывает все ветки', () => {
        const sw = loadSW();
        const netRes = new FakeResponse('ok', { ok: true, type: 'basic' });
        sw.fetch._setRouter(() => netRes);
        const paths = [
            '/js/a', '/css/a', '/vendor/a', '/icons/a',
            '/style.css', '/manifest.webmanifest',
            '/quick-cmd.js', '/command-dock.js', '/hotkeys.js', '/app.js',
        ];
        for (const p of paths) {
            const evt = makeFetchEvent({ url: 'https://app.test' + p });
            sw.handlers.fetch(evt);
            assert('статика ' + p + ' → respondWith вызван', evt._responded !== undefined);
        }
    });

    await group('fetch: похожие-но-не-статика пути не перехватываются', () => {
        const sw = loadSW();
        const paths = ['/javascript/x', '/styles.css', '/manifest.json'];
        for (const p of paths) {
            const evt = makeFetchEvent({ url: 'https://app.test' + p });
            sw.handlers.fetch(evt);
            assert('не-статика ' + p + ' → respondWith НЕ вызван', evt._responded === undefined);
        }
        // '/' без navigate-mode тоже не статика
        const root = makeFetchEvent({ url: 'https://app.test/', mode: 'cors' });
        sw.handlers.fetch(root);
        assert('"/" без navigate → respondWith НЕ вызван', root._responded === undefined);
    });

    await group('fetch: корень / без navigate-mode не считается статикой', () => {
        const sw = loadSW();
        const evt = makeFetchEvent({ url: 'https://app.test/' }); // mode undefined
        sw.handlers.fetch(evt);
        assert('respondWith НЕ вызван', evt._responded === undefined);
    });

    // ───────────────────────── fetch: data allowlist ────────────────────
    await group('fetch: data allowlist точное совпадение → network-first + кэш', async () => {
        const sw = loadSW();
        const key = 'https://app.test/api/sessions';
        const netRes = new FakeResponse('[]', { ok: true });
        sw.fetch._setRouter(() => netRes);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('отдан сетевой', out._id, netRes._id);
        await flush();
        eq('положен в DATA_CACHE', sw.caches._get(DATA_CACHE, key)._id, netRes._id);
    });

    await group('fetch: data allowlist под-пути и query', async () => {
        const sw = loadSW();
        const netRes = new FakeResponse('x', { ok: true });
        sw.fetch._setRouter(() => netRes);
        const urls = [
            '/api/tasks/42',
            '/api/todos?status=open',
            '/api/echo/conversations/abc',
            '/api/echo/memories?q=x',
            '/api/echo/daily-reports',
        ];
        for (const u of urls) {
            const evt = makeFetchEvent({ url: 'https://app.test' + u });
            sw.handlers.fetch(evt);
            assert('allowlist ' + u + ' → respondWith вызван', evt._responded !== undefined);
            const out = await evt._responded;
            eq('  отдан сетевой для ' + u, out._id, netRes._id);
        }
    });

    await group('fetch: data allowlist boundary — подстрока без разделителя НЕ матчится', () => {
        const sw = loadSW();
        // прочие /api/* → return (нет respondWith)
        const urls = ['/api/sessionsX', '/api/tasks-archive', '/api/todosbackup'];
        for (const u of urls) {
            const evt = makeFetchEvent({ url: 'https://app.test' + u });
            sw.handlers.fetch(evt);
            assert('boundary ' + u + ' → respondWith НЕ вызван (идёт в /api/ no-cache)', evt._responded === undefined);
        }
    });

    await group('fetch: data allowlist offline → отдаём кэш', async () => {
        const sw = loadSW();
        const key = 'https://app.test/api/sessions';
        const cachedRes = new FakeResponse('cached', { ok: true });
        sw.caches._seed(DATA_CACHE, key, cachedRes);
        sw.fetch._setRouter(() => { throw new Error('offline'); });
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('отдан закэшированный', out._id, cachedRes._id);
    });

    await group('fetch: data allowlist offline и нет кэша → rethrow', async () => {
        const sw = loadSW();
        sw.fetch._setRouter(() => { throw new Error('net-down'); });
        const evt = makeFetchEvent({ url: 'https://app.test/api/tasks' });
        sw.handlers.fetch(evt);
        let threw = false;
        try { await evt._responded; } catch (e) { threw = true; eq('ошибка проброшена', e.message, 'net-down'); }
        assert('promise реджектится', threw);
    });

    await group('fetch: data allowlist не-ok ответ не кэшируется', async () => {
        const sw = loadSW();
        const key = 'https://app.test/api/todos';
        const res500 = new FakeResponse('err', { ok: false, status: 500 });
        sw.fetch._setRouter(() => res500);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        const out = await evt._responded;
        eq('500 возвращён', out._id, res500._id);
        await flush();
        assert('cache.put НЕ вызван', sw.caches._get(DATA_CACHE, key) === undefined);
    });

    await group('fetch: прочие /api/* служебные НЕ кэшируются', () => {
        const sw = loadSW();
        const urls = [
            '/api/pwa/config', '/api/push/subscribe', '/api/push/test',
            '/api/whatever', '/api/echo/send',
        ];
        for (const u of urls) {
            const evt = makeFetchEvent({ url: 'https://app.test' + u });
            sw.handlers.fetch(evt);
            assert('/api/ ' + u + ' → respondWith НЕ вызван', evt._responded === undefined);
        }
        eq('fetch не дёрнут из хендлера', sw.fetch._calls.length, 0);
    });

    await group('fetch: /healthz и под-пути НЕ кэшируются', () => {
        const sw = loadSW();
        for (const u of ['/healthz', '/healthz/db']) {
            const evt = makeFetchEvent({ url: 'https://app.test' + u });
            sw.handlers.fetch(evt);
            assert(u + ' → respondWith НЕ вызван', evt._responded === undefined);
        }
    });

    await group('fetch: приоритет allowlist раньше общего /api/', async () => {
        const sw = loadSW();
        const key = 'https://app.test/api/sessions';
        const netRes = new FakeResponse('[]', { ok: true });
        sw.fetch._setRouter(() => netRes);
        const evt = makeFetchEvent({ url: key });
        sw.handlers.fetch(evt);
        // если бы попало в no-cache /api/ ветку — respondWith не был бы вызван
        assert('обработан как allowlist (respondWith вызван)', evt._responded !== undefined);
        await evt._responded;
        await flush();
        assert('закэширован в DATA_CACHE', !!sw.caches._get(DATA_CACHE, key));
    });

    await group('fetch: SWR идемпотентность при повторных GET', async () => {
        const sw = loadSW();
        const key = 'https://app.test/js/main.js';
        const netRes = new FakeResponse('v1', { ok: true, type: 'basic' });
        sw.fetch._setRouter(() => netRes);
        // первый GET — нет кэша, ждём сеть, кэшируем
        const e1 = makeFetchEvent({ url: key });
        sw.handlers.fetch(e1);
        await e1._responded;
        await flush();
        // второй GET — теперь из кэша мгновенно
        const e2 = makeFetchEvent({ url: key });
        sw.handlers.fetch(e2);
        const out2 = await e2._responded;
        eq('второй раз из кэша (тот же _id)', out2._id, netRes._id);
        // один ключ на request
        const m = sw.caches._store.get(RUNTIME_CACHE);
        eq('кэш не растёт дубликатами', m.size, 1);
    });

    // ───────────────────────────── install ──────────────────────────────
    await group('install: precache app-shell через addAll', async () => {
        const sw = loadSW();
        const reqLog = [];
        sw.fetch._setRouter((url, req) => {
            reqLog.push({ url, cache: req && req.cache });
            return new FakeResponse('asset', { ok: true });
        });
        const evt = makeLifecycleEvent();
        sw.handlers.install(evt);
        assert('waitUntil вызван', evt._waited !== undefined);
        await evt._waited;
        // caches.open(SHELL_CACHE) вызван
        assert('caches.open(SHELL_CACHE) вызван', sw.caches._log.opens.includes(SHELL_CACHE));
        // 13 URL из SHELL_ASSETS
        eq('addAll дёрнул fetch на 13 ресурсов', reqLog.length, 13);
        // все с init {cache:'reload'}
        assert('все Request с cache=reload', reqLog.every((r) => r.cache === 'reload'));
        // ключевые URL присутствуют
        const urls = reqLog.map((r) => r.url);
        for (const u of ['/', '/style.css', '/js/main.js', '/manifest.webmanifest']) {
            assert('precache содержит ' + u, urls.includes(u));
        }
        // skipWaiting НЕ вызван
        eq('skipWaiting НЕ вызван', sw.counters.skipWaiting, 0);
    });

    await group('install: атомарен — сбой одного ресурса → install падает', async () => {
        const sw = loadSW();
        sw.fetch._setRouter((url) => {
            if (url === '/js/main.js') return Promise.reject(new Error('asset-fail'));
            return new FakeResponse('asset', { ok: true });
        });
        const evt = makeLifecycleEvent();
        sw.handlers.install(evt);
        let threw = false;
        try { await evt._waited; } catch (e) { threw = true; }
        assert('waitUntil-promise реджектится (SW не активируется)', threw);
    });

    // ───────────────────────────── activate ─────────────────────────────
    await group('activate: удаляет только forge-* кэши не текущей версии', async () => {
        const sw = loadSW();
        // текущие три + старые forge-*
        sw.caches._seed(SHELL_CACHE, 'k', new FakeResponse('', {}));
        sw.caches._seed(RUNTIME_CACHE, 'k', new FakeResponse('', {}));
        sw.caches._seed(DATA_CACHE, 'k', new FakeResponse('', {}));
        sw.caches._seed('forge-shell-forge-pwa-v0', 'k', new FakeResponse('', {}));
        sw.caches._seed('forge-runtime-old', 'k', new FakeResponse('', {}));
        const evt = makeLifecycleEvent();
        sw.handlers.activate(evt);
        await evt._waited;
        const dels = sw.caches._log.deletes.sort();
        eq('удалены только старые forge-*', dels, ['forge-runtime-old', 'forge-shell-forge-pwa-v0']);
        assert('текущий SHELL не удалён', !dels.includes(SHELL_CACHE));
        assert('текущий RUNTIME не удалён', !dels.includes(RUNTIME_CACHE));
        assert('текущий DATA не удалён', !dels.includes(DATA_CACHE));
    });

    await group('activate: чужие (не forge-) кэши не трогаются', async () => {
        const sw = loadSW();
        sw.caches._seed('workbox-precache', 'k', new FakeResponse('', {}));
        sw.caches._seed('some-other-cache', 'k', new FakeResponse('', {}));
        sw.caches._seed('forge-shell-forge-pwa-v0', 'k', new FakeResponse('', {}));
        const evt = makeLifecycleEvent();
        sw.handlers.activate(evt);
        await evt._waited;
        eq('удалён только forge-shell-forge-pwa-v0', sw.caches._log.deletes, ['forge-shell-forge-pwa-v0']);
    });

    await group('activate: пустой список кэшей — без ошибок + claim', async () => {
        const sw = loadSW();
        const evt = makeLifecycleEvent();
        sw.handlers.activate(evt);
        await evt._waited;
        eq('ничего не удалено', sw.caches._log.deletes, []);
        eq('clients.claim вызван', sw.clients._rec.claimCount, 1);
    });

    await group('activate: всегда вызывает clients.claim в конце', async () => {
        const sw = loadSW();
        sw.caches._seed('forge-old', 'k', new FakeResponse('', {}));
        const evt = makeLifecycleEvent();
        sw.handlers.activate(evt);
        await evt._waited;
        eq('claim вызван ровно один раз', sw.clients._rec.claimCount, 1);
    });

    // ───────────────────────────── message ──────────────────────────────
    await group('message: SKIP_WAITING → skipWaiting', () => {
        const sw = loadSW();
        sw.handlers.message({ data: { type: 'SKIP_WAITING' } });
        eq('skipWaiting вызван один раз', sw.counters.skipWaiting, 1);
    });

    await group('message: другой type игнорируется', () => {
        const sw = loadSW();
        sw.handlers.message({ data: { type: 'PING' } });
        sw.handlers.message({ data: { type: 'skip_waiting' } }); // неверный регистр
        eq('skipWaiting НЕ вызван', sw.counters.skipWaiting, 0);
    });

    await group('message: без data / null не падает', () => {
        const sw = loadSW();
        let threw = false;
        try {
            sw.handlers.message({});
            sw.handlers.message({ data: null });
            sw.handlers.message({ data: undefined });
        } catch (e) { threw = true; }
        assert('не бросает исключение', !threw);
        eq('skipWaiting НЕ вызван', sw.counters.skipWaiting, 0);
    });

    // ───────────────────────────── push ─────────────────────────────────
    await group('push: валидный JSON-payload → showNotification с полями', async () => {
        const sw = loadSW();
        const evt = {
            data: { json: () => ({ title: 'T', body: 'B', url: '/x', tag: 'mytag' }) },
            _waited: undefined,
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.push(evt);
        await evt._waited;
        eq('одно уведомление', sw.recordedNotifications.length, 1);
        const n = sw.recordedNotifications[0];
        eq('title', n.title, 'T');
        eq('body', n.options.body, 'B');
        eq('tag', n.options.tag, 'mytag');
        eq('data.url', n.options.data.url, '/x');
        eq('icon', n.options.icon, '/icons/icon-192.png');
        eq('badge', n.options.badge, '/icons/badge-72.png');
        eq('vibrate', n.options.vibrate, [80, 40, 80]);
        eq('renotify', n.options.renotify, true);
    });

    await group('push: невалидный JSON → fallback на text()', async () => {
        const sw = loadSW();
        const evt = {
            data: { json: () => { throw new Error('bad'); }, text: () => 'plain message' },
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.push(evt);
        await evt._waited;
        const n = sw.recordedNotifications[0];
        eq('дефолтный title', n.title, 'F.O.R.G.E.');
        eq('body из text()', n.options.body, 'plain message');
        eq('дефолтный tag', n.options.tag, 'forge-attention');
        eq('дефолтный url', n.options.data.url, '/');
    });

    await group('push: без data → дефолтное уведомление', async () => {
        const sw = loadSW();
        const evt = { data: null, waitUntil(p) { this._waited = p; return p; } };
        sw.handlers.push(evt);
        await evt._waited;
        const n = sw.recordedNotifications[0];
        eq('title дефолт', n.title, 'F.O.R.G.E.');
        eq('body пустой', n.options.body, '');
        eq('tag дефолт', n.options.tag, 'forge-attention');
        eq('url дефолт', n.options.data.url, '/');
    });

    await group('push: частичный payload подставляет дефолты', async () => {
        const sw = loadSW();
        const evt = {
            data: { json: () => ({ body: 'only body' }) },
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.push(evt);
        await evt._waited;
        const n = sw.recordedNotifications[0];
        eq('title дефолт', n.title, 'F.O.R.G.E.');
        eq('tag дефолт', n.options.tag, 'forge-attention');
        eq('url дефолт', n.options.data.url, '/');
        eq('body передан', n.options.body, 'only body');
    });

    // ─────────────────────── notificationclick ──────────────────────────
    await group('notificationclick: фокус существующего окна с тем же URL', async () => {
        const sw = loadSW();
        const closed = { v: false };
        const client = makeClient('https://app.test/');
        sw.clients._clientList = [client];
        const evt = {
            notification: { data: { url: '/' }, close() { closed.v = true; } },
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.notificationclick(evt);
        await evt._waited;
        assert('notification.close() вызван', closed.v);
        eq('focus вызван', client._rec.focused, 1);
        eq('navigate НЕ вызван', client._rec.navigated, null);
        eq('openWindow НЕ вызван', sw.clients._rec.openWindow.length, 0);
    });

    await group('notificationclick: фокус + навигация при ином URL', async () => {
        const sw = loadSW();
        const closed = { v: false };
        const client = makeClient('https://app.test/old');
        sw.clients._clientList = [client];
        const evt = {
            notification: { data: { url: '/new' }, close() { closed.v = true; } },
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.notificationclick(evt);
        await evt._waited;
        assert('close() вызван', closed.v);
        eq('focus вызван', client._rec.focused, 1);
        eq('navigate на абсолютный URL', client._rec.navigated, 'https://app.test/new');
    });

    await group('notificationclick: нет окон → openWindow', async () => {
        const sw = loadSW();
        const closed = { v: false };
        sw.clients._clientList = [];
        const evt = {
            notification: { data: { url: '/x' }, close() { closed.v = true; } },
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.notificationclick(evt);
        await evt._waited;
        assert('close() вызван', closed.v);
        eq('openWindow с абсолютным URL', sw.clients._rec.openWindow, ['https://app.test/x']);
    });

    await group('notificationclick: нет data.url → дефолт /', async () => {
        const sw = loadSW();
        sw.clients._clientList = [];
        const evt = {
            notification: { data: {}, close() {} },
            waitUntil(p) { this._waited = p; return p; },
        };
        sw.handlers.notificationclick(evt);
        await evt._waited;
        eq('openWindow с origin + /', sw.clients._rec.openWindow, ['https://app.test/']);
    });

    await group('notificationclick: client без navigate но с focus', async () => {
        const sw = loadSW();
        // url отличается от target, но navigate нет → падаем на client.focus()
        const client = makeClient('https://app.test/old', { navigate: false });
        sw.clients._clientList = [client];
        const evt = {
            notification: { data: { url: '/new' }, close() {} },
            waitUntil(p) { this._waited = p; return p; },
        };
        let threw = false;
        try {
            sw.handlers.notificationclick(evt);
            await evt._waited;
        } catch (e) { threw = true; }
        assert('не падает', !threw);
        eq('focus вызван', client._rec.focused, 1);
        assert('navigate отсутствует', client.navigate === undefined);
    });

    await group('notificationclick: openWindow недоступен → не падает', async () => {
        const sw = loadSW();
        sw.clients._clientList = [];
        sw.clients._hasOpenWindow = false; // openWindow → undefined
        const evt = {
            notification: { data: { url: '/x' }, close() {} },
            waitUntil(p) { this._waited = p; return p; },
        };
        let threw = false;
        let res;
        try {
            sw.handlers.notificationclick(evt);
            res = await evt._waited;
        } catch (e) { threw = true; }
        assert('не бросает исключение', !threw);
        eq('возвращает undefined', res, undefined);
    });

    // =========================================================================
    // Summary
    // =========================================================================
    console.log('\n=================================');
    console.log('  passed: ' + passed);
    console.log('  failed: ' + failed);
    console.log('=================================');

    if (failed > 0) {
        console.log('\nFailures:');
        for (const f of failures) {
            console.log('  - ' + f.label + (f.details ? ': ' + f.details : ''));
        }
        process.exit(1);
    }
    process.exit(0);
}

run().catch((err) => {
    console.error('Unexpected test harness error:', err);
    process.exit(1);
});
