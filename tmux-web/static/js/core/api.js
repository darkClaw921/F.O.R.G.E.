// tmux-web — REST helpers (Phase 0 ES Modules refactor)
//
// 1:1 копии origin-aware API-хелперов из IIFE `tmux-web/static/app.js`:
//   - parseGlobalId   (app.js:6269)
//   - formatGlobalId  (app.js:6281)
//   - dtoOrigin       (app.js:6291)
//   - withServerParam (app.js:6303)
//   - apiFetch        (app.js:6320)
//
// Все feature-модули в Phase 1+ должны ходить за REST ТОЛЬКО через apiFetch
// (для origin-aware маршрутизации на remote-серверы). Не трогает /healthz,
// /api/themes, /api/remote-servers — они только local и могут использовать
// обычный fetch().
//
// Зависимости:
//   - state.remoteMode из core/state.js (читается напрямую через import).
//   - core/auth.js НЕ импортируется здесь, но Authorization-заголовок будет
//     автоматически добавлен подменённым window.fetch (см. core/auth.js).
//
// В Phase 0 модуль ещё НЕ подключен к index.html; готов к импорту в Phase 1.

import { isRemoteMode } from '../remote/healthz.js';

/**
 * Phase 5 — глобальный id и origin parsing.
 *
 * Глобальный id в remote-mode имеет формат `<origin>::<local-id>`, где
 * `origin` — 'local' либо `<server_id>`. Local-id (без префикса) — это id,
 * которым оперирует бэкенд target'а (локальный devforge или remote).
 *
 * В legacy-режиме id всегда «простой» (без префикса) и parseGlobalId
 * возвращает { origin: 'local', id }.
 *
 * Возвращает { origin: string, id: string }.
 */
export function parseGlobalId(s) {
    if (typeof s !== 'string' || !s) return { origin: 'local', id: '' };
    const idx = s.indexOf('::');
    if (idx < 0) return { origin: 'local', id: s };
    return { origin: s.slice(0, idx), id: s.slice(idx + 2) };
}

/**
 * Собирает глобальный id из origin + local. В remote-mode используем
 * везде, где id уходит на сервер ИЛИ хранится в state (тогда отдельные
 * helper'ы знают, как из него вырезать local-id обратно).
 */
export function formatGlobalId(origin, id) {
    if (!origin || origin === 'local') return id;
    return origin + '::' + id;
}

/**
 * Origin DTO-объекта. Бэкенд проставляет поле `origin` в Session/Project/
 * Task/Todo DTO начиная с Phase 3 (см. remote_proxy::enrich_with_origin).
 * Fallback на 'local' если поле отсутствует.
 */
export function dtoOrigin(dto) {
    if (!dto || typeof dto !== 'object') return 'local';
    return typeof dto.origin === 'string' && dto.origin ? dto.origin : 'local';
}

/**
 * Добавляет `?server=<origin>` к path если origin !== 'local'. Path может
 * уже содержать query — корректно подклеит через `&`.
 *
 * Origin='local' либо falsy → возвращает path без изменений (это покрывает
 * и legacy-режим, где origin всегда 'local').
 */
export function withServerParam(path, origin) {
    if (!origin || origin === 'local') return path;
    const sep = path.indexOf('?') >= 0 ? '&' : '?';
    return path + sep + 'server=' + encodeURIComponent(origin);
}

/**
 * Centralized fetch helper для остальных API-вызовов. Используется ТОЛЬКО
 * там, где запрос может уходить на remote (sessions/tasks/todos).
 * Не трогает /healthz, /api/themes, /api/remote-servers (они только local).
 *
 * Origin определяется так:
 *   1) Явный аргумент `origin` (если передан и !== 'local');
 *   2) Иначе path остаётся как есть.
 *
 * В legacy-режиме (remoteMode=false) — игнорирует origin (всё локально).
 */
export function apiFetch(path, init, origin) {
    if (isRemoteMode() && origin && origin !== 'local') {
        return fetch(withServerParam(path, origin), init);
    }
    return fetch(path, init);
}
