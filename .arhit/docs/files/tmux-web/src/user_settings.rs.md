# tmux-web/src/user_settings.rs

UserSettings — глобальные настройки уровня пользователя. Файл ~/.forge/user_settings.json, роут GET/PATCH /api/user-settings (main.rs), AppState.user_settings: UserSettingsStore.

## Хранилище

UserSettingsStore — Arc<RwLock<Inner>>, cheap-clonable, один экземпляр на процесс. get() — клон под read-lock. patch(PatchUserSettingsReq) — применяет только Some(..)-поля, затем atomic save (запись в <path>.tmp + fs::rename; на POSIX атомарен в рамках mount-point). Файл НЕ создаётся при чтении — только при первом успешном patch (lazy creation). Битый JSON → warning в tracing + дефолты, работа не блокируется.

## Поля

todo_default_plan_mode (false), todo_default_priority (2, clamp 0..=4), todo_default_issue_type ('task'), todo_plan_mode_suffix (''), todo_confirm_delete (true), todo_confirm_promote_on_drag (false), echo_default_model (None; пустая строка в patch → сброс в None), echo_notifications_enabled (true), cmd_hints_enabled (**false**), next_step_enabled (**false**).

Все поля #[serde(default)] — частичный/пустой/старый файл валиден без миграции.

## Инвариант «нулевая конфигурация» и его намеренное сужение

Изначально (epic tw-z6l): при полном отсутствии файла поведение системы побитово идентично состоянию «до фичи user-settings». Для всех todo_* и echo_* полей инвариант в силе.

cmd_hints_enabled и next_step_enabled его НАМЕРЕННО нарушают: обе фичи раньше были жёстко всегда включены, теперь при нулевой конфигурации выключены. Прямое требование пользователя — фичи заметные (перехват удержания ⌘) и дорогие (next_step дёргает Claude CLI), поэтому opt-in. Дефолты false — не забытая default-функция: НЕ «чините» их обратно на true. Зафиксировано тестом test_interface_flags_default_off. См. [[interface-settings-toggles]].

## Потребители

- backend: promote_todo читает todo_plan_mode_suffix; echo_host.rs::EchoHostAdapter::next_step_enabled читает next_step_enabled (гейт воркера Echo).
- frontend: кеш в state.userSettings (core/state.js, null пока не загружено), клиент settings/user-settings-api.js (fetchUserSettings / updateUserSettings с optimistic update + rollback). Шины событий нет — консьюмеры читают лениво в точке использования. hotkeys.js (классический IIFE) читает cmd_hints_enabled через window.ForgeApp.state.

## Тесты

test_default_when_no_file (field-agnostic, сравнивает с UserSettings::default()), test_create_patch_reload, test_priority_clamp, test_echo_defaults_present, test_echo_patch_set_and_clear_model, test_legacy_settings_file_loads_with_echo_defaults (старый файл без новых полей → дефолты), test_suffix_not_trimmed, test_interface_flags_default_off, test_interface_flags_patch_and_persist.
