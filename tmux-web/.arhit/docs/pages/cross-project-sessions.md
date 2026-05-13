# Cross-project sessions visibility

Фича позволяет одновременно видеть tmux-сессии всех настроенных проектов в сайдбаре tmux-web, с возможностью фильтрации по конкретному проекту. До этой фичи сайдбар показывал только сессии активного проекта (фильтр был на backend).

План: /Users/igorgerasimov/.claude/plans/wild-munching-thimble.md

## Семантика

Различаем ДВА независимых понятия:
- **active project** (state.activeProjectId) — backend-side понятие: какой проект сейчас 'выбран' для команд create-session, для рабочей директории и т.п. Управляется через POST /api/projects/{id}/activate. НЕ затрагивается фильтром.
- **project filter** (state.projectFilter) — frontend-only UI-фильтр сайдбара: что отображать в списке сессий. Значения: '__all__' (default) либо id одного из state.projects.

Переключение фильтра НЕ переключает активный проект и НЕ дёргает сервер — это чисто визуальная операция (renderSidebar + localStorage).

## Backend изменения (Phase 1)

### src/main.rs::SessionDto
- Расширен Option-полями: project_id: Option<String>, project_name: Option<String> (плюс уже существующие name, attached, windows через flatten от tmux::SessionInfo, и needs_attention).
- None означает orphan-сессия (имя не матчит ни один tmux_prefix настроенных проектов).

### src/main.rs::get_sessions (хендлер GET /api/sessions)
- Снят фильтр по активному проекту — возвращает ВСЕ tmux-сессии.
- Snapshot проектов (короткий read-lock на ProjectStore), затем для каждой сессии ищется первый матчащий проект через projects::session_belongs(prefix, name).
- needs_attention заполняется из attention.snapshot() (один snapshot на вызов для согласованности).
- Поведение при отсутствии tmux-сервера: возвращает [] (не 500).

### src/attention.rs::watcher_loop
- Сигнатура изменена: убран параметр projects: Arc<RwLock<ProjectStore>>. Теперь только attention: Arc<AttentionState>.
- Поллит ВСЕ сессии без фильтрации каждые 1500мс (раньше фильтровал по active project prefix). Это нужно чтобы оранжевая подсветка работала и для сессий неактивных проектов и orphan-сессий в режиме 'All projects'.
- Spawn в main(): tokio::spawn(attention::watcher_loop(app_state.attention.clone())).

## Frontend изменения (Phase 2)

### state.projectFilter (static/app.js)
- Новое поле: '__all__' | <project.id>. Default '__all__'.
- Persist в localStorage('forge.projectFilter'). Чтение в fetchProjects: валидируется (=== '__all__' либо matches одного из state.projects[].id), иначе fallback на '__all__'. Запись — в change-handler #project-select. localStorage обёрнут в try/catch (privacy mode).

### renderProjectSelect (static/app.js)
- Первой опцией всегда вставляет <option value='__all__'>All projects</option>.
- Selected — по state.projectFilter (НЕ по activeProjectId — это разные вещи).

### change-handler #project-select
- НЕ дёргает switchActiveProject / fetchSessions / disconnectWs.
- Только: state.projectFilter = новое значение, localStorage.setItem, renderSidebar().
- Опция 'All projects' (value='__all__') обрабатывается так же, как любой projectId.

### renderSidebar (static/app.js)
- Режим '__all__': группировка по project_id в порядке state.projects, orphan-группа (project_id===null) последней. Каждая непустая группа предваряется <li class='session-group-header'>{project.name}</li>.
- Режим <project.id>: плоский список без заголовка, только сессии этого проекта.
- Empty-states: 'Нет активных сессий' (если state.sessions пуст), 'Нет сессий в этом проекте' (если у выбранного проекта нет сессий).
- Сортировка внутри группы: по name.localeCompare.
- buildSessionItem вынесен из renderSidebar для переиспользования между режимами.

### static/style.css::.session-group-header
- padding 6px 12px 4px, font-size 11px, uppercase, letter-spacing 0.5px, color #6c7587, background #181c25, border-top 1px #232936, cursor default, list-style none.
- :first-child без верхней границы.

## Verification (Phase 3)

- cargo check + cargo build --release — clean (только pre-existing warnings в src/pty.rs::take_writer/child_pid).
- cargo test -- --test-threads=1 — 29 passed, 0 failed (parallel mode flaky на projects::tests из-за общего CWD).

## Manual e2e (для пользователя)

Предусловия:
1. В config настроены 2 проекта (например, project A с tmux_prefix='a-', project B с prefix='b-').
2. В tmux запущены: одна сессия 'a-foo' (принадлежит A), одна 'b-bar' (принадлежит B), одна 'random' (orphan).

Шаги:
1. Запустить tmux-web (cargo run --release).
2. Открыть веб-UI.
3. Проверить #project-select: первая опция 'All projects', далее A и B. Selected по умолчанию 'All projects' (либо последний выбор из localStorage).
4. **All projects**: в сайдбаре три группы с заголовками 'A' (a-foo), 'B' (b-bar), 'Orphan' (random).
5. **Single project filter**: переключить на A → виден только a-foo, без заголовка. Переключить на B → только b-bar.
6. **localStorage persistence**: refresh страницы → последний выбор фильтра восстановлен.
7. **Claude prompt orange highlight**: запустить claude в b-bar, довести до permission prompt; вкладка b-bar в сайдбаре должна стать оранжевой (за ~5с) даже если фильтр стоит на 'All projects' или на A (в А она не отобразится, но переключение на 'All projects' покажет оранжевую b-bar). Подсветка исчезает после ответа.

## Файлы

- src/main.rs (SessionDto, get_sessions)
- src/attention.rs (watcher_loop signature)
- static/app.js (state.projectFilter, fetchProjects, renderProjectSelect, change-handler, renderSidebar, buildSessionItem)
- static/style.css (.session-group-header)

## Связанные доки arhit

- main::SessionDto, main::get_sessions, main::AppState
- attention, attention::watcher_loop, attention::AttentionState, attention::detect_claude_prompt
- renderSidebar, renderProjectSelect, fetchProjects, buildSessionItem
- attention-feature (предыдущая фича, оранжевая подсветка)
- cross-project-sessions-frontend (детали Phase 2)
