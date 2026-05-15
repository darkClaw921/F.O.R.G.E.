// tmux-web — frontend logic (Phase 4)
//
// Архитектура:
//   - xterm.js Terminal mount-нут в #terminal, addon-fit ресайзит под контейнер,
//     addon-web-links делает URL в выводе кликабельными.
//   - sidebar заполняется через GET /api/sessions с polling 3s.
//   - WebSocket /ws/attach: binary I/O в обе стороны (PTY raw bytes),
//     control-сообщения (resize/switch) — JSON в Text frames.
//   - Переключение сессии через {type:"switch"} без переподключения WS.
//   - ResizeObserver на #terminal → fit.fit() + send {type:"resize",cols,rows}.

(function () {
    'use strict';

    // ---- глобальное состояние ----
    const state = {
        // Phase 5 — режим запуска бэкенда. Загружается из GET /healthz один раз
        // при bootstrap. По умолчанию false → весь новый UI (origin-табы,
        // Settings → Remote servers tab, ?server в запросах, глобальные id)
        // СКРЫТ, и поведение фронта побитово совпадает с legacy.
        // При remote_mode=true активируются ветки рендера ниже.
        remoteMode: false,
        serverVersion: null,        // строка из /healthz.version (для footer/about)
        healthzLoaded: false,       // true после первого fetch /healthz (успех или fail)
        // Phase 5 — реестр и кэши remote-серверов. Заполняются ТОЛЬКО в remote-mode.
        // remoteServers: список RemoteServerView { id, label, url } (без token).
        // remoteProjects / remoteSessions: lazy-load кэши per server-id.
        remoteServers: [],
        remoteProjects: new Map(),  // server_id → ProjectDto[] (origin-aware)
        remoteSessions: new Map(),  // server_id → SessionDto[]
        // Phase 5 — активный origin-фильтр в sidebar. Значения:
        //   'all'   — показывать всё (local + все remote);
        //   'local' — только локальные;
        //   <server_id> — только этот remote.
        // По умолчанию 'all'; сохраняется в localStorage('forge.activeOrigin').
        activeOrigin: 'all',
        term: null,
        fitAddon: null,
        webLinksAddon: null,
        ws: null,
        // Phase 7 — auto-reconnect /ws/attach с экспоненциальным backoff.
        attachWsBackoffStep: 0,
        attachWsReconnectTimer: null,
        attachWsClosedByUs: false,
        attachWsOrigin: null,    // 'local' | server_id | null — last origin
        currentSession: null,    // имя активной сессии (или null)
        sessions: [],             // последний список сессий (для рендера)
        pollTimer: null,
        encoder: new TextEncoder(),
        // anti-loop: при resize PTY эхом не порождает onResize-петлю,
        // но всё равно дроссельуем отправку.
        lastResizeKey: '',
        // ---- Phase 6.A: Tasks-таб ----
        activeTab: 'terminal',    // 'terminal' | 'tasks' | 'git'
        tasksPollTimer: null,     // setInterval handle для fetchTasks (fallback polling)
        tasksData: null,          // последний JSON snapshot {issues, total, ...} или null
        // ---- Phase 6.B: Multi-project ----
        projects: [],             // последний массив ProjectDto от /api/projects
        activeProjectId: null,    // id активного проекта (или null до первого fetch)
        // Cross-project sessions visibility: фильтр сайдбара (UI-only).
        // '__all__' = показывать сессии всех проектов (с группировкой), либо
        // конкретный project.id — показывать только сессии этого проекта.
        // Не путать с activeProjectId (backend-side активный проект).
        projectFilter: '__all__',
        // ---- Phase 6.D: Realtime tasks WS ----
        tasksWs: null,            // WebSocket | null
        tasksWsBackoffStep: 0,    // индекс в TASKS_WS_BACKOFFS_MS для следующей попытки
        tasksWsReconnectTimer: null, // setTimeout handle на reconnect
        tasksWsClosedByUs: false, // true → не реконнектиться (например, страница уходит)
        // ---- Phase 4 (TODO kanban): локальный store + realtime WS ----
        // Массив TODO-карточек активного проекта (фильтр project_id выполняет
        // бэкенд: REST /api/todos?project_id=… и WS /ws/todos?project_id=…).
        // null до первого fetch/snapshot, потом — массив (возможно пустой).
        todosData: [],
        todosWs: null,            // WebSocket | null
        todosWsBackoffStep: 0,    // индекс в TODOS_WS_BACKOFFS_MS
        todosWsReconnectTimer: null,
        todosWsClosedByUs: false,
        todosPollTimer: null,     // fallback poll setInterval handle
        // ---- Themes (Phase 3) ----
        // Активная тема, последняя применённая через applyTheme().
        // Используется Phase 5 (live preview) и для повторного применения после
        // переключения через switchTheme() / редактора кастомных тем.
        // Структура: { id, name, kind: 'preset'|'custom', ui: {...}, term: {...} }.
        // null до первого fetch /api/themes/active (см. bootstrap).
        activeTheme: null,
        // ---- Phase 4 (lazygit-tab): xterm-инстанция git-таба ----
        // Изолированный Terminal + FitAddon, монтируется в #git-term
        // лениво при первом switchTab('git') с активным проектом.
        // ws — WebSocket к /ws/lazygit?cwd=<active project path>.
        // currentCwd хранится для switch_cwd / reconnect-стратегии при
        // смене активного проекта.
        gitTerm: {
            term: null,          // xterm.js Terminal | null
            fit: null,           // FitAddon | null
            ws: null,            // WebSocket | null
            mounted: false,      // true после первого mountGitTerm()
            currentCwd: null,    // последний cwd, использованный при open WS
            errorSticky: false,  // true → не перезатирать banner на ws.close
        },
    };

    // ---- DOM-узлы ----
    const $sidebar = document.getElementById('session-list');
    const $btnNew = document.getElementById('btn-new');
    const $terminalEl = document.getElementById('terminal');
    const $placeholder = document.getElementById('placeholder');
    const $statusDot = document.getElementById('status-dot');
    const $statusText = document.getElementById('status-text');
    // Phase 6.A: tab-bar и Tasks UI.
    const $tabTerminal = document.getElementById('tab-terminal');
    const $tabTasks = document.getElementById('tab-tasks');
    const $tasksStatus = document.getElementById('tasks-status');
    const $tasksEl = document.getElementById('tasks');
    const $tasksReload = document.getElementById('tasks-reload');
    const $tasksNew = document.getElementById('tasks-new');
    const $tasksMeta = document.getElementById('tasks-meta');
    const $tasksBoard = document.getElementById('tasks-board');
    // Git tab: tab-button + контейнер #git (виден при switchTab('git')).
    const $tabGit = document.getElementById('tab-git');
    const $gitEl = document.getElementById('git');
    // lazygit-tab DOM-refs.
    const $gitTermEl = document.getElementById('git-term');
    const $gitPlaceholder = document.getElementById('git-placeholder');
    const $gitError = document.getElementById('git-error');
    const $gitErrorText = document.getElementById('git-error-text');
    const $gitErrorRetry = document.getElementById('git-error-retry');
    const $gitErrorClose = document.getElementById('git-error-close');
    const $gitInstallHelp = document.getElementById('git-install-help');
    const $gitInstallList = document.getElementById('git-install-list');
    // Phase 6.B: Project bar.
    const $projectSelect = document.getElementById('project-select');
    const $projectNew = document.getElementById('project-new');
    const $projectSettings = document.getElementById('project-settings');
    // Phase 5: origin-табы над session-list (hidden при remote_mode=false).
    const $originTabs = document.getElementById('origin-tabs');

    // =========================================================================
    // xterm.js initialization (cx2.3)
    // =========================================================================

    /**
     * Инициализация xterm.js Terminal.
     * @param {object|null} termTheme — xterm ITheme (результат mapTermTheme).
     *   Если null — используется fallback-палитра (offline / API недоступен).
     *   Тема обязана прийти ДО new Terminal — xterm рендерит фон сразу при
     *   open(), и присвоение options.theme после этого пересчитывает только
     *   глифы, оставляя background-canvas от старой темы до следующего
     *   полного перерисова.
     */
    function initTerminal(termTheme) {
        // Доступ к глобалам, которые подключены через CDN <script>:
        // window.Terminal, window.FitAddon, window.WebLinksAddon
        const Terminal = window.Terminal;
        const FitAddon = window.FitAddon && window.FitAddon.FitAddon;
        const WebLinksAddon = window.WebLinksAddon && window.WebLinksAddon.WebLinksAddon;

        if (!Terminal || !FitAddon || !WebLinksAddon) {
            console.error('xterm.js / addons not loaded — проверь CDN-ссылки');
            return;
        }

        // Fallback-палитра для случая, когда /api/themes/active недоступен
        // (offline, dev без backend). Совпадает с историческим hard-coded
        // объектом, чтобы поведение не регрессировало при сбоях API.
        const fallbackTheme = {
            background: '#000000',
            foreground: '#d8dee9',
            cursor: '#d8dee9',
            selectionBackground: '#3a4356',
        };

        const term = new Terminal({
            cursorBlink: true,
            fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
            fontSize: 13,
            scrollback: 5000,
            allowProposedApi: true,
            theme: termTheme || fallbackTheme,
        });

        const fitAddon = new FitAddon();
        const webLinksAddon = new WebLinksAddon();
        term.loadAddon(fitAddon);
        term.loadAddon(webLinksAddon);

        term.open($terminalEl);
        // Первичная подгонка под контейнер.
        try {
            fitAddon.fit();
        } catch (e) {
            console.warn('initial fit failed', e);
        }

        // Ввод пользователя — bytes → WS (cx2.5).
        term.onData((data) => {
            if (state.ws && state.ws.readyState === WebSocket.OPEN) {
                state.ws.send(state.encoder.encode(data));
            }
        });

        // Автоматический onResize от xterm (например при font-size change) —
        // тоже шлём в PTY.
        term.onResize(({ cols, rows }) => {
            sendResize(cols, rows);
        });

        state.term = term;
        state.fitAddon = fitAddon;
        state.webLinksAddon = webLinksAddon;

        // ResizeObserver на контейнер #terminal — каждый раз когда меняется размер
        // окна / sidebar / шрифт, делаем fit() + шлём resize в PTY (cx2.6).
        const ro = new ResizeObserver(() => {
            if (!state.fitAddon) return;
            try {
                state.fitAddon.fit();
            } catch (_) { /* xterm может бросить если контейнер 0×0 */ }
        });
        ro.observe($terminalEl);

        // Дополнительно слушаем window resize (страховка для старых браузеров).
        window.addEventListener('resize', () => {
            try { state.fitAddon && state.fitAddon.fit(); } catch (_) {}
        });
    }

    // =========================================================================
    // Themes runtime (Phase 3)
    //
    // Тема состоит из двух секций:
    //   - theme.ui  — палитра UI (CSS-переменные на :root): bg, bgElev, fg,
    //     fgDim, border, accent, warn, danger, p0, p1, p2.
    //   - theme.term — палитра xterm.js: foreground, background, cursor,
    //     selection, black/red/.../white, brightBlack/.../brightWhite.
    //
    // Бэкенд (themes.rs) сериализует Theme в camelCase, поэтому ключи на
    // фронте используются как есть, без snake-case → camelCase конверсии.
    //
    // applyTheme(theme):
    //   1. Для каждого ключа theme.ui — setProperty('--' + kebab) на :root.
    //   2. Если state.term существует — обновляет term.options.theme через
    //      mapTermTheme. xterm.js (cx 5.x+) поддерживает горячую смену темы
    //      через присвоение options.theme; цвета пересчитываются в следующем
    //      кадре рендера.
    //   3. Сохраняет тему в state.activeTheme — нужно для редактора (Phase 5)
    //      и для отката live preview.
    //
    // mapTermTheme(t): приводит наши имена к именам xterm.js ITheme.
    //   Единственная переименовка: selection → selectionBackground.
    //   Остальные поля совпадают (foreground/background/cursor/black/red/
    //   green/yellow/blue/magenta/cyan/white/brightBlack/.../brightWhite).
    //
    // switchTheme(id): PATCH /api/themes/active + GET active + applyTheme,
    //   без релоада страницы. При ошибке — alert (см. notify-механизм).
    // =========================================================================

    /**
     * Маппит нашу TermColors-палитру (camelCase из serde) в xterm.js ITheme.
     * Возвращает новый объект, безопасно присваиваемый в term.options.theme.
     */
    function mapTermTheme(t) {
        if (!t) return {};
        return {
            foreground: t.foreground,
            background: t.background,
            cursor: t.cursor,
            // xterm.js использует selectionBackground (ранее selection); наш
            // бэкенд хранит просто `selection` для краткости, переименовываем тут.
            selectionBackground: t.selection,
            black: t.black,
            red: t.red,
            green: t.green,
            yellow: t.yellow,
            blue: t.blue,
            magenta: t.magenta,
            cyan: t.cyan,
            white: t.white,
            brightBlack: t.brightBlack,
            brightRed: t.brightRed,
            brightGreen: t.brightGreen,
            brightYellow: t.brightYellow,
            brightBlue: t.brightBlue,
            brightMagenta: t.brightMagenta,
            brightCyan: t.brightCyan,
            brightWhite: t.brightWhite,
        };
    }

    /**
     * Применяет тему: CSS-переменные UI + xterm theme + сохранение в state.
     * @param {{id?: string, name?: string, ui?: object, term?: object}} theme
     */
    function applyTheme(theme) {
        if (!theme) return;
        const ui = theme.ui || {};
        // Маппинг camelCase ключей бэкенда → kebab-case CSS-переменных на :root.
        // Эти 11 переменных совпадают с настроенными в style.css (Phase 2).
        const cssMap = {
            bg: '--bg',
            bgElev: '--bg-elev',
            fg: '--fg',
            fgDim: '--fg-dim',
            border: '--border',
            accent: '--accent',
            warn: '--warn',
            danger: '--danger',
            p0: '--p0',
            p1: '--p1',
            p2: '--p2',
        };
        const root = document.documentElement;
        for (const [k, cssVar] of Object.entries(cssMap)) {
            const v = ui[k];
            if (typeof v === 'string' && v.length > 0) {
                root.style.setProperty(cssVar, v);
            }
        }
        // xterm theme — только если терминал уже создан (после initTerminal).
        // На bootstrap-стадии (до new Terminal) ветка не сработает; mapTermTheme
        // прокинется напрямую через initTerminal({ theme: mapTermTheme(...) }).
        if (state.term && theme.term) {
            try {
                state.term.options.theme = mapTermTheme(theme.term);
            } catch (e) {
                console.warn('xterm options.theme assignment failed', e);
            }
        }
        state.activeTheme = theme;
    }

    /**
     * Переключает активную тему на сервере и применяет её локально без релоада.
     * @param {string} id — id темы (например, 'dracula', 'default', custom uuid).
     */
    async function switchTheme(id) {
        try {
            const patchResp = await fetch('/api/themes/active', {
                method: 'PATCH',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ id }),
            });
            if (!patchResp.ok) {
                const text = await patchResp.text().catch(() => '');
                window.alert('Failed to switch theme: ' + (text || patchResp.status));
                return;
            }
            const getResp = await fetch('/api/themes/active');
            if (!getResp.ok) {
                window.alert('Failed to fetch active theme: ' + getResp.status);
                return;
            }
            const theme = await getResp.json();
            applyTheme(theme);
        } catch (e) {
            window.alert('Failed to switch theme: ' + e.message);
        }
    }

    function sendResize(cols, rows) {
        if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return;
        const key = cols + 'x' + rows;
        if (key === state.lastResizeKey) return;
        state.lastResizeKey = key;
        try {
            state.ws.send(JSON.stringify({ type: 'resize', cols, rows }));
        } catch (e) {
            console.warn('resize send failed', e);
        }
    }

    // =========================================================================
    // Sessions polling + sidebar render (cx2.4)
    // =========================================================================

    async function fetchSessions() {
        try {
            const resp = await fetch('/api/sessions', { headers: { 'Accept': 'application/json' } });
            if (!resp.ok) {
                throw new Error('HTTP ' + resp.status);
            }
            const data = await resp.json();
            state.sessions = Array.isArray(data) ? data : [];
            renderSidebar();
        } catch (e) {
            console.warn('fetchSessions failed', e);
            // Не очищаем state.sessions, чтобы не мигало при кратковременных сбоях.
        }
    }

    /**
     * Создаёт DOM-элемент <li class="session-item"> для одной сессии.
     * Вынесено в отдельную функцию, чтобы renderSidebar мог переиспользовать
     * рендер строки в обоих режимах фильтра (single / __all__).
     */
    function buildSessionItem(s) {
        const li = document.createElement('li');
        li.className = 'session-item';
        if (s.name === state.currentSession) {
            li.classList.add('active');
        }
        if (s.needs_attention) {
            li.classList.add('needs-attention');
        }
        li.dataset.session = s.name;

        const meta = document.createElement('div');
        meta.className = 'session-meta';

        const name = document.createElement('div');
        name.className = 'session-name';
        name.textContent = s.name;
        meta.appendChild(name);

        const sub = document.createElement('div');
        sub.className = 'session-sub';
        const winsTxt = `${s.windows} ${s.windows === 1 ? 'window' : 'windows'}`;
        if (s.attached > 0) {
            sub.innerHTML = `${winsTxt} · <span class="attached-flag">attached(${s.attached})</span>`;
        } else {
            sub.textContent = winsTxt;
        }
        meta.appendChild(sub);

        li.appendChild(meta);

        const sessOrigin = dtoOrigin(s);
        const btnKill = document.createElement('button');
        btnKill.type = 'button';
        btnKill.className = 'btn-kill';
        btnKill.textContent = 'kill';
        btnKill.title = `Убить сессию ${s.name}`;
        btnKill.addEventListener('click', (ev) => {
            ev.stopPropagation();
            killSession(s.name, sessOrigin);
        });
        li.appendChild(btnKill);

        // Click по элементу (но не по кнопке) — open / switch (cx2.6).
        // Phase 5: передаём origin (берётся из DTO.origin).
        li.addEventListener('click', () => openSession(s.name, sessOrigin));

        return li;
    }

    function renderSidebar() {
        // Phase 5: в remote-mode рендерим origin-табы (или прячем UI, если не).
        renderOriginTabs();

        // Phase 5: в remote-mode используем двухуровневую группировку
        // Origin → Project → Sessions. В legacy режиме — поведение Phase 6.B
        // (project-grouping) сохраняется побитово.
        if (isRemoteMode()) {
            renderSidebarWithOrigin();
            return;
        }

        $sidebar.innerHTML = '';
        if (state.sessions.length === 0) {
            const li = document.createElement('li');
            li.className = 'empty';
            li.textContent = 'Нет активных сессий';
            $sidebar.appendChild(li);
            return;
        }

        // Cross-folder sessions visibility:
        //   - projectFilter применяется ДО группировки (фильтр по project_id остаётся
        //     корректным: клик в project-bar показывает сессии одного registered-проекта).
        //   - Группируем отфильтрованные сессии по folder_id (orphan = null/undefined).
        //   - Заголовок группы = folder_label сессий внутри (basename полного пути).
        //   - Сортировка групп по folder_label (case-insensitive), Orphan в конце.
        //   - Внутри группы сессии сортируются по имени для стабильного порядка.
        // project_id остаётся в DTO и используется openSession→switchActiveProject,
        // но не участвует в группировке sidebar.
        const ORPHAN_KEY = '__orphan__';
        const projectFilter = state.projectFilter;
        const visible = projectFilter === '__all__'
            ? state.sessions
            : state.sessions.filter((s) => s.project_id === projectFilter);

        if (visible.length === 0) {
            const li = document.createElement('li');
            li.className = 'empty';
            li.textContent = projectFilter === '__all__'
                ? 'Нет активных сессий'
                : 'Нет сессий в этом проекте';
            $sidebar.appendChild(li);
            return;
        }

        const groups = new Map(); // folder_id|ORPHAN_KEY → Session[]
        for (const sess of visible) {
            const key = sess.folder_id == null ? ORPHAN_KEY : sess.folder_id;
            if (!groups.has(key)) groups.set(key, []);
            groups.get(key).push(sess);
        }
        for (const arr of groups.values()) {
            arr.sort((a, b) => a.name.localeCompare(b.name));
        }

        // Сортировка ключей по folder_label (case-insensitive), Orphan в конце.
        const nonOrphanKeys = [];
        for (const key of groups.keys()) {
            if (key !== ORPHAN_KEY) nonOrphanKeys.push(key);
        }
        nonOrphanKeys.sort((a, b) => {
            const la = (groups.get(a)[0].folder_label || a).toLowerCase();
            const lb = (groups.get(b)[0].folder_label || b).toLowerCase();
            return la.localeCompare(lb);
        });

        for (const key of nonOrphanKeys) {
            const arr = groups.get(key);
            if (!arr || arr.length === 0) continue;
            const keyDisplay = key.startsWith('__folder:') ? key.slice('__folder:'.length) : key;
            const header = document.createElement('li');
            header.className = 'session-group-header';
            header.textContent = arr[0].folder_label || keyDisplay;
            $sidebar.appendChild(header);
            for (const sess of arr) {
                $sidebar.appendChild(buildSessionItem(sess));
            }
        }
        const orphans = groups.get(ORPHAN_KEY);
        if (orphans && orphans.length > 0) {
            const header = document.createElement('li');
            header.className = 'session-group-header';
            header.textContent = 'Orphan';
            $sidebar.appendChild(header);
            for (const sess of orphans) {
                $sidebar.appendChild(buildSessionItem(sess));
            }
        }
    }

    // -------------------------------------------------------------------------
    // Phase 5 — origin-табы и origin-aware рендер sidebar
    // -------------------------------------------------------------------------

    /**
     * Восстанавливает activeOrigin из localStorage. Допустимые значения: 'all',
     * 'local', либо id любого из state.remoteServers. Иначе fallback 'all'.
     */
    function loadActiveOriginFromStorage() {
        try {
            const saved = localStorage.getItem('forge.activeOrigin');
            if (saved === 'all' || saved === 'local') {
                state.activeOrigin = saved;
            } else if (saved && state.remoteServers.some((s) => s.id === saved)) {
                state.activeOrigin = saved;
            } else {
                state.activeOrigin = 'all';
            }
        } catch (_) {
            state.activeOrigin = 'all';
        }
    }

    function saveActiveOriginToStorage() {
        try {
            localStorage.setItem('forge.activeOrigin', state.activeOrigin);
        } catch (_) { /* privacy mode — игнор */ }
    }

    /**
     * Хранилище состояния "свёрнуто/развёрнуто" для origin-секций в sidebar.
     * Map: originKey ('local' | server_id) → bool (true = collapsed).
     * Загружается из localStorage('forge.collapsedOrigins') ленивo.
     */
    let _collapsedOrigins = null;
    function getCollapsedOrigins() {
        if (_collapsedOrigins) return _collapsedOrigins;
        _collapsedOrigins = new Set();
        try {
            const raw = localStorage.getItem('forge.collapsedOrigins');
            if (raw) {
                const arr = JSON.parse(raw);
                if (Array.isArray(arr)) {
                    arr.forEach((k) => _collapsedOrigins.add(k));
                }
            }
        } catch (_) { /* ignore */ }
        return _collapsedOrigins;
    }
    function persistCollapsedOrigins() {
        try {
            localStorage.setItem(
                'forge.collapsedOrigins',
                JSON.stringify(Array.from(getCollapsedOrigins())),
            );
        } catch (_) { /* ignore */ }
    }
    function isOriginCollapsed(key) {
        return getCollapsedOrigins().has(key);
    }
    function toggleOriginCollapsed(key) {
        const set = getCollapsedOrigins();
        if (set.has(key)) {
            set.delete(key);
        } else {
            set.add(key);
        }
        persistCollapsedOrigins();
    }

    /**
     * Рендерит горизонтальные origin-табы над session-list:
     *   [All] [Local] [server-1] [server-2] … [+]
     * Клик по табу обновляет state.activeOrigin и перерисовывает sidebar.
     * '+' таб — открывает Settings modal на вкладке Remote servers.
     *
     * В legacy-режиме контейнер скрыт (hidden=true).
     */
    function renderOriginTabs() {
        if (!$originTabs) return;
        if (!isRemoteMode()) {
            $originTabs.hidden = true;
            $originTabs.innerHTML = '';
            return;
        }
        $originTabs.hidden = false;
        $originTabs.innerHTML = '';

        const mkTab = (originKey, label, dotKind) => {
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'origin-tab';
            if (state.activeOrigin === originKey) btn.classList.add('active');
            if (dotKind) {
                const dot = document.createElement('span');
                dot.className = 'origin-dot ' + dotKind;
                btn.appendChild(dot);
            }
            const span = document.createElement('span');
            span.textContent = label;
            btn.appendChild(span);
            btn.addEventListener('click', () => {
                state.activeOrigin = originKey;
                saveActiveOriginToStorage();
                // При выборе конкретного remote — lazy-load его данных.
                if (originKey !== 'all' && originKey !== 'local') {
                    if (!state.remoteSessions.has(originKey)) {
                        loadRemoteSessions(originKey).then(() => renderSidebar());
                    }
                    if (!state.remoteProjects.has(originKey)) {
                        loadRemoteProjects(originKey);
                    }
                }
                // Phase 5: реcоединяем tasks/todos WS чтобы они переподписались
                // на нужный origin (см. connectTasksWs/connectTodosWs — они
                // читают state.activeOrigin при формировании URL).
                disconnectTasksWs();
                disconnectTodosWs();
                state.tasksData = null;
                state.todosData = [];
                setTimeout(() => { connectTasksWs(); connectTodosWs(); }, 0);
                renderSidebar();
            });
            return btn;
        };

        $originTabs.appendChild(mkTab('all', 'All', null));
        $originTabs.appendChild(mkTab('local', 'Local', 'local'));
        for (const srv of state.remoteServers) {
            const status = state.remoteOnline.get(srv.id) || 'unknown';
            $originTabs.appendChild(mkTab(srv.id, srv.label || srv.id, status));
        }
        // [+] таб — открыть Settings → Remote servers.
        const plus = document.createElement('button');
        plus.type = 'button';
        plus.className = 'origin-tab origin-tab-add';
        plus.title = 'Add remote server';
        plus.textContent = '+';
        plus.addEventListener('click', () => {
            openSettingsModal('remotes');
        });
        $originTabs.appendChild(plus);
    }

    /**
     * Origin-aware рендер sidebar (только при remote_mode=true).
     *
     * Структура:
     *   ▾ LOCAL              <- origin-group-header (collapse при клике)
     *      Project A         <- project-sub-header
     *        - session1      <- session-item.in-origin
     *      Project B
     *        - session2
     *   ▸ SERVER office (online)
     *      ...
     *
     * Фильтр state.activeOrigin:
     *   'all'   — рендерим все origin'ы;
     *   'local' — только локальный;
     *   <id>    — только этот remote.
     *
     * lazy-load: при первом рендере remote-секции (origin раскрыт и нет
     * закэшированных данных) — вызываем loadRemoteProjects/loadRemoteSessions.
     */
    function renderSidebarWithOrigin() {
        $sidebar.innerHTML = '';

        const showLocal = state.activeOrigin === 'all' || state.activeOrigin === 'local';
        const remoteIds = state.remoteServers.map((s) => s.id);
        const showRemotes = state.activeOrigin === 'all'
            ? remoteIds
            : (state.activeOrigin === 'local' ? [] : remoteIds.filter((id) => id === state.activeOrigin));

        // Phase 6 — в All-view: лениво подтягиваем remote-данные для онлайн-серверов,
        // даже если их секция свёрнута. Это нужно, чтобы агрегированный вид сразу
        // показывал реальные счётчики (offline-сервера НЕ дёргаем, чтобы не штормить
        // сетью). Поведение для single-remote-таба сохраняется ниже.
        const isAllView = state.activeOrigin === 'all';

        if (showLocal) {
            renderOriginSection('local', 'Local', 'local', state.projects, state.sessions, {
                isRemote: false,
                isOffline: false,
            });
        }
        for (const sid of showRemotes) {
            const srv = state.remoteServers.find((s) => s.id === sid);
            if (!srv) continue;
            const status = state.remoteOnline.get(sid) || 'unknown';
            const isOffline = status === 'offline';
            const projects = state.remoteProjects.get(sid);
            const sessions = state.remoteSessions.get(sid);

            // Lazy-load:
            //   - В режиме single-tab: грузим, когда секция раскрыта.
            //   - В All-view: грузим для всех online/unknown серверов (даже свёрнутых),
            //     чтобы агрегированный счётчик `N sess` был достоверным.
            //   - Offline-серверы НЕ грузим (заведомо упадёт).
            const shouldLazyLoad = !isOffline && (
                isAllView || !isOriginCollapsed(sid)
            );
            if (shouldLazyLoad) {
                if (projects === undefined) {
                    loadRemoteProjects(sid);
                }
                if (sessions === undefined) {
                    loadRemoteSessions(sid).then(() => renderSidebar());
                }
            }
            renderOriginSection(
                sid,
                srv.label || sid,
                status,
                projects || [],
                sessions || [],
                {
                    isRemote: true,
                    isOffline,
                    remoteLoading: !isOffline && sessions === undefined,
                },
            );
        }

        // Если ни одной секции/сессии — пустой плейсхолдер.
        if ($sidebar.children.length === 0) {
            const li = document.createElement('li');
            li.className = 'empty';
            li.textContent = 'Нет активных сессий';
            $sidebar.appendChild(li);
        }
    }

    /**
     * Рендерит ОДНУ origin-секцию в session-list:
     *   header (свёрнут/развёрнут) → projects (header) → sessions (item).
     *
     * Параметры:
     *   originKey — 'local' либо server_id (используется как ключ collapse-state).
     *   label — текст в header'е (например, 'Local' или srv.label).
     *   dotKind — 'online'|'offline'|'local'|'unknown' (цвет точки в header'е).
     *   projects — массив ProjectDto (с .id / .name) этого origin'а.
     *   sessions — массив SessionDto этого origin'а.
     *   opts.isRemote — true если это remote origin (используется для подписи "loading…").
     *   opts.isOffline — true если remote-сервер сейчас offline. Секция всё равно
     *                    рендерится (видна в All-view), но с классом 'origin-offline'
     *                    и явным бейджем "offline" вместо счётчика. Sessions не рендерим.
     *   opts.remoteLoading — true если сессии для этого remote ещё не загружены.
     */
    function renderOriginSection(originKey, label, dotKind, projects, sessions, opts) {
        opts = opts || {};
        const collapsed = isOriginCollapsed(originKey);
        const isOffline = !!opts.isOffline;

        const header = document.createElement('li');
        header.className = 'origin-group-header';
        if (isOffline) header.classList.add('origin-offline');
        header.dataset.origin = originKey;

        const caret = document.createElement('span');
        caret.className = 'origin-caret';
        caret.textContent = collapsed ? '▸' : '▾';
        header.appendChild(caret);

        const dot = document.createElement('span');
        dot.className = 'origin-dot ' + (dotKind || 'unknown');
        header.appendChild(dot);

        const lbl = document.createElement('span');
        lbl.className = 'origin-label';
        lbl.textContent = label;
        header.appendChild(lbl);

        // Phase 6 — для offline-серверов в All-view показываем явный badge
        // "offline" вместо счётчика сессий (он всё равно был бы 0 / устаревший).
        if (isOffline) {
            const badge = document.createElement('span');
            badge.className = 'origin-badge origin-badge-offline';
            badge.textContent = 'offline';
            header.appendChild(badge);
        } else {
            const meta = document.createElement('span');
            meta.className = 'origin-meta';
            meta.textContent = `${sessions.length} sess`;
            header.appendChild(meta);
        }

        header.addEventListener('click', () => {
            toggleOriginCollapsed(originKey);
            renderSidebar();
        });
        $sidebar.appendChild(header);

        if (collapsed) return;

        // Phase 6 — offline-секция: вместо списка сессий показываем
        // disabled-плейсхолдер. Это явный сигнал пользователю, что данные
        // недоступны (а не "0 сессий").
        if (isOffline) {
            const li = document.createElement('li');
            li.className = 'empty empty-offline';
            li.textContent = 'Сервер недоступен';
            $sidebar.appendChild(li);
            return;
        }

        if (opts.remoteLoading && sessions.length === 0) {
            const li = document.createElement('li');
            li.className = 'empty';
            li.textContent = 'Loading…';
            $sidebar.appendChild(li);
            return;
        }
        if (sessions.length === 0) {
            const li = document.createElement('li');
            li.className = 'empty';
            li.textContent = 'Нет активных сессий';
            $sidebar.appendChild(li);
            return;
        }

        // Cross-folder sessions visibility (origin-aware ветка):
        //   - projectFilter применяется ДО группировки (фильтр по project_id остаётся
        //     корректным, openSession→switchActiveProject использует sess.project_id).
        //   - Группируем отфильтрованные сессии origin'а по folder_id (orphan = null).
        //   - Header группы = folder_label первой сессии или basename из __folder:<path>.
        //   - Сортировка по folder_label (case-insensitive), Orphan в конце.
        //   - Параметр `projects` остаётся в сигнатуре для backward-совместимости,
        //     но больше не используется для header'ов (folder_id формата __folder:<path>
        //     не матчится с p.id).
        const ORPHAN_KEY = '__orphan__';
        const pf = state.projectFilter;
        const visible = (pf && pf !== '__all__')
            ? sessions.filter((s) => s.project_id === pf)
            : sessions;

        if (visible.length === 0) {
            const li = document.createElement('li');
            li.className = 'empty';
            li.textContent = (pf && pf !== '__all__')
                ? 'Нет сессий в этом проекте'
                : 'Нет активных сессий';
            $sidebar.appendChild(li);
            return;
        }

        const byFolder = groupSessionsByFolder(visible, ORPHAN_KEY);

        // Сортировка ключей по folder_label (case-insensitive), Orphan в конце.
        const nonOrphanKeys = [];
        for (const key of byFolder.keys()) {
            if (key !== ORPHAN_KEY) nonOrphanKeys.push(key);
        }
        nonOrphanKeys.sort((a, b) => {
            const la = (byFolder.get(a)[0].folder_label || a).toLowerCase();
            const lb = (byFolder.get(b)[0].folder_label || b).toLowerCase();
            return la.localeCompare(lb);
        });

        for (const key of nonOrphanKeys) {
            const arr = byFolder.get(key);
            if (!arr || arr.length === 0) continue;
            const keyDisplay = key.startsWith('__folder:') ? key.slice('__folder:'.length) : key;
            const ph = document.createElement('li');
            ph.className = 'project-sub-header';
            ph.textContent = arr[0].folder_label || keyDisplay;
            $sidebar.appendChild(ph);
            for (const sess of arr) {
                const li = buildSessionItem(sess);
                li.classList.add('in-origin');
                $sidebar.appendChild(li);
            }
        }
        const orphans = byFolder.get(ORPHAN_KEY);
        if (orphans && orphans.length > 0) {
            const ph = document.createElement('li');
            ph.className = 'project-sub-header';
            ph.textContent = 'Orphan';
            $sidebar.appendChild(ph);
            for (const sess of orphans) {
                const li = buildSessionItem(sess);
                li.classList.add('in-origin');
                $sidebar.appendChild(li);
            }
        }
    }

    /**
     * Вспомогательная функция, выделенная из renderOriginSection
     * и переиспользуемая регресс-тестами. Группирует массив сессий по
     * folder_id (orphan = null/undefined → ORPHAN_KEY). Внутри каждой
     * группы сортирует по name.localeCompare(). Возвращает Map<key, sessions[]>.
     *
     * Контракт совместим с legacy renderSidebar: ключ группы — folder_id
     * (формат __folder:<path>) или ORPHAN_KEY. Заголовок отображается через
     * folder_label первой сессии.
     */
    function groupSessionsByFolder(sessions, orphanKey) {
        const ORPHAN_KEY = orphanKey || '__orphan__';
        const byFolder = new Map();
        for (const sess of sessions) {
            const key = sess.folder_id == null ? ORPHAN_KEY : sess.folder_id;
            if (!byFolder.has(key)) byFolder.set(key, []);
            byFolder.get(key).push(sess);
        }
        for (const arr of byFolder.values()) {
            arr.sort((a, b) => a.name.localeCompare(b.name));
        }
        return byFolder;
    }

    /**
     * Phase 6 — агрегатор для All-view: собирает структуру
     *   { 'local': { projects, sessions, online }, [sid]: { projects, sessions, online }, ... }
     *
     * Источник: state.projects/state.sessions (local) + state.remoteProjects/
     * state.remoteSessions (remotes). Не делает сетевых запросов — работает
     * с уже загруженными кешами.
     *
     * Используется регресс-тестами (cca8.2) для верификации структуры
     * группировки origin → project → sessions в режиме activeOrigin='all'.
     *
     * Возвращает Map<originKey, { projects: ProjectDto[], sessions: SessionDto[],
     * online: 'online'|'offline'|'unknown'|'local', label: string }>.
     */
    function aggregateAllOrigins() {
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

    // Phase 6 — экспортируем хелперы в window.__forge для регресс-тестов
    // (cca8.2). В обычной работе они не используются глобально.
    if (typeof window !== 'undefined') {
        window.__forge = window.__forge || {};
        window.__forge.groupSessionsByFolder = groupSessionsByFolder;
        window.__forge.aggregateAllOrigins = aggregateAllOrigins;
    }

    function startPolling() {
        if (state.pollTimer) clearInterval(state.pollTimer);
        state.pollTimer = setInterval(fetchSessions, 3000);
    }

    function stopPolling() {
        if (state.pollTimer) {
            clearInterval(state.pollTimer);
            state.pollTimer = null;
        }
    }

    // =========================================================================
    // Session actions: new / kill / open (cx2.6)
    // =========================================================================

    async function createSessionPrompt() {
        const name = window.prompt('Имя новой tmux-сессии:', '');
        if (!name) return;
        const trimmed = name.trim();
        if (!trimmed) return;
        try {
            const resp = await fetch('/api/sessions', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ name: trimmed }),
            });
            if (!resp.ok) {
                const text = await resp.text();
                window.alert('Не удалось создать сессию: ' + (text || resp.status));
                return;
            }
            await fetchSessions();
            // Сразу попробуем открыть только что созданную сессию.
            openSession(trimmed);
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
        }
    }

    async function killSession(name, origin) {
        if (!window.confirm(`Убить сессию "${name}"?`)) return;
        try {
            const resp = await apiFetch('/api/sessions/' + encodeURIComponent(name), {
                method: 'DELETE',
            }, origin);
            if (!resp.ok && resp.status !== 204) {
                const text = await resp.text();
                window.alert('Не удалось убить сессию: ' + (text || resp.status));
                return;
            }
            // Если убитая — текущая, закрываем WS.
            if (state.currentSession === name) {
                disconnectWs();
                state.currentSession = null;
                showPlaceholder(true);
            }
            await fetchSessions();
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
        }
    }

    async function openSession(name, origin) {
        if (!name) return;
        // Phase 5: origin (если передан) перевешивает дефолтный поиск в
        // state.sessions. Это позволяет открыть remote-сессию по клику в
        // sidebar — там есть полный DTO с origin.
        const sessionKey = name; // legacy: ключи currentSession без origin.
        if (state.currentSession === sessionKey && state.ws && state.ws.readyState === WebSocket.OPEN) {
            return;
        }

        // Если сессия принадлежит другому проекту (registered ИЛИ auto-group
        // вида '__path__:<cwd>') — переключаем active project на бэке, чтобы
        // Tasks-таб подхватил .beads/ нужного каталога. Orphan (project_id=null)
        // пропускаем — переключать не на что.
        // Для remote-сессий backend-active-project переключать не нужно — там
        // активный проект живёт на стороне remote'а.
        const sess = state.sessions.find((s) => s.name === name);
        const sessOrigin = origin || dtoOrigin(sess);
        if (sessOrigin === 'local') {
            const targetProjectId = sess && sess.project_id ? sess.project_id : null;
            if (targetProjectId && targetProjectId !== state.activeProjectId) {
                await switchActiveProject(targetProjectId);
                connectWs(name, 'local');
                return;
            }
        }

        if (state.ws && state.ws.readyState === WebSocket.OPEN) {
            switchSession(name);
            return;
        }
        connectWs(name, sessOrigin);
    }

    function switchSession(name) {
        try {
            state.ws.send(JSON.stringify({ type: 'switch', session: name }));
            state.currentSession = name;
            // Очистим экран от предыдущей сессии и попросим redraw.
            if (state.term) state.term.reset();
            renderSidebar();
            // tmux пришлёт repaint автоматически (refresh-client). На всякий случай
            // повторно отправим текущий resize.
            scheduleResizeFromTerm();
        } catch (e) {
            console.warn('switch failed', e);
            // fallback — пересоздадим WS.
            disconnectWs();
            connectWs(name);
        }
    }

    function scheduleResizeFromTerm() {
        if (!state.term) return;
        const cols = state.term.cols;
        const rows = state.term.rows;
        // Сбросим lastResizeKey чтобы повторно отправить (после switch).
        state.lastResizeKey = '';
        sendResize(cols, rows);
    }

    // =========================================================================
    // WebSocket bridge (cx2.5)
    // =========================================================================

    function setStatus(kind, text) {
        $statusDot.classList.remove(
            'status-connected', 'status-connecting',
            'status-disconnected', 'status-error',
        );
        $statusDot.classList.add('status-' + kind);
        $statusText.textContent = text;
    }

    function showPlaceholder(visible) {
        if (visible) {
            $placeholder.classList.remove('hidden');
        } else {
            $placeholder.classList.add('hidden');
        }
    }

    function connectWs(sessionName, origin) {
        // На всякий случай закрываем старый.
        disconnectWs();

        if (!state.term) {
            console.error('terminal not initialized');
            return;
        }

        // Перед подключением fit — чтобы прислать корректные cols/rows в query.
        try { state.fitAddon.fit(); } catch (_) {}
        const cols = state.term.cols || 80;
        const rows = state.term.rows || 24;

        const proto = (location.protocol === 'https:') ? 'wss:' : 'ws:';
        // Phase 5: origin !== 'local' → добавляем &server=<id>, бэкенд прокинет
        // WS на remote через remote_proxy.
        const serverParam = (isRemoteMode() && origin && origin !== 'local')
            ? `&server=${encodeURIComponent(origin)}`
            : '';
        const url = `${proto}//${location.host}/ws/attach`
            + `?session=${encodeURIComponent(sessionName)}`
            + `&cols=${cols}&rows=${rows}`
            + serverParam;

        setStatus('connecting', `connecting → ${sessionName}…`);
        state.currentSession = sessionName;
        state.attachWsOrigin = origin || null;
        renderSidebar();

        let ws;
        try {
            ws = new WebSocket(url);
        } catch (e) {
            console.error('WebSocket ctor failed', e);
            setStatus('error', 'ws ctor error');
            // Phase 7 — расценим как обрыв и попробуем backoff-reconnect.
            scheduleAttachWsReconnect();
            return;
        }
        ws.binaryType = 'arraybuffer';
        state.ws = ws;
        state.lastResizeKey = cols + 'x' + rows;

        ws.onopen = () => {
            setStatus('connected', `attached → ${sessionName}`);
            showPlaceholder(false);
            // Phase 7 — успешный коннект сбрасывает backoff.
            state.attachWsBackoffStep = 0;
            if (state.term) {
                state.term.reset();
                state.term.focus();
            }
        };

        ws.onmessage = (ev) => {
            const data = ev.data;
            if (data instanceof ArrayBuffer) {
                // Бинарь — сырые байты PTY.
                if (state.term) {
                    state.term.write(new Uint8Array(data));
                }
            } else if (typeof data === 'string') {
                // Сейчас сервер не шлёт control-сообщения клиенту, но на будущее.
                try {
                    const msg = JSON.parse(data);
                    handleControlFromServer(msg);
                } catch (_) {
                    // Не JSON — выводим как обычный текст.
                    if (state.term) state.term.write(data);
                }
            }
        };

        ws.onerror = (ev) => {
            console.warn('ws error', ev);
            setStatus('error', 'ws error');
        };

        ws.onclose = (ev) => {
            console.info('ws closed', ev.code, ev.reason);
            state.ws = null;
            // Phase 7 — если закрытие НЕ инициировано нами, пробуем reconnect
            // с экспоненциальным backoff, сохраняя currentSession и origin.
            // attachWsClosedByUs выставляется в disconnectWs() (при switch,
            // beforeunload, etc.). При успехе onopen → backoff сбрасывается.
            if (state.attachWsClosedByUs) {
                state.attachWsClosedByUs = false;
                setStatus('disconnected', 'disconnected');
                return;
            }
            setStatus('reconnecting', 'reconnecting…');
            scheduleAttachWsReconnect();
        };
    }

    /**
     * Phase 7 — backoff-серия для reconnect /ws/attach (мс, без jitter; jitter
     * добавляется в scheduleAttachWsReconnect). Аналог TASKS_WS_BACKOFFS_MS,
     * но длиннее, потому что attach — самый «дорогой» reconnect (terminal
     * resync через tmux refresh-client).
     */
    const ATTACH_WS_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000];
    const ATTACH_WS_JITTER_MAX_MS = 1000;

    function scheduleAttachWsReconnect() {
        if (state.attachWsClosedByUs) return;
        if (state.attachWsReconnectTimer) return;
        const session = state.currentSession;
        if (!session) return;
        const origin = state.attachWsOrigin || null;
        const idx = Math.min(state.attachWsBackoffStep || 0, ATTACH_WS_BACKOFFS_MS.length - 1);
        const base = ATTACH_WS_BACKOFFS_MS[idx];
        const jitter = Math.floor(Math.random() * ATTACH_WS_JITTER_MAX_MS);
        const delay = base + jitter;
        state.attachWsBackoffStep = Math.min(
            (state.attachWsBackoffStep || 0) + 1,
            ATTACH_WS_BACKOFFS_MS.length - 1,
        );
        state.attachWsReconnectTimer = setTimeout(() => {
            state.attachWsReconnectTimer = null;
            // Не реконнектим, если currentSession поменялся / снёсся.
            if (!state.currentSession) return;
            connectWs(state.currentSession, origin);
        }, delay);
    }

    function handleControlFromServer(msg) {
        // Заготовка под будущее (например, серверный bell, error).
        if (!msg || typeof msg !== 'object') return;
        if (msg.type === 'error') {
            console.warn('server reported error:', msg.message);
        }
    }

    function disconnectWs() {
        // Phase 7 — пометим, что закрытие инициировано нами; onclose не
        // запланирует reconnect.
        state.attachWsClosedByUs = true;
        if (state.attachWsReconnectTimer) {
            clearTimeout(state.attachWsReconnectTimer);
            state.attachWsReconnectTimer = null;
        }
        if (state.ws) {
            try {
                state.ws.onmessage = null;
                state.ws.onerror = null;
                state.ws.onclose = null;
                state.ws.close();
            } catch (_) {}
            state.ws = null;
        }
    }

    // =========================================================================
    // Phase 6.A — Tasks tab: switchTab / fetchTasks / renderTasks
    // =========================================================================

    /**
     * Канонический порядок колонок kanban-board.
     *
     * Phase 4: добавлена колонка 'todo' слева — это user-side TODO-карточки
     * из state.todosData (а не bd-issues). Все остальные — bd-issues из
     * state.tasksData. closed идёт последней.
     */
    const TASK_COLUMNS = ['todo', 'open', 'in_progress', 'blocked', 'deferred', 'draft', 'closed'];

    /**
     * Человекочитаемые заголовки колонок.
     */
    const COLUMN_TITLES = {
        todo: 'TODO',
        open: 'Open',
        in_progress: 'In progress',
        blocked: 'Blocked',
        deferred: 'Deferred',
        draft: 'Draft',
        closed: 'Closed',
    };

    /**
     * Максимум карточек в `closed` чтобы не нагружать DOM при больших архивах.
     */
    const CLOSED_LIMIT = 20;

    /**
     * Polling-интервал для /api/tasks как fallback, когда realtime WS недоступен.
     * Phase 6.D: при живом WS poll отключается; при reconnect-attempts либо
     * полном фейле — включается раз в 30s, чтобы UI хотя бы лениво обновлялся.
     */
    const TASKS_POLL_INTERVAL_MS = 30000;

    /**
     * Phase 6.D — backoff-серия (мс) для reconnect WS /ws/tasks.
     * После исчерпания серии остаёмся на последнем (10s).
     */
    const TASKS_WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

    /**
     * Phase 4 — backoff-серия (мс) для reconnect WS /ws/todos.
     * Полный аналог TASKS_WS_BACKOFFS_MS.
     */
    const TODOS_WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

    /**
     * Phase 4 — fallback polling-интервал /api/todos. Используется когда WS
     * /ws/todos упал (degraded mode). При живом WS poll отключается.
     */
    const TODOS_POLL_INTERVAL_MS = 30000;

    /**
     * Переключает активный таб (Terminal ↔ Tasks).
     *
     * - Toggle hidden на `#terminal` / `#tasks` и `.active` на `.tab-btn`.
     * - При переходе на Terminal: fitAddon.fit() + повторный resize, чтобы
     *   xterm пересчитал cols/rows (контейнер мог быть скрыт ранее).
     * - При переходе на Tasks: запускаем fetchTasks (если нет данных) +
     *   polling. При уходе — clearInterval.
     */
    function switchTab(name) {
        if (name !== 'terminal' && name !== 'tasks' && name !== 'git') return;
        if (state.activeTab === name) return;
        const prev = state.activeTab;
        state.activeTab = name;

        const onTerminal = name === 'terminal';
        const onTasks = name === 'tasks';
        const onGit = name === 'git';
        // Видимость контейнеров.
        $terminalEl.hidden = !onTerminal;
        if ($placeholder) $placeholder.hidden = !onTerminal;
        $tasksEl.hidden = !onTasks;
        if ($gitEl) $gitEl.hidden = !onGit;

        // Active state на кнопках.
        $tabTerminal.classList.toggle('active', onTerminal);
        $tabTasks.classList.toggle('active', onTasks);
        if ($tabGit) $tabGit.classList.toggle('active', onGit);

        // Уход с Git — закрываем WS /ws/lazygit.
        if (prev === 'git' && !onGit) {
            // При уходе с git-таба закрываем WS /ws/lazygit.
            // Сам term оставляем смонтированным — реактивация быстрее без
            // term.dispose() + повторного term.open().
            closeGitWs('tab switched away');
        }
        // Уход с Tasks — гасим fallback polling (WS остаётся живым).
        if (prev === 'tasks' && !onTasks) {
            stopTasksPolling();
        }

        if (onTerminal) {
            // Дать браузеру применить hidden=false и уже потом fit.
            requestAnimationFrame(() => {
                try { state.fitAddon && state.fitAddon.fit(); } catch (_) {}
                if (state.term) {
                    scheduleResizeFromTerm();
                    state.term.focus();
                }
            });
        } else if (onTasks) {
            // Phase 6.D: запускаем WS если ещё не подключены. Snapshot
            // придёт первым сообщением и заполнит state.tasksData.
            if (state.tasksData == null) {
                // Покажем что-нибудь немедленно даже если WS лагает.
                fetchTasks();
            } else {
                renderTasks();
            }
            connectTasksWs();
        } else if (onGit) {
            // Phase 4 (lazygit-tab): открыть xterm-сессию /ws/lazygit для
            // активного проекта. Если активного проекта нет — показать
            // placeholder, скрыть term и не открывать WS.
            openLazygitForActiveProject();
        }
    }

    // -------------------------------------------------------------------------
    // Phase 4 — lazygit-tab: xterm Terminal + /ws/lazygit WebSocket
    // -------------------------------------------------------------------------

    /**
     * Возвращает активный проект (ProjectDto с .path) или null если его нет.
     * state.activeProjectId может указывать на transient `__path__:...` id,
     * который не присутствует в state.projects — тогда возвращаем null.
     */
    function getActiveProject() {
        const id = state.activeProjectId;
        if (!id) return null;
        const list = Array.isArray(state.projects) ? state.projects : [];
        const found = list.find((p) => p && p.id === id);
        if (found) return found;
        // Transient auto-group id вида `__path__:<cwd>` — registered project
        // нет, но cwd известен. Возвращаем pseudo-project, чтобы lazygit-tab
        // мог открыть терминал с этим cwd.
        if (typeof id === 'string' && id.startsWith('__path__:')) {
            const cwd = id.slice('__path__:'.length);
            if (cwd) return { id, name: cwd, path: cwd };
        }
        return null;
    }

    /**
     * Ленивая инициализация второй инстанции xterm.js — изолированной от
     * основного state.term. Использует те же опции (theme/font), но Web
     * Links не нужен (lazygit рисует TUI, URLs там не отображаются обычно).
     *
     * Идемпотентно: повторные вызовы возвращают существующий term.
     */
    function mountGitTerm() {
        const gt = state.gitTerm;
        if (gt.mounted && gt.term) return gt.term;
        const Terminal = window.Terminal;
        const FitAddon = window.FitAddon && window.FitAddon.FitAddon;
        if (!Terminal || !FitAddon) {
            console.error('[lazygit] xterm.js / FitAddon not loaded');
            return null;
        }
        if (!$gitTermEl) {
            console.error('[lazygit] #git-term element missing');
            return null;
        }

        // Опции совпадают с основным term (initTerminal) — UX единообразен.
        const fallbackTheme = {
            background: '#000000',
            foreground: '#d8dee9',
            cursor: '#d8dee9',
            selectionBackground: '#3a4356',
        };
        // mapTermTheme возможно недоступен до bootstrap-loadActiveTheme;
        // safe-fallback на инлайн-тему. Активная тема применится при
        // следующем applyTheme через переоткрытие term (Phase 3 redraw).
        const termTheme = (state.activeTheme && typeof mapTermTheme === 'function')
            ? mapTermTheme(state.activeTheme)
            : fallbackTheme;

        const term = new Terminal({
            cursorBlink: true,
            fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
            fontSize: 13,
            scrollback: 5000,
            allowProposedApi: true,
            theme: termTheme || fallbackTheme,
        });
        const fit = new FitAddon();
        term.loadAddon(fit);
        term.open($gitTermEl);
        try { fit.fit(); } catch (e) { console.warn('[lazygit] initial fit failed', e); }

        // Ввод пользователя в xterm → WS (raw bytes).
        // lazygit ожидает байты в Binary frames — отправляем как Uint8Array.
        term.onData((data) => {
            const ws = state.gitTerm.ws;
            if (ws && ws.readyState === WebSocket.OPEN) {
                try {
                    ws.send(state.encoder.encode(data));
                } catch (e) {
                    console.warn('[lazygit] ws.send (input) failed', e);
                }
            }
        });

        // onResize xterm (изменение cols/rows) → control JSON в Text frame.
        // Срабатывает после fit.fit() если геометрия pty изменилась.
        term.onResize(({ cols, rows }) => {
            const ws = state.gitTerm.ws;
            if (ws && ws.readyState === WebSocket.OPEN) {
                try {
                    ws.send(JSON.stringify({ type: 'resize', cols, rows }));
                } catch (e) {
                    console.warn('[lazygit] ws.send (resize) failed', e);
                }
            }
        });

        // ResizeObserver на #git-term — ловит любые изменения размера контейнера
        // (window resize, sidebar collapse, font-size change, tab show/hide).
        // Дополнительно слушаем window resize как страховку.
        try {
            const ro = new ResizeObserver(() => {
                if (state.activeTab !== 'git') return;
                try { state.gitTerm.fit && state.gitTerm.fit.fit(); } catch (_) {}
            });
            ro.observe($gitTermEl);
            state.gitTerm.resizeObserver = ro;
        } catch (_) { /* ResizeObserver missing — fallback на window resize ниже */ }
        window.addEventListener('resize', () => {
            if (state.activeTab !== 'git') return;
            try { state.gitTerm.fit && state.gitTerm.fit.fit(); } catch (_) {}
        });

        state.gitTerm.term = term;
        state.gitTerm.fit = fit;
        state.gitTerm.mounted = true;
        return term;
    }

    /**
     * Открывает (или переподключает) WS /ws/lazygit для активного проекта.
     * Точка входа из switchTab('git') и из switchActiveProject (как fallback,
     * если SwitchCwd-control не сработал).
     *
     * Поведение:
     *  - Нет активного проекта → показать placeholder, скрыть #git-term,
     *    закрыть WS (если был).
     *  - Активный проект есть → скрыть placeholder, смонтировать term
     *    (если ещё не), закрыть прежнюю WS (если cwd сменился), открыть
     *    WS на новый cwd.
     */
    function openLazygitForActiveProject() {
        const project = getActiveProject();
        if (!project || !project.path) {
            // Placeholder visible, term hidden, WS closed.
            if ($gitPlaceholder) $gitPlaceholder.hidden = false;
            if ($gitTermEl) $gitTermEl.hidden = true;
            closeGitWs('no active project');
            return;
        }

        if ($gitPlaceholder) $gitPlaceholder.hidden = true;
        if ($gitTermEl) $gitTermEl.hidden = false;

        const term = mountGitTerm();
        if (!term) {
            showGitBanner('Failed to initialize terminal (xterm.js not loaded)');
            return;
        }

        // requestAnimationFrame чтобы pane получил размер (был hidden), и
        // только потом fit.fit() — иначе cols/rows будут от 0×0.
        requestAnimationFrame(() => {
            try { state.gitTerm.fit && state.gitTerm.fit.fit(); } catch (_) {}
            connectGitWs(project.path);
            // Focus в term, чтобы пользователь сразу мог жать клавиши.
            try { term.focus(); } catch (_) {}
        });
    }

    /**
     * Открывает WebSocket /ws/lazygit?cwd=...&cols=...&rows=...
     * для указанного cwd. Если есть открытый WS на тот же cwd —
     * no-op. Если на другой cwd — close + reconnect.
     */
    function connectGitWs(cwd) {
        const gt = state.gitTerm;
        // Уже подключены к нужному cwd?
        if (gt.ws && gt.ws.readyState === WebSocket.OPEN && gt.currentCwd === cwd) {
            return;
        }
        // CONNECTING к нужному cwd — дождёмся onopen.
        if (gt.ws && gt.ws.readyState === WebSocket.CONNECTING && gt.currentCwd === cwd) {
            return;
        }
        // Другой cwd или закрытое соединение — закрыть и переподключиться.
        if (gt.ws) {
            try {
                gt.ws.onopen = null;
                gt.ws.onmessage = null;
                gt.ws.onerror = null;
                gt.ws.onclose = null;
                gt.ws.close();
            } catch (_) {}
            gt.ws = null;
        }
        if (!gt.term) {
            console.warn('[lazygit] connectGitWs: term not mounted');
            return;
        }
        const cols = gt.term.cols || 80;
        const rows = gt.term.rows || 24;
        const proto = location.protocol === 'https:' ? 'wss' : 'ws';
        // Phase 5: для remote origin (state.activeOrigin не 'local'/'all') —
        // прокидываем &server=<id>, бэкенд прокинет WS на remote через
        // remote_proxy. В legacy / 'local' / 'all' — URL без параметра.
        const lgServer = (isRemoteMode()
            && state.activeOrigin
            && state.activeOrigin !== 'local'
            && state.activeOrigin !== 'all')
            ? `&server=${encodeURIComponent(state.activeOrigin)}`
            : '';
        const url = `${proto}://${location.host}/ws/lazygit?cwd=${encodeURIComponent(cwd)}&cols=${cols}&rows=${rows}${lgServer}`;

        let ws;
        try {
            ws = new WebSocket(url);
        } catch (e) {
            console.warn('[lazygit] WebSocket constructor failed', e);
            showGitBanner('Failed to open WebSocket: ' + (e && e.message ? e.message : String(e)));
            return;
        }
        ws.binaryType = 'arraybuffer';
        gt.ws = ws;
        gt.currentCwd = cwd;
        gt.errorSticky = false;

        ws.onopen = () => {
            // На успешный connect снимаем banner (если был от прошлой попытки).
            hideGitBanner();
            // Принудительно send resize — backend строит pty c cols/rows из
            // query, но если xterm к этому моменту перепосчитал размер
            // (после rAF + fit.fit), pty надо синхронизировать.
            try {
                if (gt.term) {
                    ws.send(JSON.stringify({
                        type: 'resize',
                        cols: gt.term.cols,
                        rows: gt.term.rows,
                    }));
                }
            } catch (_) {}
        };

        ws.onmessage = (ev) => {
            const data = ev.data;
            if (data instanceof ArrayBuffer) {
                // Binary frame — raw pty output, в xterm.
                try {
                    gt.term.write(new Uint8Array(data));
                } catch (e) {
                    console.warn('[lazygit] term.write failed', e);
                }
                return;
            }
            // Text frame — control JSON. Backend шлёт {type:"error",message:"..."}
            // при ошибках запуска lazygit / pty.
            if (typeof data === 'string') {
                let payload;
                try {
                    payload = JSON.parse(data);
                } catch (_) {
                    console.warn('[lazygit] non-JSON text frame:', data);
                    return;
                }
                if (payload && payload.type === 'error' && typeof payload.message === 'string') {
                    let msg = payload.message;
                    const lower = msg.toLowerCase();
                    const notFound = lower.includes('lazygit') && (lower.includes('not found') || lower.includes('no such file'));
                    if (notFound) {
                        msg = 'lazygit not found in PATH. Install it using one of the commands below:';
                    }
                    showGitBanner(msg, { showInstall: notFound });
                    gt.errorSticky = true;
                }
            }
        };

        ws.onerror = (ev) => {
            console.debug('[lazygit] ws error', ev);
        };

        ws.onclose = (ev) => {
            // Если уже показали error-banner с message (lazygit-not-found etc) —
            // не перезатираем «Connection lost». Иначе пользователь потеряет
            // конкретный диагноз.
            if (gt.ws === ws) {
                gt.ws = null;
            }
            if (!gt.errorSticky && state.activeTab === 'git') {
                const reason = ev && ev.reason ? ev.reason : '';
                const code = ev && typeof ev.code === 'number' ? ev.code : 0;
                // Нормальное закрытие (1000/1001) — не шумим.
                if (code !== 1000 && code !== 1001) {
                    showGitBanner('Connection lost' + (reason ? ': ' + reason : '') + '. Press Retry.');
                }
            }
        };
    }

    /**
     * Закрывает текущий WS /ws/lazygit (если есть). Не вызывает term.dispose().
     * Если `silent` (по умолчанию) — onclose не показывает banner.
     */
    function closeGitWs(reason) {
        const gt = state.gitTerm;
        if (!gt.ws) return;
        try {
            // Снимаем обработчики чтобы onclose не показал banner при ручном close.
            gt.ws.onopen = null;
            gt.ws.onmessage = null;
            gt.ws.onerror = null;
            gt.ws.onclose = null;
            gt.ws.close(1000, reason || 'closed');
        } catch (e) {
            console.debug('[lazygit] close failed', e);
        }
        gt.ws = null;
    }

    /**
     * Отправляет ws.send {type:"switch_cwd", cwd}. Backend перезапустит
     * lazygit в новом cwd с тем же pty. Если WS не OPEN — fallback на
     * reconnect через closeGitWs+connectGitWs.
     */
    function gitSwitchCwd(newCwd) {
        const gt = state.gitTerm;
        if (!newCwd) return;
        if (!gt.ws || gt.ws.readyState !== WebSocket.OPEN) {
            // Нет живого WS — просто переподключаемся.
            connectGitWs(newCwd);
            return;
        }
        try {
            // Очистим term перед переключением, чтобы старый вывод не
            // путал пользователя.
            if (gt.term) {
                try { gt.term.clear(); } catch (_) {}
            }
            gt.ws.send(JSON.stringify({ type: 'switch_cwd', cwd: newCwd }));
            gt.currentCwd = newCwd;
        } catch (e) {
            console.warn('[lazygit] switch_cwd send failed, falling back to reconnect', e);
            closeGitWs('switch_cwd failed');
            connectGitWs(newCwd);
        }
    }

    /**
     * Banner-плашка ошибки в git-tab. Показывает текст, кнопки Retry/×.
     * Перезаписывает текст при повторном вызове.
     */
    function showGitBanner(message, opts) {
        if (!$gitError || !$gitErrorText) return;
        $gitErrorText.textContent = message;
        $gitError.hidden = false;
        const showInstall = !!(opts && opts.showInstall);
        if (showInstall) {
            renderInstallHelp();
            if ($gitInstallHelp) $gitInstallHelp.hidden = false;
        } else if ($gitInstallHelp) {
            $gitInstallHelp.hidden = true;
        }
    }

    /**
     * Скрывает banner.
     */
    function hideGitBanner() {
        if (!$gitError) return;
        $gitError.hidden = true;
        if ($gitInstallHelp) $gitInstallHelp.hidden = true;
        state.gitTerm.errorSticky = false;
    }

    /**
     * Эвристика определения OS клиента по navigator.userAgent / userAgentData.
     * Возвращает 'mac' | 'linux-debian' | 'linux-arch' | 'linux-fedora' | 'linux' | 'windows' | null.
     * Дистрибутив Linux точно определить нельзя, поэтому возвращаем 'linux' и
     * показываем все варианты (apt/pacman/dnf) без detected-метки.
     */
    function detectClientOS() {
        const nav = (typeof navigator !== 'undefined') ? navigator : null;
        if (!nav) return null;
        const ua = (nav.userAgent || '').toLowerCase();
        const platform = (nav.platform || '').toLowerCase();
        if (platform.includes('mac') || ua.includes('mac os x') || ua.includes('macintosh')) return 'mac';
        if (platform.includes('win') || ua.includes('windows')) return 'windows';
        if (platform.includes('linux') || ua.includes('linux') || ua.includes('x11')) return 'linux';
        return null;
    }

    /**
     * Рендерит список команд установки lazygit для разных OS внутри
     * #git-install-list. Текущая OS получает класс .detected (выводится первой).
     */
    function renderInstallHelp() {
        if (!$gitInstallList) return;
        const detected = detectClientOS();
        const entries = [
            { id: 'mac',      label: 'macOS (Homebrew)',     cmd: 'brew install lazygit' },
            { id: 'mac-port', label: 'macOS (MacPorts)',     cmd: 'sudo port install lazygit' },
            { id: 'linux-debian', label: 'Debian / Ubuntu', cmd: 'LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | grep -Po \'"tag_name": "v\\K[^"]*\') && \\\ncurl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz" && \\\ntar xf lazygit.tar.gz lazygit && sudo install lazygit -D -t /usr/local/bin/' },
            { id: 'linux-arch',   label: 'Arch Linux',       cmd: 'sudo pacman -S lazygit' },
            { id: 'linux-fedora', label: 'Fedora',           cmd: 'sudo dnf copr enable atim/lazygit -y && sudo dnf install lazygit' },
            { id: 'windows',  label: 'Windows (winget)',     cmd: 'winget install -e --id=JesseDuffield.lazygit' },
            { id: 'windows-scoop', label: 'Windows (Scoop)', cmd: 'scoop install lazygit' },
            { id: 'go',       label: 'Go (any OS)',          cmd: 'go install github.com/jesseduffield/lazygit@latest' },
        ];

        const isDetected = (id) => {
            if (!detected) return false;
            if (detected === 'mac' && (id === 'mac' || id === 'mac-port')) return true;
            if (detected === 'linux' && id.startsWith('linux-')) return true;
            if (detected === 'windows' && id.startsWith('windows')) return true;
            return false;
        };

        entries.sort((a, b) => Number(isDetected(b.id)) - Number(isDetected(a.id)));

        $gitInstallList.innerHTML = '';
        for (const e of entries) {
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
            $gitInstallList.appendChild(li);
        }
    }

    /**
     * Копирует строку в буфер. Использует Clipboard API если доступен,
     * fallback — скрытый textarea + execCommand. Возвращает Promise<boolean>.
     */
    function copyToClipboardSafe(text) {
        if (navigator.clipboard && navigator.clipboard.writeText) {
            return navigator.clipboard.writeText(text).then(() => true).catch(() => fallbackCopy(text));
        }
        return Promise.resolve(fallbackCopy(text));
    }

    function fallbackCopy(text) {
        try {
            const ta = document.createElement('textarea');
            ta.value = text;
            ta.setAttribute('readonly', '');
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            const ok = document.execCommand('copy');
            document.body.removeChild(ta);
            return ok;
        } catch (_) {
            return false;
        }
    }

    /**
     * Retry: пытается переоткрыть WS для текущего активного проекта.
     */
    function retryGitConnection() {
        hideGitBanner();
        // Сбросим cwd чтобы connectGitWs точно открыл новое соединение.
        const gt = state.gitTerm;
        closeGitWs('retry');
        gt.currentCwd = null;
        openLazygitForActiveProject();
    }

    /**
     * Запускает fallback-polling /api/tasks. Используется только когда WS
     * не подключен или в режиме reconnect/error. При живом WS — poll не нужен.
     */
    function startTasksPolling() {
        if (state.tasksPollTimer) clearInterval(state.tasksPollTimer);
        state.tasksPollTimer = setInterval(fetchTasks, TASKS_POLL_INTERVAL_MS);
    }

    function stopTasksPolling() {
        if (state.tasksPollTimer) {
            clearInterval(state.tasksPollTimer);
            state.tasksPollTimer = null;
        }
    }

    // -------------------------------------------------------------------------
    // Phase 6.D — Realtime tasks WS
    // -------------------------------------------------------------------------

    /**
     * Открывает (или переоткрывает) WebSocket /ws/tasks.
     *
     * - Если уже OPEN/CONNECTING — no-op.
     * - При успешном connect — backoffStep сбрасывается, fallback polling
     *   останавливается.
     * - На onclose/onerror — schedule reconnect через TASKS_WS_BACKOFFS_MS,
     *   и поднимается fallback polling.
     */
    function connectTasksWs() {
        if (state.tasksWs && (
            state.tasksWs.readyState === WebSocket.OPEN ||
            state.tasksWs.readyState === WebSocket.CONNECTING
        )) {
            return;
        }
        state.tasksWsClosedByUs = false;

        // Cleanup pending reconnect timer (manual call перебивает schedule).
        if (state.tasksWsReconnectTimer) {
            clearTimeout(state.tasksWsReconnectTimer);
            state.tasksWsReconnectTimer = null;
        }

        const proto = location.protocol === 'https:' ? 'wss' : 'ws';
        const pid = state.activeProjectId || '';
        // Phase 5: при выборе remote origin'а через origin-табы — подписываемся
        // на /ws/tasks remote-сервера (бэкенд прокинет WS через remote_proxy).
        const server = (isRemoteMode()
            && state.activeOrigin
            && state.activeOrigin !== 'local'
            && state.activeOrigin !== 'all')
            ? state.activeOrigin
            : null;
        let qs = '';
        if (pid && !server) {
            qs = `?project_id=${encodeURIComponent(pid)}`;
        } else if (server) {
            // Для remote — pid живёт на той стороне, мы его не знаем.
            qs = `?server=${encodeURIComponent(server)}`;
        }
        const url = `${proto}://${location.host}/ws/tasks${qs}`;
        let ws;
        try {
            ws = new WebSocket(url);
        } catch (e) {
            console.warn('tasks ws constructor failed', e);
            scheduleTasksWsReconnect();
            return;
        }
        state.tasksWs = ws;
        setTasksStatus('reconnecting', 'tasks: connecting…');

        ws.onopen = () => {
            state.tasksWsBackoffStep = 0;
            // Fallback poll не нужен пока WS жив.
            stopTasksPolling();
            setTasksStatus('ok', 'tasks: live');
        };
        ws.onmessage = (ev) => {
            handleTasksWsMessage(ev.data);
        };
        ws.onerror = (ev) => {
            console.debug('tasks ws error', ev);
            setTasksStatus('error', 'tasks: ws error');
        };
        ws.onclose = () => {
            state.tasksWs = null;
            if (state.tasksWsClosedByUs) {
                setTasksStatus('ok', '');
                return;
            }
            // Live → degraded: включаем fallback polling и планируем reconnect.
            setTasksStatus('reconnecting', 'tasks: reconnecting…');
            startTasksPolling();
            scheduleTasksWsReconnect();
        };
    }

    /**
     * Закрывает WS и подавляет авто-reconnect (используется при beforeunload
     * и переключении проекта — потом мы открываем заново).
     */
    function disconnectTasksWs() {
        state.tasksWsClosedByUs = true;
        if (state.tasksWsReconnectTimer) {
            clearTimeout(state.tasksWsReconnectTimer);
            state.tasksWsReconnectTimer = null;
        }
        if (state.tasksWs) {
            try { state.tasksWs.close(); } catch (_) {}
            state.tasksWs = null;
        }
    }

    /**
     * Schedule reconnect через текущий шаг backoff. Каждый раз когда мы
     * заходим сюда, шаг увеличивается до конца серии и там фиксируется.
     */
    function scheduleTasksWsReconnect() {
        if (state.tasksWsClosedByUs) return;
        if (state.tasksWsReconnectTimer) return;
        const idx = Math.min(state.tasksWsBackoffStep, TASKS_WS_BACKOFFS_MS.length - 1);
        const delay = TASKS_WS_BACKOFFS_MS[idx];
        state.tasksWsBackoffStep = Math.min(state.tasksWsBackoffStep + 1, TASKS_WS_BACKOFFS_MS.length - 1);
        state.tasksWsReconnectTimer = setTimeout(() => {
            state.tasksWsReconnectTimer = null;
            connectTasksWs();
        }, delay);
    }

    /**
     * Обработчик одного WS-сообщения. JSON-кадры по протоколу:
     *  - {kind:"snapshot", data:{issues,total,...}}
     *  - {kind:"upsert", issue:{...}}
     *  - {kind:"removed", id:"..."}
     *  - {kind:"reload"}  → форсированный fetchTasks().
     */
    function handleTasksWsMessage(raw) {
        let msg;
        try {
            msg = JSON.parse(raw);
        } catch (e) {
            console.warn('tasks ws: non-JSON message', raw);
            return;
        }
        if (!msg || typeof msg !== 'object') return;

        switch (msg.kind) {
            case 'snapshot':
                state.tasksData = msg.data || { issues: [], total: 0 };
                renderTasks();
                break;

            case 'upsert': {
                const issue = msg.issue;
                if (!issue || typeof issue !== 'object' || !issue.id) {
                    console.warn('upsert without issue.id', msg);
                    return;
                }
                if (!state.tasksData || !Array.isArray(state.tasksData.issues)) {
                    state.tasksData = { issues: [issue], total: 1 };
                } else {
                    const arr = state.tasksData.issues;
                    const i = arr.findIndex((it) => it && it.id === issue.id);
                    if (i >= 0) {
                        arr[i] = issue;
                    } else {
                        arr.unshift(issue);
                        if (typeof state.tasksData.total === 'number') {
                            state.tasksData.total += 1;
                        }
                    }
                }
                renderTasks();
                break;
            }

            case 'removed': {
                const id = msg.id;
                if (!id || !state.tasksData || !Array.isArray(state.tasksData.issues)) return;
                const arr = state.tasksData.issues;
                const i = arr.findIndex((it) => it && it.id === id);
                if (i >= 0) {
                    arr.splice(i, 1);
                    if (typeof state.tasksData.total === 'number') {
                        state.tasksData.total = Math.max(0, state.tasksData.total - 1);
                    }
                    renderTasks();
                }
                break;
            }

            case 'reload':
                fetchTasks();
                break;

            default:
                console.debug('tasks ws: unknown kind', msg.kind);
        }
    }

    /**
     * Загружает snapshot задач c /api/tasks. На любой ошибке (network/HTTP)
     * не рушит UI, а кладёт пустой envelope, чтобы board остался отрисованным.
     */
    async function fetchTasks() {
        try {
            const r = await fetch('/api/tasks', { headers: { 'Accept': 'application/json' } });
            if (!r.ok) {
                console.warn('GET /api/tasks failed:', r.status);
                state.tasksData = { issues: [], total: 0 };
                setTasksStatus('error', 'tasks: HTTP ' + r.status);
                renderTasks();
                return;
            }
            state.tasksData = await r.json();
            setTasksStatus('ok', '');
            renderTasks();
        } catch (e) {
            console.warn('fetchTasks failed', e);
            state.tasksData = state.tasksData || { issues: [], total: 0 };
            setTasksStatus('error', 'tasks: network');
            renderTasks();
        }
    }

    function setTasksStatus(_kind, text) {
        if ($tasksStatus) $tasksStatus.textContent = text || '';
    }

    // -------------------------------------------------------------------------
    // Phase 4 — TODOs: REST + realtime WS (паттерн из tasks-ws/tasks-poll)
    // -------------------------------------------------------------------------

    /**
     * Возвращает project_id, на который должен фильтроваться TODO-стрим.
     *
     * Используем backend-side `state.activeProjectId`, а не UI-фильтр
     * `state.projectFilter` ('__all__' / id), потому что TODO привязаны к
     * конкретному `.forge/todos.json` активного проекта. Sidebar-фильтр
     * сессий — это отдельная UI-семантика.
     *
     * Возвращает null если активный проект ещё не известен (до первого
     * fetchProjects). Вызовы fetchTodos/connectTodosWs должны это учитывать
     * и сделать early-return.
     */
    function currentTodosProjectId() {
        return state.activeProjectId || null;
    }

    /**
     * GET /api/todos[?project_id=...] → state.todosData = массив TODO.
     * Если projectId пуст, бэкенд возьмёт активный проект (то же поведение,
     * что и /ws/todos без фильтра). При ошибке кладём пустой массив, чтобы
     * board остался отрисованным.
     */
    async function fetchTodos(projectId) {
        const pid = projectId || currentTodosProjectId();
        try {
            const url = pid ? '/api/todos?project_id=' + encodeURIComponent(pid) : '/api/todos';
            const r = await fetch(url, { headers: { 'Accept': 'application/json' } });
            if (!r.ok) {
                console.warn('GET /api/todos failed:', r.status);
                state.todosData = [];
                renderTasks();
                return;
            }
            const data = await r.json();
            state.todosData = Array.isArray(data) ? data : [];
            renderTasks();
        } catch (e) {
            console.warn('fetchTodos failed', e);
            state.todosData = state.todosData || [];
            renderTasks();
        }
    }

    function startTodosPolling() {
        if (state.todosPollTimer) clearInterval(state.todosPollTimer);
        state.todosPollTimer = setInterval(() => fetchTodos(), TODOS_POLL_INTERVAL_MS);
    }

    function stopTodosPolling() {
        if (state.todosPollTimer) {
            clearInterval(state.todosPollTimer);
            state.todosPollTimer = null;
        }
    }

    /**
     * Открывает (или переоткрывает) WebSocket /ws/todos?project_id=<pid>.
     *
     * - Если уже OPEN/CONNECTING — no-op.
     * - При успешном connect — backoffStep сбрасывается, fallback polling
     *   останавливается. Snapshot придёт первым сообщением и заполнит
     *   state.todosData.
     * - На onclose/onerror — schedule reconnect через TODOS_WS_BACKOFFS_MS,
     *   и поднимается fallback polling.
     */
    function connectTodosWs() {
        if (state.todosWs && (
            state.todosWs.readyState === WebSocket.OPEN ||
            state.todosWs.readyState === WebSocket.CONNECTING
        )) {
            return;
        }
        state.todosWsClosedByUs = false;

        if (state.todosWsReconnectTimer) {
            clearTimeout(state.todosWsReconnectTimer);
            state.todosWsReconnectTimer = null;
        }

        const pid = currentTodosProjectId();
        const proto = (location.protocol === 'https:') ? 'wss' : 'ws';
        // Phase 5: при выборе remote origin'а — подписываемся на /ws/todos
        // remote-сервера через прокси-бэкенд.
        const server = (isRemoteMode()
            && state.activeOrigin
            && state.activeOrigin !== 'local'
            && state.activeOrigin !== 'all')
            ? state.activeOrigin
            : null;
        let qs = '';
        if (pid && !server) {
            qs = '?project_id=' + encodeURIComponent(pid);
        } else if (server) {
            qs = '?server=' + encodeURIComponent(server);
        }
        const url = `${proto}://${location.host}/ws/todos${qs}`;

        let ws;
        try {
            ws = new WebSocket(url);
        } catch (e) {
            console.warn('todos ws constructor failed', e);
            scheduleTodosWsReconnect();
            return;
        }
        state.todosWs = ws;

        ws.onopen = () => {
            state.todosWsBackoffStep = 0;
            stopTodosPolling();
        };
        ws.onmessage = (ev) => {
            handleTodosWsMessage(ev.data);
        };
        ws.onerror = (ev) => {
            console.debug('todos ws error', ev);
        };
        ws.onclose = () => {
            state.todosWs = null;
            if (state.todosWsClosedByUs) return;
            startTodosPolling();
            scheduleTodosWsReconnect();
        };
    }

    function disconnectTodosWs() {
        state.todosWsClosedByUs = true;
        if (state.todosWsReconnectTimer) {
            clearTimeout(state.todosWsReconnectTimer);
            state.todosWsReconnectTimer = null;
        }
        if (state.todosWs) {
            try { state.todosWs.close(); } catch (_) {}
            state.todosWs = null;
        }
    }

    function scheduleTodosWsReconnect() {
        if (state.todosWsClosedByUs) return;
        if (state.todosWsReconnectTimer) return;
        const idx = Math.min(state.todosWsBackoffStep, TODOS_WS_BACKOFFS_MS.length - 1);
        const delay = TODOS_WS_BACKOFFS_MS[idx];
        state.todosWsBackoffStep = Math.min(
            state.todosWsBackoffStep + 1,
            TODOS_WS_BACKOFFS_MS.length - 1,
        );
        state.todosWsReconnectTimer = setTimeout(() => {
            state.todosWsReconnectTimer = null;
            connectTodosWs();
        }, delay);
    }

    /**
     * Обработчик одного WS-сообщения /ws/todos. Кадры (см. ws_todos.rs):
     *   {kind:"snapshot", todos:[...]}
     *   {kind:"upsert", todo:{...}}
     *   {kind:"removed", id:"..."}
     *   {kind:"reload"}
     */
    function handleTodosWsMessage(raw) {
        let msg;
        try {
            msg = JSON.parse(raw);
        } catch (e) {
            console.warn('todos ws: non-JSON message', raw);
            return;
        }
        if (!msg || typeof msg !== 'object') return;

        switch (msg.kind) {
            case 'snapshot':
                state.todosData = Array.isArray(msg.todos) ? msg.todos : [];
                renderTasks();
                break;

            case 'upsert': {
                const todo = msg.todo;
                if (!todo || typeof todo !== 'object' || !todo.id) {
                    console.warn('todos upsert without todo.id', msg);
                    return;
                }
                if (!Array.isArray(state.todosData)) state.todosData = [];
                const i = state.todosData.findIndex((t) => t && t.id === todo.id);
                if (i >= 0) {
                    state.todosData[i] = todo;
                } else {
                    state.todosData.unshift(todo);
                }
                renderTasks();
                break;
            }

            case 'removed': {
                const id = msg.id;
                if (!id || !Array.isArray(state.todosData)) return;
                const i = state.todosData.findIndex((t) => t && t.id === id);
                if (i >= 0) {
                    state.todosData.splice(i, 1);
                    renderTasks();
                }
                break;
            }

            case 'reload':
                fetchTodos();
                break;

            default:
                console.debug('todos ws: unknown kind', msg.kind);
        }
    }

    /**
     * Рендерит kanban-board из state.tasksData.
     *
     * Группирует issues по status (учитываются только TASK_COLUMNS — прочие
     * статусы игнорируются, но их список могут расширить в будущем без
     * изменения сервера). Внутри колонки сортирует по priority asc (P0 →
     * P4), затем по updated_at desc.
     */
    function renderTasks() {
        if (!$tasksBoard) return;
        const data = state.tasksData || { issues: [], total: 0 };
        const issues = Array.isArray(data.issues) ? data.issues : [];

        // Группировка bd-issues по status.
        const byStatus = {};
        for (const col of TASK_COLUMNS) byStatus[col] = [];
        for (const issue of issues) {
            const s = String(issue.status || '').toLowerCase();
            if (s === 'todo') continue; // 'todo' — это отдельный store, не bd-status.
            if (Object.prototype.hasOwnProperty.call(byStatus, s)) {
                byStatus[s].push(issue);
            }
        }

        // Phase 4: TODO-колонка наполняется из state.todosData
        // (а не из bd-issues). Сортировка — по priority asc, затем
        // updated_at desc (тот же compareIssues, что и для bd-issues).
        const todos = Array.isArray(state.todosData) ? state.todosData.slice() : [];
        todos.sort(compareIssues);
        byStatus.todo = todos;

        // Сортировка и обрезка closed для bd-колонок.
        for (const col of TASK_COLUMNS) {
            if (col === 'todo') continue; // уже отсортирован.
            byStatus[col].sort(compareIssues);
        }
        if (byStatus.closed.length > CLOSED_LIMIT) {
            byStatus.closed = byStatus.closed.slice(0, CLOSED_LIMIT);
        }

        // Render board.
        $tasksBoard.innerHTML = '';
        for (const col of TASK_COLUMNS) {
            $tasksBoard.appendChild(renderColumn(col, byStatus[col]));
        }

        // Meta.
        if ($tasksMeta) {
            const total = (typeof data.total === 'number') ? data.total : issues.length;
            $tasksMeta.textContent = `Total: ${total} · TODO: ${todos.length}`;
        }
    }

    /**
     * Сравнение двух issues для сортировки внутри колонки:
     *   1) priority asc (P0 → P4; отсутствующий = 5),
     *   2) updated_at desc (новее — выше; отсутствующий = '').
     */
    function compareIssues(a, b) {
        const pa = (typeof a.priority === 'number') ? a.priority : 5;
        const pb = (typeof b.priority === 'number') ? b.priority : 5;
        if (pa !== pb) return pa - pb;
        const ua = a.updated_at || '';
        const ub = b.updated_at || '';
        if (ua === ub) return 0;
        return ua < ub ? 1 : -1;
    }

    function renderColumn(status, items) {
        const col = document.createElement('div');
        col.className = 'kanban-col';
        col.dataset.status = status;

        const header = document.createElement('div');
        header.className = 'kanban-col-header';
        header.dataset.status = status;
        const title = document.createElement('span');
        title.textContent = COLUMN_TITLES[status] || status;

        // Правая сторона: count + плюс-кнопка quick-create.
        const right = document.createElement('span');
        right.className = 'col-meta';
        const count = document.createElement('span');
        count.className = 'col-count';
        count.textContent = String(items.length);
        right.appendChild(count);

        // Quick-create: открывает modal с preset status (например draft из draft-колонки).
        // Из closed колонки кнопку не показываем — в `closed` нельзя создать новую напрямую.
        if (status !== 'closed') {
            const addBtn = document.createElement('button');
            addBtn.type = 'button';
            addBtn.className = 'col-add';
            addBtn.textContent = '+';
            addBtn.title = `Создать задачу со статусом ${COLUMN_TITLES[status] || status}`;
            addBtn.addEventListener('click', (ev) => {
                ev.stopPropagation();
                openCreateModal({ status });
            });
            right.appendChild(addBtn);
        }
        header.appendChild(title);
        header.appendChild(right);

        const body = document.createElement('div');
        body.className = 'kanban-col-body';
        body.dataset.status = status;

        // Drag-and-drop drop zone (Phase 6.E + Phase 4 TODO routing):
        //
        // Payload в text/plain:
        //   - 'todo:<id>' — TODO-карточка (state.todosData).
        //   - '<id>' (без префикса) — bd-issue (state.tasksData.issues).
        //
        // Правила (Phase 4):
        //   - TODO → open: разрешено, действие = promoteTodo(id, currentSession?).
        //   - TODO → любой другой статус: запрещено (drop игнорируется).
        //   - bd-issue → todo: запрещено (todo не bd-status).
        //   - bd-issue → bd-status: старая логика updateTask({status}).
        //
        // dragover/dragenter обновляют dataTransfer.dropEffect: 'move' если
        // drop разрешён, 'none' иначе. Подсветка .drop-target ставится только
        // на легитимные цели (UX: пользователь видит куда можно дропнуть).
        const isLegitTarget = (raw) => {
            if (!raw) return false;
            const isTodo = raw.startsWith('todo:');
            if (isTodo) {
                // TODO принимается только колонкой 'open'.
                return body.dataset.status === 'open';
            }
            // bd-issue: запрещён в TODO-колонку.
            return body.dataset.status !== 'todo';
        };

        body.addEventListener('dragover', (ev) => {
            // dragover не даёт нам payload (security в Chrome) — поэтому
            // полагаемся на dataTransfer.types и общий dropEffect. Для
            // простоты подсвечиваем все, кроме TODO-колонки (строгая фильтрация
            // в drop). TODO-колонка отказывается принимать что-либо.
            if (body.dataset.status === 'todo') {
                if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'none';
                return; // не preventDefault — drop не сработает.
            }
            ev.preventDefault();
            if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'move';
            body.classList.add('drop-target');
        });
        body.addEventListener('dragenter', (ev) => {
            if (body.dataset.status === 'todo') return;
            ev.preventDefault();
            body.classList.add('drop-target');
        });
        body.addEventListener('dragleave', (ev) => {
            const rel = ev.relatedTarget;
            if (rel && body.contains(rel)) return;
            body.classList.remove('drop-target');
        });
        body.addEventListener('drop', (ev) => {
            ev.preventDefault();
            body.classList.remove('drop-target');
            const raw = ev.dataTransfer ? ev.dataTransfer.getData('text/plain') : '';
            if (!raw) return;

            const targetStatus = body.dataset.status || status;
            const isTodoPayload = raw.startsWith('todo:');

            if (isTodoPayload) {
                const todoId = raw.slice('todo:'.length);
                if (!todoId) return;
                if (targetStatus !== 'open') {
                    // Запрещённый дроп — ничего не делаем. Карточка
                    // визуально вернётся на место (HTML5 DnD default).
                    return;
                }
                promoteTodo(todoId);
                return;
            }

            // bd-issue payload.
            if (targetStatus === 'todo') {
                // Перенос bd-issue в TODO-колонку запрещён.
                return;
            }
            const id = raw;
            const issue = state.tasksData && Array.isArray(state.tasksData.issues)
                ? state.tasksData.issues.find((it) => it && it.id === id)
                : null;
            if (!issue) return;
            const currentStatus = String(issue.status || '').toLowerCase();
            if (currentStatus === targetStatus) return;
            updateTask(id, { status: targetStatus });
        });
        // isLegitTarget — пока не используется в обработчиках выше (мы делаем
        // упрощённую фильтрацию по body.dataset.status), но оставлен для
        // справки: точная семантика — `isLegitTarget(payload)` решает,
        // нужно ли подсвечивать колонку как drop-target.
        void isLegitTarget;

        // Render cards: TODO-колонка использует renderTodoCard (state.todosData),
        // остальные — renderCard (bd-issues).
        if (status === 'todo') {
            for (const todo of items) {
                body.appendChild(renderTodoCard(todo));
            }
        } else {
            for (const issue of items) {
                body.appendChild(renderCard(issue));
            }
        }

        col.appendChild(header);
        col.appendChild(body);
        return col;
    }

    /**
     * Phase 4 — карточка TODO.
     *
     * Отличия от renderCard (bd-issues):
     *  - data-status="todo" (для CSS — фиолетовый border).
     *  - dragstart payload = 'todo:'+id (не голый id) — чтобы drop-зона
     *    могла различать TODO и bd-issue payload.
     *  - click → openTodoEditModal (отдельная модалка без status).
     *  - кнопка ▲ promote сразу на карточке (обходит drag).
     *  - нет кнопки close/reopen, нет статус-pill (статус всегда 'todo').
     */
    function renderTodoCard(todo) {
        const card = document.createElement('div');
        card.className = 'kanban-card';
        card.dataset.id = todo.id || '';
        card.dataset.status = 'todo';
        const prio = (typeof todo.priority === 'number') ? todo.priority : 5;
        card.dataset.priority = String(prio);

        let dragMoved = false;
        card.draggable = true;
        card.addEventListener('dragstart', (ev) => {
            dragMoved = true;
            if (ev.dataTransfer) {
                try {
                    ev.dataTransfer.setData('text/plain', 'todo:' + (todo.id || ''));
                } catch (_) {}
                ev.dataTransfer.effectAllowed = 'move';
            }
            card.classList.add('dragging');
        });
        card.addEventListener('dragend', () => {
            card.classList.remove('dragging');
            document.querySelectorAll('.kanban-col-body.drop-target')
                .forEach((el) => el.classList.remove('drop-target'));
            setTimeout(() => { dragMoved = false; }, 0);
        });

        card.addEventListener('click', (ev) => {
            if (dragMoved) return;
            // Не открываем edit-модалку, если кликнули по promote-кнопке.
            const t = ev.target;
            if (t && t.classList && t.classList.contains('promote-btn')) return;
            openTodoEditModal(todo);
        });

        // Title.
        const titleEl = document.createElement('div');
        titleEl.className = 'title';
        titleEl.textContent = todo.title || '(untitled)';
        card.appendChild(titleEl);

        // Description (truncate). Опускаем если пусто.
        const descRaw = String(todo.description || '').trim();
        if (descRaw) {
            const descEl = document.createElement('div');
            descEl.className = 'desc';
            descEl.textContent = descRaw.length > 140 ? descRaw.slice(0, 140) + '…' : descRaw;
            card.appendChild(descEl);
        }

        // Meta: P-pill + type + promote-кнопка.
        const meta = document.createElement('div');
        meta.className = 'meta-row';

        const pill = document.createElement('span');
        pill.className = 'p-pill';
        pill.textContent = (prio <= 4) ? `P${prio}` : 'P?';
        meta.appendChild(pill);

        if (todo.issue_type) {
            const t = document.createElement('span');
            t.className = 'type-tag';
            t.textContent = todo.issue_type;
            meta.appendChild(t);
        }

        if (todo.plan_mode) {
            const pm = document.createElement('span');
            pm.className = 'plan-mode-badge';
            pm.title = 'Plan mode: при promote добавится «Создай план для этой задачи»';
            pm.textContent = '◆ plan';
            meta.appendChild(pm);
        }

        const promoteBtn = document.createElement('button');
        promoteBtn.type = 'button';
        promoteBtn.className = 'promote-btn';
        promoteBtn.textContent = '▲ promote';
        promoteBtn.title = 'Превратить в bd-задачу + уведомление в tmux-сессию';
        promoteBtn.addEventListener('click', (ev) => {
            ev.stopPropagation();
            promoteTodo(todo.id);
        });
        meta.appendChild(promoteBtn);

        card.appendChild(meta);

        // Labels (max 3 + +N).
        const labels = Array.isArray(todo.labels) ? todo.labels : [];
        if (labels.length > 0) {
            const lblBox = document.createElement('div');
            lblBox.className = 'labels';
            const visible = labels.slice(0, 3);
            for (const l of visible) {
                const tag = document.createElement('span');
                tag.className = 'label-tag';
                tag.textContent = l;
                lblBox.appendChild(tag);
            }
            if (labels.length > 3) {
                const more = document.createElement('span');
                more.className = 'label-tag';
                more.textContent = '+' + (labels.length - 3);
                lblBox.appendChild(more);
            }
            card.appendChild(lblBox);
        }

        return card;
    }

    /**
     * Phase 4 — POST /api/todos/:id/promote.
     *
     * Алгоритм:
     *   1. Резолвим целевую сессию: state.currentSession || первая сессия
     *      активного проекта || показать alert и выйти.
     *   2. Optimistic remove: убираем TODO из state.todosData до ответа
     *      сервера, чтобы карточка исчезла мгновенно. На ошибке возвращаем.
     *   3. POST /api/todos/:id/promote с body {session}.
     *   4. На успехе: WS пришлёт removed (idempotent — карточки уже нет)
     *      и /ws/tasks упадёт upsert новой bd-задачи в колонке Open.
     */
    async function promoteTodo(id, sessionOverride) {
        if (!id) return;

        // Найти TODO в локальном store (нужен снимок для возможного rollback'а
        // и для определения проекта при выборе сессии).
        const idx = Array.isArray(state.todosData)
            ? state.todosData.findIndex((t) => t && t.id === id)
            : -1;
        const prev = (idx >= 0) ? state.todosData[idx] : null;

        // Резолвим сессию. Приоритет: явный override → текущая активная →
        // первая сессия проекта (по name asc) → ошибка.
        let session = sessionOverride && String(sessionOverride).trim()
            ? String(sessionOverride).trim()
            : (state.currentSession || null);
        if (!session) {
            const projectId = prev && prev.project_id
                ? prev.project_id
                : (state.activeProjectId || null);
            const projectSessions = (state.sessions || [])
                .filter((s) => projectId ? s.project_id === projectId : true)
                .map((s) => s.name)
                .sort((a, b) => String(a).localeCompare(String(b)));
            if (projectSessions.length > 0) {
                session = projectSessions[0];
            }
        }
        if (!session) {
            window.alert('Нет активной сессии для уведомления. Открой/создай tmux-сессию для проекта.');
            return;
        }

        // Optimistic remove.
        if (idx >= 0) {
            state.todosData.splice(idx, 1);
            renderTasks();
        }

        try {
            // Phase 5: для remote-todo (origin !== 'local') прокидываем ?server=.
            const origin = dtoOrigin(prev) || 'local';
            const r = await apiFetch('/api/todos/' + encodeURIComponent(id) + '/promote', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ session }),
            }, origin);
            if (!r.ok) {
                const text = await r.text();
                window.alert('Promote не удался: ' + (text || r.status));
                // Rollback.
                if (prev) {
                    state.todosData.splice(idx >= 0 ? idx : 0, 0, prev);
                    renderTasks();
                }
                return null;
            }
            return await r.json();
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
            if (prev) {
                state.todosData.splice(idx >= 0 ? idx : 0, 0, prev);
                renderTasks();
            }
            return null;
        }
    }

    function renderCard(issue) {
        const card = document.createElement('div');
        card.className = 'kanban-card';
        card.dataset.id = issue.id || '';
        card.dataset.status = String(issue.status || '').toLowerCase();
        const prio = (typeof issue.priority === 'number') ? issue.priority : 5;
        card.dataset.priority = String(prio);

        // Phase 6.E: HTML5 DnD source.
        //  - draggable=true делает элемент перетаскиваемым.
        //  - dragstart кладёт issue.id в text/plain payload и помечает .dragging.
        //  - dragend снимает .dragging и чистит висящие .drop-target подсветки
        //    (на случай если drop произошёл вне колонки).
        //  - dragMoved-флаг позволяет подавить click сразу после drag, чтобы
        //    отпускание мыши не открывало edit-modal.
        let dragMoved = false;
        card.draggable = true;
        card.addEventListener('dragstart', (ev) => {
            dragMoved = true;
            if (ev.dataTransfer) {
                try {
                    ev.dataTransfer.setData('text/plain', issue.id || '');
                } catch (_) { /* старые браузеры */ }
                ev.dataTransfer.effectAllowed = 'move';
            }
            card.classList.add('dragging');
        });
        card.addEventListener('dragend', () => {
            card.classList.remove('dragging');
            // Drop мог не сработать (вне drop-зоны) — снять висящие подсветки.
            document.querySelectorAll('.kanban-col-body.drop-target')
                .forEach((el) => el.classList.remove('drop-target'));
            // Сброс флага через микротик: к моменту click он уже учтён.
            setTimeout(() => { dragMoved = false; }, 0);
        });

        // Phase 6.C: клик по карточке открывает modal-edit. stash issue в dataset
        // для случаев optimistic-update (не теряем данные между renderTasks).
        // Phase 6.E: подавляем click если он порождён drag-end (dragMoved=true).
        card.addEventListener('click', () => {
            if (dragMoved) return;
            openEditModal(issue);
        });

        // Top row: id (mono dim).
        const idEl = document.createElement('div');
        idEl.className = 'id';
        idEl.textContent = issue.id || '';
        card.appendChild(idEl);

        // Title.
        const titleEl = document.createElement('div');
        titleEl.className = 'title';
        titleEl.textContent = issue.title || '(untitled)';
        card.appendChild(titleEl);

        // Meta row: P-pill + type.
        const meta = document.createElement('div');
        meta.className = 'meta-row';

        const pill = document.createElement('span');
        pill.className = 'p-pill';
        pill.textContent = (prio <= 4) ? `P${prio}` : 'P?';
        meta.appendChild(pill);

        if (issue.issue_type) {
            const t = document.createElement('span');
            t.className = 'type-tag';
            t.textContent = issue.issue_type;
            meta.appendChild(t);
        }
        card.appendChild(meta);

        // Labels (max 3 + +N).
        const labels = Array.isArray(issue.labels) ? issue.labels : [];
        if (labels.length > 0) {
            const lblBox = document.createElement('div');
            lblBox.className = 'labels';
            const visible = labels.slice(0, 3);
            for (const l of visible) {
                const tag = document.createElement('span');
                tag.className = 'label-tag';
                tag.textContent = l;
                lblBox.appendChild(tag);
            }
            if (labels.length > 3) {
                const more = document.createElement('span');
                more.className = 'label-tag';
                more.textContent = '+' + (labels.length - 3);
                lblBox.appendChild(more);
            }
            card.appendChild(lblBox);
        }

        return card;
    }

    // =========================================================================
    // Phase 6.B — Multi-project: fetchProjects / switchActive / new / init / remove
    // =========================================================================

    /**
     * Загружает список проектов c /api/projects и перерисовывает <select>.
     * Активный проект — тот у кого `active: true` в DTO.
     */
    async function fetchProjects() {
        try {
            const r = await fetch('/api/projects', { headers: { 'Accept': 'application/json' } });
            if (!r.ok) {
                console.warn('GET /api/projects failed:', r.status);
                return;
            }
            const data = await r.json();
            state.projects = Array.isArray(data) ? data : [];
            const active = state.projects.find((p) => p.active);
            state.activeProjectId = active ? active.id : (state.projects[0] ? state.projects[0].id : null);
            // Cross-project sessions visibility: восстанавливаем фильтр сайдбара
            // из localStorage. Допустимые значения — '__all__' либо id одного из
            // существующих проектов; иначе fallback на '__all__'.
            try {
                const saved = localStorage.getItem('forge.projectFilter');
                if (saved === '__all__') {
                    state.projectFilter = '__all__';
                } else if (saved && state.projects.some((p) => p.id === saved)) {
                    state.projectFilter = saved;
                } else {
                    state.projectFilter = '__all__';
                }
            } catch (_) {
                // localStorage недоступен (privacy mode) — оставляем default.
                state.projectFilter = '__all__';
            }
            renderProjectSelect();
            // Если git-таб активен и до этого не было активного проекта
            // (placeholder висел), теперь когда projects загрузились —
            // переоткрываем lazygit-сессию.
            if (state.activeTab === 'git' && !state.gitTerm.ws) {
                openLazygitForActiveProject();
            }
        } catch (e) {
            console.warn('fetchProjects failed', e);
        }
    }

    function renderProjectSelect() {
        if (!$projectSelect) return;
        $projectSelect.innerHTML = '';
        // Cross-project sessions visibility: первая опция — '__all__' (фильтр).
        // Selected по state.projectFilter (UI-only), а не по activeProjectId.
        const allOpt = document.createElement('option');
        allOpt.value = '__all__';
        allOpt.textContent = 'All projects';
        if (state.projectFilter === '__all__') {
            allOpt.selected = true;
        }
        $projectSelect.appendChild(allOpt);
        for (const p of state.projects) {
            const opt = document.createElement('option');
            opt.value = p.id;
            opt.textContent = p.name + (p.tmux_prefix ? ` [${p.tmux_prefix}]` : '');
            if (p.id === state.projectFilter) {
                opt.selected = true;
            }
            $projectSelect.appendChild(opt);
        }
    }

    /**
     * Переключает активный проект:
     *   1) POST /api/projects/active
     *   2) Закрываем текущий tmux WS-attach (новый проект — другой набор сессий).
     *   3) Сбрасываем tasksData и перезагружаем sessions+tasks.
     */
    async function switchActiveProject(id) {
        if (!id || id === state.activeProjectId) return;
        try {
            const r = await fetch('/api/projects/active', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ id }),
            });
            if (!r.ok) {
                const text = await r.text();
                window.alert('Не удалось переключить проект: ' + (text || r.status));
                // Откатим select на прежнее значение.
                renderProjectSelect();
                return;
            }
            // 2) Закрыть tmux WS — текущая сессия принадлежала старому проекту.
            disconnectWs();
            state.currentSession = null;
            showPlaceholder(true);
            setStatus('disconnected', 'disconnected');
            // 3) Сбросить tasks snapshot, чтобы Tasks-таб перерисовался.
            state.tasksData = null;
            // 4) Reload список проектов (active flag сменится) + sessions + tasks.
            await fetchProjects();
            // Для transient id (`__path__:...`) ни один registered project не
            // получит active=true → fetchProjects fallback'нёт activeProjectId
            // на projects[0]. Перебиваем явно.
            state.activeProjectId = id;
            await fetchSessions();
            // Phase 6.D: переподключаем tasks WS — на сервере watcher уже
            // переключился на новый .beads/, и нам нужен свежий snapshot
            // именно для нового проекта (broadcast — process-wide, нельзя
            // было «пропустить» события другого проекта).
            disconnectTasksWs();
            // Маленький async-tick, чтобы close-event обработался до connect.
            setTimeout(connectTasksWs, 0);
            if (state.activeTab === 'tasks') {
                fetchTasks();
            }
            // Phase 4: TODO-стрим привязан к activeProjectId — переподключаем.
            state.todosData = [];
            disconnectTodosWs();
            setTimeout(connectTodosWs, 0);
            fetchTodos();
            // Phase 4 (lazygit-tab): если git-таб открыт — переключим cwd
            // через {type:"switch_cwd"} (или fallback reconnect).
            if (state.activeTab === 'git') {
                const newActive = getActiveProject();
                if (newActive && newActive.path) {
                    if ($gitPlaceholder) $gitPlaceholder.hidden = true;
                    if ($gitTermEl) $gitTermEl.hidden = false;
                    gitSwitchCwd(newActive.path);
                } else {
                    if ($gitPlaceholder) $gitPlaceholder.hidden = false;
                    if ($gitTermEl) $gitTermEl.hidden = true;
                    closeGitWs('no active project after switch');
                }
            }
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
            renderProjectSelect();
        }
    }

    /**
     * Modal для создания нового проекта.
     *
     * Поля: name, path, [x] Initialize project.
     * - Если Initialize — POST /api/projects/init (mkdir + git init + br init).
     * - Иначе POST /api/projects (только регистрация уже существующей папки).
     */
    function openNewProjectModal() {
        const overlay = buildModalOverlay();
        const card = document.createElement('div');
        card.className = 'modal-card';

        card.innerHTML = `
            <h2>New project</h2>
            <label>Name<br><input type="text" id="np-name" placeholder="my-project"></label>
            <label>Path<br><input type="text" id="np-path" placeholder="/Users/me/work/my-project"></label>
            <label class="modal-check"><input type="checkbox" id="np-init" checked> Initialize (mkdir + git init + br init + scaffold)</label>
            <div class="modal-actions">
                <button type="button" id="np-cancel">Cancel</button>
                <button type="button" id="np-create" class="primary">Create</button>
            </div>
        `;
        overlay.appendChild(card);
        document.body.appendChild(overlay);

        const $name = card.querySelector('#np-name');
        const $path = card.querySelector('#np-path');
        const $init = card.querySelector('#np-init');
        const $cancel = card.querySelector('#np-cancel');
        const $create = card.querySelector('#np-create');

        $name.focus();

        const close = () => overlay.remove();
        $cancel.addEventListener('click', close);
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
        });
        $create.addEventListener('click', async () => {
            const name = ($name.value || '').trim();
            const path = ($path.value || '').trim();
            if (!name || !path) {
                window.alert('Заполни name и path');
                return;
            }
            const url = $init.checked ? '/api/projects/init' : '/api/projects';
            try {
                const r = await fetch(url, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ name, path }),
                });
                if (!r.ok) {
                    const text = await r.text();
                    window.alert('Создание не удалось: ' + (text || r.status));
                    return;
                }
                close();
                await fetchProjects();
            } catch (e) {
                window.alert('Ошибка запроса: ' + e.message);
            }
        });
    }

    /**
     * Modal с управлением проектами: список + кнопка remove у каждого.
     * Активный — disabled (у него remove недоступен).
     *
     * Phase 5: каждая строка проекта раскрывается в секцию «Notifications»
     * с per-project настройками доставки уведомлений из notifier-loop:
     *   - notify_template     — шаблон сообщения (плейсхолдеры
     *                            {id} {title} {description} {priority} {type}).
     *   - notify_delay_minutes — задержка перед отправкой (мин, ≥0; 0 = немедленно).
     *   - notify_wait_previous — ждать закрытия предыдущей задачи перед отправкой
     *                            (override delay).
     *   - notify_session       — override tmux-сессии (пусто = текущая сессия проекта).
     * Save → PATCH /api/projects/:id/settings → optimistic-обновление state.projects.
     */
    function openSettingsModal(initialTab) {
        const overlay = buildModalOverlay();
        const card = document.createElement('div');
        card.className = 'modal-card settings-modal';
        // Структура модала: tab-bar (Notifications | Themes [| Remote servers]) → активная панель.
        // По умолчанию активна Notifications (исторически — это исходный контент
        // settings-модала с проектами + per-project формой нотификаций).
        // Themes — добавлена в Phase 4: пресеты + custom (см. renderThemesPanel).
        // Phase 5: вкладка Remote servers видна только при remote_mode=true.
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

        // Кэш состояния Themes-вкладки (чтобы при переключении табов не дёргать
        // GET /api/themes повторно и не терять промежуточный rerender).
        const themesState = {
            loaded: false,
            data: null, // { presets, custom, active }
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
                renderRemotesTable();
            }
        };
        $tabBtns.forEach((btn) => {
            btn.addEventListener('click', () => showTab(btn.dataset.tab));
        });

        // Phase 5 — Remote servers tab: таблица + форма Add.
        const $remotesTbody = card.querySelector('#ps-remotes-table tbody');
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

        // Pre-flight через /healthz remote-сервера. Используем dry-run путь:
        // POSTить ничего не будем; вместо этого временно отправим запрос
        // прямо на remote URL через no-cors? — нет, это не сработает с CORS.
        // Правильный путь: POST сразу на /api/remote-servers (бэкенд сохранит
        // запись и можно будет вызвать /api/remote-servers/:id/healthz).
        // Здесь идём проще: при Test просто включаем кнопку Save (полагаемся
        // на бэкенд: при сохранении неверного token health-poll сразу пометит
        // offline). Однако делаем минимальную проверку URL — что он http(s).
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
                // Pre-flight через временный POST + delete? Чтобы не плодить
                // мусор, используем такой паттерн: POST создаёт запись (валидация
                // на бэке: label/url/token непустые, url начинается с http(s)),
                // и сразу делаем GET /api/remote-servers/:id/healthz. Если
                // online=false — оставляем запись (пользователь может Save вручную
                // или Delete).
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
                    // Ping healthz.
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
                        // Очистка формы; запись уже сохранена → перерисуем
                        // таблицу и обновим state.remoteServers.
                        $rsLabel.value = '';
                        $rsUrl.value = '';
                        $rsToken.value = '';
                        refreshSaveBtn();
                        await fetchRemoteServers();
                        renderRemotesTable();
                        renderSidebar();
                    } else {
                        $rsTestStatus.textContent = 'Offline' + detail + '. Запись сохранена — можно проверить позже.';
                        $rsTestStatus.className = 'rs-test-status warn';
                        await fetchRemoteServers();
                        renderRemotesTable();
                    }
                } catch (e) {
                    $rsTestStatus.textContent = 'Network error: ' + e.message;
                    $rsTestStatus.className = 'rs-test-status error';
                }
            });
        }

        if ($rsSave) {
            // Save без preflight — POST и обновить state.remoteServers.
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
                    renderRemotesTable();
                    renderSidebar();
                } catch (e) {
                    $rsTestStatus.textContent = 'Network error: ' + e.message;
                    $rsTestStatus.className = 'rs-test-status error';
                }
            });
        }

        function renderRemotesTable() {
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
                editBtn.addEventListener('click', () => openEditRemoteRow(tr, srv));
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
                        // Reset активный origin если удалили его.
                        if (state.activeOrigin === srv.id) {
                            state.activeOrigin = 'all';
                            saveActiveOriginToStorage();
                        }
                        await fetchRemoteServers();
                        state.remoteProjects.delete(srv.id);
                        state.remoteSessions.delete(srv.id);
                        renderRemotesTable();
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

        function openEditRemoteRow(tr, srv) {
            // Заменяем строку формой inline. Token — опциональный; если оставить
            // пустым, бэкенд оставит старый token (см. RemoteServerStore::update).
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
            formTr.querySelector('.rs-edit-cancel').addEventListener('click', renderRemotesTable);
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
                    renderRemotesTable();
                    renderSidebar();
                } catch (e) {
                    window.alert('Network error: ' + e.message);
                }
            });
        }

        // Локальный набор раскрытых секций Notifications (по project.id), чтобы
        // re-render списка не схлопывал открытые формы.
        const expanded = new Set();

        const renderList = () => {
            $list.innerHTML = '';
            for (const p of state.projects) {
                const li = document.createElement('li');
                li.className = 'modal-project-item' + (p.active ? ' active' : '');

                // Верхняя строка: meta + кнопки (settings, remove).
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

                // Раскрывающийся блок Notifications.
                if (isOpen) {
                    li.appendChild(buildNotificationsForm(p, () => {
                        // После save — re-render списка; expanded остаётся открытым,
                        // так что форма перерисуется с обновлёнными значениями.
                        renderList();
                    }));
                }

                $list.appendChild(li);
            }
        };
        renderList();

        // Phase 5 — initialTab переключает на нужный таб сразу при открытии
        // (например, кликом по '+' в origin-табах). Допустимые значения:
        // 'notifications' (default), 'themes', 'remotes' (только в remote-mode).
        if (initialTab && (initialTab === 'themes' || (initialTab === 'remotes' && isRemoteMode()))) {
            showTab(initialTab);
        }

        const close = () => overlay.remove();
        card.querySelector('#ps-close').addEventListener('click', close);
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
        });
    }

    /**
     * Phase 5 — Безопасное экранирование строки для вставки в innerHTML
     * (используется в openEditRemoteRow для подстановки label в input value).
     */
    function escapeHtml(s) {
        return String(s == null ? '' : s)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    // =========================================================================
    // Phase 4 (themes) — Themes-вкладка в settings-модале.
    //
    // loadThemesIntoPanel(panel, themesState):
    //   GET /api/themes → { presets: Theme[], custom: Theme[], active: string }
    //   и рендерит две секции в `panel`:
    //     - «Presets» — сетка карточек встроенных тем (см. theme-card-grid).
    //     - «Custom themes» — список пользовательских тем + кнопка «+ New custom».
    //   Активная карточка подсвечивается рамкой var(--accent).
    //
    // renderThemesPanel(panel, data, themesState):
    //   Идемпотентно перерисовывает содержимое панели по `data` (с учётом active id).
    //   Каждый клик по карточке → switchTheme(id) (Phase 3); при успехе локально
    //   обновляем themesState.data.active и перерисовываем без повторного GET.
    //
    // buildThemeCard(theme, isActive, onClick):
    //   Возвращает .theme-card с .theme-card-name + .theme-card-preview из
    //   8 ANSI-полосок (black/red/green/yellow/blue/magenta/cyan/white) +
    //   foreground/background. Превью рисуется в порядке: bg, fg, чёрный, красный,
    //   зелёный, жёлтый, синий, мажента, циан, белый.
    //
    // openThemeEditor(themeOrNull):
    //   Полноценный редактор палитры (модал с пикерами и live preview).
    //   Используется кнопками «Edit» в custom-карточках и «+ New custom».
    //   Реализация — ниже в этом файле (см. отдельную секцию).
    // =========================================================================

    /**
     * Загружает темы с бэкенда и рисует panel. При ошибке — показывает inline-
     * сообщение в panel + кнопку retry.
     * themesState — внешний кэш ({ loaded: bool, data }), переданный из
     * openSettingsModal — при повторном открытии Themes-вкладки не дёргаем GET.
     */
    async function loadThemesIntoPanel(panel, themesState) {
        if (!panel) return;
        panel.innerHTML = '<div class="themes-loading">Loading themes…</div>';
        try {
            const r = await fetch('/api/themes');
            if (!r.ok) {
                throw new Error('HTTP ' + r.status);
            }
            const data = await r.json();
            // Нормализуем shape: ожидаем { presets, custom, active }.
            const norm = {
                presets: Array.isArray(data && data.presets) ? data.presets : [],
                custom: Array.isArray(data && data.custom) ? data.custom : [],
                active: (data && typeof data.active === 'string') ? data.active : null,
            };
            themesState.data = norm;
            themesState.loaded = true;
            renderThemesPanel(panel, themesState);
        } catch (e) {
            panel.innerHTML = '';
            const err = document.createElement('div');
            err.className = 'themes-error';
            err.textContent = 'Failed to load themes: ' + (e && e.message ? e.message : String(e));
            panel.appendChild(err);
            const retry = document.createElement('button');
            retry.type = 'button';
            retry.className = 'themes-retry';
            retry.textContent = 'Retry';
            retry.addEventListener('click', () => loadThemesIntoPanel(panel, themesState));
            panel.appendChild(retry);
        }
    }

    /**
     * Перерисовывает содержимое Themes-панели из themesState.data.
     * Безопасно вызывать многократно (например, после switchTheme).
     */
    function renderThemesPanel(panel, themesState) {
        if (!panel || !themesState || !themesState.data) return;
        const data = themesState.data;
        panel.innerHTML = '';

        // ---- Секция «Presets» ----
        const presetsSection = document.createElement('section');
        presetsSection.className = 'themes-section';

        const presetsTitle = document.createElement('h3');
        presetsTitle.className = 'themes-section-title';
        presetsTitle.textContent = 'Presets';
        presetsSection.appendChild(presetsTitle);

        const presetsGrid = document.createElement('div');
        presetsGrid.className = 'theme-card-grid';
        for (const theme of data.presets) {
            const isActive = theme && theme.id === data.active;
            const card = buildThemeCard(theme, isActive, async () => {
                if (!theme || !theme.id) return;
                if (theme.id === data.active) return;
                await switchTheme(theme.id);
                // switchTheme обновляет state.activeTheme — берём id оттуда,
                // потому что бэкенд мог нормализовать (на случай invalid id).
                if (state.activeTheme && state.activeTheme.id) {
                    themesState.data.active = state.activeTheme.id;
                } else {
                    themesState.data.active = theme.id;
                }
                renderThemesPanel(panel, themesState);
            });
            presetsGrid.appendChild(card);
        }
        presetsSection.appendChild(presetsGrid);
        panel.appendChild(presetsSection);

        // ---- Секция «Custom themes» ----
        const customSection = document.createElement('section');
        customSection.className = 'themes-section themes-section-custom';

        const customHeader = document.createElement('div');
        customHeader.className = 'themes-section-header';
        const customTitle = document.createElement('h3');
        customTitle.className = 'themes-section-title';
        customTitle.textContent = 'Custom themes';
        customHeader.appendChild(customTitle);

        const newBtn = document.createElement('button');
        newBtn.type = 'button';
        newBtn.className = 'theme-new-btn';
        newBtn.textContent = '+ New custom';
        newBtn.addEventListener('click', () => {
            openThemeEditor(null);
        });
        customHeader.appendChild(newBtn);
        customSection.appendChild(customHeader);

        const customGrid = document.createElement('div');
        customGrid.className = 'theme-card-grid';
        if (!data.custom.length) {
            const empty = document.createElement('div');
            empty.className = 'themes-empty';
            empty.textContent = 'No custom themes yet.';
            customGrid.appendChild(empty);
        } else {
            for (const theme of data.custom) {
                const isActive = theme && theme.id === data.active;
                const card = buildThemeCard(theme, isActive, async () => {
                    if (!theme || !theme.id) return;
                    if (theme.id === data.active) return;
                    await switchTheme(theme.id);
                    if (state.activeTheme && state.activeTheme.id) {
                        themesState.data.active = state.activeTheme.id;
                    } else {
                        themesState.data.active = theme.id;
                    }
                    renderThemesPanel(panel, themesState);
                });

                // Иконки edit/delete в правом верхнем углу карточки.
                const tools = document.createElement('div');
                tools.className = 'theme-card-tools';

                const editBtn = document.createElement('button');
                editBtn.type = 'button';
                editBtn.className = 'theme-card-tool';
                editBtn.title = 'Edit';
                editBtn.textContent = 'edit';
                editBtn.addEventListener('click', (ev) => {
                    ev.stopPropagation();
                    openThemeEditor(theme);
                });
                tools.appendChild(editBtn);

                const delBtn = document.createElement('button');
                delBtn.type = 'button';
                delBtn.className = 'theme-card-tool theme-card-tool-danger';
                delBtn.title = 'Delete';
                delBtn.textContent = 'del';
                delBtn.addEventListener('click', async (ev) => {
                    ev.stopPropagation();
                    if (!theme || !theme.id) return;
                    if (!window.confirm(`Удалить тему "${theme.name || theme.id}"?`)) return;
                    try {
                        const r = await fetch('/api/themes/' + encodeURIComponent(theme.id), {
                            method: 'DELETE',
                        });
                        if (!r.ok && r.status !== 204) {
                            const text = await r.text().catch(() => '');
                            window.alert('Failed to delete: ' + (text || r.status));
                            return;
                        }
                        // Локально удалим тему и перерисуем; если удалили активную —
                        // бэкенд должен был переключить active на default; перечитаем.
                        themesState.loaded = false;
                        await loadThemesIntoPanel(panel, themesState);
                    } catch (err) {
                        window.alert('Failed to delete: ' + (err && err.message ? err.message : err));
                    }
                });
                tools.appendChild(delBtn);

                card.appendChild(tools);
                customGrid.appendChild(card);
            }
        }
        customSection.appendChild(customGrid);
        panel.appendChild(customSection);
    }

    /**
     * Строит карточку темы: name + .theme-card-preview из 10 цветных полосок
     * (background, foreground, 8 ANSI: black/red/green/yellow/blue/magenta/cyan/white).
     * Активная — добавляется класс .active (рамка var(--accent)).
     * Клик по карточке → onClick().
     */
    function buildThemeCard(theme, isActive, onClick) {
        const card = document.createElement('div');
        card.className = 'theme-card' + (isActive ? ' active' : '');
        if (theme && theme.id) {
            card.dataset.themeId = theme.id;
        }
        if (typeof onClick === 'function') {
            card.addEventListener('click', onClick);
        }

        const name = document.createElement('div');
        name.className = 'theme-card-name';
        name.textContent = (theme && theme.name) ? theme.name : (theme && theme.id ? theme.id : '—');
        card.appendChild(name);

        const preview = document.createElement('div');
        preview.className = 'theme-card-preview';
        const term = (theme && theme.term) ? theme.term : {};
        const swatches = [
            { key: 'background', color: term.background },
            { key: 'foreground', color: term.foreground },
            { key: 'black', color: term.black },
            { key: 'red', color: term.red },
            { key: 'green', color: term.green },
            { key: 'yellow', color: term.yellow },
            { key: 'blue', color: term.blue },
            { key: 'magenta', color: term.magenta },
            { key: 'cyan', color: term.cyan },
            { key: 'white', color: term.white },
        ];
        for (const sw of swatches) {
            const cell = document.createElement('span');
            cell.className = 'theme-card-swatch theme-card-swatch-' + sw.key;
            if (typeof sw.color === 'string' && sw.color) {
                cell.style.background = sw.color;
            }
            cell.title = sw.key + (sw.color ? ': ' + sw.color : '');
            preview.appendChild(cell);
        }
        card.appendChild(preview);

        if (isActive) {
            const badge = document.createElement('div');
            badge.className = 'theme-card-badge';
            badge.textContent = 'active';
            card.appendChild(badge);
        }

        return card;
    }

    // =========================================================================
    // Custom theme editor.
    //
    // openThemeEditor(themeOrNull):
    //   Открывает крупный модал поверх settings (или поверх любого UI). Имеет
    //   два режима:
    //     - create: themeOrNull === null. Заголовок «New custom theme», baseline
    //       черновика — клон активной темы (state.activeTheme) либо первого
    //       пресета. Имя пустое.
    //     - edit:   themeOrNull — объект Theme с id, который существует в
    //       custom. Заголовок «Edit theme: {name}», поля заполнены из объекта.
    //
    //   Состояние черновика держится в локальной переменной `draft = { name,
    //   ui:{11 полей}, term:{20 полей} }`. Любое изменение пикера — мутация
    //   draft + обновление live preview без сохранения на сервер.
    //
    //   «Duplicate from preset» dropdown — при выборе пресета все 31 цветовых
    //   значения перезаписываются из этого пресета; если name пустой,
    //   подставляется «Copy of {presetName}». В режиме edit поведение то же.
    //
    //   Save:
    //     - create или themeOrNull без существующего id → POST /api/themes/custom
    //     - edit с существующим id → PUT /api/themes/custom/:id
    //     При успехе: модал закрывается, themesState (Settings) сбрасывается и
    //     перезагружается через loadThemesIntoPanel — список обновляется. На
    //     активную тему НЕ переключаемся автоматически.
    //
    //   Cancel/X/click-outside: модал закрывается без сохранения.
    // =========================================================================

    /**
     * Список ключей UI-цветов (camelCase, как в Theme.ui на бэкенде) и их
     * человекочитаемые подписи. Порядок определяет визуальный порядок пикеров.
     */
    const THEME_UI_KEYS = [
        { key: 'bg',      label: 'Background' },
        { key: 'bgElev',  label: 'Background (elev)' },
        { key: 'fg',      label: 'Foreground' },
        { key: 'fgDim',   label: 'Foreground (dim)' },
        { key: 'border',  label: 'Border' },
        { key: 'accent',  label: 'Accent' },
        { key: 'warn',    label: 'Warning' },
        { key: 'danger',  label: 'Danger' },
        { key: 'p0',      label: 'Priority P0' },
        { key: 'p1',      label: 'Priority P1' },
        { key: 'p2',      label: 'Priority P2' },
    ];

    /**
     * Базовая часть Terminal-секции (foreground/background/cursor/selection).
     * Эти 4 ключа рендерятся отдельной строкой над ANSI-сеткой.
     */
    const THEME_TERM_BASE_KEYS = [
        { key: 'foreground', label: 'Foreground' },
        { key: 'background', label: 'Background' },
        { key: 'cursor',     label: 'Cursor' },
        { key: 'selection',  label: 'Selection' },
    ];

    /**
     * 16 ANSI цветов — базовые 8 + bright 8. Рендерятся в сетке 4×4
     * (см. CSS .theme-ansi-grid).
     */
    const THEME_TERM_ANSI_KEYS = [
        { key: 'black',         label: 'black' },
        { key: 'red',           label: 'red' },
        { key: 'green',         label: 'green' },
        { key: 'yellow',        label: 'yellow' },
        { key: 'blue',          label: 'blue' },
        { key: 'magenta',       label: 'magenta' },
        { key: 'cyan',          label: 'cyan' },
        { key: 'white',         label: 'white' },
        { key: 'brightBlack',   label: 'br.black' },
        { key: 'brightRed',     label: 'br.red' },
        { key: 'brightGreen',   label: 'br.green' },
        { key: 'brightYellow',  label: 'br.yellow' },
        { key: 'brightBlue',    label: 'br.blue' },
        { key: 'brightMagenta', label: 'br.magenta' },
        { key: 'brightCyan',    label: 'br.cyan' },
        { key: 'brightWhite',   label: 'br.white' },
    ];

    /** Все ключи term секции (для итераций по draft.term). */
    const THEME_TERM_KEYS = THEME_TERM_BASE_KEYS.concat(THEME_TERM_ANSI_KEYS);

    /** Регэксп для валидного hex-цвета вида #rrggbb (case-insensitive). */
    const HEX_COLOR_RE = /^#[0-9a-fA-F]{6}$/;

    /**
     * Возвращает строку, гарантированно валидный hex-цвет.
     * Если value не строка/невалид — возвращает fallback (#000000 если не задан).
     */
    function normalizeHex(value, fallback) {
        if (typeof value === 'string' && HEX_COLOR_RE.test(value)) {
            return value.toLowerCase();
        }
        return fallback || '#000000';
    }

    /**
     * Глубокий клон ui/term из произвольного theme. Гарантирует, что в
     * draft присутствуют ВСЕ ключи из THEME_UI_KEYS / THEME_TERM_KEYS даже
     * если бэкенд прислал тему с пропусками (нормализация).
     */
    function cloneThemeColors(theme) {
        const srcUi = (theme && theme.ui) ? theme.ui : {};
        const srcTerm = (theme && theme.term) ? theme.term : {};
        const ui = {};
        for (const { key } of THEME_UI_KEYS) {
            ui[key] = normalizeHex(srcUi[key], '#000000');
        }
        const term = {};
        for (const { key } of THEME_TERM_KEYS) {
            term[key] = normalizeHex(srcTerm[key], '#000000');
        }
        return { ui, term };
    }

    /**
     * Главный entry-point редактора кастомных тем.
     *
     * @param {object|null} themeOrNull — null для create-режима, объект Theme
     *   (с id) для edit-режима.
     */
    function openThemeEditor(themeOrNull) {
        const isEdit = !!(themeOrNull && themeOrNull.id);
        // Для baseline черновика в create-режиме берём активную тему, либо
        // первый пресет из последнего загруженного списка, либо нули.
        const baseline = isEdit
            ? themeOrNull
            : (state.activeTheme || null);
        const cloned = cloneThemeColors(baseline);
        const draft = {
            id: isEdit ? themeOrNull.id : '',
            name: isEdit ? (themeOrNull.name || '') : '',
            ui: cloned.ui,
            term: cloned.term,
        };

        // Список пресетов нужен для dropdown «Duplicate from preset». Берём
        // его из последнего GET /api/themes (если settings уже открывались и
        // подгружали /api/themes — этот fetch свежий). Если нет — пустой
        // dropdown с лейблом "From scratch".
        let presets = [];
        try {
            // themesState закрыт в openSettingsModal; здесь надёжнее всего
            // дёрнуть свежий GET. Но это блокирует UI на долю секунды и не
            // критично — открытие редактора всё равно асинхронное событие.
            // Для простоты — используем кэш state.activeTheme + дополнительно
            // подгрузим список ниже (асинхронно).
        } catch (_) { /* ignore */ }

        // Overlay + card.
        const overlay = buildModalOverlay();
        const card = document.createElement('div');
        card.className = 'modal-card theme-editor-modal';

        // ---- Header (с кнопкой close) ----
        const header = document.createElement('div');
        header.className = 'theme-editor-header';
        const title = document.createElement('h2');
        title.textContent = isEdit
            ? `Edit theme: ${themeOrNull.name || themeOrNull.id}`
            : 'New custom theme';
        header.appendChild(title);
        const closeBtn = document.createElement('button');
        closeBtn.type = 'button';
        closeBtn.className = 'theme-editor-close';
        closeBtn.setAttribute('aria-label', 'Close');
        closeBtn.textContent = '×';
        header.appendChild(closeBtn);
        card.appendChild(header);

        // ---- Body ----
        const body = document.createElement('div');
        body.className = 'theme-editor-body';

        // Name + Duplicate-from-preset row.
        const metaRow = document.createElement('div');
        metaRow.className = 'theme-editor-section theme-editor-meta';

        const nameWrap = document.createElement('label');
        nameWrap.className = 'theme-editor-row';
        const nameLbl = document.createElement('span');
        nameLbl.className = 'theme-editor-row-label';
        nameLbl.textContent = 'Name';
        nameWrap.appendChild(nameLbl);
        const nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'theme-editor-name';
        nameInput.placeholder = 'My theme';
        nameInput.value = draft.name;
        nameInput.addEventListener('input', () => {
            draft.name = nameInput.value;
        });
        nameWrap.appendChild(nameInput);
        metaRow.appendChild(nameWrap);

        const dupWrap = document.createElement('label');
        dupWrap.className = 'theme-editor-row';
        const dupLbl = document.createElement('span');
        dupLbl.className = 'theme-editor-row-label';
        dupLbl.textContent = 'Duplicate from preset';
        dupWrap.appendChild(dupLbl);
        const dupSelect = document.createElement('select');
        dupSelect.className = 'theme-editor-duplicate';
        const dupDefault = document.createElement('option');
        dupDefault.value = '';
        dupDefault.textContent = 'From scratch';
        dupSelect.appendChild(dupDefault);
        dupWrap.appendChild(dupSelect);
        metaRow.appendChild(dupWrap);
        body.appendChild(metaRow);

        // UI Section.
        const uiSection = document.createElement('section');
        uiSection.className = 'theme-editor-section';
        const uiTitle = document.createElement('h3');
        uiTitle.className = 'theme-editor-section-title';
        uiTitle.textContent = 'UI colors';
        uiSection.appendChild(uiTitle);
        const uiGrid = document.createElement('div');
        uiGrid.className = 'theme-editor-ui-grid';
        // Хранилище DOM-ссылок пикеров для programmatic-обновления при
        // duplicate-from-preset (без полного re-render).
        const uiRefs = {};
        for (const def of THEME_UI_KEYS) {
            const row = buildColorPickerRow(def, draft.ui[def.key], (newHex) => {
                draft.ui[def.key] = newHex;
                updatePreview();
            });
            uiRefs[def.key] = row;
            uiGrid.appendChild(row.el);
        }
        uiSection.appendChild(uiGrid);
        body.appendChild(uiSection);

        // Terminal Section.
        const termSection = document.createElement('section');
        termSection.className = 'theme-editor-section';
        const termTitle = document.createElement('h3');
        termTitle.className = 'theme-editor-section-title';
        termTitle.textContent = 'Terminal colors';
        termSection.appendChild(termTitle);
        const termBaseGrid = document.createElement('div');
        termBaseGrid.className = 'theme-editor-term-base-grid';
        const termRefs = {};
        for (const def of THEME_TERM_BASE_KEYS) {
            const row = buildColorPickerRow(def, draft.term[def.key], (newHex) => {
                draft.term[def.key] = newHex;
                updatePreview();
            });
            termRefs[def.key] = row;
            termBaseGrid.appendChild(row.el);
        }
        termSection.appendChild(termBaseGrid);
        const ansiTitle = document.createElement('div');
        ansiTitle.className = 'theme-editor-ansi-title';
        ansiTitle.textContent = 'ANSI palette';
        termSection.appendChild(ansiTitle);
        const ansiGrid = document.createElement('div');
        ansiGrid.className = 'theme-editor-ansi-grid';
        for (const def of THEME_TERM_ANSI_KEYS) {
            const row = buildColorPickerRow(def, draft.term[def.key], (newHex) => {
                draft.term[def.key] = newHex;
                updatePreview();
            }, /*compact*/ true);
            termRefs[def.key] = row;
            ansiGrid.appendChild(row.el);
        }
        termSection.appendChild(ansiGrid);
        body.appendChild(termSection);

        // Live Preview.
        const previewSection = document.createElement('section');
        previewSection.className = 'theme-editor-section theme-editor-preview-section';
        const previewTitle = document.createElement('h3');
        previewTitle.className = 'theme-editor-section-title';
        previewTitle.textContent = 'Live preview';
        previewSection.appendChild(previewTitle);
        const previewContainer = buildLivePreviewContainer();
        previewSection.appendChild(previewContainer.el);
        body.appendChild(previewSection);

        card.appendChild(body);

        // ---- Footer (Cancel + Save) ----
        const footer = document.createElement('div');
        footer.className = 'modal-actions theme-editor-actions';
        const cancelBtn = document.createElement('button');
        cancelBtn.type = 'button';
        cancelBtn.textContent = 'Cancel';
        const saveBtn = document.createElement('button');
        saveBtn.type = 'button';
        saveBtn.className = 'primary';
        saveBtn.textContent = 'Save';
        footer.appendChild(cancelBtn);
        footer.appendChild(saveBtn);
        card.appendChild(footer);

        overlay.appendChild(card);
        document.body.appendChild(overlay);

        // ---- Functions in scope ----

        /** Перерисовывает live preview по текущему draft. */
        function updatePreview() {
            previewContainer.update(draft);
        }

        /**
         * Применяет пресет к draft + всем пикерам без пересоздания DOM.
         * Если name пустой — подставляет «Copy of {presetName}».
         */
        function applyPresetToDraft(preset) {
            if (!preset) return;
            const cloned = cloneThemeColors(preset);
            draft.ui = cloned.ui;
            draft.term = cloned.term;
            for (const def of THEME_UI_KEYS) {
                uiRefs[def.key].setValue(draft.ui[def.key]);
            }
            for (const def of THEME_TERM_KEYS) {
                termRefs[def.key].setValue(draft.term[def.key]);
            }
            if (!draft.name.trim()) {
                draft.name = `Copy of ${preset.name || preset.id || 'preset'}`;
                nameInput.value = draft.name;
            }
            updatePreview();
        }

        // Initial preview render.
        updatePreview();

        // ---- Async: подгружаем presets для dropdown ----
        fetch('/api/themes')
            .then((r) => r.ok ? r.json() : null)
            .then((data) => {
                if (!data || !Array.isArray(data.presets)) return;
                presets = data.presets;
                for (const p of presets) {
                    const opt = document.createElement('option');
                    opt.value = p.id || '';
                    opt.textContent = p.name || p.id || '—';
                    dupSelect.appendChild(opt);
                }
            })
            .catch(() => { /* dropdown останется только с "From scratch" */ });

        dupSelect.addEventListener('change', () => {
            const id = dupSelect.value;
            if (!id) return;
            const preset = presets.find((p) => p && p.id === id);
            if (preset) applyPresetToDraft(preset);
            // Сбрасываем select на «From scratch», чтобы повторный выбор того
            // же пресета снова срабатывал (change event не стрельнёт без
            // изменения value).
            dupSelect.value = '';
        });

        // ---- Close handlers ----
        const close = () => overlay.remove();
        closeBtn.addEventListener('click', close);
        cancelBtn.addEventListener('click', close);
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
        });

        // ---- Save handler ----
        saveBtn.addEventListener('click', async () => {
            const result = validateDraft(draft);
            if (!result.ok) {
                window.alert(result.error);
                return;
            }
            const payload = buildThemePayload(draft, isEdit);
            saveBtn.disabled = true;
            cancelBtn.disabled = true;
            try {
                let resp;
                if (isEdit) {
                    resp = await fetch(
                        '/api/themes/custom/' + encodeURIComponent(draft.id),
                        {
                            method: 'PUT',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify(payload),
                        }
                    );
                } else {
                    resp = await fetch('/api/themes/custom', {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify(payload),
                    });
                }
                if (!resp.ok) {
                    const text = await resp.text().catch(() => '');
                    window.alert('Failed to save theme: ' + (text || resp.status));
                    return;
                }
                // Успех — закрываем редактор и перезагружаем Themes-панель,
                // если она открыта в settings-модале.
                close();
                const panel = document.getElementById('ps-panel-themes');
                if (panel) {
                    // themesState закрыт в openSettingsModal; чтобы корректно
                    // перерисовать — ищем уже-показанную панель и зовём loader,
                    // который сам обновит DOM. Пробрасываем фиктивный
                    // themesState — loader заполнит его сам.
                    const themesState = { loaded: false, data: null };
                    loadThemesIntoPanel(panel, themesState);
                }
            } catch (e) {
                window.alert('Failed to save theme: ' + (e && e.message ? e.message : e));
            } finally {
                saveBtn.disabled = false;
                cancelBtn.disabled = false;
            }
        });
    }

    /**
     * Создаёт строку с парой <input type=color> + <input type=text> (hex),
     * двусторонняя синхронизация. Возвращает { el, setValue(hex) }.
     *
     * @param {{key:string,label:string}} def — определение поля.
     * @param {string} initialHex — стартовое значение, валидный hex.
     * @param {(newHex:string)=>void} onChange — колбэк, вызывается при
     *   валидном изменении (через любой из инпутов).
     * @param {boolean} compact — если true, рисуется компактнее (для ANSI 4×4).
     */
    function buildColorPickerRow(def, initialHex, onChange, compact) {
        const el = document.createElement('div');
        el.className = 'theme-editor-row theme-editor-color-row'
            + (compact ? ' theme-editor-color-row-compact' : '');

        const label = document.createElement('span');
        label.className = 'theme-editor-row-label';
        label.textContent = def.label;
        el.appendChild(label);

        const pair = document.createElement('div');
        pair.className = 'theme-editor-color-pair';

        const colorInput = document.createElement('input');
        colorInput.type = 'color';
        colorInput.className = 'theme-editor-color-input';
        colorInput.value = normalizeHex(initialHex, '#000000');

        const hexInput = document.createElement('input');
        hexInput.type = 'text';
        hexInput.className = 'theme-editor-hex-input';
        hexInput.maxLength = 7;
        hexInput.spellcheck = false;
        hexInput.value = colorInput.value;

        // color → hex: всегда валиден, color picker даёт #rrggbb.
        colorInput.addEventListener('input', () => {
            const v = colorInput.value.toLowerCase();
            hexInput.value = v;
            hexInput.classList.remove('invalid');
            onChange(v);
        });

        // hex → color: валидируем; невалидный hex подсвечивает border var(--danger)
        // и НЕ пробрасывает onChange + НЕ обновляет color picker.
        hexInput.addEventListener('input', () => {
            const v = hexInput.value.trim();
            if (HEX_COLOR_RE.test(v)) {
                hexInput.classList.remove('invalid');
                colorInput.value = v.toLowerCase();
                onChange(v.toLowerCase());
            } else {
                hexInput.classList.add('invalid');
            }
        });
        // На blur — если осталось невалидное значение, восстанавливаем последнее
        // валидное из color picker (чтобы UI не висел в «битом» состоянии).
        hexInput.addEventListener('blur', () => {
            if (!HEX_COLOR_RE.test(hexInput.value.trim())) {
                hexInput.value = colorInput.value;
                hexInput.classList.remove('invalid');
            }
        });

        pair.appendChild(colorInput);
        pair.appendChild(hexInput);
        el.appendChild(pair);

        return {
            el,
            setValue(hex) {
                const v = normalizeHex(hex, '#000000');
                colorInput.value = v;
                hexInput.value = v;
                hexInput.classList.remove('invalid');
            },
        };
    }

    /**
     * Создаёт DOM live-preview блока. Возвращает { el, update(draft) }.
     *
     * Внутренняя структура:
     *   - .theme-preview-ui — мини-приложение (sidebar + main + buttons + tags).
     *     Использует scoped CSS-переменные, выставленные inline на корне через
     *     setProperty. Это позволяет применять «текущую тему черновика» внутри
     *     preview БЕЗ влияния на :root.
     *   - .theme-preview-term — мини-терминал. Inline background/color с
     *     16 цветными span'ами для каждого ANSI цвета.
     */
    function buildLivePreviewContainer() {
        const el = document.createElement('div');
        el.className = 'theme-editor-preview';

        // ----- мини-UI -----
        const uiBlock = document.createElement('div');
        uiBlock.className = 'theme-preview-ui';
        // Сайдбар.
        const side = document.createElement('div');
        side.className = 'theme-preview-sidebar';
        const sideTitle = document.createElement('div');
        sideTitle.className = 'theme-preview-sidebar-title';
        sideTitle.textContent = 'Sessions';
        side.appendChild(sideTitle);
        const sideList = document.createElement('ul');
        sideList.className = 'theme-preview-sidebar-list';
        ['main', 'logs', 'editor'].forEach((s, i) => {
            const li = document.createElement('li');
            li.className = 'theme-preview-sidebar-item' + (i === 0 ? ' active' : '');
            li.textContent = s;
            sideList.appendChild(li);
        });
        side.appendChild(sideList);
        uiBlock.appendChild(side);
        // Main panel.
        const main = document.createElement('div');
        main.className = 'theme-preview-main';
        const text = document.createElement('div');
        text.className = 'theme-preview-text';
        text.textContent = 'Sample text — primary foreground.';
        main.appendChild(text);
        const dim = document.createElement('div');
        dim.className = 'theme-preview-text-dim';
        dim.textContent = 'Dimmer secondary text — fg-dim.';
        main.appendChild(dim);
        const tags = document.createElement('div');
        tags.className = 'theme-preview-tags';
        ['p0', 'p1', 'p2'].forEach((p) => {
            const t = document.createElement('span');
            t.className = 'theme-preview-tag theme-preview-tag-' + p;
            t.textContent = p.toUpperCase();
            tags.appendChild(t);
        });
        main.appendChild(tags);
        const btnRow = document.createElement('div');
        btnRow.className = 'theme-preview-buttons';
        const btnAccent = document.createElement('button');
        btnAccent.className = 'theme-preview-btn theme-preview-btn-accent';
        btnAccent.type = 'button';
        btnAccent.textContent = 'Action';
        const btnWarn = document.createElement('button');
        btnWarn.className = 'theme-preview-btn theme-preview-btn-warn';
        btnWarn.type = 'button';
        btnWarn.textContent = 'Warn';
        const btnDanger = document.createElement('button');
        btnDanger.className = 'theme-preview-btn theme-preview-btn-danger';
        btnDanger.type = 'button';
        btnDanger.textContent = 'Danger';
        btnRow.appendChild(btnAccent);
        btnRow.appendChild(btnWarn);
        btnRow.appendChild(btnDanger);
        main.appendChild(btnRow);
        uiBlock.appendChild(main);
        el.appendChild(uiBlock);

        // ----- мини-Terminal -----
        const term = document.createElement('div');
        term.className = 'theme-preview-term';
        const termLine1 = document.createElement('div');
        termLine1.textContent = '$ ls --color';
        term.appendChild(termLine1);
        // Строка с base 8 цветами.
        const termLine2 = document.createElement('div');
        const base8 = ['black','red','green','yellow','blue','magenta','cyan','white'];
        const span8 = {};
        base8.forEach((k) => {
            const s = document.createElement('span');
            s.className = 'theme-preview-ansi';
            s.textContent = k + ' ';
            span8[k] = s;
            termLine2.appendChild(s);
        });
        term.appendChild(termLine2);
        // Строка с bright 8.
        const termLine3 = document.createElement('div');
        const bright8 = ['brightBlack','brightRed','brightGreen','brightYellow',
                        'brightBlue','brightMagenta','brightCyan','brightWhite'];
        const spanBright = {};
        bright8.forEach((k) => {
            const s = document.createElement('span');
            s.className = 'theme-preview-ansi';
            s.textContent = k.replace('bright', 'br.').toLowerCase() + ' ';
            spanBright[k] = s;
            termLine3.appendChild(s);
        });
        term.appendChild(termLine3);
        // Cursor + selection sample.
        const termLine4 = document.createElement('div');
        const sel = document.createElement('span');
        sel.className = 'theme-preview-selection';
        sel.textContent = 'selected';
        const cur = document.createElement('span');
        cur.className = 'theme-preview-cursor';
        cur.textContent = '█';
        termLine4.appendChild(document.createTextNode('cursor '));
        termLine4.appendChild(cur);
        termLine4.appendChild(document.createTextNode(' selection '));
        termLine4.appendChild(sel);
        term.appendChild(termLine4);
        el.appendChild(term);

        function update(draft) {
            // 11 UI vars выставляем как scoped CSS-переменные на корневом контейнере
            // preview. Внутри классы .theme-preview-* используют var(--bg) и т.д.
            const cssMap = {
                bg: '--bg',
                bgElev: '--bg-elev',
                fg: '--fg',
                fgDim: '--fg-dim',
                border: '--border',
                accent: '--accent',
                warn: '--warn',
                danger: '--danger',
                p0: '--p0',
                p1: '--p1',
                p2: '--p2',
            };
            for (const [k, cssVar] of Object.entries(cssMap)) {
                const v = draft.ui[k];
                if (typeof v === 'string') {
                    el.style.setProperty(cssVar, v);
                }
            }
            // Terminal — inline стили (без CSS-переменных, т.к. цветов 20 и
            // достаточно прямого setStyle на каждом span).
            term.style.background = draft.term.background;
            term.style.color = draft.term.foreground;
            term.style.border = '1px solid ' + draft.term.foreground;
            for (const k of base8) {
                if (span8[k]) span8[k].style.color = draft.term[k];
            }
            for (const k of bright8) {
                if (spanBright[k]) spanBright[k].style.color = draft.term[k];
            }
            cur.style.background = draft.term.cursor;
            cur.style.color = draft.term.background;
            sel.style.background = draft.term.selection;
            sel.style.color = draft.term.foreground;
        }

        return { el, update };
    }

    /**
     * Валидирует draft перед save. Возвращает { ok: true } или
     * { ok: false, error: 'human readable message' }.
     */
    function validateDraft(draft) {
        const trimmed = (draft.name || '').trim();
        if (!trimmed) {
            return { ok: false, error: 'Name is required.' };
        }
        for (const { key, label } of THEME_UI_KEYS) {
            if (!HEX_COLOR_RE.test(draft.ui[key] || '')) {
                return { ok: false, error: `UI / ${label}: invalid hex color.` };
            }
        }
        for (const { key, label } of THEME_TERM_KEYS) {
            if (!HEX_COLOR_RE.test(draft.term[key] || '')) {
                return { ok: false, error: `Terminal / ${label}: invalid hex color.` };
            }
        }
        return { ok: true };
    }

    /**
     * Собирает payload для POST/PUT — полная Theme-структура с camelCase
     * полями, как ожидает бэкенд (#[serde(rename_all = "camelCase")]).
     *
     * В create-режиме id оставляем пустой — сервер сгенерирует UUID
     * (см. create_custom_theme в main.rs). В edit-режиме id игнорируется
     * сервером (path-параметр каноничен), но прокидываем для прозрачности.
     */
    function buildThemePayload(draft, isEdit) {
        return {
            id: isEdit ? draft.id : '',
            name: (draft.name || '').trim(),
            kind: 'custom',
            ui: { ...draft.ui },
            term: { ...draft.term },
        };
    }

    /**
     * Phase 5 — форма настроек Notifications для одного проекта.
     *
     * Возвращает <fieldset> с полями notify_template / notify_delay_minutes /
     * notify_wait_previous / notify_session + кнопкой Save. Значения берутся
     * из переданного `project` (DTO, прилетевший из GET /api/projects).
     *
     * Save вызывает [`saveProjectSettings`], при успехе зовёт `onSaved()`
     * (callback из openSettingsModal — для re-render списка). При ошибке
     * показывает inline-сообщение в пределах формы и не закрывает форму.
     */
    function buildNotificationsForm(project, onSaved) {
        const fs = document.createElement('fieldset');
        fs.className = 'notify-fieldset';

        const legend = document.createElement('legend');
        legend.textContent = 'Notifications';
        fs.appendChild(legend);

        // Подсказка про шаблон + семантику delay/wait_previous.
        const hint = document.createElement('div');
        hint.className = 'notify-hint';
        hint.textContent =
            'Шаблон: плейсхолдеры {id} {title} {description} {priority} {type}. ' +
            'delay_minutes=0 — отправлять сразу; wait_previous переопределяет delay ' +
            '(сообщение уходит после закрытия предыдущей задачи в той же сессии).';
        fs.appendChild(hint);

        // notify_template — textarea (multiline, удобнее для шаблонов с \n).
        const tplWrap = document.createElement('label');
        tplWrap.className = 'notify-field';
        tplWrap.textContent = 'Template';
        const tpl = document.createElement('textarea');
        tpl.className = 'notify-template';
        tpl.rows = 3;
        tpl.placeholder = 'task: {title}\n{description}';
        tpl.value = (project && typeof project.notify_template === 'string')
            ? project.notify_template
            : '';
        tpl.title = 'Шаблон. Поддержка плейсхолдеров: {id} {title} {description} {priority} {type}.';
        tplWrap.appendChild(tpl);
        fs.appendChild(tplWrap);

        // notify_delay_minutes — number, ≥0.
        const delayWrap = document.createElement('label');
        delayWrap.className = 'notify-field';
        delayWrap.textContent = 'Delay (minutes)';
        const delay = document.createElement('input');
        delay.type = 'number';
        delay.min = '0';
        delay.step = '1';
        delay.className = 'notify-delay';
        const delayVal = (project && typeof project.notify_delay_minutes === 'number')
            ? project.notify_delay_minutes
            : 0;
        delay.value = String(delayVal);
        delay.title = '0 — отправлять сразу. Игнорируется, если включён wait_previous.';
        delayWrap.appendChild(delay);
        fs.appendChild(delayWrap);

        // notify_wait_previous — checkbox.
        const waitWrap = document.createElement('label');
        waitWrap.className = 'modal-check notify-check';
        const wait = document.createElement('input');
        wait.type = 'checkbox';
        wait.className = 'notify-wait';
        wait.checked = !!(project && project.notify_wait_previous);
        wait.title = 'Ждать закрытия предыдущей задачи перед отправкой следующей. Переопределяет delay.';
        waitWrap.appendChild(wait);
        const waitText = document.createTextNode(' Wait for previous (overrides delay)');
        waitWrap.appendChild(waitText);
        fs.appendChild(waitWrap);

        // notify_session — text input, опционально.
        const sessWrap = document.createElement('label');
        sessWrap.className = 'notify-field';
        sessWrap.textContent = 'Session override';
        const sess = document.createElement('input');
        sess.type = 'text';
        sess.className = 'notify-session';
        sess.placeholder = 'override: tmux session name (пусто = текущая сессия проекта)';
        sess.value = (project && typeof project.notify_session === 'string')
            ? project.notify_session
            : '';
        sess.title = 'Если задано — все нотификации этого проекта пойдут в указанную сессию.';
        sessWrap.appendChild(sess);
        fs.appendChild(sessWrap);

        // Сообщение об ошибке (скрыто по умолчанию).
        const err = document.createElement('div');
        err.className = 'notify-error';
        err.style.display = 'none';
        fs.appendChild(err);

        // Save-кнопка.
        const actions = document.createElement('div');
        actions.className = 'notify-actions';
        const saveBtn = document.createElement('button');
        saveBtn.type = 'button';
        saveBtn.className = 'primary';
        saveBtn.textContent = 'Save';
        saveBtn.addEventListener('click', async () => {
            err.style.display = 'none';
            err.textContent = '';
            saveBtn.disabled = true;

            // Сборка payload. Семантика:
            //  - notify_template: всегда отправляем (string, может быть пустой).
            //  - notify_delay_minutes: parseInt, fallback на 0; clamp в u32.
            //  - notify_wait_previous: bool из checkbox.
            //  - notify_session: пустая строка → null (стереть override на бэке).
            const rawDelay = parseInt(delay.value, 10);
            const safeDelay = Number.isFinite(rawDelay) && rawDelay >= 0 ? rawDelay : 0;
            const rawSess = String(sess.value || '').trim();
            const payload = {
                notify_template: String(tpl.value || ''),
                notify_delay_minutes: safeDelay,
                notify_wait_previous: !!wait.checked,
                notify_session: rawSess === '' ? null : rawSess,
            };

            const result = await saveProjectSettings(project.id, payload);
            saveBtn.disabled = false;
            if (result.ok) {
                if (typeof onSaved === 'function') onSaved();
            } else {
                err.style.display = '';
                err.textContent = result.error || 'Не удалось сохранить настройки.';
            }
        });
        actions.appendChild(saveBtn);
        fs.appendChild(actions);

        return fs;
    }

    /**
     * Phase 5 — PATCH /api/projects/:id/settings.
     *
     * payload — `{notify_template, notify_delay_minutes, notify_wait_previous,
     * notify_session}`. notify_session = null → стереть override на бэкенде
     * (см. `deserialize_optional_optional_string` в main.rs).
     *
     * Optimistic-апдейт: state.projects сразу мержится с payload, чтобы UI
     * показал новые значения до ответа сервера. На ошибку — откатываем
     * к prev-снимку и возвращаем `{ok:false, error}`. На успех — мержим в
     * state.projects обновлённый DTO с сервера.
     *
     * Возвращает `{ok: true, project}` либо `{ok: false, error}`.
     */
    async function saveProjectSettings(projectId, payload) {
        if (!projectId) {
            return { ok: false, error: 'no project id' };
        }

        // Optimistic: запоминаем prev и применяем patch к state.projects.
        const idx = Array.isArray(state.projects)
            ? state.projects.findIndex((p) => p && p.id === projectId)
            : -1;
        const prev = (idx >= 0) ? state.projects[idx] : null;
        if (idx >= 0 && prev) {
            state.projects[idx] = Object.assign({}, prev, {
                notify_template: payload.notify_template,
                notify_delay_minutes: payload.notify_delay_minutes,
                notify_wait_previous: payload.notify_wait_previous,
                notify_session: payload.notify_session,
            });
        }

        try {
            const r = await fetch('/api/projects/' + encodeURIComponent(projectId) + '/settings', {
                method: 'PATCH',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(payload),
            });
            if (!r.ok) {
                // Rollback.
                if (idx >= 0 && prev) {
                    state.projects[idx] = prev;
                }
                const text = await r.text();
                return { ok: false, error: text || ('HTTP ' + r.status) };
            }
            const updated = await r.json();
            // Реконсайл: подставляем DTO с сервера.
            if (idx >= 0) {
                state.projects[idx] = updated;
            } else if (Array.isArray(state.projects)) {
                state.projects.push(updated);
            }
            return { ok: true, project: updated };
        } catch (e) {
            if (idx >= 0 && prev) {
                state.projects[idx] = prev;
            }
            return { ok: false, error: e && e.message ? e.message : String(e) };
        }
    }

    /**
     * Создаёт overlay + базовый style hook. CSS — в style.css секция Modals.
     */
    function buildModalOverlay() {
        const overlay = document.createElement('div');
        overlay.className = 'modal-overlay';
        return overlay;
    }

    // =========================================================================
    // Phase 6.C — Tasks CRUD: createTask / updateTask / closeTask / reopenTask
    // + modal builders openCreateModal / openEditModal + optimistic UI
    // =========================================================================

    /**
     * Все статусы, которые юзер может выставить через UI (минус closed —
     * close идёт через DELETE с reason).
     */
    const TASK_EDIT_STATUSES = ['open', 'in_progress', 'blocked', 'deferred', 'draft', 'closed'];

    /**
     * Список типов задач, поддерживаемых beads_rust.
     */
    const TASK_TYPES = ['task', 'bug', 'feature', 'epic', 'chore', 'docs', 'question'];

    /**
     * Optimistic UI helpers:
     *
     * - На createTask мы прокидываем «черновую» issue в state.tasksData.issues
     *   ещё до ответа сервера, чтобы карточка появилась в нужной колонке мгновенно.
     *   После ответа реконсайлим — заменяем placeholder реальным issue (с реальным
     *   id) либо при ошибке откатываем.
     * - На updateTask — обновляем issue in-place в state.tasksData и перерисовываем.
     * - На closeTask — переводим в `closed` статус сразу, при ошибке откатываем.
     * - На reopenTask — переводим в `open` сразу.
     */
    function getIssueIndex(id) {
        if (!state.tasksData || !Array.isArray(state.tasksData.issues)) return -1;
        return state.tasksData.issues.findIndex((it) => it && it.id === id);
    }

    function applyOptimisticPatch(id, patch) {
        const idx = getIssueIndex(id);
        if (idx < 0) return null;
        const prev = state.tasksData.issues[idx];
        const next = Object.assign({}, prev, patch);
        state.tasksData.issues[idx] = next;
        renderTasks();
        return prev;
    }

    function rollbackIssue(id, prev) {
        if (!prev) return;
        const idx = getIssueIndex(id);
        if (idx < 0) {
            state.tasksData.issues.unshift(prev);
        } else {
            state.tasksData.issues[idx] = prev;
        }
        renderTasks();
    }

    /**
     * POST /api/tasks. На вход payload {title, description?, type?, priority?,
     * labels?, parent?}. Optimistic prepend в state.tasksData.issues со
     * сгенерированным временным id `tmp-<rand>`. После ответа — заменяем
     * на реальный issue. При ошибке убираем placeholder.
     */
    async function createTask(payload) {
        const tempId = 'tmp-' + Math.random().toString(36).slice(2, 8);
        const optimistic = {
            id: tempId,
            title: payload.title,
            description: payload.description || '',
            issue_type: payload.type || 'task',
            priority: (typeof payload.priority === 'number') ? payload.priority : 2,
            status: payload.status || 'open',
            labels: (payload.labels || '').split(',').map((s) => s.trim()).filter(Boolean),
            updated_at: new Date().toISOString(),
            __optimistic: true,
        };
        if (state.tasksData && Array.isArray(state.tasksData.issues)) {
            state.tasksData.issues.unshift(optimistic);
            renderTasks();
        }

        try {
            const r = await fetch('/api/tasks', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(payload),
            });
            if (!r.ok) {
                const text = await r.text();
                window.alert('Создание не удалось: ' + (text || r.status));
                if (state.tasksData) {
                    state.tasksData.issues = state.tasksData.issues.filter((it) => it.id !== tempId);
                    renderTasks();
                }
                return null;
            }
            const created = await r.json();
            // Реконсайл: заменяем placeholder на реальный issue.
            if (state.tasksData) {
                const idx = getIssueIndex(tempId);
                if (idx >= 0) {
                    state.tasksData.issues[idx] = created;
                } else {
                    state.tasksData.issues.unshift(created);
                }
                renderTasks();
            } else {
                // Если данных не было — просто перетянем snapshot.
                fetchTasks();
            }
            return created;
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
            if (state.tasksData) {
                state.tasksData.issues = state.tasksData.issues.filter((it) => it.id !== tempId);
                renderTasks();
            }
            return null;
        }
    }

    /**
     * PATCH /api/tasks/:id. payload — частичный набор полей. Optimistic
     * apply, при ошибке — rollback.
     */
    async function updateTask(id, payload) {
        const optimisticPatch = {};
        if ('status' in payload) optimisticPatch.status = payload.status;
        if ('title' in payload) optimisticPatch.title = payload.title;
        if ('priority' in payload) optimisticPatch.priority = payload.priority;
        if ('description' in payload) optimisticPatch.description = payload.description;
        if ('labels' in payload) {
            optimisticPatch.labels = (payload.labels || '').split(',').map((s) => s.trim()).filter(Boolean);
        }
        const prev = applyOptimisticPatch(id, optimisticPatch);

        try {
            // Phase 5: проксируем PATCH на remote, если задача origin !== 'local'.
            const origin = taskOriginById(id);
            const r = await apiFetch('/api/tasks/' + encodeURIComponent(id), {
                method: 'PATCH',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(payload),
            }, origin);
            if (!r.ok) {
                const text = await r.text();
                window.alert('Update не удался: ' + (text || r.status));
                rollbackIssue(id, prev);
                return null;
            }
            const updatedArr = await r.json();
            // br update --json возвращает массив (даже для одного id) с подмножеством
            // полей. Применяем поля из ответа поверх существующего issue.
            const updated = Array.isArray(updatedArr) ? updatedArr.find((u) => u && u.id === id) : null;
            if (updated && state.tasksData) {
                const idx = getIssueIndex(id);
                if (idx >= 0) {
                    state.tasksData.issues[idx] = Object.assign({}, state.tasksData.issues[idx], updated);
                    renderTasks();
                }
            }
            return updated;
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
            rollbackIssue(id, prev);
            return null;
        }
    }

    /**
     * Phase 5 — Возвращает origin для задачи по id (ищем в state.tasksData.issues).
     * Если задачи нет в текущем snapshot'е — fallback 'local'.
     */
    function taskOriginById(id) {
        if (!state.tasksData || !Array.isArray(state.tasksData.issues)) return 'local';
        const issue = state.tasksData.issues.find((it) => it && it.id === id);
        return dtoOrigin(issue);
    }

    /**
     * DELETE /api/tasks/:id?reason=... Optimistic переводит карточку в
     * `closed`, при ошибке возвращает прежний status.
     *
     * Phase 5: для remote-задач (issue.origin !== 'local') добавляем ?server=.
     */
    async function closeTask(id, reason) {
        const prev = applyOptimisticPatch(id, { status: 'closed' });
        try {
            const origin = taskOriginById(id);
            let url = '/api/tasks/' + encodeURIComponent(id)
                + (reason ? ('?reason=' + encodeURIComponent(reason)) : '');
            const r = await apiFetch(url, { method: 'DELETE' }, origin);
            if (!r.ok && r.status !== 204) {
                const text = await r.text();
                window.alert('Close не удался: ' + (text || r.status));
                rollbackIssue(id, prev);
                return false;
            }
            return true;
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
            rollbackIssue(id, prev);
            return false;
        }
    }

    /**
     * POST /api/tasks/:id/reopen. Переводит карточку в `open`.
     *
     * Phase 5: для remote-задач (issue.origin !== 'local') добавляем ?server=.
     */
    async function reopenTask(id) {
        const prev = applyOptimisticPatch(id, { status: 'open' });
        try {
            const origin = taskOriginById(id);
            const r = await apiFetch('/api/tasks/' + encodeURIComponent(id) + '/reopen', {
                method: 'POST',
            }, origin);
            if (!r.ok && r.status !== 204) {
                const text = await r.text();
                window.alert('Reopen не удался: ' + (text || r.status));
                rollbackIssue(id, prev);
                return false;
            }
            return true;
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
            rollbackIssue(id, prev);
            return false;
        }
    }

    /**
     * Строит markup для form с полями task. Используется и в create-,
     * и в edit-modal.
     *
     * @param {object} initial — initial values (title/description/priority/status/type/labels)
     * @param {boolean} isEdit — true для edit modal (показываем status поле и id)
     */
    function buildTaskFormHtml(initial, isEdit) {
        const i = initial || {};
        const sel = (val, current) => (String(val) === String(current) ? ' selected' : '');
        const statusOptions = TASK_EDIT_STATUSES
            .map((s) => `<option value="${s}"${sel(s, i.status || 'open')}>${COLUMN_TITLES[s] || s}</option>`).join('');
        const typeOptions = TASK_TYPES
            .map((t) => `<option value="${t}"${sel(t, i.issue_type || 'task')}>${t}</option>`).join('');
        const prioOptions = [0, 1, 2, 3, 4]
            .map((p) => `<option value="${p}"${sel(p, (typeof i.priority === 'number') ? i.priority : 2)}>P${p}</option>`).join('');

        const labelsCsv = Array.isArray(i.labels) ? i.labels.join(',') : (i.labels || '');
        const idLine = isEdit && i.id ? `<div class="modal-id">${i.id}</div>` : '';
        const statusBlock = isEdit
            ? `<label>Status<br><select id="tm-status">${statusOptions}</select></label>`
            : '';

        return `
            ${idLine}
            <label>Title<br><input type="text" id="tm-title" value="${escapeAttr(i.title || '')}" placeholder="Краткое описание"></label>
            <label>Description<br><textarea id="tm-desc" placeholder="Подробности (опционально)">${escapeText(i.description || '')}</textarea></label>
            <div class="field-row">
                <label>Priority<br><select id="tm-prio">${prioOptions}</select></label>
                <label>Type<br><select id="tm-type">${typeOptions}</select></label>
                ${statusBlock || '<label>&nbsp;<br><span style="opacity:0.5;font-size:0.7rem">status выставится open</span></label>'}
            </div>
            <label>Labels (csv)<br><input type="text" id="tm-labels" value="${escapeAttr(labelsCsv)}" placeholder="phase-6,api"></label>
        `;
    }

    function escapeAttr(s) {
        return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');
    }
    function escapeText(s) {
        return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    }

    /**
     * Modal для создания новой задачи. preset — необязательный объект
     * с полями initial (например {status: 'in_progress'} из quick-create-кнопки
     * в колонке).
     */
    function openCreateModal(preset) {
        const overlay = buildModalOverlay();
        const card = document.createElement('div');
        card.className = 'modal-card task-modal';

        const initial = Object.assign({ status: 'open' }, preset || {});
        const isTodo = initial.status === 'todo';
        const heading = isTodo ? 'New TODO' : 'New task';
        const planModeBlock = isTodo
            ? `<label class="checkbox-row"><input type="checkbox" id="tm-plan-mode"> Включить план мод
                 <span class="hint">— при promote добавит «Создай план для этой задачи»</span></label>`
            : '';
        card.innerHTML = `
            <h2>${heading}</h2>
            ${buildTaskFormHtml(initial, false)}
            ${planModeBlock}
            <div class="modal-actions">
                <button type="button" id="tm-cancel">Cancel</button>
                <button type="button" id="tm-save" class="primary">Create</button>
            </div>
        `;
        overlay.appendChild(card);
        document.body.appendChild(overlay);

        const $title = card.querySelector('#tm-title');
        const $desc = card.querySelector('#tm-desc');
        const $prio = card.querySelector('#tm-prio');
        const $type = card.querySelector('#tm-type');
        const $labels = card.querySelector('#tm-labels');
        $title.focus();

        const close = () => overlay.remove();
        card.querySelector('#tm-cancel').addEventListener('click', close);
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
        });
        card.querySelector('#tm-save').addEventListener('click', async () => {
            const title = ($title.value || '').trim();
            if (!title) {
                window.alert('Title обязателен');
                return;
            }

            // Phase 4: TODO-режим → POST /api/todos (отдельный store, не bd).
            if (isTodo) {
                const projectId = state.activeProjectId;
                if (!projectId) {
                    window.alert('Активный проект не выбран');
                    return;
                }
                const $planMode = card.querySelector('#tm-plan-mode');
                const todoPayload = {
                    project_id: projectId,
                    title,
                    description: ($desc.value || '').trim() || undefined,
                    plan_mode: $planMode ? !!$planMode.checked : false,
                };
                close();
                try {
                    const r = await fetch('/api/todos', {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify(todoPayload),
                    });
                    if (!r.ok) {
                        const text = await r.text();
                        window.alert('Создание TODO не удалось: ' + (text || r.status));
                        return;
                    }
                    // Карточка появится через WS upsert. Если WS лагает —
                    // подстрахуемся ручным fetchTodos через короткую паузу.
                    setTimeout(() => {
                        if (!state.todosWs || state.todosWs.readyState !== WebSocket.OPEN) {
                            fetchTodos();
                        }
                    }, 200);
                } catch (e) {
                    window.alert('Ошибка запроса: ' + e.message);
                }
                return;
            }

            const payload = {
                title,
                description: ($desc.value || '').trim() || undefined,
                type: $type.value,
                priority: parseInt($prio.value, 10),
                labels: ($labels.value || '').trim() || undefined,
            };
            // preset.status пробрасываем в optimistic-плейсхолдер, чтобы карточка
            // появилась в нужной колонке (POST /api/tasks игнорирует status —
            // br create без -s даёт open; для других колонок применим follow-up
            // PATCH).
            const wantStatus = initial.status && initial.status !== 'open' ? initial.status : null;
            close();
            const created = await createTask(Object.assign({}, payload, wantStatus ? { status: wantStatus } : {}));
            if (created && wantStatus && created.status !== wantStatus) {
                // Follow-up patch чтобы статус соответствовал колонке quick-create.
                await updateTask(created.id, { status: wantStatus });
            }
        });
    }

    /**
     * Modal для редактирования существующей задачи. Поля заполнены текущими
     * значениями. Кнопки: Save (PATCH), Close (DELETE с prompt reason),
     * Reopen (если status === closed), Cancel.
     */
    function openEditModal(issue) {
        if (!issue || !issue.id) return;
        const overlay = buildModalOverlay();
        const card = document.createElement('div');
        card.className = 'modal-card task-modal';

        const isClosed = String(issue.status || '').toLowerCase() === 'closed';
        const reopenBtn = isClosed
            ? `<button type="button" id="tm-reopen" class="warn">Reopen</button>`
            : `<button type="button" id="tm-close-task" class="warn">Close…</button>`;

        card.innerHTML = `
            <h2>Edit task</h2>
            ${buildTaskFormHtml(issue, true)}
            <div class="modal-actions">
                ${reopenBtn}
                <span class="spacer"></span>
                <button type="button" id="tm-cancel">Cancel</button>
                <button type="button" id="tm-save" class="primary">Save</button>
            </div>
        `;
        overlay.appendChild(card);
        document.body.appendChild(overlay);

        const $title = card.querySelector('#tm-title');
        const $desc = card.querySelector('#tm-desc');
        const $prio = card.querySelector('#tm-prio');
        const $type = card.querySelector('#tm-type');
        const $status = card.querySelector('#tm-status');
        const $labels = card.querySelector('#tm-labels');

        const close = () => overlay.remove();
        card.querySelector('#tm-cancel').addEventListener('click', close);
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
        });

        // Save: собираем только изменённые поля (минимизируем br update).
        card.querySelector('#tm-save').addEventListener('click', async () => {
            const newTitle = ($title.value || '').trim();
            if (!newTitle) {
                window.alert('Title обязателен');
                return;
            }
            const patch = {};
            if (newTitle !== (issue.title || '')) patch.title = newTitle;
            const newDesc = ($desc.value || '');
            if (newDesc !== (issue.description || '')) patch.description = newDesc;
            const newPrio = parseInt($prio.value, 10);
            if (Number.isFinite(newPrio) && newPrio !== issue.priority) patch.priority = newPrio;
            const newStatus = $status.value;
            if (newStatus !== (issue.status || '')) patch.status = newStatus;
            // type у br update тоже есть, шлём если изменился.
            const newType = $type.value;
            if (newType !== (issue.issue_type || '')) {
                // br update --type существует, но мы в API не маппили — пропускаем
                // изменения типа в UI (можно добавить позже). Пока — без действия.
            }
            const newLabels = ($labels.value || '').trim();
            const oldLabelsCsv = Array.isArray(issue.labels) ? issue.labels.join(',') : (issue.labels || '');
            if (newLabels !== oldLabelsCsv) patch.labels = newLabels;

            if (Object.keys(patch).length === 0) {
                close();
                return;
            }
            close();
            await updateTask(issue.id, patch);
        });

        // Close-кнопка (для не-closed): спрашиваем reason.
        const $closeTask = card.querySelector('#tm-close-task');
        if ($closeTask) {
            $closeTask.addEventListener('click', async () => {
                const reason = window.prompt('Причина закрытия задачи:', '') || '';
                close();
                await closeTask(issue.id, reason.trim() || undefined);
            });
        }

        // Reopen-кнопка (для closed).
        const $reopen = card.querySelector('#tm-reopen');
        if ($reopen) {
            $reopen.addEventListener('click', async () => {
                close();
                await reopenTask(issue.id);
            });
        }
    }

    /**
     * Phase 4 — модалка редактирования TODO-карточки.
     *
     * Отличия от openEditModal (bd-issues):
     *  - Поля: только title и description (без status/priority/type/labels —
     *    у TODO эти поля есть в модели, но UI пока упрощён по плану).
     *  - Кнопки: Save (PATCH /api/todos/:id), Delete (DELETE /api/todos/:id),
     *    Promote (POST /api/todos/:id/promote с опциональным session-input).
     *  - При успешном Save/Delete/Promote модалка закрывается, а WS-event
     *    upsert/removed синхронизирует state автоматически.
     */
    function openTodoEditModal(todo) {
        if (!todo || !todo.id) return;
        const overlay = buildModalOverlay();
        const card = document.createElement('div');
        card.className = 'modal-card task-modal';

        // Дефолтное значение для поля session: текущая активная сессия,
        // иначе первая сессия проекта (по name asc), иначе пусто.
        let defaultSession = state.currentSession || '';
        if (!defaultSession) {
            const projectId = todo.project_id || state.activeProjectId || null;
            const projectSessions = (state.sessions || [])
                .filter((s) => projectId ? s.project_id === projectId : true)
                .map((s) => s.name)
                .sort((a, b) => String(a).localeCompare(String(b)));
            if (projectSessions.length > 0) defaultSession = projectSessions[0];
        }

        const planChecked = todo.plan_mode ? ' checked' : '';
        card.innerHTML = `
            <h2>Edit TODO</h2>
            <div class="modal-id">${escapeText(todo.id)}</div>
            <label>Title<br><input type="text" id="td-title" value="${escapeAttr(todo.title || '')}" placeholder="Краткое описание"></label>
            <label>Description<br><textarea id="td-desc" placeholder="Подробности (опционально)">${escapeText(todo.description || '')}</textarea></label>
            <label class="checkbox-row"><input type="checkbox" id="td-plan-mode"${planChecked}> Включить план мод
                <span class="hint">— при promote добавит «Создай план для этой задачи»</span></label>
            <label>Promote → tmux session<br><input type="text" id="td-session" value="${escapeAttr(defaultSession)}" placeholder="${escapeAttr(defaultSession || 'session name')}"></label>
            <div class="modal-actions">
                <button type="button" id="td-delete" class="warn">Delete</button>
                <span class="spacer"></span>
                <button type="button" id="td-cancel">Cancel</button>
                <button type="button" id="td-promote" class="primary">Promote</button>
                <button type="button" id="td-save" class="primary">Save</button>
            </div>
        `;
        overlay.appendChild(card);
        document.body.appendChild(overlay);

        const $title = card.querySelector('#td-title');
        const $desc = card.querySelector('#td-desc');
        const $session = card.querySelector('#td-session');
        const $planMode = card.querySelector('#td-plan-mode');
        $title.focus();

        const close = () => overlay.remove();
        card.querySelector('#td-cancel').addEventListener('click', close);
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
        });

        // Save → PATCH /api/todos/:id (только изменённые поля).
        card.querySelector('#td-save').addEventListener('click', async () => {
            const newTitle = ($title.value || '').trim();
            if (!newTitle) {
                window.alert('Title обязателен');
                return;
            }
            const patch = {};
            if (newTitle !== (todo.title || '')) patch.title = newTitle;
            const newDesc = ($desc.value || '');
            if (newDesc !== (todo.description || '')) patch.description = newDesc;
            const newPlanMode = $planMode ? !!$planMode.checked : false;
            if (newPlanMode !== !!todo.plan_mode) patch.plan_mode = newPlanMode;
            if (Object.keys(patch).length === 0) {
                close();
                return;
            }
            close();
            try {
                // Phase 5: для remote-todo (origin !== 'local') проксируем.
                const origin = dtoOrigin(todo);
                const r = await apiFetch('/api/todos/' + encodeURIComponent(todo.id), {
                    method: 'PATCH',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(patch),
                }, origin);
                if (!r.ok) {
                    const text = await r.text();
                    window.alert('Update не удался: ' + (text || r.status));
                    return;
                }
                // WS upsert обновит state.todosData автоматически.
            } catch (e) {
                window.alert('Ошибка запроса: ' + e.message);
            }
        });

        // Delete → DELETE /api/todos/:id (с подтверждением).
        card.querySelector('#td-delete').addEventListener('click', async () => {
            if (!window.confirm('Удалить TODO?')) return;
            close();
            try {
                // Phase 5: для remote-todo (origin !== 'local') проксируем.
                const origin = dtoOrigin(todo);
                const r = await apiFetch('/api/todos/' + encodeURIComponent(todo.id), {
                    method: 'DELETE',
                }, origin);
                if (!r.ok && r.status !== 204) {
                    const text = await r.text();
                    window.alert('Delete не удался: ' + (text || r.status));
                    return;
                }
                // WS removed обновит state.todosData автоматически.
            } catch (e) {
                window.alert('Ошибка запроса: ' + e.message);
            }
        });

        // Promote → переиспользуем общий promoteTodo (валидация сессии,
        // optimistic remove, rollback при ошибке).
        card.querySelector('#td-promote').addEventListener('click', async () => {
            const sessionVal = ($session.value || '').trim() || undefined;
            close();
            await promoteTodo(todo.id, sessionVal);
        });
    }

    // =========================================================================
    // Bootstrap
    // =========================================================================

    /**
     * Загружает активную тему с сервера и применяет CSS-переменные ДО создания
     * xterm.Terminal. Возвращает xterm ITheme (или null если API недоступен —
     * вызывающий передаст null в initTerminal, тот применит fallback).
     */
    async function loadActiveThemeOrNull() {
        try {
            const resp = await fetch('/api/themes/active', { headers: { 'Accept': 'application/json' } });
            if (!resp.ok) {
                console.warn('GET /api/themes/active failed:', resp.status);
                return null;
            }
            const theme = await resp.json();
            // Применяем CSS-переменные сразу (state.term ещё нет, xterm-ветка
            // пропустится — это ок, xterm-палитру передаём отдельно ниже).
            applyTheme(theme);
            return theme.term ? mapTermTheme(theme.term) : null;
        } catch (e) {
            console.warn('loadActiveThemeOrNull failed:', e);
            return null;
        }
    }

    /**
     * Phase 5 — GET /healthz; пишет state.remoteMode и state.serverVersion.
     *
     * Контракт: эндпоинт доступен без Bearer-auth (см. auth::is_path_excluded
     * на бэке) и отдаёт { status, remote_mode, version }. Если запрос упал —
     * считаем remote_mode=false (legacy-friendly fallback), но всё равно
     * выставляем healthzLoaded=true чтобы остальной bootstrap продолжился.
     */
    async function loadHealthz() {
        try {
            const r = await fetch('/healthz', { headers: { 'Accept': 'application/json' } });
            if (!r.ok) {
                console.warn('GET /healthz failed:', r.status);
                state.remoteMode = false;
                state.healthzLoaded = true;
                return;
            }
            const data = await r.json();
            state.remoteMode = !!data.remote_mode;
            state.serverVersion = typeof data.version === 'string' ? data.version : null;
            state.healthzLoaded = true;
        } catch (e) {
            console.warn('loadHealthz failed:', e);
            state.remoteMode = false;
            state.healthzLoaded = true;
        }
    }

    /**
     * Phase 5 — true если frontend должен рендерить новый UI (origin-табы,
     * Settings → Remote servers tab, кнопка add-remote, и т.п.).
     * Используется как guard в renderSidebar, openSettingsModal и API-helper'ах.
     * При false поведение фронта побитово совпадает с legacy.
     */
    function isRemoteMode() {
        return state.remoteMode === true;
    }

    // -------------------------------------------------------------------------
    // Phase 5 — Global id (origin::local-id) и origin-aware API-helpers
    // -------------------------------------------------------------------------

    /**
     * Глобальный id в remote-mode имеет формат `<origin>::<local-id>`, где
     * `origin` — 'local' либо `<server_id>`. Local-id (без префикса) — это id,
     * которым оперирует бэкенд target'а (локальный devforge или remote).
     *
     * В legacy-режиме id всегда «простой» (без префикса) и parseGlobalId
     * возвращает { origin: 'local', id }.
     *
     * Возвращает { origin: string, id: string }.
     */
    function parseGlobalId(s) {
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
    function formatGlobalId(origin, id) {
        if (!origin || origin === 'local') return id;
        return origin + '::' + id;
    }

    /**
     * Origin DTO-объекта. Бэкенд проставляет поле `origin` в Session/Project/
     * Task/Todo DTO начиная с Phase 3 (см. remote_proxy::enrich_with_origin).
     * Fallback на 'local' если поле отсутствует.
     */
    function dtoOrigin(dto) {
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
    function withServerParam(path, origin) {
        if (!origin || origin === 'local') return path;
        const sep = path.indexOf('?') >= 0 ? '&' : '?';
        return path + sep + 'server=' + encodeURIComponent(origin);
    }

    /**
     * Centralized fetch helper для остальных API-вызовов. Используется ТОЛЬКО
     * там, где запрос может уходить на remote (sessions/projects/tasks/todos).
     * Не трогает /healthz, /api/themes, /api/remote-servers (они только local).
     *
     * Origin определяется так:
     *   1) Явный аргумент `origin` (если передан и !== 'local');
     *   2) Иначе path остаётся как есть.
     *
     * В legacy-режиме (remoteMode=false) — игнорирует origin (всё локально).
     */
    function apiFetch(path, init, origin) {
        if (isRemoteMode() && origin && origin !== 'local') {
            return fetch(withServerParam(path, origin), init);
        }
        return fetch(path, init);
    }

    // -------------------------------------------------------------------------
    // Phase 5 — Remote servers registry + lazy-load remote projects/sessions
    // -------------------------------------------------------------------------

    /**
     * Phase 5/7 — карта server_id → 'online'|'offline'|'unknown'. Обновляется
     * `probeRemoteServer()` per-server с экспоненциальным backoff (см. ниже).
     * До первого пинга — 'unknown' (UI рендерит серую точку). Используется в
     * sidebar для индикатора online/offline у origin-секции и в
     * Settings-таблице.
     */
    state.remoteOnline = new Map();

    // Phase 7 — per-server exponential backoff (с jitter) для health-probe.
    //
    // Базовая серия задержек: 2s → 4s → 8s → 16s → 32s → 60s (cap). После
    // успеха backoffStep сбрасывается; начинаем со второй точки (4s),
    // чтобы UI не дёргался слишком часто на стабильных серверах.
    //
    // Состояние per-server хранится в state.remoteProbeState[serverId]:
    //   { timer, step, lastResult: 'online'|'offline'|null }.
    //
    // Реализация: при каждом probe → schedule следующий probe через delay
    // соответствующего step'а. Это «event-driven backoff»: при потере связи
    // step растёт (medленнее retries), при восстановлении step=0 (опять 2s)
    // и UI быстро вернётся в online.
    const REMOTE_PROBE_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000];
    const REMOTE_PROBE_STEADY_INDEX = 1; // 4s — интервал когда сервер online
    const REMOTE_PROBE_JITTER_MAX_MS = 1000;
    /** @type {Map<string, {timer: any, step: number, inFlight: boolean}>} */
    const remoteProbeState = new Map();

    /**
     * GET /api/remote-servers → state.remoteServers. Затем стартует периодический
     * health-poll (если ещё не запущен). Возвращает Promise, чтобы caller'ы могли
     * дождаться загрузки (например, перед первым renderSidebar в remote-mode).
     *
     * No-op при remoteMode=false (registry-эндпоинты доступны только в
     * remote-mode; в legacy режиме отдают 404 и захламят консоль).
     */
    async function fetchRemoteServers() {
        if (!isRemoteMode()) return;
        try {
            const r = await fetch('/api/remote-servers', { headers: { 'Accept': 'application/json' } });
            if (!r.ok) {
                console.warn('GET /api/remote-servers failed:', r.status);
                return;
            }
            const data = await r.json();
            state.remoteServers = Array.isArray(data) ? data : [];
            // Сбрасываем online-статусы для серверов которых больше нет в реестре.
            const knownIds = new Set(state.remoteServers.map((s) => s.id));
            for (const id of Array.from(state.remoteOnline.keys())) {
                if (!knownIds.has(id)) state.remoteOnline.delete(id);
            }
            // Обеспечим запуск health-poll'а.
            startRemoteHealthPoll();
        } catch (e) {
            console.warn('fetchRemoteServers failed:', e);
        }
    }

    /**
     * Lazy-load массива проектов с конкретного remote-сервера.
     * Кладёт результат в state.remoteProjects[serverId] (Map.set).
     * При ошибке кладёт пустой массив, чтобы UI не зависал на "Loading…".
     */
    async function loadRemoteProjects(serverId) {
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

    /**
     * Lazy-load массива сессий с конкретного remote-сервера.
     * Кладёт результат в state.remoteSessions[serverId] (Map.set).
     */
    async function loadRemoteSessions(serverId) {
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

    /**
     * Phase 7 — выполняет ОДИН health-probe конкретного remote-сервера.
     *
     * Алгоритм:
     * 1. GET /api/remote-servers/:id/healthz.
     * 2. На !ok / network / catch → онлайн-статус сервера в state.remoteOnline
     *    переключается в 'offline', step backoff'а увеличивается до cap'а.
     * 3. На ok с {online:true} → статус 'online', step сбрасывается в
     *    REMOTE_PROBE_STEADY_INDEX (медленный устойчивый polling).
     * 4. Расчитывается следующий delay = backoffs[step] + jitter(0..jitterMax).
     * 5. setTimeout планирует следующий probe того же сервера.
     *
     * Состояние UI: при смене remoteOnline-статуса вызывается renderSidebar(),
     * чтобы offline-badge у origin-секции мгновенно обновился. WS-подписки
     * (tasksWs/todosWs/attachWs/lazygitWs) не трогаются — они имеют
     * собственный auto-reconnect и сами вернутся в OPEN при восстановлении
     * связи. UI-состояние (открытая сессия, активный origin) сохраняется.
     */
    async function probeRemoteServer(serverId) {
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

        // Обновляем UI-состояние.
        const prev = state.remoteOnline.get(serverId);
        if (prev !== nextStatus) {
            state.remoteOnline.set(serverId, nextStatus);
            renderSidebar();
        }

        // Управляем step backoff'а.
        if (nextStatus === 'online') {
            entry.step = REMOTE_PROBE_STEADY_INDEX;
        } else {
            entry.step = Math.min(entry.step + 1, REMOTE_PROBE_BACKOFFS_MS.length - 1);
        }

        // Schedule следующий probe.
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

    /**
     * Phase 7 — синхронизирует таблицу remoteProbeState с текущим списком
     * state.remoteServers:
     *   - Для каждого нового сервера: создаёт запись (step=0) и сразу запускает
     *     первый probe.
     *   - Для удалённых из реестра: clearTimeout и удаление записи.
     *
     * Идемпотентна: повторный вызов не порождает дубль-таймеров.
     */
    function startRemoteHealthPoll() {
        if (!isRemoteMode()) return;
        const knownIds = new Set(state.remoteServers.map((s) => s.id));
        // Стартуем новые probes.
        for (const srv of state.remoteServers) {
            if (!remoteProbeState.has(srv.id)) {
                remoteProbeState.set(srv.id, { timer: null, step: 0, inFlight: false });
                // Первый probe — немедленно.
                probeRemoteServer(srv.id);
            }
        }
        // Сносим probes для удалённых.
        for (const id of Array.from(remoteProbeState.keys())) {
            if (!knownIds.has(id)) {
                const e = remoteProbeState.get(id);
                if (e && e.timer) clearTimeout(e.timer);
                remoteProbeState.delete(id);
                state.remoteOnline.delete(id);
            }
        }
    }

    function stopRemoteHealthPoll() {
        for (const e of remoteProbeState.values()) {
            if (e.timer) clearTimeout(e.timer);
        }
        remoteProbeState.clear();
    }

    async function bootstrap() {
        // Phase 5: GET /healthz сразу — нужно знать remote_mode ДО initTerminal,
        // т.к. некоторые ветки рендера (sidebar header, project bar) проверяют
        // isRemoteMode() уже на первом рендере.
        await loadHealthz();

        // Phase 3: тема грузится ДО initTerminal — иначе xterm создаётся со
        // старой темой и переключение через options.theme не применит
        // background сразу. См. комментарий в initTerminal.
        const termTheme = await loadActiveThemeOrNull();
        initTerminal(termTheme);
        showPlaceholder(true);
        setStatus('disconnected', 'disconnected');

        $btnNew.addEventListener('click', createSessionPrompt);

        // Phase 6.A: Tab-bar listeners.
        if ($tabTerminal) $tabTerminal.addEventListener('click', () => switchTab('terminal'));
        if ($tabTasks) $tabTasks.addEventListener('click', () => switchTab('tasks'));
        if ($tasksReload) $tasksReload.addEventListener('click', () => fetchTasks());
        // Phase 6.C: + New task → openCreateModal без preset (status default open).
        if ($tasksNew) $tasksNew.addEventListener('click', () => openCreateModal());

        // Git-таб: переключение через tab-bar.
        if ($tabGit) $tabGit.addEventListener('click', () => switchTab('git'));
        // lazygit-tab: error-banner кнопки.
        if ($gitErrorRetry) $gitErrorRetry.addEventListener('click', retryGitConnection);
        if ($gitErrorClose) $gitErrorClose.addEventListener('click', hideGitBanner);

        // Phase 6.B: Project bar listeners.
        if ($projectSelect) {
            // Cross-project sessions visibility: селектор стал UI-фильтром
            // сайдбара. НЕ дёргаем switchActiveProject / fetchSessions /
            // disconnectWs — backend-side активный проект и WS-attach к
            // текущей сессии остаются нетронутыми.
            $projectSelect.addEventListener('change', (ev) => {
                const id = ev.target.value;
                state.projectFilter = id;
                try {
                    localStorage.setItem('forge.projectFilter', id);
                } catch (_) { /* privacy mode — игнор */ }
                renderSidebar();
            });
        }
        if ($projectNew) {
            $projectNew.addEventListener('click', openNewProjectModal);
        }
        if ($projectSettings) {
            $projectSettings.addEventListener('click', openSettingsModal);
        }

        // Phase 5 — параллельно с проектами загружаем реестр remote-серверов.
        // No-op в legacy (remote_mode=false). fetchRemoteServers сам запускает
        // health-poll после успешной загрузки.
        if (isRemoteMode()) {
            fetchRemoteServers().then(() => {
                // Восстанавливаем activeOrigin (зависит от состава remoteServers).
                loadActiveOriginFromStorage();
                renderSidebar();
            });
        }

        // Сначала проекты (нужны для контекста sessions/tasks), потом — sessions+polling.
        fetchProjects().finally(() => {
            fetchSessions();
            startPolling();
            // Phase 6.D: открываем realtime WS сразу, не дожидаясь, пока
            // пользователь переключится на Tasks. Snapshot маленький, а
            // upsert-стрим без подписчиков всё равно ходит в холостую,
            // так что подключиться заранее проще, чем ловить race на
            // первом switchTab('tasks').
            connectTasksWs();
            // Phase 4: realtime TODOs WS + первичный fetch (snapshot из WS
            // придёт всё равно, но fetch даёт быстрый initial paint
            // если WS чуть лагает или connect не успел).
            fetchTodos();
            connectTodosWs();
        });

        // На unload — стопаем polling и закрываем оба WS.
        window.addEventListener('beforeunload', () => {
            stopPolling();
            stopTasksPolling();
            stopTodosPolling();
            disconnectTasksWs();
            disconnectTodosWs();
            disconnectWs();
            // Phase 4: закрываем lazygit WS при unload.
            closeGitWs('beforeunload');
            // Phase 5: остановим periodic health-poll'инг remote-серверов.
            stopRemoteHealthPoll();
        });
        // На скрытие страницы (mobile) — пауза polling, на показ — возобновление.
        // WS оставляем — браузер сам разорвёт его если надо, а connect-ретраи
        // дешевы. При возврате — connectTasksWs() гарантирует, что соединение
        // живое (или поднимется заново).
        document.addEventListener('visibilitychange', () => {
            if (document.hidden) {
                stopPolling();
                stopTasksPolling();
                stopTodosPolling();
            } else {
                fetchSessions();
                startPolling();
                if (state.activeTab === 'tasks') {
                    // Если WS упал во время скрытой вкладки — переподключим.
                    connectTasksWs();
                    // Если WS живой, snapshot уже актуален; если нет —
                    // fallback fetchTasks подтянет свежий envelope.
                    if (!state.tasksWs || state.tasksWs.readyState !== WebSocket.OPEN) {
                        fetchTasks();
                    }
                }
                // TODOs WS — переподключаем независимо от tab,
                // т.к. колонка TODO живёт прямо на kanban, и пользователь
                // мог быть на Tasks-табе ровно из-за неё.
                connectTodosWs();
                if (!state.todosWs || state.todosWs.readyState !== WebSocket.OPEN) {
                    fetchTodos();
                }
            }
        });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', bootstrap);
    } else {
        bootstrap();
    }
})();
