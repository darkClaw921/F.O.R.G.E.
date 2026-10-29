/** Shared metadata formatting, calendar filters and visible-only polling. */
export const PROVIDERS = ['codex', 'claude', 'opencode', 'copilot'];
export function providerName(id) { return ({codex: 'Codex', claude: 'Claude', opencode: 'OpenCode', copilot: 'GitHub Copilot'})[id] || String(id || 'Неизвестный агент'); }
export function el(doc, tag, cls, text) { const n = doc.createElement(tag); if (cls) n.className = cls; if (text !== undefined) n.textContent = String(text); return n; }
export function clear(n) { while (n && n.firstChild) n.removeChild(n.firstChild); }
export function number(value) { return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null; }
export function count(value) { const n = number(value); return n === null ? 'Нет данных' : new Intl.NumberFormat('ru-RU').format(n); }
export function cost(value) { const n = number(value); return n === null ? 'Нет данных' : n > 0 && n < 0.00005 ? '<0,0001 $' : new Intl.NumberFormat('ru-RU', {style: 'currency', currency: 'USD', maximumFractionDigits: n > 0 && n < 0.01 ? 4 : 2}).format(n); }
export function tokenTotal(tokens) { if (!tokens || typeof tokens !== 'object') return null; return ['input', 'output', 'cacheRead', 'cacheWrite'].reduce((n, k) => n + (number(tokens[k]) || 0), 0); }
export function clock(ms) { const n = number(ms); if (n === null) return 'Нет данных'; const s = Math.floor(n / 1000); return s >= 3600 ? `${Math.floor(s / 3600)}:${String(Math.floor(s / 60) % 60).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}` : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; }
export function duration(ms) { const n = number(ms); if (n === null) return 'Нет данных'; const m = Math.floor(n / 60000); return m >= 60 ? `${Math.floor(m / 60)} ч ${m % 60} мин` : m > 0 ? `${m} мин` : `${Math.floor(n / 1000)} с`; }
export function shortPath(path) { return String(path || '').replace(/\/$/, '').split(/[\\/]/).filter(Boolean).pop() || String(path || 'Без проекта'); }
export function localDay(date) { return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`; }
export function rangeQuery(range, from, to, now = new Date()) {
    if (range === 'custom') { if (!/^\d{4}-\d{2}-\d{2}$/.test(from || '') || !/^\d{4}-\d{2}-\d{2}$/.test(to || '') || from > to) throw new Error('Выберите корректные даты начала и конца.'); return {from, to, bucket: 'day'}; }
    if (range === 'today') return {bucket: 'day'};
    const days = [7, 30, 91].includes(Number(range)) ? Number(range) : 1;
    const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()); start.setDate(start.getDate() - days + 1);
    return {from: localDay(start), to: localDay(now), bucket: 'day'};
}
export function elapsed(task, receivedAt, now) { const base = number(task && task.elapsedMs); return base === null ? null : base + (task.state === 'working' || task.state === 'waiting' ? Math.max(0, now - receivedAt) : 0); }

/** A request generation owns its response; hidden/old generations never draw. */
export function createPoller({load, onData, onError, getQuery = () => ({}), timers = globalThis, intervalMs = 2000}) {
    let visible = false, generation = 0, timer = null, pending = null;
    async function refresh() {
        if (!visible || pending === generation) return;
        const current = generation; pending = current;
        try { const data = await load(getQuery(), {isCurrent: () => visible && generation === current}); if (visible && generation === current) onData(data); }
        catch (error) { if (visible && generation === current) onError(error); }
        finally { if (pending === current) pending = null; }
    }
    return {
        show() { if (visible) return; visible = true; generation += 1; refresh(); timer = timers.setInterval(refresh, intervalMs); },
        hide() { visible = false; generation += 1; pending = null; if (timer !== null) timers.clearInterval(timer); timer = null; },
        change() { generation += 1; pending = null; return refresh(); },
        refresh,
        isVisible: () => visible,
    };
}
