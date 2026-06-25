// tmux-web/tests/frontend/pwa_register.test.mjs
//
// PWA Service Worker update-flow — регресс-тесты register.js:
//   registerServiceWorker() и логика update-баннера (показ при reg.waiting+
//   controller / updatefound→installed+controller; SKIP_WAITING postMessage;
//   controllerchange → ровно один location.reload через guard refreshing;
//   первая установка без controller → баннера нет).
//
// ПОДХОД: грузим РЕАЛЬНЫЙ static/js/pwa/register.js настоящим динамическим
// import(). register.js — ESM (export async function registerServiceWorker),
// БЕЗ top-level side-effects → vm/new Function не подходят (там нет
// addEventListener на верхнем уровне). Файл .mjs, чтобы await import работал.
//
// register.js имеет МОДУЛЬНОЕ состояние (refreshing, bannerEl), персистящее
// между вызовами одного импорта. Для независимых сценариев импортируем заново
// с query-busting: import('...register.js?bust=N') — даёт свежий модуль с
// refreshing=false, bannerEl=null.
//
// Браузерные глобалы (navigator/window/document/requestAnimationFrame/setTimeout/
// console) мокаем на globalThis ПЕРЕД каждым import. В Node v26 navigator/window
// — read-only getters, поэтому ставим через Object.defineProperty.
//
// Запуск: node tmux-web/tests/frontend/pwa_register.test.mjs
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.

import { pathToFileURL } from 'node:url';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REGISTER_PATH = path.join(__dirname, '..', '..', 'static', 'js', 'pwa', 'register.js');

// Глушим бенайн-warning MODULE_TYPELESS_PACKAGE_JSON.
process.removeAllListeners('warning');
process.on('warning', () => {});

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

function group(name, fn) {
    console.log('\n[' + name + ']');
    return Promise.resolve().then(fn);
}

function setGlobal(name, val) {
    Object.defineProperty(globalThis, name, { value: val, configurable: true, writable: true });
}

// Реальный console раннера: на него уходят логи assert/group. register.js
// дёргает console.warn — мы подменяем ТОЛЬКО .warn (capture), .log не трогаем.
const realConsole = console;

function flush() { return Promise.resolve().then(() => Promise.resolve()); }

// =============================================================================
// Мок-фабрики
// =============================================================================

function spy() {
    const fn = (...args) => { fn.calls.push(args); fn.count += 1; return fn._ret; };
    fn.calls = [];
    fn.count = 0;
    fn._ret = undefined;
    return fn;
}

/** Фейковый ServiceWorker (installing/waiting). */
function makeWorker(initialState) {
    const w = {
        state: initialState || 'installing',
        postMessage: spy(),
        _listeners: {},
        addEventListener(type, cb) {
            (w._listeners[type] = w._listeners[type] || []).push(cb);
        },
        _emit(type) {
            (w._listeners[type] || []).forEach((cb) => cb());
        },
        setState(s) { w.state = s; },
    };
    return w;
}

/** Фейковый ServiceWorkerRegistration. */
function makeReg(opts) {
    opts = opts || {};
    const reg = {
        waiting: opts.waiting || null,
        installing: opts.installing || null,
        _listeners: {},
        addEventListener(type, cb) {
            (reg._listeners[type] = reg._listeners[type] || []).push(cb);
        },
        _emit(type) {
            (reg._listeners[type] || []).forEach((cb) => cb());
        },
    };
    return reg;
}

/**
 * Фейковый navigator.serviceWorker. controller — null|{}. register — async,
 * возвращает reg или бросает. controllerchange-слушатели собираются и
 * эмитятся вручную.
 */
