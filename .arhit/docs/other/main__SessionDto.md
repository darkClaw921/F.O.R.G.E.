# main::SessionDto

DTO в src/main.rs (~line 901), сериализуемый в JSON для GET /api/sessions. Структура: name, attached, windows и пр. из tmux::SessionInfo через #[serde(flatten)] + needs_attention: bool + is_generating: bool + project_id/project_name: Option<String> + folder_id/folder_label: Option<String> + origin: String.

Поле needs_attention заполняется из snapshot'а AppState.attention в хендлере: attention.get(&s.name).copied().unwrap_or(false). Snapshot снимается ОДИН РАЗ за вызов хендлера, чтобы все сессии в ответе видели согласованное состояние watcher'а.

Поле is_generating заполняется из generating_snapshot() аналогично. Семантика: true когда за прошедший 1.5с тик watcher'а содержимое последних 30 строк pane изменилось (Claude печатает, идёт tool call, или любая другая активность типа htop/tail -f — фолз-позитивы приняты). Independent от needs_attention: оба флага могут гореть одновременно.

project_id/project_name — id и имя проекта-владельца сессии через projects::session_belongs или None для orphan. folder_id/folder_label — папочная группировка для sidebar (__folder:<path>). origin — всегда 'local' для локально-сгенерированных DTO; remote-сессии прокидываются через remote_proxy::enrich_with_origin, минуя SessionDto.

Используется фронтендом sessions.js при polling /api/sessions каждые 3с. Файл: src/main.rs.
