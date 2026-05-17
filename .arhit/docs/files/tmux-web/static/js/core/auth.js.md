# tmux-web/static/js/core/auth.js

Auth bootstrap + fetch override (Phase 0 ES Modules refactor).

## Назначение
1:1 копия auth-логики из IIFE `tmux-web/static/app.js` (строки 15-87). Обеспечивает:
1. Извлечение токена из URL (#token=...) и сохранение в localStorage (QR-flow).
2. Подмена `window.fetch` — добавляет `Authorization: Bearer <token>` ко всем same-origin запросам.
3. Хелпер `withWsToken(url)` для WebSocket (нельзя ставить headers на WS из JS).

## ⚠️ Side-effect import
Модуль выполняет на верхнем уровне при импорте:
1. IIFE `bootstrapAuthToken()` — читает hash из location, кладёт token в localStorage, чистит hash.
2. Подмена `window.fetch` — оборачивает оригинальный fetch.

**Контракт:** в Phase 1 main.js должен импортировать `./core/auth.js` ПЕРВЫМ, ДО любого другого модуля, который может позвать fetch(). Иначе override произойдёт после первых запросов и они уйдут без токена.

## Экспорты
- `getAuthToken(): string` — getter из localStorage. Возвращает '' если токена нет или localStorage недоступен (privacy mode).
- `withWsToken(wsUrl: string): string` — добавляет `?token=...` (или `&token=...` если уже есть query) к WS URL.

## НЕ экспортируется
- `AUTH_TOKEN_KEY` — внутренняя константа `'forge.authToken'`.
- `bootstrapAuthToken` — выполняется как IIFE при импорте.
- Fetch override — top-level statement.

## Зависимости
НЕТ — только браузерные globals (localStorage, location, history, window.fetch, Headers, URL).

## Поведение fetch override
- Если токена нет — pass-through к оригинальному fetch.
- Если input — Request, читаем `.url`.
- Если URL cross-origin — pass-through (не трогаем CDN типа xterm.js).
- Не перезаписывает Authorization если он уже задан в headers.
- Если Authorization уже стоит — оставляет.

## withWsToken
- Если token пустой — возвращает URL без изменений (legacy localhost).
- Auto-detect `?` vs `&` separator.

## Ограничения
- В Phase 0 модуль не подключен; legacy app.js делает то же самое в IIFE.
