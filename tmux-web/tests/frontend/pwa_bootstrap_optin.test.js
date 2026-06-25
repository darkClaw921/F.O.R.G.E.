// tmux-web/tests/frontend/pwa_bootstrap_optin.test.js
//
// Тесты opt-in-гейта PWA — bootstrap.js (единственный всегда-загружаемый
// PWA-файл). Закрывает пробел, оставшийся после основного прогона генерации
// тестов (recon-агент по домену bootstrap упал на StructuredOutput).
//
// Что покрываем (КРИТИЧЕСКИЙ инвариант проекта «фичи opt-in»):
//   • config 404 / non-ok      → disablePwa: unregister всех SW + удаление
//                                 forge-* кэшей, БЕЗ инжекта в <head>;
//   • config enabled:false     → то же (opt-out при перезапуске без --pwa);
//   • сетевая ошибка fetch      → то же (безопасный дефолт, без throw);
//   • serviceWorker отсутствует → disablePwa не падает;
//   • Cache API отсутствует     → disablePwa не падает;
//   • config enabled:true       → window.__FORGE_PWA выставлен с vapidPublicKey,
//                                 injectHead создаёт manifest/theme-color/apple-*/
//                                 apple-touch-icon/pwa.css, register.js импортируется
//                                 и registerServiceWorker() вызывается;
//   • disablePwa чистит ТОЛЬКО forge-* кэши (чужие не трогает);
//   • injectHead идемпотентен (повторный запуск не плодит дубликаты).
//
// Запуск: node tmux-web/tests/frontend/pwa_bootstrap_optin.test.js
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.
//
// Подход: грузим РЕАЛЬНЫЙ static/js/pwa/bootstrap.js в node:vm-песочнице
// (как sw.test.js), заменив синтаксис `import(` на мок-функцию `__dynImport(`
// (статических import/export в файле нет — он валиден как classic script).

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// ─────────────────────────── мини-раннер ──────────────────────────────
let passed = 0;
let failed = 0;
const failures = [];

function assert(cond, msg) {
    if (cond) { passed++; } else { failed++; failures.push(msg); }
}
function eq(actual, expected, msg) {
    assert(actual === expected, `${msg} — ожидалось ${JSON.stringify(expected)}, получено ${JSON.stringify(actual)}`);
}
async function group(name, fn) {
    try {
        await fn();
    } catch (err) {
        failed++;
        failures.push(`[${name}] исключение: ${err && err.stack ? err.stack : err}`);
    }
}

