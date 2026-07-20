/*
 * sw.js — Service Worker devforge (Фаза 4 PWA).
 *
 * Классический (не module) worker, scope '/'. Регистрируется register.js
 * ТОЛЬКО когда bootstrap.js увидел enabled=true от /api/pwa/config.
 *
 * Стратегии:
 *   • install  — precache критического app-shell (НЕ весь граф модулей),
 *                БЕЗ skipWaiting (обновление подтверждает пользователь).
 *   • activate — удалить кэши не текущей версии + clients.claim().
 *   • fetch    — только GET (прочее — early return к сети):
 *       - navigate (HTML)        → network-first, fallback кэш '/';
 *       - статика js/css/vendor/icons/style.css/manifest → stale-while-revalidate;
 *       - read-only data allowlist → network-first + cache fallback (офлайн-чтение);
 *       - прочие /api/*, /healthz, /api/push/*, /api/pwa/config → НЕ кэшировать;
 *       - /ws/*                  → НИКОГДА не перехватывать (WebSocket).
 *   • message {SKIP_WAITING}     → skipWaiting() (по клику в баннере обновления);
 *   • push                       → showNotification (payload из push.rs, Фаза 3);
 *   • notificationclick          → focus существующего клиента или openWindow(url).
 *
 * CACHE_VERSION — единая точка бампа: при изменении app-shell поднимаем версию,
 * register.js покажет ненавязчивый баннер «Доступно обновление».
 */

'use strict';

// ──────────────────────────── версии кэшей ────────────────────────────

// v2: hotkeys.js получил гейт настройки cmd_hints_enabled. Он раздаётся через
// stale-while-revalidate, поэтому без бампа версии первый reload после
// обновления выполнил бы старую копию и тумблер молча не сработал бы.
const CACHE_VERSION = 'forge-pwa-v2';
const SHELL_CACHE = `forge-shell-${CACHE_VERSION}`;
const RUNTIME_CACHE = `forge-runtime-${CACHE_VERSION}`;
const DATA_CACHE = `forge-data-${CACHE_VERSION}`;

// Префикс всех наших кэшей — bootstrap.js при enabled=false удаляет всё
// начинающееся с 'forge-' (строгий opt-out).
const CACHE_PREFIX = 'forge-';

// ───────────────────────── precache app-shell ─────────────────────────

// Критический app-shell: то, без чего страница не нарисуется офлайн.
// Реальные entry-js взяты из index.html (module-вход /js/main.js + классические
// /quick-cmd.js, /command-dock.js, /hotkeys.js + xterm-вендор). Граф модулей под
// /js/main.js целиком НЕ precache'им — он подтянется через runtime SWR.
const SHELL_ASSETS = [
    '/',
    '/style.css',
    '/vendor/xterm/xterm.min.css',
    '/vendor/xterm/xterm.min.js',
    '/vendor/xterm/xterm-addon-fit.min.js',
    '/vendor/xterm/xterm-addon-web-links.min.js',
    '/js/main.js',
    '/quick-cmd.js',
    '/command-dock.js',
    '/hotkeys.js',
    '/js/pwa/bootstrap.js',
    '/icons/icon-192.png',
    '/manifest.webmanifest',
];

// ─────────────────── allowlist read-only data (офлайн) ─────────────────

// Только идемпотентные read-only GET-эндпоинты — их ответы кэшируем
// network-first, чтобы установленное приложение показывало последние данные
// офлайн. Префиксное совпадение (startsWith) покрывает под-пути с query.
const DATA_ALLOWLIST = [
    '/api/sessions',
    '/api/tasks',
    '/api/todos',
    '/api/echo/conversations',
    '/api/echo/memories',
    '/api/echo/daily-reports',
];

// ──────────────────────────────── install ─────────────────────────────

