# next_step::insert_rule

Репозиторий правил «Следующего шага» (таблица next_step_rules, миграция V005). Файл plugins/echo/src/db/repo/next_step.rs.

insert_rule(db, project_id: Option<&str>, context_summary: &str, suggested_next: &str) -> Rule: вставляет новое правило обратной связи с UUIDv4 id и created_at=now (unix). project_id=None означает ГЛОБАЛЬНОЕ правило (применимо к любой сессии); не-NULL — правило для непрозрачного ярлыка проекта (git-корня). Возвращает созданную запись Rule{id, project_id, context_summary, suggested_next, created_at}.

Назначение: память обратной связи. Когда пользователь исправляет предложенный воркером шаг через POST .../feedback, бэкенд пишет правило (context_summary = pane-выдержка + отвергнутое предложение, suggested_next = коррекция пользователя). При последующих генерациях list_rules подмешивает эти правила в prompt, чтобы предложения становились точнее.

Repo-слой ничего не знает про axum/воркер — принимает &Db. Зависимости: crate::db::Db (rusqlite через tokio), uuid, chrono. Используется routes::next_step::feedback (запись) и next_step::generate_for_session (чтение через list_rules).
