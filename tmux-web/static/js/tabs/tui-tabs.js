// tmux-web — TUI tabs framework (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - createTuiTab               (app.js:2058)
//   - LAZYGIT_INSTALL_ENTRIES    (app.js:2499)
//   - LAZYDOCKER_INSTALL_ENTRIES (app.js:2510)
//   - TELESCOPE_INSTALL_ENTRIES  (app.js:2524)
//   - initTuiTabs                (app.js:2538)
//   - getActiveProject           (app.js:2655)
//   - mountGitTerm/openLazygitForActiveProject/connectGitWs/closeGitWs/
//     gitSwitchCwd/showGitBanner/hideGitBanner/retryGitConnection
//     (app.js:2680-2718)
//   - sendToActivePty            (app.js:6714)

import { state } from '../core/state.js';
import { withWsToken } from '../core/auth.js';
import { isRemoteMode } from '../remote/healthz.js';
import { detectClientOS, copyToClipboardSafe, fallbackCopy } from '../core/utils.js';
import { mapTermTheme } from '../terminal/theme-mapper.js';
import {
    $gitTermEl, $gitPlaceholder, $gitError, $gitErrorText,
    $gitErrorRetry, $gitErrorClose, $gitInstallHelp, $gitInstallList,
    $dockerTermEl, $dockerPlaceholder, $dockerError, $dockerErrorText,
    $dockerErrorRetry, $dockerErrorClose, $dockerInstallHelp, $dockerInstallList,
    $telescopeTermEl, $telescopePlaceholder, $telescopeError, $telescopeErrorText,
    $telescopeErrorRetry, $telescopeErrorClose, $telescopeInstallHelp, $telescopeInstallList,
    $telescopeChannelBar,
} from '../core/dom.js';

