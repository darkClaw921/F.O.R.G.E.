# serve_static

Axum fallback-handler embedded-static (rust-embed StaticAssets). Отдаёт файлы из tmux-web/static/ с mime по расширению. СПЕЦ-СЛУЧАЙ (Фаза 4 PWA): для path=='sw.js' и path=='manifest.webmanifest' добавляет заголовок Cache-Control: no-cache, чтобы браузер всегда сверял service worker/manifest и update-flow PWA был надёжным (иначе старый sw.js залипает в HTTP-кэше). Прочая статика — без Cache-Control. Нормализует пустой путь и trailing-slash к index.html, 404 при отсутствии.
