/** All agent requests use FORGE's auth/CSRF and origin-aware API entry point. */
import { apiFetch } from '../core/api.js';
const ROOT = '/api/agents';
const KEYS = ['from', 'to', 'provider', 'profile', 'project', 'limit', 'offset', 'bucket'];
export function queryString(query = {}) { const p = new URLSearchParams(); for (const key of KEYS) if (query[key] !== undefined && query[key] !== null && query[key] !== '') p.set(key, String(query[key])); return p.size ? `?${p}` : ''; }
export function createAgentsApi({request = apiFetch, origin} = {}) {
    async function call(path, init) {
        const response = await request(`${ROOT}${path}`, init, origin);
        if (response.status === 204) return null;
        let body = null;
        try { body = (response.headers.get('content-type') || '').includes('application/json') ? await response.json() : await response.text(); } catch (_) { /* malformed error stays a status */ }
        if (!response.ok) { const error = new Error(body && body.error || typeof body === 'string' && body || `HTTP ${response.status}`); error.status = response.status; throw error; }
        return body;
    }
    const json = (method, body) => ({method, headers: {'Content-Type': 'application/json', Accept: 'application/json'}, body: JSON.stringify(body)});
    return {
        status: () => call('/status'), live: () => call('/live'),
        summary: (q) => call(`/summary${queryString(q)}`), projects: (q) => call(`/projects${queryString(q)}`), sessions: (q) => call(`/sessions${queryString(q)}`),
        rescan: () => call('/rescan', {method: 'POST', headers: {Accept: 'application/json'}}),
        config: () => call('/config'), configure: (config) => call('/config', json('PUT', config)),
        bindings: () => call('/bindings'), inventory: () => call('/inventory'),
        bind: (sessionId, paneId) => call('/bindings', json('POST', {sessionId, paneId})),
        unbind: (sessionId) => call('/bindings', json('DELETE', {sessionId})),
        refreshPrices: () => call('/prices/refresh', {method: 'POST', headers: {Accept: 'application/json'}}),
    };
}
export const agentsApi = createAgentsApi();