// ───────────────── загрузка и трансформация исходника ──────────────────
const BOOTSTRAP_PATH = path.join(__dirname, '..', '..', 'static', 'js', 'pwa', 'bootstrap.js');
const RAW = fs.readFileSync(BOOTSTRAP_PATH, 'utf8');
// Заменяем динамический import() на мок __dynImport(). Статических import/export
// в bootstrap.js нет, поэтому замена затрагивает только dynamic-import (и пару
// упоминаний в комментариях — безвредно).
const SRC = RAW.replace(/\bimport\(/g, '__dynImport(');
const SCRIPT = new vm.Script(SRC, { filename: 'bootstrap.js' });

const flush = () => new Promise((resolve) => setTimeout(resolve, 15));

// ─────────────────────────── DOM/head мок ─────────────────────────────
function makeHead() {
    const children = [];
    function matches(el, sel) {
        let m;
        if ((m = sel.match(/^link\[rel="([^"]+)"\]$/))) return el.tag === 'link' && el.rel === m[1];
        if ((m = sel.match(/^meta\[name="([^"]+)"\]$/))) return el.tag === 'meta' && el.name === m[1];
        if (sel === 'link[data-forge-pwa-css]') return el.tag === 'link' && el.attrs['data-forge-pwa-css'] !== undefined;
        return false;
    }
    return {
        children,
        appendChild(el) { children.push(el); return el; },
        querySelector(sel) { return children.find((el) => matches(el, sel)) || null; },
    };
}
function makeDocument() {
    const head = makeHead();
    return {
        head,
        createElement(tag) {
            return { tag, attrs: {}, setAttribute(k, v) { this.attrs[k] = v; } };
        },
    };
}

// ─────────────────────────── сборка песочницы ─────────────────────────
function makeSandbox(opts) {
    const o = opts || {};
    const record = {
        unregisterCount: 0,
        cachesDeleted: [],
        dynImports: [],
        registerSWCalled: 0,
        document: makeDocument(),
        window: {},
        warns: [],
    };

    // navigator с/без serviceWorker
    let navigator;
    if (o.noServiceWorker) {
        navigator = {};
    } else {
        const regs = (o.registrations !== undefined)
            ? o.registrations
            : [{ unregister: async () => { record.unregisterCount++; return true; } }];
        navigator = {
            serviceWorker: { getRegistrations: async () => regs },
        };
    }

    // caches (self.caches + глобальный caches)
    let caches;
    if (o.noCaches) {
        caches = undefined;
    } else {
        const keys = (o.cacheKeys !== undefined) ? o.cacheKeys : [];
        caches = {
            keys: async () => keys,
            delete: async (k) => { record.cachesDeleted.push(k); return true; },
        };
    }

    // fetch
    const fetch = async () => {
        if (o.fetchThrows) throw new Error('network down');
        return {
            ok: o.resOk !== undefined ? o.resOk : true,
            json: async () => o.cfg,
        };
    };

    // __dynImport — мок динамического import()
    const __dynImport = async (spec) => {
        record.dynImports.push(spec);
        if (spec.endsWith('register.js')) {
            return { registerServiceWorker: async () => { record.registerSWCalled++; } };
        }
        if (spec.endsWith('install.js')) return { initInstall() {} };
        if (spec.endsWith('push.js')) return { initPush() {} };
        if (spec.endsWith('mobile.js')) return { initMobile() {} };
        return {};
    };

    const sandbox = {
        fetch,
        navigator,
        caches,
        self: { caches },
        window: record.window,
        document: record.document,
        location: { search: o.search || '' },
        console: {
            warn: (...a) => record.warns.push(a.join(' ')),
            log() {}, error() {},
        },
        URLSearchParams,
        Event,
        setTimeout,
        clearTimeout,
        Date,
        Promise,
        __dynImport,
    };
    sandbox.globalThis = sandbox;
    vm.createContext(sandbox);
    record.sandbox = sandbox;
    return record;
}

async function run(opts) {
    const rec = makeSandbox(opts);
    SCRIPT.runInContext(rec.sandbox);
    await flush();
    return rec;
}

// ─────────────────────────────── тесты ────────────────────────────────
(async function main() {
    // 1) config 404 (res.ok=false) → opt-out
    await group('config-404-opt-out', async () => {
        const r = await run({ resOk: false, cfg: null });
        assert(r.unregisterCount === 1, '404: должен unregister существующий SW');
        eq(r.window.__FORGE_PWA, undefined, '404: window.__FORGE_PWA не выставляется');
        eq(r.document.head.children.length, 0, '404: <head> не трогается (нет инжекта)');
        eq(r.registerSWCalled, 0, '404: registerServiceWorker НЕ вызывается');
    });

    // 2) enabled:false → opt-out
    await group('enabled-false-opt-out', async () => {
        const r = await run({ resOk: true, cfg: { enabled: false } });
        assert(r.unregisterCount === 1, 'enabled:false: unregister SW');
        eq(r.window.__FORGE_PWA, undefined, 'enabled:false: __FORGE_PWA не выставляется');
        eq(r.document.head.children.length, 0, 'enabled:false: нет инжекта в head');
    });

    // 3) сетевая ошибка fetch → opt-out, без throw
    await group('fetch-throws-opt-out', async () => {
        const r = await run({ fetchThrows: true });
        assert(r.unregisterCount === 1, 'fetch throw: всё равно unregister SW (безопасный дефолт)');
        eq(r.document.head.children.length, 0, 'fetch throw: head не трогается');
    });

    // 4) enabled:true но enabled-значение не строго true (truthy число) → opt-out
    await group('enabled-truthy-not-true-opt-out', async () => {
        const r = await run({ resOk: true, cfg: { enabled: 1 } });
        eq(r.window.__FORGE_PWA, undefined, 'enabled:1 (не ===true): остаётся выключенным');
        eq(r.document.head.children.length, 0, 'enabled:1: нет инжекта');
    });

    // 5) serviceWorker отсутствует в navigator → disablePwa не падает
    await group('no-serviceworker-no-throw', async () => {
        const r = await run({ resOk: false, cfg: null, noServiceWorker: true });
        eq(r.unregisterCount, 0, 'нет SW API: ничего не unregister');
        eq(r.window.__FORGE_PWA, undefined, 'нет SW API: остаётся выключенным, без исключения');
    });

    // 6) Cache API отсутствует → disablePwa не падает
    await group('no-cache-api-no-throw', async () => {
        const r = await run({ resOk: false, cfg: null, noCaches: true });
        assert(r.unregisterCount === 1, 'нет Cache API: SW всё равно снят');
        eq(r.cachesDeleted.length, 0, 'нет Cache API: нечего удалять, без исключения');
    });

    // 7) disablePwa чистит ТОЛЬКО forge-* кэши
    await group('opt-out-deletes-only-forge-caches', async () => {
        const r = await run({
            resOk: false,
            cfg: null,
            cacheKeys: ['forge-shell-v1', 'forge-data-v1', 'other-app-cache', 'workbox-precache'],
        });
        assert(r.cachesDeleted.includes('forge-shell-v1'), 'удалён forge-shell-v1');
        assert(r.cachesDeleted.includes('forge-data-v1'), 'удалён forge-data-v1');
        assert(!r.cachesDeleted.includes('other-app-cache'), 'чужой other-app-cache НЕ удалён');
        assert(!r.cachesDeleted.includes('workbox-precache'), 'чужой workbox-precache НЕ удалён');
        eq(r.cachesDeleted.length, 2, 'удалено ровно 2 forge-* кэша');
    });

    // 8) enabled:true → инжект + register
    await group('enabled-true-injects-and-registers', async () => {
        const r = await run({ resOk: true, cfg: { enabled: true, vapidPublicKey: 'PUBKEY123' }, search: '' });
        const h = r.document.head;
        assert(r.window.__FORGE_PWA && r.window.__FORGE_PWA.enabled === true, 'enabled:true: __FORGE_PWA.enabled=true');
        eq(r.window.__FORGE_PWA.vapidPublicKey, 'PUBKEY123', 'enabled:true: vapidPublicKey проброшен');
        assert(h.querySelector('link[rel="manifest"]'), 'инжектнут <link rel=manifest>');
        const manifest = h.querySelector('link[rel="manifest"]');
        eq(manifest.href, '/manifest.webmanifest', 'manifest href корректен');
        assert(h.querySelector('meta[name="theme-color"]'), 'инжектнут theme-color');
        eq(h.querySelector('meta[name="theme-color"]').content, '#0e1116', 'theme-color = #0e1116');
        assert(h.querySelector('meta[name="apple-mobile-web-app-capable"]'), 'инжектнут apple-mobile-web-app-capable');
        assert(h.querySelector('link[rel="apple-touch-icon"]'), 'инжектнут apple-touch-icon');
        assert(h.querySelector('link[data-forge-pwa-css]'), 'инжектнут <link> на /css/pwa.css');
        assert(r.dynImports.includes('./register.js'), 'импортирован register.js');
        eq(r.registerSWCalled, 1, 'registerServiceWorker() вызван ровно один раз');
    });

    // 9) injectHead идемпотентен — не дублирует уже существующие в <head>
    //    элементы (ensureEl/ensureMeta пропускают по селектору). Модуль грузится
    //    один раз, поэтому проверяем против ПРЕДСУЩЕСТВУЮЩЕЙ разметки, а не через
    //    повторный запуск (повторный const-декларацию JS не допускает).
    await group('injectHead-idempotent-vs-preexisting', async () => {
        const r = makeSandbox({ resOk: true, cfg: { enabled: true, vapidPublicKey: 'K' }, search: '' });
        // Засеваем head существующими элементами (как если бы они уже были в HTML).
        r.document.head.children.push({ tag: 'link', rel: 'manifest', href: '/preexisting.webmanifest', attrs: {} });
        r.document.head.children.push({ tag: 'meta', name: 'theme-color', content: '#000000', attrs: {} });
        SCRIPT.runInContext(r.sandbox);
        await flush();
        const links = r.document.head.children.filter((el) => el.tag === 'link' && el.rel === 'manifest');
        eq(links.length, 1, 'не дублирует существующий <link rel=manifest>');
        eq(links[0].href, '/preexisting.webmanifest', 'существующий manifest не перезаписан');
        const themeMetas = r.document.head.children.filter((el) => el.tag === 'meta' && el.name === 'theme-color');
        eq(themeMetas.length, 1, 'не дублирует существующий theme-color');
        eq(themeMetas[0].content, '#000000', 'существующий theme-color не перезаписан');
    });

    // ─────────────────────────── итог ─────────────────────────────────
    console.log('');
    console.log('pwa_bootstrap_optin.test.js');
    if (failures.length) {
        console.log('FAILURES:');
        for (const f of failures) console.log('  ✗ ' + f);
    }
    console.log(`passed: ${passed}`);
    console.log(`failed: ${failed}`);
    process.exit(failed ? 1 : 0);
})();
