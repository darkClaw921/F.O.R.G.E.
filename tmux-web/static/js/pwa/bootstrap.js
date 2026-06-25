/*
 * bootstrap.js — единственный ВСЕГДА-загружаемый PWA-файл devforge.
 *
 * Подключён статически из index.html (<script type="module" src=
 * "/js/pwa/bootstrap.js">). Грузится всегда (это статика), но сам решает,
 * включать ли PWA — строгий opt-in через /api/pwa/config.
 *
 * Логика:
 *   1) fetch('/api/pwa/config'):
 *        • не-200 / enabled !== true  → PWA ВЫКЛЮЧЕНО:
 *            - снять регистрацию ЛЮБОГО SW (getRegistrations → unregister);
 *            - удалить все кэши с именем, начинающимся на 'forge-';
 *            - выйти, НЕ трогая разметку страницы.
 *          Это гарантирует opt-out при перезапуске сервера БЕЗ флага --pwa.
 *        • enabled === true → PWA ВКЛЮЧЕНО:
 *            - инжект в <head>: <link rel=manifest>, theme-color, apple-*-meta,
 *              <link rel=apple-touch-icon>, <link rel=stylesheet href=/css/pwa.css>;
 *            - сохранить vapidPublicKey в window.__FORGE_PWA (для push.js, Фаза 5);
 *            - import('./register.js') → registerServiceWorker();
 *            - хук для Фазы 5: ленивый импорт install.js/push.js/mobile.js и
 *              разбор location.search (?tab=/?view=/?share-target=).
 *
 * Без enabled НИКАКИХ побочных эффектов на разметку — поведение страницы как
 * без PWA (строгий opt-in, ценой одного кадра задержки theme-color).
 */

'use strict';

const CONFIG_URL = '/api/pwa/config';
const CACHE_PREFIX = 'forge-';

/** Точка входа — самовызывается при загрузке модуля. */
(async function pwaBootstrap() {
    let cfg = null;
    try {
        const res = await fetch(CONFIG_URL, {
            // конфиг не кэшируем — opt-in/opt-out должен срабатывать сразу
            cache: 'no-store',
            credentials: 'same-origin',
        });
        if (res.ok) {
            cfg = await res.json();
        }
    } catch (_) {
        // сеть недоступна / ошибка — считаем PWA выключенным (безопасный дефолт)
        cfg = null;
    }

    const enabled = !!(cfg && cfg.enabled === true);

    if (!enabled) {
        await disablePwa();
        return;
    }

    // ─── PWA включено ───
    window.__FORGE_PWA = {
        enabled: true,
        vapidPublicKey: (cfg && cfg.vapidPublicKey) || null,
    };

    injectHead();

    try {
        const mod = await import('./register.js');
        if (mod && typeof mod.registerServiceWorker === 'function') {
            await mod.registerServiceWorker();
        }
    } catch (err) {
        console.warn('[pwa] register.js load failed:', err);
    }

    // ─── Фаза 5: install / push / mobile + launch-параметры ───
    // Ленивая загрузка модулей PWA-UI. Каждый импорт в своём try/catch, чтобы
    // сбой одного модуля не ломал остальные и базовую PWA-функциональность.
    loadInstall();
    loadPush();
    loadMobile();

    // Разбор shortcuts / share-target из location.search. Выполняется после
    // готовности основного UI (ожидание appReady) — иначе switchTab/
    // showDailySummary вызовутся до инициализации main.js и упадут.
    handleLaunchParams();
})();

// ──────────────────────── Фаза 5: ленивые модули ──────────────────────

/** Лениво грузит install.js (кнопка установки + iOS-подсказка). */
async function loadInstall() {
    try {
        const mod = await import('./install.js');
        if (mod && typeof mod.initInstall === 'function') mod.initInstall();
    } catch (err) {
        console.warn('[pwa] install.js load failed:', err);
    }
}

/** Лениво грузит push.js (тоггл push в Settings + subscribe/unsubscribe). */
async function loadPush() {
    try {
        const mod = await import('./push.js');
        if (mod && typeof mod.initPush === 'function') mod.initPush();
    } catch (err) {
        console.warn('[pwa] push.js load failed:', err);
    }
}

