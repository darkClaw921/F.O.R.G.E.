# main::resolve_folder

Helper в tmux-web/src/main.rs (примерно строки 1396-1407, рядом с resolve_project), резолвящий папочно-ориентированную группу для активной tmux-сессии.

Сигнатура: fn resolve_folder(s: &tmux::SessionInfo) -> (Option<String>, Option<String>) — приватная (НЕ pub).

Возвращает кортеж (folder_id, folder_label):
- folder_id — стабильный ключ группы вида '__folder:<absolute_path>'. Префикс '__folder:' гарантирует отсутствие коллизий с project_id (формы registered-uuid, '__path__:<cwd>', tmux-префикс), используемыми в switchActiveProject и фильтрах TODO/.beads.
- folder_label — basename последней папки session.path для отображения в заголовке группы sidebar.

Edge cases:
- Пустой session.path или path = '/' → Path::file_name() даёт None → возвращается (None, None).
- file_name есть, но to_str() возвращает None (не-UTF8) → (None, None).
- Пустая строка basename → (None, None).
- Orphan-сессии (None) фронт отрисует через ORPHAN_KEY-ветку sidebar.

Отличие от resolve_project: не учитывает зарегистрированные проекты и tmux_prefix — чисто файловая группировка для UI, независимая от семантики project_id. Это позволяет иметь две различные классификации одной и той же сессии: проектную (для бизнес-логики переключения проекта и .beads-scoping) и папочную (для визуальной группировки в sidebar).

Вызывается: из get_sessions (main.rs, ~810) для каждой SessionInfo, результат кладётся в SessionDto.folder_id / SessionDto.folder_label.

Сериализация: фронт всегда видит ключи folder_id / folder_label в JSON ответа /api/sessions (без skip_serializing_if; null при None).

Добавлен в Phase 1 epic forge-fl3t (P1.2 = forge-t3aw).
