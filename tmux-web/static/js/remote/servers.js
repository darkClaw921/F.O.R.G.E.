// tmux-web — Remote servers registry + lazy-load + health-poll
// (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - state.remoteOnline init      (app.js:6338)
//   - REMOTE_PROBE_BACKOFFS_MS/STEADY_INDEX/JITTER_MAX_MS (app.js:6353-6355)
//   - remoteProbeState              (app.js:6357)
//   - fetchRemoteServers           (app.js:6367)
//   - loadRemoteProjects           (app.js:6394)
//   - loadRemoteSessions           (app.js:6419)
//   - probeRemoteServer            (app.js:6458)
//   - startRemoteHealthPoll        (app.js:6523)
//   - stopRemoteHealthPoll         (app.js:6545)
//   - aggregateAllOrigins          (app.js:1277)

import { state } from '../core/state.js';
import { isRemoteMode } from './healthz.js';
import { renderSidebar } from '../sidebar/sidebar.js';

// runtime-инициализация state.remoteOnline (как было в legacy app.js:6338).
state.remoteOnline = new Map();

const REMOTE_PROBE_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000];
const REMOTE_PROBE_STEADY_INDEX = 1;
const REMOTE_PROBE_JITTER_MAX_MS = 1000;

/** @type {Map<string, {timer: any, step: number, inFlight: boolean}>} */
const remoteProbeState = new Map();

export async function fetchRemoteServers() {
    if (!isRemoteMode()) return;
    try {
        const r = await fetch('/api/remote-servers', { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/remote-servers failed:', r.status);
            return;
        }
        const data = await r.json();
        state.remoteServers = Array.isArray(data) ? data : [];
        const knownIds = new Set(state.remoteServers.map((s) => s.id));
        for (const id of Array.from(state.remoteOnline.keys())) {
            if (!knownIds.has(id)) state.remoteOnline.delete(id);
        }
        startRemoteHealthPoll();
    } catch (e) {
        console.warn('fetchRemoteServers failed:', e);
    }
}

export async function loadRemoteProjects(serverId) {
    if (!isRemoteMode() || !serverId) return [];
    try {
        const url = '/api/projects?server=' + encodeURIComponent(serverId);
        const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/projects?server=' + serverId + ' failed:', r.status);
            state.remoteProjects.set(serverId, []);
            return [];
        }
        const data = await r.json();
        const arr = Array.isArray(data) ? data : [];
        state.remoteProjects.set(serverId, arr);
        return arr;
    } catch (e) {
        console.warn('loadRemoteProjects(' + serverId + ') failed:', e);
        state.remoteProjects.set(serverId, []);
        return [];
    }
}

export async function loadRemoteSessions(serverId) {
    if (!isRemoteMode() || !serverId) return [];
    try {
        const url = '/api/sessions?server=' + encodeURIComponent(serverId);
        const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /api/sessions?server=' + serverId + ' failed:', r.status);
            state.remoteSessions.set(serverId, []);
            return [];
        }
        const data = await r.json();
        const arr = Array.isArray(data) ? data : [];
        state.remoteSessions.set(serverId, arr);
        return arr;
    } catch (e) {
        console.warn('loadRemoteSessions(' + serverId + ') failed:', e);
        state.remoteSessions.set(serverId, []);
        return [];
    }
}

export async function probeRemoteServer(serverId) {
    if (!isRemoteMode()) return;
    const entry = remoteProbeState.get(serverId);
    if (!entry) return;
    if (entry.inFlight) return;
    entry.inFlight = true;
    let nextStatus = 'offline';
    try {
        const r = await fetch(
            '/api/remote-servers/' + encodeURIComponent(serverId) + '/healthz',
            { headers: { 'Accept': 'application/json' } },
        );
        if (r.ok) {
            try {
                const data = await r.json();
                nextStatus = data && data.online ? 'online' : 'offline';
            } catch (_) {
                nextStatus = 'offline';
            }
        } else {
            nextStatus = 'offline';
        }
    } catch (_) {
        nextStatus = 'offline';
    } finally {
        entry.inFlight = false;
    }

    const prev = state.remoteOnline.get(serverId);
    if (prev !== nextStatus) {
        state.remoteOnline.set(serverId, nextStatus);
        renderSidebar();
    }

    if (nextStatus === 'online') {
        entry.step = REMOTE_PROBE_STEADY_INDEX;
    } else {
        entry.step = Math.min(entry.step + 1, REMOTE_PROBE_BACKOFFS_MS.length - 1);
    }

    const stillTracked = remoteProbeState.has(serverId);
    if (!stillTracked || !isRemoteMode()) return;
    const baseDelay = REMOTE_PROBE_BACKOFFS_MS[entry.step];
    const jitter = Math.floor(Math.random() * REMOTE_PROBE_JITTER_MAX_MS);
    const delay = baseDelay + jitter;
    entry.timer = setTimeout(() => {
        const e = remoteProbeState.get(serverId);
        if (!e) return;
        e.timer = null;
        probeRemoteServer(serverId);
    }, delay);
}

export function startRemoteHealthPoll() {
    if (!isRemoteMode()) return;
    const knownIds = new Set(state.remoteServers.map((s) => s.id));
    for (const srv of state.remoteServers) {
        if (!remoteProbeState.has(srv.id)) {
            remoteProbeState.set(srv.id, { timer: null, step: 0, inFlight: false });
            probeRemoteServer(srv.id);
        }
    }
    for (const id of Array.from(remoteProbeState.keys())) {
        if (!knownIds.has(id)) {
            const e = remoteProbeState.get(id);
            if (e && e.timer) clearTimeout(e.timer);
            remoteProbeState.delete(id);
            state.remoteOnline.delete(id);
        }
    }
}

export function stopRemoteHealthPoll() {
    for (const e of remoteProbeState.values()) {
        if (e.timer) clearTimeout(e.timer);
    }
    remoteProbeState.clear();
}

export function aggregateAllOrigins() {
    const out = new Map();
    out.set('local', {
        label: 'Local',
        online: 'local',
        projects: Array.isArray(state.projects) ? state.projects.slice() : [],
        sessions: Array.isArray(state.sessions) ? state.sessions.slice() : [],
    });
    for (const srv of (state.remoteServers || [])) {
        const sid = srv.id;
        out.set(sid, {
            label: srv.label || sid,
            online: state.remoteOnline.get(sid) || 'unknown',
            projects: state.remoteProjects.get(sid) || [],
            sessions: state.remoteSessions.get(sid) || [],
        });
    }
    return out;
}