self.addEventListener('install', (event) => {
    event.waitUntil(
        caches.open(SHELL_CACHE).then((cache) =>
            // {cache:'reload'} — обходим HTTP-кэш браузера при precache,
            // чтобы в кэш SW лёг свежий шелл. addAll атомарен: при сбое любого
            // ресурса install падает и SW не активируется (это желаемо).
            cache.addAll(
                SHELL_ASSETS.map((url) => new Request(url, { cache: 'reload' }))
            )
        )
        // БЕЗ self.skipWaiting() — новый SW ждёт подтверждения пользователя.
    );
});

// ─────────────────────────────── activate ─────────────────────────────

const CURRENT_CACHES = new Set([SHELL_CACHE, RUNTIME_CACHE, DATA_CACHE]);

self.addEventListener('activate', (event) => {
    event.waitUntil(
        caches
            .keys()
            .then((keys) =>
                Promise.all(
                    keys
                        // удаляем только наши кэши прошлых версий, чужие не трогаем
                        .filter(
                            (key) =>
                                key.startsWith(CACHE_PREFIX) &&
                                !CURRENT_CACHES.has(key)
                        )
                        .map((key) => caches.delete(key))
                )
            )
            .then(() => self.clients.claim())
    );
});

// ──────────────────────────── fetch-стратегии ─────────────────────────

self.addEventListener('fetch', (event) => {
    const { request } = event;

    // Только GET — POST/PUT/DELETE и т.п. идут в сеть без перехвата.
    if (request.method !== 'GET') return;

    const url = new URL(request.url);

    // Перехватываем только same-origin (CDN/внешние ресурсы — мимо).
    if (url.origin !== self.location.origin) return;

    // WebSocket-апгрейды и /ws/* НИКОГДА не перехватываем.
    if (url.pathname.startsWith('/ws/') || url.pathname === '/ws') return;
    if (request.headers.get('upgrade') === 'websocket') return;

    // Навигации (HTML-документы) → network-first, fallback на кэш '/'.
    if (request.mode === 'navigate') {
        event.respondWith(navigationStrategy(request));
        return;
    }

    const path = url.pathname;

    // Read-only data allowlist → network-first + cache fallback (офлайн-чтение).
    if (isDataAllowlisted(path)) {
        event.respondWith(networkFirstData(request));
        return;
    }

    // Прочие /api/*, /healthz, push/config — НЕ кэшируем (всегда сеть).
    if (
        path.startsWith('/api/') ||
        path === '/healthz' ||
        path.startsWith('/healthz')
    ) {
        return; // нет respondWith → дефолтный сетевой запрос
    }

    // Статика приложения → stale-while-revalidate.
    if (isStaticAsset(path)) {
        event.respondWith(staleWhileRevalidate(request));
        return;
    }

    // Остальное — не вмешиваемся.
});

function isDataAllowlisted(path) {
    return DATA_ALLOWLIST.some(
        (prefix) => path === prefix || path.startsWith(prefix + '/') || path.startsWith(prefix + '?')
    );
}

function isStaticAsset(path) {
    return (
        path.startsWith('/js/') ||
        path.startsWith('/css/') ||
        path.startsWith('/vendor/') ||
        path.startsWith('/icons/') ||
        path === '/style.css' ||
        path === '/manifest.webmanifest' ||
        path === '/quick-cmd.js' ||
        path === '/command-dock.js' ||
        path === '/hotkeys.js' ||
        path === '/app.js'
    );
}

/**
 * Навигация: пытаемся сеть, при ошибке отдаём закэшированный '/'.
 * Свежий ответ кладём в SHELL_CACHE под ключ '/' (app-shell обновляется).
 */
async function navigationStrategy(request) {
    const cache = await caches.open(SHELL_CACHE);
    try {
        const response = await fetch(request);
        // Кладём успешный HTML под '/' — стабильный ключ app-shell.
        if (response && response.ok) {
            cache.put('/', response.clone());
        }
        return response;
    } catch (err) {
        const cached = (await cache.match('/')) || (await cache.match(request));
        if (cached) return cached;
        throw err;
    }
}