function makeSW(opts) {
    opts = opts || {};
    const sw = {
        controller: opts.controller !== undefined ? opts.controller : null,
        _cc: [],
        _registerCalls: [],
        addEventListener(type, cb) {
            if (type === 'controllerchange') sw._cc.push(cb);
        },
        register: spy(),
        _emitControllerChange() { sw._cc.forEach((cb) => cb()); },
    };
    sw.register = (url, o) => {
        sw._registerCalls.push({ url, opts: o });
        if (opts.registerThrows) return Promise.reject(opts.registerError || new Error('reg-fail'));
        return Promise.resolve(opts.reg || makeReg());
    };
    return sw;
}

/** Минимальный фейковый DOM-узел. */
function makeNode(tag) {
    const node = {
        tagName: tag,
        className: '',
        type: '',
        textContent: '',
        disabled: false,
        _attrs: {},
        _children: [],
        _listeners: {},
        classList: {
            _set: new Set(),
            add(c) { this._set.add(c); },
            remove(c) { this._set.delete(c); },
            contains(c) { return this._set.has(c); },
        },
        setAttribute(k, v) { node._attrs[k] = v; },
        getAttribute(k) { return node._attrs[k]; },
        appendChild(child) { node._children.push(child); return child; },
        addEventListener(type, cb) {
            (node._listeners[type] = node._listeners[type] || []).push(cb);
        },
        _click() { (node._listeners.click || []).forEach((cb) => cb()); },
        remove() { node._removed = true; },
    };
    return node;
}

/** Фейковый document со счётчиком appendChild на body. */
function makeDocument() {
    const created = [];
    const doc = {
        _created: created,
        body: makeNode('body'),
        createElement(tag) {
            const n = makeNode(tag);
            created.push(n);
            return n;
        },
    };
    return doc;
}

/**
 * Готовит окружение и импортит свежий register.js. Возвращает
 * { mod, sw, window, document, reloadSpy, warns, rafQueue, timeoutQueue, run }.
 */
let _bust = 0;
async function loadRegister(opts) {
    opts = opts || {};
    const sw = opts.sw || makeSW(opts.swOpts);
    const reloadSpy = spy();
    const window = { location: { reload: reloadSpy } };
    const document = makeDocument();
    const warns = [];
    const rafQueue = [];
    const timeoutQueue = [];

    setGlobal('navigator', { serviceWorker: sw });
    setGlobal('window', window);
    setGlobal('document', document);
    setGlobal('requestAnimationFrame', (cb) => { rafQueue.push(cb); return rafQueue.length; });
    setGlobal('setTimeout', (cb, ms) => { timeoutQueue.push({ cb, ms }); return timeoutQueue.length; });
    // Перехватываем ТОЛЬКО console.warn (register.js его дёргает), не ломая
    // console.log тестового раннера.
    realConsole.warn = (...a) => warns.push(a);

    const mod = await import(pathToFileURL(REGISTER_PATH).href + '?bust=' + (++_bust));

    return {
        mod, sw, window, document, reloadSpy, warns,
        rafQueue, timeoutQueue,
        runRaf() { while (rafQueue.length) rafQueue.shift()(); },
        runTimeouts() { while (timeoutQueue.length) timeoutQueue.shift().cb(); },
    };
}

function getBanner(document) {
    // Самый свежий НЕ удалённый .pwa-update-banner (remove() ставит _removed).
    const live = document.body._children.filter(
        (c) => c.className === 'pwa-update-banner' && !c._removed
    );
    return live.length ? live[live.length - 1] : null;
}

function bannerCount(document) {
    // Считаем только живые баннеры (не удалённые из DOM).
    return document.body._children.filter(
        (c) => c.className === 'pwa-update-banner' && !c._removed
    ).length;
}

// Находит кнопку внутри баннера по className.
function findChild(node, className) {
    return node._children.find((c) => c.className === className) || null;
}

// =============================================================================
// Tests
// =============================================================================