/** Лениво грузит mobile.js (safe-area, клавиатура, overscroll, badge, …). */
async function loadMobile() {
    try {
        const mod = await import('./mobile.js');
        if (mod && typeof mod.initMobile === 'function') mod.initMobile();
    } catch (err) {
        console.warn('[pwa] mobile.js load failed:', err);
    }
}

// ──────────────────── Фаза 5: shortcuts / share-target ────────────────

/**
 * Ждёт готовности основного UI приложения. main.js выставляет
 * window.__FORGE_APP_READY = true и диспатчит событие 'forge:app-ready' по
 * завершении инициализации. Если флаг уже стоит — резолвимся сразу; иначе
 * слушаем событие с таймаут-фолбэком (чтобы не зависнуть, если событие не
 * придёт — тогда просто пробуем разобрать параметры как есть).
 */
function whenAppReady(timeoutMs = 8000) {
    return new Promise((resolve) => {
        if (window.__FORGE_APP_READY) {
            resolve(true);
            return;
        }
        let done = false;
        const finish = (ok) => {
            if (done) return;
            done = true;
            window.removeEventListener('forge:app-ready', onReady);
            resolve(ok);
        };
        const onReady = () => finish(true);
        window.addEventListener('forge:app-ready', onReady, { once: true });
        // Фолбэк-поллинг + таймаут (на случай, если main.js не диспатчит событие).
        const started = Date.now();
        const tick = () => {
            if (done) return;
            if (window.__FORGE_APP_READY) { finish(true); return; }
            if (Date.now() - started >= timeoutMs) { finish(false); return; }
            setTimeout(tick, 120);
        };
        setTimeout(tick, 120);
    });
}

/**
 * Разбирает launch-параметры из manifest shortcuts / share_target:
 *   • ?tab=tasks|echo|git|docker|telescope|terminal → switchTab(name)
 *   • ?view=daily-summary                           → showDailySummary()
 *   • ?share-target=1 (+ title/text/url)            → вставить в Echo-композер
 * Никакого backend-роута: share_target в manifest — метод GET, обработка чисто
 * клиентская (csrf не задействован). Делается ПОСЛЕ готовности UI.
 */
async function handleLaunchParams() {
    let params;
    try {
        params = new URLSearchParams(location.search);
    } catch (_) {
        return;
    }
    const tab = params.get('tab');
    const view = params.get('view');
    const isShare = params.get('share-target') === '1';

    // Нет ни одного relevant-параметра — дефолтное поведение не трогаем.
    if (!tab && !view && !isShare) return;

    const ready = await whenAppReady();
    if (!ready) {
        // UI так и не сообщил о готовности — не рискуем, чтобы не упасть.
        console.warn('[pwa] app not ready, skip launch params');
        return;
    }

    try {
        if (isShare) {
            await routeShareTarget(params);
        } else if (view === 'daily-summary') {
            await routeView(view);
        } else if (tab) {
            await routeTab(tab);
        }
    } catch (err) {
        console.warn('[pwa] launch params routing failed:', err);
    }
}

/** Переключает активную вкладку через существующий switchTab из tabs.js. */
async function routeTab(name) {
    const allowed = ['terminal', 'tasks', 'git', 'docker', 'telescope', 'echo'];
    if (!allowed.includes(name)) return;
    const mod = await import('../tabs/tabs.js');
    if (mod && typeof mod.switchTab === 'function') mod.switchTab(name);
}

/** Открывает представление по ?view= (сейчас — только daily-summary). */
async function routeView(view) {
    if (view !== 'daily-summary') return;
    const mod = await import('../daily-summary/daily-summary.js');
    if (mod && typeof mod.showDailySummary === 'function') mod.showDailySummary();
}

/**
 * Обрабатывает share-target: открывает вкладку Echo и вставляет переданный
 * контент (title/text/url) в композер чата. Если Echo-инпут недоступен —
 * мягко выходим (без ошибок).
 */
