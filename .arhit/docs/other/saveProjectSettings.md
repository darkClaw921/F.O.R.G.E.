# saveProjectSettings

Phase 5 — PATCH /api/projects/:id/settings с optimistic UI (tmux-web/static/app.js).

Сигнатура: async saveProjectSettings(projectId: string, payload: {notify_template, notify_delay_minutes, notify_wait_previous, notify_session}) → {ok: true, project: ProjectDto} | {ok: false, error: string}.

Семантика:
- notify_template, notify_delay_minutes, notify_wait_previous — отправляются как есть.
- notify_session: '' → null (стереть override на бэке через deserialize_optional_optional_string в main.rs); строка → set; отсутствует → не трогать. Текущая реализация всегда передаёт явное значение (string|null).

Алгоритм:
1. Optimistic-обновление state.projects[idx] мерджем payload в существующий DTO (мгновенный UI-фидбэк).
2. fetch PATCH /api/projects/:id/settings с JSON body.
3. На !r.ok — rollback к prev-снимку и возврат {ok:false, error: text|status}.
4. На r.ok — реконсайл state.projects[idx] = updated DTO, возврат {ok:true, project: updated}.
5. На исключение — rollback и {ok:false, error: e.message}.

Не закрывает модалку — это решает caller (buildNotificationsForm: при ошибке показывает inline-error, при успехе зовёт onSaved для re-render списка проектов с обновлёнными значениями).

Зависит от: backend route patch_project_settings (tmux-web/src/main.rs:790).
