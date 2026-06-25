bootstrap.js — единственный ВСЕГДА-загружаемый PWA-файл devforge. Файл: tmux-web/static/js/pwa/bootstrap.js (НЕ путать с js/core/bootstrap.js). Подключён статически из index.html (<script type=module src=/js/pwa/bootstrap.js>). Грузится всегда (это статика в бинаре), но сам решает включать ли PWA — строгий opt-in через /api/pwa/config.

ЛОГИКА (самовызывающаяся async-функция pwaBootstrap):
1) fetch('/api/pwa/config', cache:no-store, credentials:same-origin):
   - не-200 / enabled !== true -> PWA ВЫКЛЮЧЕНО: disablePwa() — снять регистрацию ЛЮБОГО SW (getRegistrations -> unregister), удалить все кэши с именем на 'forge-', выйти НЕ трогая разметку. Гарантирует opt-out при рестарте сервера БЕЗ --pwa (config станет 404).
   - enabled === true -> PWA ВКЛЮЧЕНО: window.__FORGE_PWA={enabled:true, vapidPublicKey} (для push.js); injectHead() — <link rel=manifest>, theme-color, apple-*-meta, apple-touch-icon, <link rel=stylesheet href=/css/pwa.css>, body-класс .pwa-active; import('./register.js')->registerServiceWorker(); ленивый импорт install.js/push.js/mobile.js (каждый в своём try/catch); handleLaunchParams() — разбор location.search (?tab=/?view=/?share-target=) после whenAppReady.

КЛЮЧЕВЫЕ ФУНКЦИИ: disablePwa (opt-out cleanup), injectHead (<head>-инъекции + pwa.css), loadInstall/loadPush/loadMobile (ленивые импорты Фазы 5), handleLaunchParams/routeTab/routeView/routeShareTarget (shortcuts + share_target из manifest), whenAppReady (ждёт инициализации main.js перед switchTab), ensureMeta/ensureEl (идемпотентная вставка meta/link).

БИЗНЕС-ЛОГИКА: без enabled НИКАКИХ побочных эффектов на разметку — поведение страницы как без PWA (строгий opt-in, ценой одного кадра theme-color). Config не кэшируем (no-store) — opt-in/opt-out срабатывает сразу. Сеть недоступна -> считаем PWA выключенным (безопасный дефолт).

ЗАВИСИМОСТИ: /api/pwa/config (pwa.rs), register.js, install.js, push.js, mobile.js, css/pwa.css, manifest.webmanifest, index.html (статический <script>).