export function createTuiTab(opts) {
    const name = opts.name;
    const wsPath = opts.wsPath;
    const refs = opts.refs || {};
    const installHelp = opts.installHelp || null;
    const activeTabName = opts.activeTabName || name;
    const autoReconnectOnClose = !!opts.autoReconnectOnClose;
    const autoReconnectDelayMs = typeof opts.autoReconnectDelayMs === 'number'
        ? opts.autoReconnectDelayMs
        : 150;

    const tabState = {
        term: null,
        fit: null,
        ws: null,
        mounted: false,
        currentCwd: null,
        errorSticky: false,
        resizeObserver: null,
    };

    function mount() {
        if (tabState.mounted && tabState.term) return tabState.term;
        const Terminal = window.Terminal;
        const FitAddon = window.FitAddon && window.FitAddon.FitAddon;
        if (!Terminal || !FitAddon) {
            console.error('[' + name + '] xterm.js / FitAddon not loaded');
            return null;
        }
        if (!refs.termEl) {
            console.error('[' + name + '] term element missing');
            return null;
        }

        const fallbackTheme = {
            background: '#000000',
            foreground: '#d8dee9',
            cursor: '#d8dee9',
            selectionBackground: '#3a4356',
        };
        const termTheme = (state.activeTheme && typeof mapTermTheme === 'function')
            ? mapTermTheme(state.activeTheme)
            : fallbackTheme;

        const term = new Terminal({
            cursorBlink: true,
            fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
            fontSize: 13,
            scrollback: 5000,
            allowProposedApi: true,
            macOptionClickForcesSelection: true,
            rightClickSelectsWord: true,
            theme: termTheme || fallbackTheme,
        });
        const fit = new FitAddon();
        term.loadAddon(fit);
        term.open(refs.termEl);
        try { fit.fit(); } catch (e) { console.warn('[' + name + '] initial fit failed', e); }

        term.attachCustomKeyEventHandler((ev) => {
            if (ev.type !== 'keydown') return true;
            const isMac = navigator.platform.toUpperCase().includes('MAC');
            const copyShortcut =
                (isMac && ev.metaKey && !ev.ctrlKey && ev.key === 'c') ||
                (!isMac && ev.ctrlKey && ev.shiftKey && ev.key.toUpperCase() === 'C');
            if (copyShortcut) {
                const sel = term.getSelection();
                if (sel && navigator.clipboard && navigator.clipboard.writeText) {
                    navigator.clipboard.writeText(sel).catch((e) => {
                        console.warn('[' + name + '] clipboard.writeText failed', e);
                    });
                    return false;
                }
            }
            return true;
        });

        let lastCopied = '';
        term.onSelectionChange(() => {
            const sel = term.getSelection();
            if (sel && sel.length > 0 && sel !== lastCopied) {
                lastCopied = sel;
                if (navigator.clipboard && navigator.clipboard.writeText) {
                    navigator.clipboard.writeText(sel).catch((e) => {
                        console.debug('[' + name + '] auto-copy failed', e);
                    });
                } else {
                    fallbackCopy(sel);
                }
            }
        });

        term.onData((data) => {
            if (window.QuickCmd && typeof window.QuickCmd.onPtyInput === 'function') {
                try { window.QuickCmd.onPtyInput(data); } catch (e) { console.debug('[quick-cmd] onPtyInput failed', e); }
            }
            const ws = tabState.ws;
            if (ws && ws.readyState === WebSocket.OPEN) {
                try {
                    ws.send(state.encoder.encode(data));
                } catch (e) {
                    console.warn('[' + name + '] ws.send (input) failed', e);
                }
            }
        });

        term.onResize(({ cols, rows }) => {
            const ws = tabState.ws;
            if (ws && ws.readyState === WebSocket.OPEN) {
                try {
                    ws.send(JSON.stringify({ type: 'resize', cols, rows }));
                } catch (e) {
                    console.warn('[' + name + '] ws.send (resize) failed', e);
                }
            }
        });

        try {
            const ro = new ResizeObserver(() => {
                if (state.activeTab !== activeTabName) return;
                try { tabState.fit && tabState.fit.fit(); } catch (_) {}
            });
            ro.observe(refs.termEl);
            tabState.resizeObserver = ro;
        } catch (_) {}
        window.addEventListener('resize', () => {
            if (state.activeTab !== activeTabName) return;
            try { tabState.fit && tabState.fit.fit(); } catch (_) {}
        });

        tabState.term = term;
        tabState.fit = fit;
        tabState.mounted = true;
        return term;
    }

    function connect(cwd) {
        if (tabState.ws && tabState.ws.readyState === WebSocket.OPEN && tabState.currentCwd === cwd) {
            return;
        }
        if (tabState.ws && tabState.ws.readyState === WebSocket.CONNECTING && tabState.currentCwd === cwd) {
            return;
        }
        if (tabState.ws) {
            try {
                tabState.ws.onopen = null;
                tabState.ws.onmessage = null;
                tabState.ws.onerror = null;
                tabState.ws.onclose = null;
                tabState.ws.close();
            } catch (_) {}
            tabState.ws = null;
        }
        if (!tabState.term) {
            console.warn('[' + name + '] connect: term not mounted');
            return;
        }
        const cols = tabState.term.cols || 80;
        const rows = tabState.term.rows || 24;
        const proto = location.protocol === 'https:' ? 'wss' : 'ws';
        const srv = (isRemoteMode()
            && state.activeOrigin
            && state.activeOrigin !== 'local'
            && state.activeOrigin !== 'all')
            ? `&server=${encodeURIComponent(state.activeOrigin)}`
            : '';
        const chParam = (tabState.channel && String(tabState.channel).trim())
            ? `&channel=${encodeURIComponent(tabState.channel)}`
            : '';
        const url = `${proto}://${location.host}${wsPath}?cwd=${encodeURIComponent(cwd)}&cols=${cols}&rows=${rows}${srv}${chParam}`;

        let ws;
        try {
            ws = new WebSocket(withWsToken(url));
        } catch (e) {
            console.warn('[' + name + '] WebSocket constructor failed', e);
            showBanner('Failed to open WebSocket: ' + (e && e.message ? e.message : String(e)));
            return;
        }
        ws.binaryType = 'arraybuffer';
        tabState.ws = ws;
        tabState.currentCwd = cwd;
        tabState.errorSticky = false;

        ws.onopen = () => {
            hideBanner();
            try {
                if (tabState.term) {
                    ws.send(JSON.stringify({
                        type: 'resize',
                        cols: tabState.term.cols,
                        rows: tabState.term.rows,
                    }));
                }
            } catch (_) {}
        };

        ws.onmessage = (ev) => {
            const data = ev.data;
            if (data instanceof ArrayBuffer) {
                try {
                    tabState.term.write(new Uint8Array(data));
                } catch (e) {
                    console.warn('[' + name + '] term.write failed', e);
                }
                return;
            }
            if (typeof data === 'string') {
                let payload;
                try {
                    payload = JSON.parse(data);
                } catch (_) {
                    console.warn('[' + name + '] non-JSON text frame:', data);
                    return;
                }
                if (payload && payload.type === 'error' && typeof payload.message === 'string') {
                    let msg = payload.message;
                    const lower = msg.toLowerCase();
                    const binary = installHelp && installHelp.binary ? installHelp.binary.toLowerCase() : null;
                    const notFound = binary
                        && lower.includes(binary)
                        && (lower.includes('not found') || lower.includes('no such file'));
                    if (notFound && installHelp && installHelp.notFoundMsg) {
                        msg = installHelp.notFoundMsg;
                    }
                    showBanner(msg, { showInstall: !!notFound });
                    tabState.errorSticky = true;
                }
            }
        };

        ws.onerror = (ev) => {
            console.debug('[' + name + '] ws error', ev);
        };

        ws.onclose = (ev) => {
            if (tabState.ws === ws) {
                tabState.ws = null;
            }
            if (tabState.errorSticky || state.activeTab !== activeTabName) {
                return;
            }
            const reason = ev && ev.reason ? ev.reason : '';
            const code = ev && typeof ev.code === 'number' ? ev.code : 0;
            if (autoReconnectOnClose) {
                const cwd = tabState.currentCwd;
                if (cwd) {
                    setTimeout(() => {
                        if (state.activeTab !== activeTabName) return;
                        if (tabState.ws) return;
                        if (tabState.term) {
                            try { tabState.term.clear(); } catch (_) {}
                            try { tabState.term.reset(); } catch (_) {}
                        }
                        connect(cwd);
                    }, autoReconnectDelayMs);
                    return;
                }
            }
            if (code !== 1000 && code !== 1001) {
                showBanner('Connection lost' + (reason ? ': ' + reason : '') + '. Press Retry.');
            }
        };
    }

    function close(reason) {
        if (!tabState.ws) return;
        try {
            tabState.ws.onopen = null;
            tabState.ws.onmessage = null;
            tabState.ws.onerror = null;
            tabState.ws.onclose = null;
            tabState.ws.close(1000, reason || 'closed');
        } catch (e) {
            console.debug('[' + name + '] close failed', e);
        }
        tabState.ws = null;
    }

    function switchCwd(newCwd) {
        if (!newCwd) return;
        if (!tabState.ws || tabState.ws.readyState !== WebSocket.OPEN) {
            connect(newCwd);
            return;
        }
        try {
            if (tabState.term) {
                try { tabState.term.clear(); } catch (_) {}
            }
            tabState.ws.send(JSON.stringify({ type: 'switch_cwd', cwd: newCwd }));
            tabState.currentCwd = newCwd;
        } catch (e) {
            console.warn('[' + name + '] switch_cwd send failed, falling back to reconnect', e);
            close('switch_cwd failed');
            connect(newCwd);
        }
    }

    function showBanner(message, bannerOpts) {
        if (!refs.errorEl || !refs.errorTextEl) return;
        refs.errorTextEl.textContent = message;
        refs.errorEl.hidden = false;
        const showInstall = !!(bannerOpts && bannerOpts.showInstall);
        if (showInstall) {
            renderInstallHelp();
            if (refs.installHelpEl) refs.installHelpEl.hidden = false;
        } else if (refs.installHelpEl) {
            refs.installHelpEl.hidden = true;
        }
    }

    function hideBanner() {
        if (!refs.errorEl) return;
        refs.errorEl.hidden = true;
        if (refs.installHelpEl) refs.installHelpEl.hidden = true;
        tabState.errorSticky = false;
    }

    function renderInstallHelp() {
        if (!refs.installListEl || !installHelp || !Array.isArray(installHelp.entries)) return;
        const detected = detectClientOS();
        const entries = installHelp.entries;

        const isDetected = (id) => {
            if (!detected) return false;
            if (detected === 'mac' && (id === 'mac' || id === 'mac-port')) return true;
            if (detected === 'linux' && id.startsWith('linux-')) return true;
            if (detected === 'windows' && id.startsWith('windows')) return true;
            return false;
        };

        const sorted = entries.slice().sort(
            (a, b) => Number(isDetected(b.id)) - Number(isDetected(a.id))
        );

        refs.installListEl.innerHTML = '';
        for (const e of sorted) {
            const li = document.createElement('li');
            const label = document.createElement('span');
            label.className = 'os-label' + (isDetected(e.id) ? ' detected' : '');
            label.textContent = e.label;
            const cmd = document.createElement('code');
            cmd.className = 'os-cmd';
            cmd.textContent = e.cmd;
            const copy = document.createElement('button');
            copy.type = 'button';
            copy.className = 'os-copy';
            copy.textContent = 'Copy';
            copy.addEventListener('click', () => {
                copyToClipboardSafe(e.cmd).then((ok) => {
                    if (!ok) return;
                    const prev = copy.textContent;
                    copy.textContent = 'Copied';
                    copy.classList.add('copied');
                    setTimeout(() => {
                        copy.textContent = prev;
                        copy.classList.remove('copied');
                    }, 1400);
                });
            });
            li.appendChild(label);
            li.appendChild(cmd);
            li.appendChild(copy);
            refs.installListEl.appendChild(li);
        }
    }

    function retry() {
        hideBanner();
        close('retry');
        tabState.currentCwd = null;
        openForActiveProject();
    }

    function openForActiveProject() {
        // Резолвер cwd: вкладка передаёт resolveCwd (sessionCwdOrNull) —
        // lazygit/docker/telescope открывается в cwd активной tmux-сессии.
        let cwd = null;
        if (typeof opts.resolveCwd === 'function') {
            try { cwd = opts.resolveCwd(); } catch (_) { cwd = null; }
        }
        if (!cwd) {
            if (refs.placeholderEl) refs.placeholderEl.hidden = false;
            if (refs.termEl) refs.termEl.hidden = true;
            close('no cwd');
            return;
        }
        if (refs.placeholderEl) refs.placeholderEl.hidden = true;
        if (refs.termEl) refs.termEl.hidden = false;
        const term = mount();
        if (!term) {
            showBanner('Failed to initialize terminal (xterm.js not loaded)');
            return;
        }
        requestAnimationFrame(() => {
            try { tabState.fit && tabState.fit.fit(); } catch (_) {}
            connect(cwd);
            try { term.focus(); } catch (_) {}
        });
    }

    if (refs.retryBtn) refs.retryBtn.addEventListener('click', retry);
    if (refs.closeBtn) refs.closeBtn.addEventListener('click', hideBanner);

    tabState.mount = mount;
    tabState.connect = connect;
    tabState.close = close;
    tabState.switchCwd = switchCwd;
    tabState.showBanner = showBanner;
    tabState.hideBanner = hideBanner;
    tabState.retry = retry;
    tabState.openForActiveProject = openForActiveProject;
    tabState.name = name;
    tabState.activeTabName = activeTabName;
    return tabState;
}

