mobile.js — мобильные улучшения PWA devforge (Фаза 5). Файл: tmux-web/static/js/pwa/mobile.js (НЕ путать с js/sidebar/mobile.js). Лениво импортируется из bootstrap.js ТОЛЬКО при enabled===true (opt-in). Каждая фича feature-detected + try/catch — отсутствие API не ломает остальное. Точка входа initMobile() идемпотентна (guard window.__FORGE_PWA_MOBILE_INIT).

КРИТИЧНЫЕ ФИЧИ:
1) safe-area — правила в css/pwa.css; JS ставит body-класс .pwa-active чтобы правила применялись только при enabled.
2) visualViewport keyboard — при сжатии вьюпорта экранной клавиатурой ставит CSS-var --pwa-keyboard-height и пересчитывает xterm (state.fitAddon.fit() + scheduleResizeFromTerm()) — ввод в терминал работает над клавиатурой.
3) overscroll — overscroll-behavior:contain на #terminal/#tasks-board (css/pwa.css), блокирует pull-to-refresh (иначе случайный reload рвёт WebSocket).

NICE-TO-HAVE: App Badge — navigator.setAppBadge(count) где count=число сессий с needs_attention (лёгкий поллинг /api/sessions); Screen Wake Lock — тоггл 'Не гасить экран', re-acquire на visibilitychange; online/offline баннер — navigator.onLine + события, синхронизация со status-dot. vibrate — уже в sw.js (push), здесь не дублируется.

ЗАВИСИМОСТИ: bootstrap.js (ленивый импорт), core/state.js (state.fitAddon), terminal/xterm.js (scheduleResizeFromTerm), css/pwa.css (safe-area/overscroll/keyboard правила), /api/sessions (badge poll).