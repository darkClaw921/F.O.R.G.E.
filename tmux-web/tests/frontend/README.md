# Frontend regression tests

Standalone Node-based regression tests for `tmux-web/static/*.js` (включая
PWA-слой `static/sw.js` и `static/js/pwa/*`).
No package manager / no jest — pure Node, just run the file directly.

## Running

```bash
node tmux-web/tests/frontend/sidebar_grouping.test.js
node tmux-web/tests/frontend/sw.test.js
node tmux-web/tests/frontend/pwa_push_helper.test.js
node tmux-web/tests/frontend/pwa_mobile_helper.test.js
node tmux-web/tests/frontend/pwa_bootstrap_optin.test.js
node tmux-web/tests/frontend/pwa_register.test.mjs   # ESM-вариант (.mjs)
```

Exit code 0 — all assertions pass.
Exit code 1 — at least one assertion failed (see `[FAIL ...]` lines).

> `.mjs`-файлы — для тестов, грузящих РЕАЛЬНЫЙ ES-модуль через `await import()`
> (register.js). Команда запуска та же — `node <file>`.

## Files

| File | What it covers |
| --- | --- |
| `sidebar_grouping.test.js` | Phase 6 / forge-cca8.2 — группировка sessions внутри origin'а, двухуровневая фильтрация origin → project, contract `aggregateAllOrigins`, projectFilter не сбрасывается при смене origin. |
| `sw.test.js` | PWA Service Worker (`static/sw.js`) — грузит РЕАЛЬНЫЙ `sw.js` в `node:vm`-песочнице с моками `self`/`caches`/`fetch`/`clients`/`Request`/`Response`. Краевые случаи fetch-роутинга (не-GET, `/ws/*`, upgrade=websocket, cross-origin, navigate network-first + fallback, статика SWR, data allowlist + boundary, прочие `/api/*` и `/healthz` без кэша), install precache (`addAll` `{cache:'reload'}`, атомарность), activate cleanup (только `forge-*` не текущей версии + `clients.claim`), message SKIP_WAITING, push (JSON/text/без data/частичный payload), notificationclick (focus/navigate/openWindow/гварды). |
| `pwa_push_helper.test.js` | PWA push helper — грузит РЕАЛЬНУЮ `urlBase64ToUint8Array` из `static/js/pwa/push.js` через `await import()` + cross-check с локальной репликой. 65-байтный VAPID-ключ, padding по mod4, замена `-_`→`+/`, пустая строка, инвариант длины, бросок на невалидном base64, чистота функции. |
| `pwa_mobile_helper.test.js` | PWA mobile pure-логика (РЕПЛИКА из `static/js/pwa/mobile.js`, т.к. он импортит DOM/xterm) — `countNeedsAttention`, `keyboardHeight`, маршрутизация `updateBadge` (setAppBadge/clearAppBadge/фолбэк/дедуп/проглатывание ошибки), feature-guard-предикаты (отсутствие API → early-return), `safe()`-изоляция фич. |
| `pwa_bootstrap_optin.test.js` | PWA opt-in gate (`static/js/pwa/bootstrap.js`) — грузит РЕАЛЬНЫЙ `bootstrap.js` в `node:vm`-песочнице (синтаксис `import(` → мок `__dynImport(`). Краевые случаи строгого opt-in: config 404 / `enabled:false` / сетевая ошибка fetch / `enabled` truthy-но-не-`true` → `disablePwa` (unregister всех SW + удаление ТОЛЬКО `forge-*` кэшей, без инжекта в `<head>`); отсутствие `serviceWorker`/Cache API → без исключения; `enabled:true` → `window.__FORGE_PWA`+vapidPublicKey, инжект manifest/theme-color/apple-meta/apple-touch-icon/pwa.css, импорт register.js + `registerServiceWorker()`; идемпотентность инжекта против предсуществующей разметки. |
| `pwa_register.test.mjs` | PWA SW update-flow — грузит РЕАЛЬНЫЙ `static/js/pwa/register.js` через `await import('...?bust=N')` (свежий модуль на сценарий) с моками globalThis (`navigator`/`window`/`document`/`requestAnimationFrame`/`setTimeout`). Первая установка без баннера, `reg.waiting`+controller → баннер, updatefound→installed+controller, SKIP_WAITING postMessage, controllerchange → ровно один reload (guard `refreshing`), register reject → warn+null, идемпотентность/dismiss баннера, структура DOM. |

## Контракт

Тесты реплицируют **pure-логику** из `static/app.js` (без DOM). Это значит,
что при изменении любой из функций ниже — нужно одновременно поправить и
ассерты в тестовых файлах:

- `groupSessionsByProject(sessions, orphanKey)` — экспортирована в
  `window.__forge.groupSessionsByProject`;
- `aggregateAllOrigins()` — экспортирована в `window.__forge.aggregateAllOrigins`;
- логика "какие origin'ы видны" в `renderSidebarWithOrigin`;
- логика двухуровневой фильтрации в `renderOriginSection`.

Если упал тест после правки `app.js` — это сигнал что меняется публичный
контракт, проверь что:
1. backend/UI остаются совместимы;
2. документация в `arhit doc` обновлена;
3. legacy режим (`remote_mode=false`, `renderSidebar` без `renderSidebarWithOrigin`)
   не задет — это явное требование Phase 6.

### PWA-контракты

`sw.test.js` и `pwa_register.test.mjs` грузят **РЕАЛЬНЫЙ** исходник
(`static/sw.js`, `static/js/pwa/register.js`) — они ловят дрейф автоматически:
любое изменение fetch-роутинга / lifecycle / update-flow, ломающее контракт,
валит тест. То же для `urlBase64ToUint8Array` в `pwa_push_helper.test.js`
(грузится через `import()` из `static/js/pwa/push.js` + сверка с репликой).

`pwa_mobile_helper.test.js` использует **РЕПЛИКУ** pure-логики (mobile.js нельзя
импортировать в Node — он тянет `core/state.js` + `terminal/xterm.js` с
DOM/WebSocket/xterm top-level). При правке `mobile.js` синхронно обнови реплики
в тесте:

- `countNeedsAttention(data)` — `Array.isArray(data) ? data.filter(s => s && s.needs_attention).length : 0` (mobile.js строки 136-138);
- `keyboardHeight(innerHeight, vvHeight, vvOffsetTop)` — `Math.max(0, Math.round(innerHeight - vvHeight - vvOffsetTop))` (mobile.js строки 78-81);
- `updateBadge`-маршрутизация App Badge API (mobile.js строки 163-175);
- feature-guard-предикаты `'setAppBadge' in navigator` / `!!window.visualViewport` / `'wakeLock' in navigator` и `safe()`-изоляция фич.

Контракт SW push-handshake между `register.js` и `sw.js`: сообщение строго
`{type:'SKIP_WAITING'}` (проверяется в обоих файлах) — расхождение в имени type
рвёт обновление.