const LAZYGIT_INSTALL_ENTRIES = [
    { id: 'mac',      label: 'macOS (Homebrew)',     cmd: 'brew install lazygit' },
    { id: 'mac-port', label: 'macOS (MacPorts)',     cmd: 'sudo port install lazygit' },
    { id: 'linux-debian', label: 'Debian / Ubuntu', cmd: 'LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | grep -Po \'"tag_name": "v\\K[^"]*\') && \\\ncurl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz" && \\\ntar xf lazygit.tar.gz lazygit && sudo install lazygit -D -t /usr/local/bin/' },
    { id: 'linux-arch',   label: 'Arch Linux',       cmd: 'sudo pacman -S lazygit' },
    { id: 'linux-fedora', label: 'Fedora',           cmd: 'sudo dnf copr enable atim/lazygit -y && sudo dnf install lazygit' },
    { id: 'windows',  label: 'Windows (winget)',     cmd: 'winget install -e --id=JesseDuffield.lazygit' },
    { id: 'windows-scoop', label: 'Windows (Scoop)', cmd: 'scoop install lazygit' },
    { id: 'go',       label: 'Go (any OS)',          cmd: 'go install github.com/jesseduffield/lazygit@latest' },
];

const LAZYDOCKER_INSTALL_ENTRIES = [
    { id: 'mac',      label: 'macOS (Homebrew)',     cmd: 'brew install jesseduffield/lazydocker/lazydocker' },
    { id: 'linux-debian', label: 'Linux (script)',   cmd: 'curl https://raw.githubusercontent.com/jesseduffield/lazydocker/master/scripts/install_update_linux.sh | bash' },
    { id: 'linux-arch',   label: 'Arch Linux (AUR)', cmd: 'yay -S lazydocker' },
    { id: 'windows',  label: 'Windows (Scoop)',      cmd: 'scoop install lazydocker' },
    { id: 'go',       label: 'Go (any OS)',          cmd: 'go install github.com/jesseduffield/lazydocker@latest' },
];

