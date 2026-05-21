# main::SessionDto

DTO в tmux-web/src/main.rs, сериализуемый в JSON для GET /api/sessions. После remove-projects-concept (Phase 4) поля project_id/project_name удалены — SessionDto не содержит привязки к понятию project. Структура: name, attached, windows и прочее из tmux::SessionInfo через #[serde(flatten)] + needs_attention: bool + is_generating: bool + folder_id/folder_label: Option<String> + origin: String.

## Поля

### needs_attention: bool
Заполняется из snapshot'а AppState.attention в хендлере: attention.snapshot().await.get(&s.name).copied().unwrap_or(false). Snapshot снимается ОДИН РАЗ за вызов хендлера, чтобы все сессии в ответе видели согласованное состояние watcher'а.

Семантика: true когда в pane сессии обнаружен Claude prompt (permission/plan/question) И сессия является primary в дедупе группы. См. attention::deduplicate_attention.

### is_generating: bool
Заполняется из generating_snapshot() аналогично needs_attention.

Семантика:
- true когда за прошедший тик watcher'а (1500мс) содержимое ПОСЛЕДНИХ 50 СТРОК pane изменилось (gen_hash50 prev≠current) И сессия выбрана primary в дедупе по ключу (session_group, gen_hash50).
- Per-tick дедуп через attention::deduplicate_generating оставляет true только у primary в группе.

### folder_id / folder_label: Option<String>
Папочная группировка для sidebar в формате __folder:<path>. Источник истины — cwd сессии (session.path). Вычисляется в resolve_folder() в main.rs (единственный сохранившийся хелпер из удалённого projects-блока).

### origin: String
Всегда 'local' для локально-сгенерированных DTO. Remote-сессии прокидываются через remote_proxy::enrich_with_origin, минуя SessionDto.

## Использование

Фронтендом sessions.js при polling GET /api/sessions каждые 3с. is_generating рендерится индикатором генерации, needs_attention — оранжевой подсветкой вкладки. Sidebar группировка идёт только по folder_id/folder_label (project-headers удалены в Phase 5).

## Файл

tmux-web/src/main.rs, struct SessionDto.
