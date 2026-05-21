# remove-projects-concept — обзор изменений

План-обзор удаления концепции «проект» из F.O.R.G.E. Реализовано в 7 фаз (Phase 1-6 выполнены, Phase 7 — verification).
Источник: /Users/igorgerasimov/.claude/plans/remove-projects-concept.md.

## Контекст (до)

Раньше в F.O.R.G.E. вся фильтрация/группировка ресурсов шла через сущность Project { id, name, path, tmux_prefix, notify_template, notify_delay_minutes, notify_wait_previous, notify_session, ... }. Это давало двойную модель: TODO/themes/user_settings/notifier жили «per project», а tasks (beads) — уже «per cwd через .beads/-lookup». Симптом: TODO одни и те же у 5 сессий разных папок, потому что они в одном «широком» проекте.

## Решение (после)

Полностью удалено понятие project. Источник истины — cwd сессии (session.path). Группировка/фильтрация — только по folder-headers в sidebar (folder_id/folder_label в SessionDto). TODO/notifier привязаны к «корню» — ближайшая папка вверх с маркером .beads/ или .git/, иначе сам cwd (paths::resolve_root).

## Что было удалено

### Backend
- tmux-web/src/projects.rs — модуль ProjectStore полностью снят.
- AppState.projects: Arc<RwLock<ProjectStore>> — поле удалено.
- /api/projects/* routes — удалены все 6 endpoint'ов (get/create/delete/patch/set_active/init).
- DTO: ProjectDto, CreateProjectReq, PatchProjectSettingsReq, SetActiveReq, InitProjectReq.
- Helpers: touch_if_missing, run_in, resolve_project, folder_name, session_belongs.
- SessionDto.project_id / SessionDto.project_name / SessionDto.tmux_prefix — поля удалены.

### Echo plugin boundary
- echo_host_api::HostApi методы list_projects() / active_project_id() — удалены.
- ProjectInfo DTO — снят.
- Echo продолжает хранить опциональный project_id в SQLite (chat_sessions.project_id, memories.project_id) как непрозрачный soft-FK label — но валидация и enumeration снаружи плагина не предоставляются.

### Frontend
- tmux-web/static/js/projects/* (projects.js, new-project.js) — директория удалена.
- tmux-web/static/css/project-bar.css — удалён.
- #project-bar и #project-select в index.html — удалены, оставлена кнопка #project-settings (глобальный settings modal).
- state.activeProjectId / state.projects / state.projectFilter / state.remoteProjects — поля сняты со state.js.
- state.todosCurrentProjectId переименован в state.todosCurrentPath.
- Sidebar: project-headers убраны, остались только folder-группы.
- Settings modal: вкладка Project удалена, notifier-настройки переехали в общий блок.

## Что появилось

### Backend
- tmux-web/src/paths.rs — новая функция resolve_root(cwd: &Path) -> PathBuf. Ищет ближайшую папку вверх с маркером .beads/ → .git/ → fallback на сам cwd. Используется TodoStore, ws_todos, notifier.
- tmux-web/src/notifier_config.rs — новый модуль с NotifierConfigStore (template, delay_minutes, wait_previous, session) в ~/.config/forge/notifier.json. Один глобальный конфиг на пользователя вместо per-project.
- TodoStore.by_path: HashMap<root_path: String, Vec<Todo>> вместо ранее by_project_id.
- Todo.root_path вместо Todo.project_id. #[serde(alias = "project_id")] обеспечивает обратную совместимость.
- ~/.config/forge/todos.json — путь к глобальному хранилищу TODO (раньше per-project .forge/todos.json).
- WS /ws/todos?path=<cwd> — параметр сменился с project_id на path. resolve_root(cwd) применяется на стороне сервера.
- REST: /api/notifier-config (GET/PATCH) — новый endpoint для управления notifier-конфигом.

### Migration logic
- TodoStore.load_with_projects(file_path, projects_path): при загрузке если в JSON встречается project_id или хотя бы один root_path не выглядит абсолютным путём, читаем ~/.config/forge/projects.json для маппинга project_id → project.path и пишем todos.json в новом формате. После первой записи projects.json больше не читается (но остаётся на диске для отката).
- Fallback при отсутствии projects.json: project_id остаётся как root_path (deg-fallback, данные не теряются, tracing::warn).

## Фазы реализации

1. Phase 1 — Backend foundation: paths::resolve_root + миграция TODO storage в глобальный todos.json (forge-1nu).
2. Phase 2 — Backend REST/WS API на cwd-path: /api/todos и /ws/todos переехали на ?path= (forge-t37).
3. Phase 3 — Notifier и глобальные настройки без project: notifier_config.rs + миграция NotifyJob.project_id → root_path (forge-z97).
4. Phase 4 — Удалить ProjectStore и адаптировать Echo plugin boundary: snять /api/projects/* и поля HostApi (forge-yzy).
5. Phase 5 — Frontend cwd-only: удалить js/projects/*, css/project-bar.css, project-headers в sidebar, переписать ws/todos-ws.js (forge-lbr).
6. Phase 6 — Документация и memory cleanup (forge-j95, текущая).
7. Phase 7 — Verification: cargo build/test --workspace + manual UI smoke (forge-3um, открыта).

## Migration для пользователей

При первом старте после апгрейда:
- Старый todos.json (per-project) автоматически мигрирует в ~/.config/forge/todos.json с маппингом project_id → root_path через ~/.config/forge/projects.json.
- projects.json остаётся на диске, но больше не читается (можно удалить вручную).
- notifier-настройки из projects.json НЕ мигрируют автоматически — пользователю нужно один раз настроить ~/.config/forge/notifier.json через REST /api/notifier-config (или UI Settings → Notifications).

## Откат

git revert всех Phase 1-5 коммитов (или ручной checkout предыдущего тега). Старый projects.json остаётся на диске и будет подхвачен старой версией без миграции.