const TELESCOPE_INSTALL_ENTRIES = [
    { id: 'mac',      label: 'macOS (Homebrew) — все 4 пакета одной командой', cmd: 'brew install television fd bat ripgrep' },
    { id: 'linux-arch',   label: 'Arch Linux',       cmd: 'sudo pacman -S television fd bat ripgrep' },
    { id: 'linux-fedora', label: 'Fedora',           cmd: 'sudo dnf copr enable atim/television -y && sudo dnf install television fd-find bat ripgrep' },
    { id: 'linux-debian', label: 'Debian / Ubuntu',  cmd: 'sudo apt install fd-find bat ripgrep && cargo install --locked television   # tv недоступен в apt, ставим через cargo' },
    { id: 'cargo',    label: 'Cargo (any OS, требует Rust)', cmd: 'cargo install --locked television fd-find bat ripgrep' },
];

export function initTuiTabs() {
    state.gitTerm = createTuiTab({
        name: 'lazygit',
        wsPath: '/ws/lazygit',
        activeTabName: 'git',
        refs: {
            termEl: $gitTermEl,
            placeholderEl: $gitPlaceholder,
            errorEl: $gitError,
            errorTextEl: $gitErrorText,
            retryBtn: $gitErrorRetry,
            closeBtn: $gitErrorClose,
            installHelpEl: $gitInstallHelp,
            installListEl: $gitInstallList,
        },
        installHelp: {
            binary: 'lazygit',
            notFoundMsg: 'lazygit not found in PATH. Install it using one of the commands below:',
            entries: LAZYGIT_INSTALL_ENTRIES,
        },
        // git привязан к cwd текущей сессии, а не к корню проекта. Это
        // даёт корректный git-контекст: разные сессии одного проекта
        // могут жить в разных подпапках (orphan-сессии без folder_id)
        // тоже получают свой git. Fallback на project.path — внутри
        // openForActiveProject, если сессия не выбрана.
        resolveCwd: () => sessionCwdOrNull(),
    });

    state.dockerTerm = createTuiTab({
        name: 'lazydocker',
        wsPath: '/ws/lazydocker',
        activeTabName: 'docker',
        refs: {
            termEl: $dockerTermEl,
            placeholderEl: $dockerPlaceholder,
            errorEl: $dockerError,
            errorTextEl: $dockerErrorText,
            retryBtn: $dockerErrorRetry,
            closeBtn: $dockerErrorClose,
            installHelpEl: $dockerInstallHelp,
            installListEl: $dockerInstallList,
        },
        installHelp: {
            binary: 'lazydocker',
            notFoundMsg: 'lazydocker not found in PATH. Install it using one of the commands below:',
            entries: LAZYDOCKER_INSTALL_ENTRIES,
        },
        // lazydocker привязан к cwd текущей tmux-сессии — `docker compose`
        // / `docker-compose.yml` обычно лежат в каталоге проекта, который
        // совпадает с cwd сессии. Без resolveCwd placeholder остаётся
        // навсегда (Phase 4 убрала fallback на activeProject.path).
        resolveCwd: () => sessionCwdOrNull(),
    });

    state.telescopeTerm = createTuiTab({
        name: 'telescope',
        wsPath: '/ws/telescope',
        activeTabName: 'telescope',
        autoReconnectOnClose: true,
        autoReconnectDelayMs: 150,
        refs: {
            termEl: $telescopeTermEl,
            placeholderEl: $telescopePlaceholder,
            errorEl: $telescopeError,
            errorTextEl: $telescopeErrorText,
            retryBtn: $telescopeErrorRetry,
            closeBtn: $telescopeErrorClose,
            installHelpEl: $telescopeInstallHelp,
            installListEl: $telescopeInstallList,
        },
        installHelp: {
            binary: 'tv',
            notFoundMsg: 'television (tv) и helper-утилиты fd / bat / rg (ripgrep) нужны для Find-вкладки. Установите все 4 одной командой:',
            entries: TELESCOPE_INSTALL_ENTRIES,
        },
        // telescope (Find) привязан к cwd текущей сессии, как git/tasks:
        // fuzzy-finder должен искать в каталоге сессии, а не в корне проекта.
        // Fallback на project.path делается внутри openForActiveProject.
        resolveCwd: () => sessionCwdOrNull(),
    });

    state.telescopeTerm.channel = 'files';

    if ($telescopeChannelBar) {
        $telescopeChannelBar.querySelectorAll('.tui-channel-btn').forEach((btn) => {
            btn.addEventListener('click', () => {
                const newChannel = btn.dataset.channel || 'files';
                if (state.telescopeTerm.channel === newChannel
                    && state.telescopeTerm.ws
                    && state.telescopeTerm.ws.readyState === WebSocket.OPEN) {
                    return;
                }
                state.telescopeTerm.channel = newChannel;
                $telescopeChannelBar.querySelectorAll('.tui-channel-btn').forEach((b) => {
                    b.classList.toggle('active', b.dataset.channel === newChannel);
                });
                state.telescopeTerm.close('channel switched');
                if (state.telescopeTerm.term) {
                    try { state.telescopeTerm.term.clear(); } catch (_) {}
                    try { state.telescopeTerm.term.reset(); } catch (_) {}
                }
                state.telescopeTerm.openForActiveProject();
            });
        });
    }
}

