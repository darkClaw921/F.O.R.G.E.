# user_settings

Модуль tmux-web/src/user_settings.rs — глобальные user-level настройки.

## Назначение
Хранит настройки уровня пользователя в ~/.forge/user_settings.json. В отличие от projects.json (per-data_dir) и todos.json (per-project), эти настройки применяются ко всем сессиям пользователя.

## Структуры

### UserSettings
Состав полей (все #[serde(default)] для backward compat — пропущенные поля берутся из Default):
- todo_default_plan_mode: bool (default false) — значение plan-mode-чекбокса по умолчанию при создании TODO
- todo_default_priority: u8 (default 2, clamp 0..=4) — приоритет новой TODO
- todo_default_issue_type: String (default 'task') — тип issue по умолчанию
- todo_plan_mode_suffix: String (default '') — текст для notify-сообщения при promote с plan_mode=true. Пустая строка → fallback к константе PLAN_MODE_SUFFIX в main.rs
- todo_confirm_delete: bool (default true) — спрашивать подтверждение при удалении TODO
- todo_confirm_promote_on_drag: bool (default false) — подтверждение при promote через drag-and-drop

КРИТИЧЕСКИЙ ИНВАРИАНТ: дефолты подобраны так, что при отсутствии user_settings.json поведение системы идентично состоянию до фичи.

### PatchUserSettingsReq
DTO с Option<T> для всех полей. Применяются только Some-варианты. Используется в PATCH /api/user-settings.

### UserSettingsStore
Cheap-clonable: Arc<RwLock<Inner>>. Один экземпляр на процесс в AppState.

API:
- new(path: PathBuf) -> Self — пытается прочитать файл. Если не существует или повреждён → UserSettings::default() + warn в tracing. Файл НЕ создаётся (lazy).
- get(&self) -> UserSettings — клон под read-lock
- patch(&self, payload) -> Result<UserSettings> — применяет Some-поля, priority.min(4), suffix без trim, atomic save, возвращает обновлённую копию

## Persistence
Atomic save: запись в <path>.tmp + std::fs::rename (POSIX-атомарность). create_dir_all для parent если не существует. Паттерн идентичен todos::save_locked.

## Юнит-тесты
- test_default_when_no_file: на несуществующий путь new() даёт Default, файл не создаётся
- test_create_patch_reload: patch({plan_mode:true, priority:3}) сохраняется и видим новым store на том же пути
- test_priority_clamp: priority=10 → итог 4
- test_suffix_not_trimmed: '  spaced  ' хранится как есть

## Зависимости
- anyhow для Result/Context
- serde / serde_json для (де)сериализации
- uuid (v4) — в тестах для генерации уникальных temp-путей
- tracing для логов
