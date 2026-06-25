# register.js

Регистрация Service Worker + ненавязчивый update-flow (Фаза 4 PWA). Файл: tmux-web/static/js/pwa/register.js. Экспортирует registerServiceWorker(), вызывается из bootstrap.js при enabled=true.

ЛОГИКА: navigator.serviceWorker.register('/sw.js', {scope:'/'}). На событие 'updatefound' ждёт reg.installing -> state==='installed'; если в этот момент есть navigator.serviceWorker.controller (значит это ОБНОВЛЕНИЕ, не первая установка) -> показывает баннер 'Доступно обновление — Обновить'. Первая установка (controller отсутствует) баннер НЕ показывает — нечего обновлять.

UPDATE-FLOW: клик 'Обновить' -> reg.waiting.postMessage({type:'SKIP_WAITING'}) -> новый SW вызывает skipWaiting()->activate->'controllerchange' -> на controllerchange (ОДНОКРАТНО, guard refreshing против петли перезагрузок) -> location.reload(). Итог: один reload на новую версию без зависания. Также проверяет reg.waiting при старте (если SW уже ждёт + есть controller -> сразу баннер).

UI: баннер .pwa-update-banner (стили из css/pwa.css), role=status, кнопка 'Обновить'. Создаётся один раз (guard bannerEl).

ЗАВИСИМОСТИ: sw.js (регистрируемый worker, обрабатывает SKIP_WAITING), bootstrap.js (вызывает registerServiceWorker), css/pwa.css (.pwa-update-banner). Связь с CACHE_VERSION в sw.js: бамп версии -> новый sw.js -> updatefound -> баннер.