/**
 * Возвращает cwd текущей tmux-сессии (`state.currentSession.path`) или null,
 * если сессия не выбрана / не найдена в state.sessions / не имеет path.
 *
 * Используется git-вкладкой как resolveCwd: lazygit показывает репо
 * именно той сессии, в которой юзер сейчас работает, а не корень проекта.
 * Это важно для:
 *   1) Разных сессий одного проекта в разных подпапках.
 *   2) сессий без cwd (sess.path=null), для которых resolveCwd не даёт path.
 */
function sessionCwdOrNull() {
    const name = state.currentSession;
    if (!name) return null;
    const list = Array.isArray(state.sessions) ? state.sessions : [];
    const sess = list.find((s) => s && s.name === name);
    return sess && sess.path ? sess.path : null;
}

/**
 * Синхронизирует git-WS с cwd текущей сессии. Вызывается из openSession /
 * switchSession сразу после установки state.currentSession. Логика:
 *   - Если git-WS открыт и cwd сессии отличается от текущего — switchCwd
 *     (на бэке lazygit перезапускается под новым cwd).
 *   - Если git-WS не открыт — ничего не делаем; resolveCwd подхватит свежий
 *     session.path при первом openForActiveProject (т.е. при клике на вкладку).
 */
export function syncGitToCurrentSession() {
    const t = state.gitTerm;
    if (!t || !t.ws) return;
    const cwd = sessionCwdOrNull();
    if (!cwd) return;
    if (t.currentCwd === cwd) return;
    t.switchCwd(cwd);
}

