// tmux-web — Remote servers settings tab (Phase 1 ES Modules refactor)
//
// 1:1 копии renderRemotesTable / openEditRemoteRow из IIFE
// `tmux-web/static/app.js` (внутри openSettingsModal, app.js:4187, 4267).
//
// API:
//   renderRemotesTable($remotesTbody) — перерисовывает строки таблицы.
//   openEditRemoteRow(tr, srv, $remotesTbody) — заменяет строку формой inline.

import { state } from '../core/state.js';
import { escapeHtml } from '../core/utils.js';
import { fetchRemoteServers } from '../remote/servers.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { saveActiveOriginToStorage } from '../sidebar/origin-tabs.js';

export function renderRemotesTable($remotesTbody) {
    if (!$remotesTbody) return;
    $remotesTbody.innerHTML = '';
    if (state.remoteServers.length === 0) {
        const tr = document.createElement('tr');
        const td = document.createElement('td');
        td.colSpan = 4;
        td.className = 'remotes-empty';
        td.textContent = 'No remote servers configured yet.';
        tr.appendChild(td);
        $remotesTbody.appendChild(tr);
        return;
    }
    for (const srv of state.remoteServers) {
        const tr = document.createElement('tr');
        const tdLabel = document.createElement('td');
        tdLabel.textContent = srv.label || srv.id;
        tr.appendChild(tdLabel);

        const tdUrl = document.createElement('td');
        tdUrl.className = 'remotes-url';
        tdUrl.textContent = srv.url;
        tr.appendChild(tdUrl);

        const tdStatus = document.createElement('td');
        const status = state.remoteOnline.get(srv.id) || 'unknown';
        const dot = document.createElement('span');
        dot.className = 'origin-dot ' + status;
        tdStatus.appendChild(dot);
        const stxt = document.createElement('span');
        stxt.textContent = ' ' + status;
        tdStatus.appendChild(stxt);
        tr.appendChild(tdStatus);

        const tdActions = document.createElement('td');
        tdActions.className = 'remotes-actions';
        const editBtn = document.createElement('button');
        editBtn.type = 'button';
        editBtn.className = 'btn-edit-remote';
        editBtn.textContent = 'Edit';
        editBtn.addEventListener('click', () => openEditRemoteRow(tr, srv, $remotesTbody));
        tdActions.appendChild(editBtn);

        const delBtn = document.createElement('button');
        delBtn.type = 'button';
        delBtn.className = 'btn-remove';
        delBtn.textContent = 'Delete';
        delBtn.addEventListener('click', async () => {
            if (!window.confirm('Delete remote server `' + (srv.label || srv.id) + '`?')) return;
            try {
                const r = await fetch(
                    '/api/remote-servers/' + encodeURIComponent(srv.id),
                    { method: 'DELETE' },
                );
                if (!r.ok && r.status !== 204) {
                    const t = await r.text();
                    window.alert('Delete failed: ' + (t || r.status));
                    return;
                }
                if (state.activeOrigin === srv.id) {
                    state.activeOrigin = 'all';
                    saveActiveOriginToStorage();
                }
                await fetchRemoteServers();
                state.remoteSessions.delete(srv.id);
                renderRemotesTable($remotesTbody);
                renderSidebar();
            } catch (e) {
                window.alert('Network error: ' + e.message);
            }
        });
        tdActions.appendChild(delBtn);

        tr.appendChild(tdActions);
        $remotesTbody.appendChild(tr);
    }
}

export function openEditRemoteRow(tr, srv, $remotesTbody) {
    const formTr = document.createElement('tr');
    formTr.className = 'remotes-edit-row';
    const td = document.createElement('td');
    td.colSpan = 4;
    td.innerHTML = `
        <label>Label <input type="text" value="${escapeHtml(srv.label || '')}"></label>
        <label>New token (optional) <input type="password" placeholder="leave empty to keep"></label>
        <button type="button" class="primary rs-edit-save">Save</button>
        <button type="button" class="rs-edit-cancel">Cancel</button>
    `;
    formTr.appendChild(td);
    tr.replaceWith(formTr);
    const inputs = formTr.querySelectorAll('input');
    formTr.querySelector('.rs-edit-cancel').addEventListener('click', () => renderRemotesTable($remotesTbody));
    formTr.querySelector('.rs-edit-save').addEventListener('click', async () => {
        const body = { label: inputs[0].value.trim() };
        const newTok = inputs[1].value.trim();
        if (newTok) body.token = newTok;
        try {
            const r = await fetch('/api/remote-servers/' + encodeURIComponent(srv.id), {
                method: 'PATCH',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(body),
            });
            if (!r.ok) {
                const t = await r.text();
                window.alert('Update failed: ' + (t || r.status));
                return;
            }
            await fetchRemoteServers();
            renderRemotesTable($remotesTbody);
            renderSidebar();
        } catch (e) {
            window.alert('Network error: ' + e.message);
        }
    });
}
