# sw.js

Service Worker devforge (Фаза 4 PWA). Файл: tmux-web/static/sw.js. Классический (не module) worker, scope '/'. Регистрируется register.js ТОЛЬКО когда bootstrap.js увидел enabled=true от /api/pwa/config (строгий opt-in).

LIFECYCLE:
- install: precache критического app-shell (SHELL_ASSETS: /, /style.css, xterm-вендор, /js/main.js, quick-cmd/command-dock/hotkeys.js, /js/pwa/bootstrap.js, /icons/icon-192.png, /manifest.webmanifest). БЕЗ skipWaiting — новый SW ждёт подтверждения пользователя (баннер обновления). Граф модулей под /js/main.js НЕ precache'им — подтянется через runtime SWR.
- activate: удалить все кэши с именем != текущей версии + clients.claim().
- fetch (только GET, прочее early return к сети): navigate(HTML) -> network-first с fallback на кэш '/' (офлайн app-shell); статика js/css/vendor/icons/style.css/manifest -> stale-while-revalidate; read-only data allowlist (DATA_ALLOWLIST: /api/sessions, /api/tasks, /api/todos, /api/echo/conversations|memories|daily-reports) -> network-first + cache fallback (офлайн-чтение последних данных); прочие /api/*, /healthz, /api/push/*, /api/pwa/config -> НЕ кэшировать; /ws/* -> НИКОГДА не перехватывать (WebSocket).
- message{SKIP_WAITING} -> skipWaiting() (по клику в баннере register.js).
- push -> showNotification (payload из push.rs Фаза 3: {title, body, data:{url}}).
- notificationclick -> focus существующего клиента или openWindow(data.url).

ВЕРСИОНИРОВАНИЕ КЭШЕЙ: CACHE_VERSION='forge-pwa-v1' — единая точка бампа. SHELL_CACHE/RUNTIME_CACHE/DATA_CACHE = forge-{shell,runtime,data}-{version}. Префикс 'forge-' — bootstrap.js при enabled=false удаляет всё начинающееся с 'forge-' (строгий opt-out). При изменении app-shell поднимаем CACHE_VERSION -> activate удалит старые кэши, register.js покажет баннер 'Доступно обновление'.

CACHE-CONTROL: static_embed.rs отдаёт sw.js и manifest.webmanifest с Cache-Control: no-cache (браузер всегда сверяет с сетью, иначе старый SW залипает в HTTP-кэше и update-flow ломается).

ЗАВИСИМОСТИ: register.js (регистрация + update-flow), bootstrap.js (gate по /api/pwa/config), push.rs (payload пушей), static_embed.rs (отдача из бинаря + no-cache).