/**
 * Синхронизирует telescope-WS (Find) с cwd текущей сессии. По образцу
 * syncGitToCurrentSession: если WS открыт и сессия сменила cwd —
 * switchCwd (на бэке tv перезапускается под новым cwd). Если WS не
 * открыт — ничего не делаем; resolveCwd подхватит свежий session.path
 * при следующем openForActiveProject.
 */
export function syncTelescopeToCurrentSession() {
    const t = state.telescopeTerm;
    if (!t || !t.ws) return;
    const cwd = sessionCwdOrNull();
    if (!cwd) return;
    if (t.currentCwd === cwd) return;
    t.switchCwd(cwd);
}

/**
 * Синхронизирует docker-WS (lazydocker) с cwd текущей сессии. По образцу
 * syncGitToCurrentSession. Если WS открыт и сессия сменила cwd —
 * switchCwd (на бэке lazydocker перезапускается под новым cwd, что
 * подхватывает локальный docker-compose.yml текущего проекта). Если WS
 * не открыт — resolveCwd подхватит свежий session.path при следующем
 * openForActiveProject.
 */
export function syncDockerToCurrentSession() {
    const t = state.dockerTerm;
    if (!t || !t.ws) return;
    const cwd = sessionCwdOrNull();
    if (!cwd) return;
    if (t.currentCwd === cwd) return;
    t.switchCwd(cwd);
}