async function routeShareTarget(params) {
    const title = (params.get('title') || '').trim();
    const text = (params.get('text') || '').trim();
    const url = (params.get('url') || '').trim();
    const shared = [title, text, url].filter(Boolean).join('\n').trim();
    if (!shared) return;

    // Переходим на вкладку Echo, чтобы композер был виден/инициализирован.
    await routeTab('echo');

    // Вставляем после короткой паузы — initEcho() в switchTab асинхронно
    // строит DOM композера.
    const tryInsert = (attempt = 0) => {
        const input = document.getElementById('echo-input');
        if (input) {
            const prev = input.value ? input.value.replace(/\s+$/, '') + '\n' : '';
            input.value = prev + shared;
            input.dispatchEvent(new Event('input', { bubbles: true }));
            try { input.focus(); } catch (_) {}
            return;
        }
        if (attempt < 25) setTimeout(() => tryInsert(attempt + 1), 120);
    };
    tryInsert();
}

// ────────────────────────── opt-out (disable) ─────────────────────────

/**
 * Полный opt-out: снимаем регистрацию любого SW и чистим forge-* кэши.
 * Вызывается, когда сервер запущен БЕЗ --pwa (config 404 или enabled=false).
 */
async function disablePwa() {
    // 1) Снять регистрацию всех service worker'ов этого origin.
    if ('serviceWorker' in navigator) {
        try {
            const regs = await navigator.serviceWorker.getRegistrations();
            await Promise.all(regs.map((reg) => reg.unregister().catch(() => {})));
        } catch (_) {
            /* нет доступа к SW — ничего страшного */
        }
    }

    // 2) Удалить все наши кэши (имя начинается с 'forge-').
    if (self.caches && typeof caches.keys === 'function') {
        try {
            const keys = await caches.keys();
            await Promise.all(
                keys
                    .filter((key) => key.startsWith(CACHE_PREFIX))
                    .map((key) => caches.delete(key).catch(() => {}))
            );
        } catch (_) {
            /* Cache API недоступен — нечего чистить */
        }
    }
    // НИКАКИХ изменений разметки — страница ведёт себя как без PWA.
}

// ──────────────────────────── head-инжект ─────────────────────────────

/**
 * Инжект PWA-метаданных в <head>. Идемпотентно (по data-атрибуту), чтобы
 * повторный вызов не плодил дубликаты. Все элементы помечены
 * data-forge-pwa для отладки.
 */
function injectHead() {
    const head = document.head;

    // <link rel="manifest">
    ensureEl(head, 'link[rel="manifest"]', () => {
        const l = document.createElement('link');
        l.rel = 'manifest';
        l.href = '/manifest.webmanifest';
        return l;
    });

    // <meta name="theme-color">
    ensureEl(head, 'meta[name="theme-color"]', () => {
        const m = document.createElement('meta');
        m.name = 'theme-color';
        m.content = '#0e1116';
        return m;
    });

    // iOS standalone-метатеги
    ensureMeta(head, 'apple-mobile-web-app-capable', 'yes');
    ensureMeta(head, 'apple-mobile-web-app-status-bar-style', 'black-translucent');
    ensureMeta(head, 'apple-mobile-web-app-title', 'FORGE');
    ensureMeta(head, 'mobile-web-app-capable', 'yes');

    // <link rel="apple-touch-icon">
    ensureEl(head, 'link[rel="apple-touch-icon"]', () => {
        const l = document.createElement('link');
        l.rel = 'apple-touch-icon';
        l.href = '/icons/apple-touch-icon.png';
        return l;
    });

    // <link rel="stylesheet" href="/css/pwa.css">
    ensureEl(head, 'link[data-forge-pwa-css]', () => {
        const l = document.createElement('link');
        l.rel = 'stylesheet';
        l.href = '/css/pwa.css';
        l.setAttribute('data-forge-pwa-css', '');
        return l;
    });
}

/** Создать элемент через factory, если его ещё нет (по селектору). */
function ensureEl(head, selector, factory) {
    if (head.querySelector(selector)) return;
    const el = factory();
    el.setAttribute('data-forge-pwa', '');
    head.appendChild(el);
}

/** Создать <meta name=.. content=..>, если такого имени ещё нет. */
function ensureMeta(head, name, content) {
    if (head.querySelector(`meta[name="${name}"]`)) return;
    const m = document.createElement('meta');
    m.name = name;
    m.content = content;
    m.setAttribute('data-forge-pwa', '');
    head.appendChild(m);
}
