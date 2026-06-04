# next_step::list_rules

Репозиторий правил «Следующего шага» (таблица next_step_rules). Файл plugins/echo/src/db/repo/next_step.rs.

list_rules(db, project_id: Option<&str>, limit: i64) -> Vec<Rule>: возвращает релевантные правила для генерации prompt'а — ГЛОБАЛЬНЫЕ (project_id IS NULL) ПЛЮС правила указанного project_id (если задан), отсортированные по created_at DESC и ограниченные limit последними. При project_id=None отдаёт только глобальные. Один SQL для обоих случаев: WHERE project_id IS NULL OR project_id = ?1.

DEFAULT_RULES_LIMIT=20 — кап на число правил в одном prompt, защищает контекст от разрастания и держит его сфокусированным на последних коррекциях.

Назначение: подмешивание выученных правил в генерацию следующего шага. next_step::generate_for_session вызывает list_rules(db, None, DEFAULT_RULES_LIMIT) и вставляет блок [learned_rules] в prompt (формат 'когда: <context> предлагай: <suggested>'). Парная функция — insert_rule (запись из feedback). Зависимости: crate::db::Db.
