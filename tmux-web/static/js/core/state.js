// tmux-web — singleton state (Phase 0 ES Modules refactor)
//
// 1:1 копия объекта state из IIFE `tmux-web/static/app.js` (строки ~89-171).
// Используется как shared-singleton всеми feature-модулями: `import { state }
// from '../core/state.js'`. Чтение/запись полей — прямое, как в legacy IIFE,
// семантика замыкания эмулируется shared ES-модулем (один import =>
// один экземпляр объекта).
//
// ВАЖНО: модуль не импортирует ничего (нижний слой). Никакой логики не
// содержит — только инициализация полей. Описание каждого поля — в
// комментариях ниже (1:1 как в app.js).
//
// В Phase 0 этот модуль ещё НЕ подключен к index.html — app.js работает как
// раньше; модуль готов к импорту из main.js в Phase 1.

// ---- глобальное состояние ----
export const state = {
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
    currentWindows: [],       // окна активной сессии (для window-bar)
    windowsPollTimer: null,   // setInterval для poll окон активной сессии
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
    // ---- User Settings (TODO behavior) ----
    // Кэш пользовательских настроек, загружается через GET /api/user-settings
    // на bootstrap (best-effort) и обновляется через PATCH в settings/user-settings-api.js.
    // null до первого успешного fetch; при ошибке остаётся null — Tasks UI должен
    // обращаться к дефолтам, поведение совпадает с legacy (до фичи).
    // Структура: { todo_default_plan_mode, todo_default_priority,
    //   todo_default_issue_type, todo_plan_mode_suffix,
    //   todo_confirm_delete, todo_confirm_promote_on_drag }.
    userSettings: null,
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
    // ---- TUI-tabs: xterm-инстанции lazygit / lazydocker / telescope ----
    // Каждая вкладка — отдельный экземпляр TuiTab, созданный через
    // createTuiTab() (см. ниже). Поля term/fit/ws/mounted/currentCwd/
    // errorSticky лежат внутри tab.state. Здесь оставлены прямые ссылки,
    // чтобы существующий код (state.gitTerm.*) не сломался.
    gitTerm: null,          // TuiTab.state — заполняется в bootstrap createTuiTab
    dockerTerm: null,
    telescopeTerm: null,
};