function _git() {
    return state.gitTerm;
}

export function mountGitTerm() {
    const t = _git();
    return t ? t.mount() : null;
}

export function openLazygitForActiveProject() {
    const t = _git();
    if (t) t.openForActiveProject();
}

export function connectGitWs(cwd) {
    const t = _git();
    if (t) t.connect(cwd);
}

export function closeGitWs(reason) {
    const t = _git();
    if (t) t.close(reason);
}

export function gitSwitchCwd(newCwd) {
    const t = _git();
    if (t) t.switchCwd(newCwd);
}

export function showGitBanner(message, opts) {
    const t = _git();
    if (t) t.showBanner(message, opts);
}

export function hideGitBanner() {
    const t = _git();
    if (t) t.hideBanner();
}

export function retryGitConnection() {
    const t = _git();
    if (t) t.retry();
}

/**
 * sendToActivePty(text) — отправляет произвольный текст в WS активной
 * вкладки (terminal / git / docker / telescope). Используется
 * quick-cmd.js для отправки команд из quick-command bar и spec-keys.
 */
export function sendToActivePty(text) {
    if (typeof text !== 'string' || text.length === 0) return;
    let ws = null;
    const tab = state.activeTab;
    if (tab === 'terminal') {
        ws = state.ws;
    } else if (tab === 'git' && state.gitTerm) {
        ws = state.gitTerm.ws;
    } else if (tab === 'docker' && state.dockerTerm) {
        ws = state.dockerTerm.ws;
    } else if (tab === 'telescope' && state.telescopeTerm) {
        ws = state.telescopeTerm.ws;
    }
    if (!ws || ws.readyState !== WebSocket.OPEN) {
        console.debug('[ForgeApp] sendToActivePty: WS not open for tab', tab);
        return;
    }
    try {
        ws.send(state.encoder.encode(text));
    } catch (e) {
        console.warn('[ForgeApp] sendToActivePty failed', e);
    }
}