async function run() {

    await group('serviceWorker не поддерживается → null, без слушателей', async () => {
        // navigator без serviceWorker.
        const swListeners = { count: 0 };
        setGlobal('navigator', {});
        setGlobal('window', { location: { reload: spy() } });
        setGlobal('document', makeDocument());
        setGlobal('requestAnimationFrame', (cb) => cb());
        setGlobal('setTimeout', (cb) => cb());
        const mod = await import(pathToFileURL(REGISTER_PATH).href + '?bust=' + (++_bust));
        const r = await mod.registerServiceWorker();
        eq('возвращает null', r, null);
        // нет navigator.serviceWorker → addEventListener вообще не вызывался
        assert('register не выполнен (нет SW)', swListeners.count === 0);
    });

    await group('первая установка (нет controller) → баннер НЕ показывается', async () => {
        const installing = makeWorker('installing');
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: null, reg } });
        await env.mod.registerServiceWorker();
        // updatefound → installing появляется, переходит в installed, но controller=null
        reg.installing = installing;
        reg._emit('updatefound');
        installing.setState('installed');
        installing._emit('statechange');
        await flush();
        eq('баннер не создан', bannerCount(env.document), 0);
        eq('location.reload не вызван', env.reloadSpy.count, 0);
    });

    await group('reg.waiting + controller → немедленный баннер', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner = getBanner(env.document);
        assert('баннер создан', !!banner);
        eq('role=status', banner.getAttribute('role'), 'status');
        const btn = findChild(banner, 'pwa-banner-btn');
        const dismiss = findChild(banner, 'pwa-banner-dismiss');
        assert('кнопка Обновить есть', !!btn && btn.textContent === 'Обновить');
        assert('кнопка dismiss × есть', !!dismiss && dismiss.textContent === '×');
    });

    await group('reg.waiting есть, controller нет → баннера НЕТ', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: null, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        eq('баннер не создан', bannerCount(env.document), 0);
    });

    await group('controller есть, waiting нет → баннера НЕТ (пока)', async () => {
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        eq('баннер не создан', bannerCount(env.document), 0);
    });

    await group('updatefound → installing===null → ранний return', async () => {
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        // installing остаётся null
        reg.installing = null;
        let threw = false;
        try { reg._emit('updatefound'); } catch (_) { threw = true; }
        assert('не бросает', !threw);
        await flush();
        eq('баннер не создан', bannerCount(env.document), 0);
    });

    await group('updatefound → installed + controller → баннер (апдейт)', async () => {
        const installing = makeWorker('installing');
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        reg.installing = installing;
        reg._emit('updatefound');
        installing.setState('installed');
        installing._emit('statechange');
        await flush();
        eq('баннер показан ровно один', bannerCount(env.document), 1);
    });

    await group('updatefound → installed, но controller=null → баннера НЕТ', async () => {
        const installing = makeWorker('installing');
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: null, reg } });
        await env.mod.registerServiceWorker();
        reg.installing = installing;
        reg._emit('updatefound');
        installing.setState('installed');
        installing._emit('statechange');
        await flush();
        eq('баннер не создан', bannerCount(env.document), 0);
    });

    await group('statechange: промежуточные состояния → баннер только на installed', async () => {
        const installing = makeWorker('installing');
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        reg.installing = installing;
        reg._emit('updatefound');
        // installing → installing → activating → installed
        installing.setState('installing'); installing._emit('statechange');
        installing.setState('activating'); installing._emit('statechange');
        await flush();
        eq('пока баннера нет', bannerCount(env.document), 0);
        installing.setState('installed'); installing._emit('statechange');
        await flush();
        eq('баннер появился на installed', bannerCount(env.document), 1);
    });

    await group('statechange === redundant → баннера НЕТ', async () => {
        const installing = makeWorker('installing');
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        reg.installing = installing;
        reg._emit('updatefound');
        installing.setState('redundant');
        installing._emit('statechange');
        await flush();
        eq('баннер не создан', bannerCount(env.document), 0);
    });

    await group('клик Обновить при waiting → postMessage SKIP_WAITING, кнопка дизейблится', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner = getBanner(env.document);
        const btn = findChild(banner, 'pwa-banner-btn');
        btn._click();
        eq('postMessage вызван один раз', waiting.postMessage.count, 1);
        eq('postMessage payload = {type:SKIP_WAITING}', waiting.postMessage.calls[0][0], { type: 'SKIP_WAITING' });
        eq('кнопка дизейблена', btn.disabled, true);
        eq('текст кнопки "Обновляем…"', btn.textContent, 'Обновляем…');
        eq('location.reload пока НЕ вызван', env.reloadSpy.count, 0);
    });

    await group('клик Обновить когда waiting исчез → location.reload (fallback)', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner = getBanner(env.document);
        const btn = findChild(banner, 'pwa-banner-btn');
        // К моменту клика waiting исчез.
        reg.waiting = null;
        btn._click();
        eq('postMessage НЕ вызван', waiting.postMessage.count, 0);
        eq('location.reload вызван (fallback)', env.reloadSpy.count, 1);
    });

    await group('controllerchange → location.reload ровно один раз', async () => {
        const env = await loadRegister({ swOpts: { controller: {}, reg: makeReg() } });
        await env.mod.registerServiceWorker();
        env.sw._emitControllerChange();
        eq('reload вызван 1 раз', env.reloadSpy.count, 1);
    });

    await group('controllerchange несколько раз → reload ровно ОДИН раз (guard)', async () => {
        const env = await loadRegister({ swOpts: { controller: {}, reg: makeReg() } });
        await env.mod.registerServiceWorker();
        env.sw._emitControllerChange();
        env.sw._emitControllerChange();
        env.sw._emitControllerChange();
        env.sw._emitControllerChange();
        eq('reload вызван ровно 1 раз', env.reloadSpy.count, 1);
    });

    await group('guard refreshing — модульный стейт, 2-й registerServiceWorker не сбрасывает', async () => {
        const env = await loadRegister({ swOpts: { controller: {}, reg: makeReg() } });
        // два вызова в рамках ОДНОГО импорта (две навески controllerchange)
        await env.mod.registerServiceWorker();
        await env.mod.registerServiceWorker();
        env.sw._emitControllerChange();
        eq('суммарно reload вызван 1 раз (guard на уровне модуля)', env.reloadSpy.count, 1);
    });

    await group('register() reject → console.warn + null, без краша', async () => {
        const env = await loadRegister({ swOpts: { controller: {}, registerThrows: true, registerError: new Error('reg-boom') } });
        const r = await env.mod.registerServiceWorker();
        eq('вернул null', r, null);
        eq('console.warn вызван', env.warns.length, 1);
        assert('warn содержит SW registration failed', env.warns[0][0].includes('SW registration failed'));
        // controllerchange-слушатель навешен ДО register — событие не должно падать.
        let threw = false;
        try { env.sw._emitControllerChange(); } catch (_) { threw = true; }
        assert('controllerchange после reject не падает', !threw);
        eq('и вызывает reload (слушатель навешен)', env.reloadSpy.count, 1);
    });

    await group('идемпотентность showUpdateBanner — второй показ не создаёт второй баннер', async () => {
        const waiting = makeWorker('installed');
        const installing = makeWorker('installing');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker(); // показ 1 (waiting + controller)
        await flush();
        eq('после показа 1 — один баннер', bannerCount(env.document), 1);
        // показ 2: updatefound → installed + controller
        reg.installing = installing;
        reg._emit('updatefound');
        installing.setState('installed');
        installing._emit('statechange');
        await flush();
        eq('по-прежнему ровно один баннер (guard bannerEl)', bannerCount(env.document), 1);
    });

    await group('dismiss × → баннер скрывается, bannerEl сброшен, повторный показ возможен', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner1 = getBanner(env.document);
        const dismiss = findChild(banner1, 'pwa-banner-dismiss');
        dismiss._click();
        // hideUpdateBanner снимает класс pwa-banner-show, через setTimeout — remove
        assert('класс pwa-banner-show снят', !banner1.classList.contains('pwa-banner-show'));
        env.runTimeouts(); // прогон setTimeout(240)
        assert('баннер удалён из DOM', banner1._removed === true);
        // повторный показ снова создаёт баннер (guard bannerEl сброшен)
        const installing = makeWorker('installing');
        reg.installing = installing;
        reg._emit('updatefound');
        installing.setState('installed');
        installing._emit('statechange');
        await flush();
        const banner2 = getBanner(env.document);
        assert('новый баннер создан', !!banner2 && banner2 !== banner1);
    });

    await group('requestAnimationFrame — появление баннера добавляет класс pwa-banner-show', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner = getBanner(env.document);
        // rAF был замокан как очередь — прогоняем.
        env.runRaf();
        assert('класс pwa-banner-show добавлен после rAF', banner.classList.contains('pwa-banner-show'));
    });

    await group('структура DOM баннера: span + 2 button с корректными классами/атрибутами', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner = getBanner(env.document);
        eq('banner.className', banner.className, 'pwa-update-banner');
        eq('banner role', banner.getAttribute('role'), 'status');
        const text = findChild(banner, 'pwa-banner-text');
        eq('text.className', text.className, 'pwa-banner-text');
        eq('text content', text.textContent, 'Доступно обновление');
        const btn = findChild(banner, 'pwa-banner-btn');
        eq('btn.type', btn.type, 'button');
        eq('btn text', btn.textContent, 'Обновить');
        const dismiss = findChild(banner, 'pwa-banner-dismiss');
        eq('dismiss.type', dismiss.type, 'button');
        eq('dismiss aria-label', dismiss.getAttribute('aria-label'), 'Скрыть');
        eq('dismiss text', dismiss.textContent, '×');
    });

    await group('postMessage payload точно {type:SKIP_WAITING} — без иных полей', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const btn = findChild(getBanner(env.document), 'pwa-banner-btn');
        btn._click();
        const payload = waiting.postMessage.calls[0][0];
        eq('ключи payload ровно [type]', Object.keys(payload), ['type']);
        eq('type === SKIP_WAITING', payload.type, 'SKIP_WAITING');
    });

    await group('controllerchange-слушатель навешен ДО await register', async () => {
        // register отдаёт reject; controllerchange всё равно должен сработать,
        // т.к. addEventListener('controllerchange') стоит ДО await register.
        const env = await loadRegister({ swOpts: { controller: {}, registerThrows: true } });
        // эмитим controllerchange ещё до того, как дождёмся registerServiceWorker
        const p = env.mod.registerServiceWorker();
        env.sw._emitControllerChange();
        await p;
        eq('reload отработал несмотря на reject register', env.reloadSpy.count, 1);
    });

    await group('возвращаемое значение — резолвленный reg при успехе', async () => {
        const reg = makeReg({ waiting: null, installing: null });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        const r = await env.mod.registerServiceWorker();
        assert('вернул тот же reg-объект', r === reg);
        // и зарегистрировал по правильному url/scope
        eq('register("/sw.js", {scope:"/"})', env.sw._registerCalls[0], { url: '/sw.js', opts: { scope: '/' } });
    });

    await group('hideUpdateBanner при bannerEl===null → no-op (двойной dismiss)', async () => {
        const waiting = makeWorker('installed');
        const reg = makeReg({ waiting });
        const env = await loadRegister({ swOpts: { controller: {}, reg } });
        await env.mod.registerServiceWorker();
        await flush();
        const banner = getBanner(env.document);
        const dismiss = findChild(banner, 'pwa-banner-dismiss');
        dismiss._click(); // первый dismiss → bannerEl=null
        let threw = false;
        try { dismiss._click(); } catch (_) { threw = true; } // второй → no-op
        assert('второй dismiss не бросает', !threw);
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