/**
 * Stale-while-revalidate: мгновенно отдаём из кэша (если есть) и параллельно
 * обновляем кэш свежим ответом из сети. Если кэша нет — ждём сеть.
 */
async function staleWhileRevalidate(request) {
    const cache = await caches.open(RUNTIME_CACHE);
    const cached = await cache.match(request);

    const networkPromise = fetch(request)
        .then((response) => {
            if (response && response.ok && response.type === 'basic') {
                cache.put(request, response.clone());
            }
            return response;
        })
        .catch(() => undefined);

    if (cached) {
        // не ждём сеть — фоновое обновление
        networkPromise;
        return cached;
    }
    const network = await networkPromise;
    if (network) return network;
    // нет ни кэша, ни сети
    return new Response('', { status: 504, statusText: 'Offline' });
}

/**
 * Network-first для read-only data: свежий ответ кэшируем и отдаём; при
 * сетевой ошибке — отдаём последнюю закэшированную версию (офлайн-чтение).
 */
async function networkFirstData(request) {
    const cache = await caches.open(dataCacheName());
    try {
        const response = await fetch(request);
        if (response && response.ok) {
            cache.put(request, response.clone());
        }
        return response;
    } catch (err) {
        const cached = await cache.match(request);
        if (cached) return cached;
        throw err;
    }
}

/**
 * Имя data-кэша. В remote-mode желательно партиционировать кэш по короткому
 * хэшу Bearer-токена, чтобы данные одного сервера не утекали в другой.
 * TODO(Фаза 5/remote): SW не имеет прямого доступа к токену (он в памяти
 * клиента) — пробросить хэш через postMessage из bootstrap при регистрации
 * и хранить в self. Пока используем единый DATA_CACHE (локальный режим).
 */
function dataCacheName() {
    return DATA_CACHE;
}

// ──────────────────────── message: SKIP_WAITING ───────────────────────

self.addEventListener('message', (event) => {
    if (event.data && event.data.type === 'SKIP_WAITING') {
        self.skipWaiting();
    }
});

// ─────────────────────────────── push ─────────────────────────────────

// Payload совпадает с push.rs (Фаза 3). Формат — JSON
// { title, body, url, tag? }. Если payload не JSON — показываем дефолт.
self.addEventListener('push', (event) => {
    let data = {};
    if (event.data) {
        try {
            data = event.data.json();
        } catch (_) {
            data = { body: event.data.text() };
        }
    }
    const title = data.title || 'F.O.R.G.E.';
    const options = {
        body: data.body || '',
        icon: '/icons/icon-192.png',
        badge: '/icons/badge-72.png',
        vibrate: [80, 40, 80],
        tag: data.tag || 'forge-attention',
        renotify: true,
        data: { url: data.url || '/' },
    };
    event.waitUntil(self.registration.showNotification(title, options));
});

// ───────────────────────── notificationclick ──────────────────────────

self.addEventListener('notificationclick', (event) => {
    event.notification.close();
    const targetUrl = (event.notification.data && event.notification.data.url) || '/';
    const targetAbs = new URL(targetUrl, self.location.origin).href;

    event.waitUntil(
        self.clients
            .matchAll({ type: 'window', includeUncontrolled: true })
            .then((clientList) => {
                // Фокусим уже открытое окно приложения, если есть.
                for (const client of clientList) {
                    if ('focus' in client) {
                        // навигируем существующий клиент на целевой URL при наличии API
                        if (client.url !== targetAbs && 'navigate' in client) {
                            return client.focus().then((c) =>
                                c && c.navigate ? c.navigate(targetAbs).catch(() => c) : c
                            );
                        }
                        return client.focus();
                    }
                }
                // Нет открытых окон — открываем новое.
                if (self.clients.openWindow) {
                    return self.clients.openWindow(targetAbs);
                }
                return undefined;
            })
    );
});
