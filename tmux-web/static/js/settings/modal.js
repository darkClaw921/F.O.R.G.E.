// tmux-web — Settings modal (Phase 1 ES Modules refactor)
//
// 1:1 копия openSettingsModal из IIFE `tmux-web/static/app.js` (3940-4402).
// Локальные функции (renderRemotesTable / openEditRemoteRow) вынесены в
// remotes-tab.js; здесь — диспетчер табов + Notifications список проектов.

import { state } from '../core/state.js';
import { buildModalOverlay } from '../core/utils.js';
import { isRemoteMode } from '../remote/healthz.js';
import { fetchRemoteServers } from '../remote/servers.js';
import { fetchProjects } from '../projects/projects.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { loadThemesIntoPanel } from '../themes/panel.js';
import { buildNotificationsForm } from './notifications-tab.js';
import { renderRemotesTable } from './remotes-tab.js';
import { buildTodoBehaviorForm } from './todo-tab.js';
import { fetchUserSettings } from './user-settings-api.js';
import { renderEchoSettingsTab } from '../echo/settings.js';

export function openSettingsModal(initialTab) {
    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card settings-modal';
    const remoteTabBtn = isRemoteMode()
        ? '<button type="button" class="modal-tab-btn" data-tab="remotes" role="tab">Remote servers</button>'
        : '';
    const remotePanel = isRemoteMode()
        ? `<div class="modal-tab-panel" id="ps-panel-remotes" data-panel="remotes" hidden>
            <h2>Remote servers</h2>
            <div id="ps-remotes-content">
                <table class="remotes-table" id="ps-remotes-table">
                    <thead><tr><th>Label</th><th>URL</th><th>Status</th><th></th></tr></thead>
                    <tbody></tbody>
                </table>
                <div class="remotes-add">
                    <h3>Add new server</h3>
                    <label>Label<br><input type="text" id="rs-label" placeholder="Office laptop"></label>
                    <label>URL<br><input type="text" id="rs-url" placeholder="http://192.168.1.5:7331"></label>
                    <label>Token<br>
                        <span class="rs-token-wrap">
                            <input type="password" id="rs-token" placeholder="Paste token">
                            <button type="button" id="rs-token-toggle" class="rs-token-toggle" title="Show/hide">👁</button>
                        </span>
                    </label>
                    <div class="rs-actions">
                        <button type="button" id="rs-test">Test connection</button>
                        <button type="button" id="rs-save" class="primary" disabled>Save</button>
                        <span class="rs-test-status" id="rs-test-status"></span>
                    </div>
                    <details class="rs-help">
                        <summary>How to pair?</summary>
                        <div class="rs-help-body">
                            На удалённой машине запустите:
                            <pre><code>devforge pair --generate</code></pre>
                            Скопируйте URL и token из вывода и вставьте сюда.
                        </div>
                    </details>
                </div>
            </div>
        </div>`
        : '';
    card.innerHTML = `
        <div class="modal-tabs" role="tablist">
            <button type="button" class="modal-tab-btn active" data-tab="notifications" role="tab">Notifications</button>
            <button type="button" class="modal-tab-btn" data-tab="themes" role="tab">Themes</button>
            <button type="button" class="modal-tab-btn" data-tab="todo" role="tab">TODO behavior</button>
            <button type="button" class="modal-tab-btn" data-tab="echo" role="tab">Echo</button>
            ${remoteTabBtn}
        </div>
        <div class="modal-tab-panel" id="ps-panel-notifications" data-panel="notifications">
            <h2>Projects</h2>
            <ul class="modal-projects" id="ps-list"></ul>
        </div>
        <div class="modal-tab-panel" id="ps-panel-themes" data-panel="themes" hidden>
            <h2>Themes</h2>
            <div class="themes-content" id="ps-themes-content">
                <div class="themes-loading">Loading themes…</div>
            </div>
        </div>
        <div class="modal-tab-panel" id="ps-panel-todo" data-panel="todo" hidden>
            <h2>TODO behavior</h2>
            <div class="todo-settings-content" id="ps-todo-content"></div>
        </div>
        <div class="modal-tab-panel" id="ps-panel-echo" data-panel="echo" hidden>
            <h2>Echo</h2>
            <div class="echo-settings-content" id="ps-echo-content"></div>
        </div>
        ${remotePanel}
        <div class="modal-actions">
            <button type="button" id="ps-close" class="primary">Close</button>
        </div>
    `;
    overlay.appendChild(card);
    document.body.appendChild(overlay);

    const $list = card.querySelector('#ps-list');
    const $tabBtns = card.querySelectorAll('.modal-tab-btn');
    const $panels = card.querySelectorAll('.modal-tab-panel');
    const $themesContent = card.querySelector('#ps-themes-content');

    const themesState = {
        loaded: false,
        data: null,
    };

    const $remotesTbody = card.querySelector('#ps-remotes-table tbody');
    const $todoContent = card.querySelector('#ps-todo-content');
    const $echoContent = card.querySelector('#ps-echo-content');

    // TODO behavior tab state: рендерим форму один раз при первом клике.
    // userSettings fetch выполняется лениво, если bootstrap-preload не успел
    // или вернул null. defaults используются как graceful-fallback, если
    // backend down — тогда форма всё равно показывается.
    const todoState = {
        loaded: false,
    };
    const echoTabState = {
        loaded: false,
    };

    const renderTodoPanel = async () => {
        if (todoState.loaded) return;
        todoState.loaded = true;
        $todoContent.innerHTML = '<div class="themes-loading">Loading settings…</div>';
        if (state.userSettings === null) {
            try {
                await fetchUserSettings();
            } catch (_) { /* fetchUserSettings swallows errors itself */ }
        }
        $todoContent.innerHTML = '';
        // Если state.userSettings всё ещё null (backend down) — передаём пустой
        // объект; buildTodoBehaviorForm подставит дефолты из своей константы.
        const settingsArg = state.userSettings || {};
        $todoContent.appendChild(buildTodoBehaviorForm(settingsArg, (updated) => {
            if (updated) state.userSettings = updated;
        }));
    };

    // Echo tab: ленивая загрузка user-settings (если ещё не подгружены) и
    // рендер renderEchoSettingsTab. Использует тот же кеш state.userSettings,
    // что и TODO panel — fetch выполняется один раз на сессию.
    const renderEchoPanel = async () => {
        if (echoTabState.loaded) return;
        echoTabState.loaded = true;
        $echoContent.innerHTML = '<div class="themes-loading">Loading settings…</div>';
        if (state.userSettings === null) {
            try {
                await fetchUserSettings();
            } catch (_) { /* fetchUserSettings swallows errors itself */ }
        }
        const settingsArg = state.userSettings || {};
        renderEchoSettingsTab($echoContent, settingsArg, (updated) => {
            if (updated) state.userSettings = updated;
        });
    };

    const showTab = (name) => {
        $tabBtns.forEach((btn) => {
            const isActive = btn.dataset.tab === name;
            btn.classList.toggle('active', isActive);
            btn.setAttribute('aria-selected', isActive ? 'true' : 'false');
        });
        $panels.forEach((p) => {
            p.hidden = p.dataset.panel !== name;
        });
        if (name === 'themes' && !themesState.loaded) {
            loadThemesIntoPanel($themesContent, themesState);
        }
        if (name === 'remotes') {
            renderRemotesTable($remotesTbody);
        }
        if (name === 'todo') {
            renderTodoPanel();
        }
        if (name === 'echo') {
            renderEchoPanel();
        }
    };
    $tabBtns.forEach((btn) => {
        btn.addEventListener('click', () => showTab(btn.dataset.tab));
    });

    const $rsLabel = card.querySelector('#rs-label');
    const $rsUrl = card.querySelector('#rs-url');
    const $rsToken = card.querySelector('#rs-token');
    const $rsTokenToggle = card.querySelector('#rs-token-toggle');
    const $rsTest = card.querySelector('#rs-test');
    const $rsSave = card.querySelector('#rs-save');
    const $rsTestStatus = card.querySelector('#rs-test-status');

    if ($rsTokenToggle && $rsToken) {
        $rsTokenToggle.addEventListener('click', () => {
            $rsToken.type = $rsToken.type === 'password' ? 'text' : 'password';
        });
    }

    const validate = () => {
        const label = ($rsLabel.value || '').trim();
        const url = ($rsUrl.value || '').trim();
        const token = ($rsToken.value || '').trim();
        return label && (url.startsWith('http://') || url.startsWith('https://')) && token;
    };
    const refreshSaveBtn = () => {
        $rsSave.disabled = !validate();
    };
    [$rsLabel, $rsUrl, $rsToken].forEach((el) => {
        if (el) el.addEventListener('input', () => {
            $rsTestStatus.textContent = '';
            refreshSaveBtn();
        });
    });

    if ($rsTest) {
        $rsTest.addEventListener('click', async () => {
            if (!validate()) {
                $rsTestStatus.textContent = 'Fill label, URL (http/https) and token';
                $rsTestStatus.className = 'rs-test-status error';
                return;
            }
            $rsTestStatus.textContent = 'Pinging…';
            $rsTestStatus.className = 'rs-test-status pending';
            try {
                const r = await fetch('/api/remote-servers', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({
                        label: $rsLabel.value.trim(),
                        url: $rsUrl.value.trim(),
                        token: $rsToken.value.trim(),
                    }),
                });
                if (!r.ok) {
                    const t = await r.text();
                    $rsTestStatus.textContent = 'Create failed: ' + (t || r.status);
                    $rsTestStatus.className = 'rs-test-status error';
                    return;
                }
                const created = await r.json();
                const h = await fetch(
                    '/api/remote-servers/' + encodeURIComponent(created.id) + '/healthz',
                    { headers: { 'Accept': 'application/json' } },
                );
                let online = false;
                let detail = '';
                if (h.ok) {
                    const data = await h.json();
                    online = !!data.online;
                    if (!online && data.error) detail = ' (' + data.error + ')';
                }
                if (online) {
                    $rsTestStatus.textContent = 'OK — saved as `' + created.id + '`';
                    $rsTestStatus.className = 'rs-test-status ok';
                    $rsLabel.value = '';
                    $rsUrl.value = '';
                    $rsToken.value = '';
                    refreshSaveBtn();
                    await fetchRemoteServers();
                    renderRemotesTable($remotesTbody);
                    renderSidebar();
                } else {
                    $rsTestStatus.textContent = 'Offline' + detail + '. Запись сохранена — можно проверить позже.';
                    $rsTestStatus.className = 'rs-test-status warn';
                    await fetchRemoteServers();
                    renderRemotesTable($remotesTbody);
                }
            } catch (e) {
                $rsTestStatus.textContent = 'Network error: ' + e.message;
                $rsTestStatus.className = 'rs-test-status error';
            }
        });
    }

    if ($rsSave) {
        $rsSave.addEventListener('click', async () => {
            if (!validate()) return;
            try {
                const r = await fetch('/api/remote-servers', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({
                        label: $rsLabel.value.trim(),
                        url: $rsUrl.value.trim(),
                        token: $rsToken.value.trim(),
                    }),
                });
                if (!r.ok) {
                    const t = await r.text();
                    $rsTestStatus.textContent = 'Save failed: ' + (t || r.status);
                    $rsTestStatus.className = 'rs-test-status error';
                    return;
                }
                $rsLabel.value = '';
                $rsUrl.value = '';
                $rsToken.value = '';
                $rsTestStatus.textContent = '';
                refreshSaveBtn();
                await fetchRemoteServers();
                renderRemotesTable($remotesTbody);
                renderSidebar();
            } catch (e) {
                $rsTestStatus.textContent = 'Network error: ' + e.message;
                $rsTestStatus.className = 'rs-test-status error';
            }
        });
    }

    const expanded = new Set();

    const renderList = () => {
        $list.innerHTML = '';
        for (const p of state.projects) {
            const li = document.createElement('li');
            li.className = 'modal-project-item' + (p.active ? ' active' : '');

            const row = document.createElement('div');
            row.className = 'modal-project-row';

            const meta = document.createElement('div');
            meta.className = 'modal-project-meta';
            const name = document.createElement('div');
            name.className = 'modal-project-name';
            name.textContent = p.name + (p.active ? ' (active)' : '');
            const sub = document.createElement('div');
            sub.className = 'modal-project-sub';
            sub.textContent = `${p.id} · ${p.path}`;
            meta.appendChild(name);
            meta.appendChild(sub);
            row.appendChild(meta);

            const settingsBtn = document.createElement('button');
            settingsBtn.type = 'button';
            settingsBtn.className = 'btn-settings';
            const isOpen = expanded.has(p.id);
            settingsBtn.textContent = isOpen ? '▾ notifications' : '▸ notifications';
            settingsBtn.title = 'Настройки нотификаций';
            settingsBtn.addEventListener('click', () => {
                if (expanded.has(p.id)) {
                    expanded.delete(p.id);
                } else {
                    expanded.add(p.id);
                }
                renderList();
            });
            row.appendChild(settingsBtn);

            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'btn-remove';
            btn.textContent = 'remove';
            btn.disabled = !!p.active;
            btn.title = p.active ? 'Нельзя удалить активный проект' : `Удалить ${p.id}`;
            btn.addEventListener('click', async () => {
                if (!window.confirm(`Удалить проект "${p.id}"?`)) return;
                try {
                    const r = await fetch('/api/projects/' + encodeURIComponent(p.id), {
                        method: 'DELETE',
                    });
                    if (!r.ok && r.status !== 204) {
                        const text = await r.text();
                        window.alert('Не удалось удалить: ' + (text || r.status));
                        return;
                    }
                    await fetchProjects();
                    renderList();
                } catch (e) {
                    window.alert('Ошибка запроса: ' + e.message);
                }
            });
            row.appendChild(btn);
            li.appendChild(row);

            if (isOpen) {
                li.appendChild(buildNotificationsForm(p, () => {
                    renderList();
                }));
            }

            $list.appendChild(li);
        }
    };
    renderList();

    if (initialTab && (initialTab === 'themes' || initialTab === 'todo' || initialTab === 'echo' || (initialTab === 'remotes' && isRemoteMode()))) {
        showTab(initialTab);
    }

    const close = () => overlay.remove();
    card.querySelector('#ps-close').addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });
}
