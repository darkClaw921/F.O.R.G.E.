PWA (Progressive Web App) для devforge — opt-in фича. Эпик forge-pwa-*, Фазы 1-6 завершены.

=== НАЗНАЧЕНИЕ ===
Превращает веб-UI devforge в устанавливаемое приложение с офлайн app-shell, иконкой на 'Домой', standalone-режимом и Web Push-уведомлениями 'требуется внимание' (когда сессия Claude поднимает permission-prompt). Полезно для управления с телефона.

=== OPT-IN ИНВАРИАНТ (жёсткий) ===
Вся фича активируется ТОЛЬКО флагом старта --pwa (или server_config.json pwa:true / env DEVFORGE_PWA). По умолчанию (без флага) поведение байт-в-байт как раньше: НЕТ роутов /api/pwa/* и /api/push/* (404), НЕ создаются ~/.forge/vapid.json и push_subscriptions.json, push-воркер НЕ спавнится. Источник гейтинга: server_config::EffectiveConfig.pwa_enabled -> AppState.pwa: Option<PwaCtx>. На фронте bootstrap.js всегда грузится (статика), но при config!=enabled снимает регистрацию SW и чистит forge-* кэши без правок разметки (строгий opt-out при рестарте без флага). Файлы /manifest.webmanifest и /sw.js всегда в бинаре (статика), но безвредны пока bootstrap не включил PWA.

=== АРХИТЕКТУРА BACKEND (Rust, tmux-web/src/) ===
- server_config.rs: резолв pwa_enabled (CLI>file>env>default false); print_public_bind_warning с доп. строкой про HTTPS для web push при --pwa.
- main.rs: при pwa_enabled создаёт PwaCtx{vapid: VapidStore, subs: PushSubscriptionStore}, кладёт в AppState.pwa=Some; регистрирует роуты в блоке 'if app_state.pwa.is_some()' ДО auth-layer (в remote-mode под Bearer+csrf_guard); спавнит push::attention_watcher.
- vapid.rs: VAPID ECDSA P-256 пара (RFC8292), ~/.forge/vapid.json, load_or_generate (переиспользуется между рестартами).
- push_store.rs: PushSubscriptionStore (Arc<RwLock>, atomic save), ~/.forge/push_subscriptions.json, upsert/remove/prune.
- pwa.rs: хендлеры GET /api/pwa/config (отдаёт vapidPublicKey), POST /api/push/{subscribe,unsubscribe,test}.
- push.rs: RFC8291 (aes128gcm) шифрование + VAPID-JWT (ES256) на чистом RustCrypto; send_one/send_to_all (POST на endpoint, прун 404/410); attention_watcher (edge-trigger false->true раз в 1.5с).

=== АРХИТЕКТУРА FRONTEND (tmux-web/static/) ===
- manifest.webmanifest: name/short_name/icons(192/512/maskable)/shortcuts(Tasks/Echo/Daily)/share_target/display:standalone. icons/ — PNG-иконки.
- sw.js: Service Worker (scope /), precache app-shell, network-first navigate + SWR статика + офлайн read-only data allowlist, push->showNotification, CACHE_VERSION='forge-pwa-v1'.
- js/pwa/bootstrap.js: всегда-загружаемый gate по /api/pwa/config (opt-in/opt-out).
- js/pwa/register.js: регистрация SW + update-flow (баннер 'Обновить'->SKIP_WAITING->один reload).
- js/pwa/install.js: install UI (beforeinstallprompt chip + iOS-подсказка).
- js/pwa/push.js: тоггл подписки в Settings>Notifications (requestPermission в user-gesture, pushManager.subscribe).
- js/pwa/mobile.js: safe-area, visualViewport keyboard для xterm, overscroll-contain, App Badge, Wake Lock.
- css/pwa.css: PWA-стили (.pwa-active, safe-area, баннер обновления).
- static_embed.rs: Cache-Control:no-cache для sw.js и manifest.webmanifest (надёжный update-flow).

=== СТЕК ШИФРОВАНИЯ — ПОЧЕМУ НЕ web-push ===
Крейт web-push НЕ используется: он безусловно тянет ece с backend-openssl (у ece нет rust-crypto-фичи) и фиксирует hyper 0.14, несовместимый с axum 0.7. Весь проект на rustls. Поэтому VAPID-JWT и RFC8291/8188-шифрование реализованы на ЧИСТОМ RustCrypto: p256, ecdsa, aes-gcm, hkdf, hmac, sha2. reqwest 0.12 на rustls 0.23. ИНВАРИАНТ: cargo tree -i openssl/ece/web-push — ПУСТО.

=== ОГРАНИЧЕНИЯ iOS / HTTPS ===
Web push с телефона/iOS требует ВАЛИДНОГО HTTPS (Service Worker и Push API только в secure context; localhost — исключение, но удалённый телефон ходит по IP/домену). iOS 16.4+ принимает только валидный серт (самоподписанный отвергается); web push на iOS работает ТОЛЬКО в установленном на 'Домой' PWA (standalone) — в браузерном iOS тоггл задизейблен с подсказкой установить. Поставить TLS: reverse-proxy Caddy/nginx/Traefik или Tailscale с TLS. По plain HTTP push с телефона не заработает (warning печатается в remote-mode при --pwa).

=== КАК ВКЛЮЧИТЬ ===
devforge run --pwa  (localhost: push работает только на самом устройстве, secure-context exception для localhost).
devforge run --remote --pwa  (+ HTTPS-прокси для push с телефона). Проверить: GET /api/pwa/config -> {enabled:true, vapidPublicKey}. В Chrome DevTools>Application: manifest валиден, SW зарегистрирован, install доступен. Подписка: Settings>Notifications>тоггл Push.

=== ВЕРИФИКАЦИЯ (Фаза 6) ===
cargo build зелёный 0 warnings; 383 unit + 5 integration тестов; cargo tree -i openssl/ece/web-push пусто. Opt-in инвариант подтверждён e2e (с флагом и без, изолированный HOME). Push-флоу подтверждён мок-сервисом (subscribe->test{sent,pruned}->410 prune; реальный attention_watcher доставил пуш по edge-триггеру). Ручная проверка: Lighthouse PWA installable в DevTools и реальное устройство по HTTPS.