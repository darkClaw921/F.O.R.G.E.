// tmux-web — клиент REST-API аналитики использования Claude Code (Фаза 3).
//
// Только сеть: ни одного узла DOM, ни одной строки разметки. Дашборд
// (`analytics.js`, Фаза 5) и вкладка настроек (`analytics-tab.js`, Фаза 6)
// берут данные отсюда.
//
// Все пути под `/api/analytics/*` зарегистрированы в `tmux-web/src/main.rs`
// рядом с `/api/user-settings`, ПОСЛЕ чего на них слоями навешиваются
// bearer-auth и CSRF. Поэтому здесь нет ни заголовка `Authorization`, ни
// CSRF-токена: их добавляет подменённый `window.fetch` из `core/auth.js`,
// а `apiFetch` — единственная дверь, через которую это работает. Прямой
// `fetch(...)` в этом файле означал бы 401 в remote-режиме, поэтому его тут
// нет и быть не должно (проверяется контрактным тестом).

import { apiFetch } from '../core/api.js';

/**
 * Универсальный helper — запрос через apiFetch с разбором JSON.
 * При не-2xx бросает Error со статусом и текстом ответа.
 *
 * @param {string} path — относительный URL (например '/api/analytics/status')
 * @param {RequestInit} [init] — опции fetch
 * @returns {Promise<any>}
 */
async function call(path, init) {
    const res = await apiFetch(path, init);
    const ct = res.headers.get('content-type') || '';
    let body = null;
    try {
        if (ct.includes('application/json')) {
            body = await res.json();
        } else {
            body = await res.text();
        }
    } catch (e) {
        // ignore — оставляем body=null
    }
    if (!res.ok) {
        const detail = (body && body.error) ? body.error : (typeof body === 'string' ? body : '');
        const err = new Error(`HTTP ${res.status}${detail ? ': ' + detail : ''}`);
        err.status = res.status;
        err.body = body;
        throw err;
    }
    return body;
}

// ==========================================================================
// Таблица-контракт
// ==========================================================================

/**
 * Поверхность аналитики глазами фронта.
 *
 * Сверяется с роутером Rust В ОБЕ СТОРОНЫ тестом
 * `tmux-web/tests/analytics_routes_contract.rs`: путь, которого нет в
 * `main.rs`, и роут `/api/analytics/*`, которого нет здесь, роняют тест.
 * Без этой сверки расхождение не ломает ни сборку, ни консоль — оно
 * проявляется как молчаливый 404, который дашборд покажет пустым экраном.
 *
 * `path` — ДОСЛОВНАЯ строка из `.route(...)`, сравнение текстовое.
 *
 * @type {Array<{method: string, path: string, fn: string}>}
 */
export const ANALYTICS_ROUTES = [
    { method: 'GET', path: '/api/analytics/status', fn: 'getAnalyticsStatus' },
    { method: 'GET', path: '/api/analytics/projects', fn: 'getAnalyticsProjects' },
    { method: 'GET', path: '/api/analytics/summary', fn: 'getAnalyticsSummary' },
    { method: 'POST', path: '/api/analytics/rescan', fn: 'rescanAnalytics' },
];

// ==========================================================================
// Роуты
// ==========================================================================

/**
 * `GET /api/analytics/status` — состояние и прогресс индексации.
 *
 * ЭТОТ ЗАПРОС ЗАПУСКАЕТ индексацию, если она ещё не запускалась: на старте
 * devforge транскрипты намеренно не читаются. Поэтому первый ответ почти
 * всегда `state: "scanning"` — это нормальный ход событий, а не ошибка.
 *
 * @returns {Promise<{state: 'idle'|'scanning'|'ready', done: number,
 *   total: number, errors: number, filesRead: number, bytesRead: number,
 *   updatedAt: number, sessions: number, files: number, projects: number,
 *   lastError?: string}>}
 */
export function getAnalyticsStatus() {
    return call('/api/analytics/status');
}

/**
 * `GET /api/analytics/projects` — список проектов для селектора.
 *
 * Отсортирован по последней активности, новые сверху. Пустой массив во время
 * первого скана — валидный ответ, а не пустая база.
 *
 * @returns {Promise<Array<{id: string, path: string, displayName: string,
 *   sessions: number, messages: number, lastActiveDate: string}>>}
 */
export function getAnalyticsProjects() {
    return call('/api/analytics/projects');
}

/**
 * `GET /api/analytics/summary` — KPI и все серии за период.
 *
 * Пустой/отсутствующий параметр не отправляется вовсе: сервер сам подставит
 * умолчание (весь диапазон индекса, `bucket=day`, все проекты).
 *
 * ОТВЕТ ВО ВРЕМЯ ИНДЕКСАЦИИ — ТОЖЕ 200: `state: "scanning"`, `progress`
 * заполнен, серии пустые. Проверять надо `state`, а не наличие данных.
 *
 * @param {{from?: string, to?: string, project?: string,
 *   bucket?: 'day'|'week'}} [q] — `from`/`to` в формате `YYYY-MM-DD`
 * @returns {Promise<any>}
 */
export function getAnalyticsSummary(q) {
    const p = new URLSearchParams();
    const src = q || {};
    for (const key of ['from', 'to', 'project', 'bucket']) {
        const v = src[key];
        if (v !== undefined && v !== null && String(v) !== '') {
            p.set(key, String(v));
        }
    }
    const qs = p.toString();
    return call(`/api/analytics/summary${qs ? `?${qs}` : ''}`);
}

/**
 * `POST /api/analytics/rescan` — форс полного пересбора индекса.
 *
 * Возвращает СРАЗУ, текущим статусом (`state: "scanning"`): пересбор идёт в
 * фоне, и дальше нужно поллить {@link getAnalyticsStatus} до `"ready"`.
 * Повторный вызов во время скана второго прохода не запускает.
 *
 * @returns {Promise<any>} тот же объект, что у {@link getAnalyticsStatus}
 */
export function rescanAnalytics() {
    return call('/api/analytics/rescan', {
        method: 'POST',
        headers: { 'Accept': 'application/json' },
    });
}
