// tmux-web — Settings modal.
//
// Диспетчер табов: Notifications (global notifier-config), Themes,
// TODO behavior, Echo, Сводка дня, Интерфейс, и опциональный Remote servers
// tab в remote-mode.

import { state } from '../core/state.js';
import { buildModalOverlay } from '../core/utils.js';
import { isRemoteMode } from '../remote/healthz.js';
import { fetchRemoteServers } from '../remote/servers.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { loadThemesIntoPanel } from '../themes/panel.js';
import { buildNotificationsForm, fetchNotifierConfig } from './notifications-tab.js';
import { renderRemotesTable } from './remotes-tab.js';
import { buildTodoBehaviorForm } from './todo-tab.js';
import { fetchUserSettings } from './user-settings-api.js';
import { renderEchoSettingsTab } from '../echo/settings.js';
import { renderDailySummaryTab } from './daily-summary-tab.js';
import { buildInterfaceForm } from './interface-tab.js';

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
            <button type="button" class="modal-tab-btn" data-tab="daily-summary" role="tab">Сводка дня</button>
            <button type="button" class="modal-tab-btn" data-tab="interface" role="tab">Интерфейс</button>
            ${remoteTabBtn}
        </div>
        <div class="modal-tab-panel" id="ps-panel-notifications" data-panel="notifications">
            <h2>Notifications</h2>
            <div class="notifier-content" id="ps-notifier-content">
                <div class="themes-loading">Loading…</div>
            </div>
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
        <div class="modal-tab-panel" id="ps-panel-daily-summary" data-panel="daily-summary" hidden>
            <h2>Сводка дня</h2>
            <div class="daily-summary-settings-content" id="ps-daily-summary-content"></div>
        </div>
        <div class="modal-tab-panel" id="ps-panel-interface" data-panel="interface" hidden>
            <h2>Интерфейс</h2>
            <div class="interface-settings-content" id="ps-interface-content"></div>
        </div>
        ${remotePanel}
        <div class="modal-actions">
            <button type="button" id="ps-close" class="primary">Close</button>
        </div>
    `;
    overlay.appendChild(card);
    document.body.appendChild(overlay);

    const $notifierContent = card.querySelector('#ps-notifier-content');
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
    const $dailySummaryContent = card.querySelector('#ps-daily-summary-content');
    const $interfaceContent = card.querySelector('#ps-interface-content');

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
    const dailySummaryTabState = {
        loaded: false,
    };
    const interfaceTabState = {
        loaded: false,
    };

    // Закрытие модалки — определено заранее, т.к. вкладка «Сводка дня»
    // передаёт его как onClose в кнопку «Открыть страницу».
    const close = () => overlay.remove();

    // «Сводка дня» tab: рендерим один раз при первом показе. Вкладка не хранит
    // пользовательских настроек — только действия (генерация / открытие страницы),
    // поэтому fetchUserSettings не нужен.
    const renderDailySummaryPanel = () => {
        if (dailySummaryTabState.loaded) return;
        dailySummaryTabState.loaded = true;
        renderDailySummaryTab($dailySummaryContent, { onClose: close });
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

    // Интерфейс tab: тумблеры opt-in фич (Cmd-подсказки, «Следующий шаг»).
    // Тот же ленивый паттерн, что у TODO/Echo — общий кеш state.userSettings.
    // Отдельного «применения» настройки не требуется: консьюмеры (hotkeys.js
    // через window.ForgeApp.state, sessions.js) читают флаги лениво в точке
    // использования, а updateUserSettings уже обновил state.userSettings.
    const renderInterfacePanel = async () => {
        if (interfaceTabState.loaded) return;
        interfaceTabState.loaded = true;
        $interfaceContent.innerHTML = '<div class="themes-loading">Loading settings…</div>';
        if (state.userSettings === null) {
            try {
                await fetchUserSettings();
            } catch (_) { /* fetchUserSettings swallows errors itself */ }
        }
        $interfaceContent.innerHTML = '';
        // Если state.userSettings всё ещё null (backend down) — пустой объект:
        // buildInterfaceForm покажет обе фичи выключенными, как и дефолт.
        const settingsArg = state.userSettings || {};
        $interfaceContent.appendChild(buildInterfaceForm(settingsArg, (updated) => {
            if (updated) state.userSettings = updated;
        }));
    };

    const notifierState = {
        loaded: false,
    };

    const renderNotifierPanel = async () => {
        if (notifierState.loaded) return;
        notifierState.loaded = true;
        $notifierContent.innerHTML = '<div class="themes-loading">Loading…</div>';
        const res = await fetchNotifierConfig();
        $notifierContent.innerHTML = '';
        if (!res || !res.ok) {
            // Загрузка не удалась — НЕ показываем форму с дефолтами (иначе Save
            // затрёт реальный конфиг). Показываем ошибку и кнопку Retry;
            // редактирование/сохранение заблокировано до успешной загрузки.
            const err = document.createElement('div');
            err.className = 'settings-load-error';
            const msg = document.createElement('div');
            msg.textContent = 'Не удалось загрузить настройки: '
                + (res && res.error ? res.error : 'неизвестная ошибка');
            err.appendChild(msg);
            const retry = document.createElement('button');
            retry.type = 'button';
            retry.className = 'primary';
            retry.textContent = 'Повторить';
            retry.addEventListener('click', () => {
                // Сбрасываем флаг, чтобы повторно загрузить.
                notifierState.loaded = false;
                renderNotifierPanel();
            });
            err.appendChild(retry);
            $notifierContent.appendChild(err);
            return;
        }
        $notifierContent.appendChild(buildNotificationsForm(res.config, () => {
            // no-op: form keeps текущие значения после успешного PATCH
        }));
    };

    // Notifications — дефолтная вкладка; рендерим её сразу.
    renderNotifierPanel();

    const showTab = (name) => {
        $tabBtns.forEach((btn) => {
            const isActive = btn.dataset.tab === name;
            btn.classList.toggle('active', isActive);
            btn.setAttribute('aria-selected', isActive ? 'true' : 'false');
        });
        $panels.forEach((p) => {
            p.hidden = p.dataset.panel !== name;
        });
        if (name === 'notifications' && !notifierState.loaded) {
            renderNotifierPanel();
        }
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
        if (name === 'daily-summary') {
            renderDailySummaryPanel();
        }
        if (name === 'interface') {
            renderInterfacePanel();
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

    if (initialTab && (initialTab === 'notifications' || initialTab === 'themes' || initialTab === 'todo' || initialTab === 'echo' || initialTab === 'daily-summary' || initialTab === 'interface' || (initialTab === 'remotes' && isRemoteMode()))) {
        showTab(initialTab);
    }

    card.querySelector('#ps-close').addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });
}
