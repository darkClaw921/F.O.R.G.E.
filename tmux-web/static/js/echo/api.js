// tmux-web — Echo plugin REST API client (Phase 5c)
//
// Тонкие wrappers вокруг `apiFetch` из core/api.js. Каждая функция
// возвращает распарсенный JSON либо throw'ит Error с осмысленным
// сообщением (status + текст ответа).
//
// Все пути под `/api/echo/*`. Сервер плагина регистрирует роуты до
// auth middleware (см. tmux-web/src/main.rs), так что Bearer-токен
// автоматически добавляется подменённым window.fetch в core/auth.js.

import { apiFetch } from '../core/api.js';

/**
 * Универсальный helper — отправляет запрос через apiFetch и парсит JSON.
 * При не-2xx статусе бросает Error со status'ом и текстом ответа.
 *
 * @param {string} path — relative URL (например '/api/echo/conversations')
 * @param {RequestInit} [init] — fetch options
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
        // ignore — leave body=null
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

function jsonInit(method, payload) {
    return {
        method,
        headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
        body: JSON.stringify(payload || {}),
    };
}

// -------- conversations --------

export async function listConversations(projectId) {
    const qs = projectId ? `?project_id=${encodeURIComponent(projectId)}` : '';
    return call(`/api/echo/conversations${qs}`);
}

export async function createConversation({ title, projectId, model }) {
    return call('/api/echo/conversations', jsonInit('POST', {
        title,
        project_id: projectId || null,
        model: model || null,
    }));
}

export async function deleteConversation(id) {
    return call(`/api/echo/conversations/${encodeURIComponent(id)}`, {
        method: 'DELETE',
    });
}

export async function listMessages(conversationId, opts) {
    const o = opts || {};
    const qs = new URLSearchParams();
    if (o.limit != null) qs.set('limit', String(o.limit));
    if (o.before != null) qs.set('before', String(o.before));
    const tail = qs.toString();
    const sep = tail ? '?' : '';
    return call(`/api/echo/conversations/${encodeURIComponent(conversationId)}/messages${sep}${tail}`);
}

// -------- memories --------

export async function listMemories({ scope, projectId, day } = {}) {
    const qs = new URLSearchParams();
    if (scope) qs.set('scope', scope);
    if (projectId) qs.set('project_id', projectId);
    if (day) qs.set('day', day);
    const tail = qs.toString();
    const sep = tail ? '?' : '';
    return call(`/api/echo/memories${sep}${tail}`);
}

export async function createMemory({ scope, projectId, day, content, source }) {
    return call('/api/echo/memories', jsonInit('POST', {
        scope,
        project_id: projectId || null,
        day: day || null,
        content,
        source: source || 'manual',
    }));
}

export async function patchMemory(id, content) {
    return call(`/api/echo/memories/${encodeURIComponent(id)}`, jsonInit('PATCH', { content }));
}

export async function deleteMemory(id) {
    return call(`/api/echo/memories/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export async function regenerateMemory({ scope, projectId, day }) {
    return call('/api/echo/memories/regenerate', jsonInit('POST', {
        scope,
        project_id: projectId || null,
        day: day || null,
    }));
}

// -------- autonomous tasks --------

export async function listAutonomousTasks() {
    return call('/api/echo/autonomous-tasks');
}

export async function createAutonomousTask(payload) {
    return call('/api/echo/autonomous-tasks', jsonInit('POST', payload));
}

export async function patchAutonomousTask(id, patch) {
    return call(`/api/echo/autonomous-tasks/${encodeURIComponent(id)}`, jsonInit('PATCH', patch));
}

export async function deleteAutonomousTask(id) {
    return call(`/api/echo/autonomous-tasks/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export async function runAutonomousNow(id) {
    return call(`/api/echo/autonomous-tasks/${encodeURIComponent(id)}/run-now`, jsonInit('POST', {}));
}

export async function listAutonomousRuns(id, limit) {
    const qs = limit != null ? `?limit=${encodeURIComponent(limit)}` : '';
    return call(`/api/echo/autonomous-tasks/${encodeURIComponent(id)}/runs${qs}`);
}

// -------- stats --------

/**
 * range: 'minute' | 'hour' | 'day' (что поддерживает /api/echo/stats).
 * Если бэкенд игнорирует параметр — это всё равно безопасный no-op.
 */
export async function getStats(range) {
    const qs = range ? `?range=${encodeURIComponent(range)}` : '';
    return call(`/api/echo/stats${qs}`);
}

// -------- daily reports (сводка дня) --------

/**
 * Список последних сводок дня. Возвращает {items:[{id,day,content,source,
 * created_at,updated_at}]}.
 *
 * @param {number} [limit] — максимум записей
 */
export async function listDailyReports(limit) {
    const qs = limit != null ? `?limit=${encodeURIComponent(limit)}` : '';
    return call(`/api/echo/daily-reports${qs}`);
}

/**
 * Сводка за конкретный день (YYYY-MM-DD). Возвращает {id,day,content,...}.
 * Если за день сводки нет — сервер отвечает 404, call() бросит Error со
 * status=404 (вызывающий должен это обработать).
 *
 * @param {string} day — дата в формате YYYY-MM-DD
 */
export async function getDailyReport(day) {
    return call(`/api/echo/daily-reports/${encodeURIComponent(day)}`);
}

/**
 * Сгенерировать (или пересоздать) сводку дня. Возвращает {id,day,content}.
 *
 * @param {string} [day] — день для генерации (YYYY-MM-DD); по умолчанию сегодня
 */
export async function generateDailyReport(day) {
    return call('/api/echo/daily-reports/generate', jsonInit('POST', day ? { day } : {}));
}

/**
 * Текущие промпты генерации сводки дня + их дефолты. Возвращает
 * {report_prompt, suggest_prompt, report_prompt_default, suggest_prompt_default}.
 */
export async function getDailyReportPrompts() {
    return call('/api/echo/daily-reports/prompts');
}

/**
 * Сохранить (или сбросить) оверрайды промптов генерации сводки дня.
 * body = {report_prompt?, suggest_prompt?}; пустая строка в поле = сброс
 * к дефолту. Отвечает актуальным состоянием (как getDailyReportPrompts).
 *
 * @param {{report_prompt?: string, suggest_prompt?: string}} body
 */
export async function saveDailyReportPrompts(body) {
    return call('/api/echo/daily-reports/prompts', jsonInit('PUT', body));
}

// -------- runs --------

export async function cancelRun(runId) {
    return call(`/api/echo/run/${encodeURIComponent(runId)}/cancel`, jsonInit('POST', {}));
}
