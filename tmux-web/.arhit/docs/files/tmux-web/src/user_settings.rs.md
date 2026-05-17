# tmux-web/src/user_settings.rs

Модуль глобальных пользовательских настроек уровня машины. Хранилище: один файл ~/.forge/user_settings.json (отличается от projects.json/themes.json per-data_dir и todos.json per-project — настройки общие для всех проектов пользователя).

# Структура UserSettings

6 полей, все помечены #[serde(default)] — гарантия backward-compat (старый/пустой/неполный JSON парсится, недостающие поля берутся из Default impl):
- todo_default_plan_mode: bool, default false. Чекбокс Plan Mode по умолчанию при создании TODO.
- todo_default_priority: u8, default 2 (medium). Приоритет новой TODO. Клампится в 0..=4 при patch (MAX_PRIORITY=4).
- todo_default_issue_type: String, default 'task'. Тип issue по умолчанию.
- todo_plan_mode_suffix: String, default '' (пустая строка). Текст, добавляемый к сообщению при promote TODO с plan_mode=true. Пустая строка → используется константа PLAN_MODE_SUFFIX из main.rs (legacy fallback, см. promote_todo).
- todo_confirm_delete: bool, default true. UI-флаг: спрашивать ли confirm при удалении TODO (backend хранит, frontend применяет).
- todo_confirm_promote_on_drag: bool, default false. UI-флаг: confirm при drag TODO→Open.

# UserSettingsStore API

Cheap-clonable обёртка Arc<RwLock<Inner>>, один экземпляр на процесс — кладётся в AppState (см. main.rs).

Методы:
- new(path: PathBuf) -> Self. Lazy-load: пытается прочитать файл, при ошибке парсинга/чтения логирует warning через tracing и возвращает Default. Файл НЕ создаётся, если его нет — критично для инварианта 'нулевая конфигурация = поведение как до фичи'.
- get(&self) -> UserSettings. Клон текущего состояния под read-lock.
- patch(&self, payload: PatchUserSettingsReq) -> Result<UserSettings>. Применяет только поля Some(..), валидирует priority (clamp 0..=4), вызывает save_locked (atomic) и возвращает обновлённый снапшот.

PatchUserSettingsReq — DTO с Option<T>-полями, позволяет клиенту менять одно поле без отправки полного объекта (REST PATCH semantics).

# Atomic save pattern

save_locked() пишет в <path>.tmp, затем fs::rename поверх. На POSIX rename атомарен в рамках одного mount-point — при kill -9 получим либо старое, либо новое состояние, но никогда битый JSON. Стратегия идентична todos::save_locked и projects::ProjectStore::save.

create_dir_all для parent — гарантирует создание ~/.forge/ при первом patch.

# Backward compat

1. #[serde(default)] на всех полях UserSettings — миграция не нужна никогда.
2. Lazy file creation — отсутствие файла не отличается от пустого UserSettings::default().
3. Warning в tracing вместо паники при битом JSON — пользователь увидит проблему по логам, но фронт продолжит работать с дефолтами.

# Юнит-тесты

4 теста (test_default_when_no_file, test_create_patch_reload, test_priority_clamp, test_suffix_not_trimmed) покрывают:
- Чтение без файла → defaults, файл не создаётся.
- Patch + reload через новый Store на тот же путь — изменения видны.
- Clamp priority > 4 → 4 (на диске тоже).
- Suffix сохраняется as-is (без trim) — клиент сам решает.

Тестовые пути генерируются через uuid::Uuid::new_v4() в std::env::temp_dir().

# Связи

- AppState: поле user_settings: UserSettingsStore (см. main.rs, интеграция P1.2).
- REST: handlers GET/PATCH /api/user-settings (см. main.rs, P1.3).
- promote_todo: читает user_settings.todo_plan_mode_suffix как fallback к PLAN_MODE_SUFFIX (P1.4).
- Frontend: state.userSettings подтягивается через GET в bootstrap.js (preload), редактируется через todo-tab.js (PATCH).

Файл: tmux-web/src/user_settings.rs (350 строк, ~15KB).
