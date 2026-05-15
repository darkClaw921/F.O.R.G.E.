# main::SessionDto

DTO-структура для JSON-сериализации записи tmux-сессии в ответе GET /api/sessions (tmux-web/src/main.rs:756-787).

Derive: #[derive(Debug, Serialize)].

Поля:
- info: SessionInfo — флаттенится через #[serde(flatten)], то есть поля SessionInfo (name, path, attached, windows, created, session_group и т.д.) выливаются в корень JSON-объекта на уровне с остальными полями DTO.
- needs_attention: bool — индикатор наличия Claude permission prompt в панели сессии (см. attention::detect_claude_prompt). true → фронт подсвечивает вкладку оранжевым.
- project_id: Option<String> — id зарегистрированного проекта (uuid) либо синтетический ключ '__path__:<cwd>' либо tmux_prefix-матч. Используется в switchActiveProject и фильтрах TODO/.beads. None для orphan-сессий с пустым path.
- project_name: Option<String> — отображаемое имя проекта (basename последней папки project.path либо Project::name как fallback).
- folder_id: Option<String> — (Phase 1 forge-fl3t) идентификатор папочно-ориентированной группы вида '__folder:<absolute_path>'. Заполняется helper'ом resolve_folder. Префикс '__folder:' исключает коллизии с project_id. None зеркалит резолв basename → пустая строка / отсутствует.
- folder_label: Option<String> — (Phase 1 forge-fl3t) человекочитаемая метка папочной группы — basename последней папки session.path. Используется во фронте в group-header sidebar.
- origin: String — (Phase 3) источник записи: всегда 'local' для локально-сгенерированных DTO. Прокси через ?server=<id> НЕ строит SessionDto на этой стороне — там прокидывается уже готовый JSON remote'а, обогащённый remote_proxy::enrich_with_origin. Поле сериализуется ВСЕГДА, чтобы фронт получал унифицированный формат.

Сериализация всех Option<String> полей идёт БЕЗ skip_serializing_if — ключи folder_id, folder_label, project_id, project_name присутствуют в JSON всегда (значение null при None). Это упрощает контракт для фронта.

Конструируется только в одном месте — внутри замыкания .map() в get_sessions (main.rs ~807-822) после snapshot'а projects и attention. Поля заполняются по очереди:
  let (project_id, project_name) = resolve_project(&s, &projects_snap);
  let (folder_id, folder_label) = resolve_folder(&s);
  SessionDto { needs_attention, project_id, project_name, folder_id, folder_label, info: s, origin: 'local'.to_string() }

Связи: SessionInfo (tmux.rs) — flatten-источник; resolve_project (main.rs) — заполняет project_*; resolve_folder (main.rs) — заполняет folder_*; attention::AttentionState — источник needs_attention.
