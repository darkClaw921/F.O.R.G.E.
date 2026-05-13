# main::get_sessions

Хендлер axum GET /api/sessions в src/main.rs. Возвращает Vec<SessionDto> со ВСЕМИ tmux-сессиями (фильтр по активному проекту снят в Phase 1 cross-project sessions visibility — раньше фильтровал через session_belongs по active project prefix).

Алгоритм:
1. Snapshot проектов через state.projects.read().await.list() (короткий read-lock, копирует Vec<Project>).
2. tmux::list_sessions() — все сессии tmux-сервера. Если сервер не запущен — возвращает [] (не 500).
3. Snapshot attention-state: state.attention.snapshot().await — один раз для согласованности.
4. Для каждой сессии:
   - Итерируем по projects_snap, находим первый проект чей tmux_prefix матчит имя через projects::session_belongs(prefix, name) → заполняем project_id/project_name из p.id/p.name.
   - Если ни один не подошёл — оба None (orphan-сессия, не принадлежит ни одному настроенному проекту).
   - needs_attention = attention.get(&s.name).copied().unwrap_or(false).
5. Возвращаем Json(Vec<SessionDto>).

Frontend сам решает что показывать: фильтр '__all__' группирует по project_id, фильтр <project.id> рендерит только сессии этого проекта.
