# main::AppState

DEPRECATED entry (исторический snapshot Phase 6.A/Phase 7). Актуальная документация структуры — см. tmux-web/src/main.rs::AppState. После remove-projects-concept (Phase 4) поле projects: Arc<RwLock<ProjectStore>> удалено; AppState больше не хранит реестр проектов. См. paths::resolve_root для cwd-only архитектуры.
