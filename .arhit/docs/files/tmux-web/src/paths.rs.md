# tmux-web/src/paths.rs

Резолвинг 'корня' по cwd. Phase 1 cwd-only архитектуры (план remove-projects-concept.md): функция resolve_root(cwd: &Path) -> PathBuf поднимается по ancestors() в поиске первой папки с маркером (приоритет .beads/, затем .git/, иначе сам cwd). Используется TodoStore для группировки карточек по 'корням'; в Phase 2 будет вызываться REST/WS хендлерами при ?path= параметре, в Phase 3 — notifier'ом. Не делает canonicalize (не паникует на отсутствующих путях, не разворачивает /tmp на macOS — это упрощает тесты).
