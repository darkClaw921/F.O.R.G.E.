# UserSettings — TODO behavior feature

Глобальные пользовательские настройки уровня машины, управляющие поведением TODO-карточек в Tasks UI. Хранятся в ~/.forge/user_settings.json (один файл на пользователя — не привязан к data_dir или конкретному проекту).

## Контекст и мотивация

До этой фичи UI-поведение TODO-карточек было захардкожено:
- При создании TODO: plan_mode=false, priority=2, issue_type='task'.
- При удалении TODO: window.confirm() показывался всегда.
- При drag TODO→Open: promote происходил мгновенно, без confirm.
- При promote TODO с plan_mode=true: к notify добавлялась константа 'Создай план для этой задачи'.

Пользователи хотели:
- Настраивать дефолты создания TODO под свой workflow (например, priority=1 по умолчанию).
- Отключать confirm удаления, который замедляет работу.
- Включать confirm на drag (защита от случайного промоушна).
- Менять текст plan-mode-суффикса (например, на свой язык / другой стиль промптинга).

## Структура данных

UserSettings (tmux-web/src/user_settings.rs), 6 полей:

| Поле | Тип | Default | Эффект |
|------|-----|---------|--------|
| todo_default_plan_mode | bool | false | Чекбокс Plan Mode при создании TODO |
| todo_default_priority | u8 (0..=4) | 2 | Приоритет новой TODO |
| todo_default_issue_type | String | 'task' | Тип issue новой TODO |
| todo_plan_mode_suffix | String | '' | Текст plan-mode-суффикса (пустая → константа PLAN_MODE_SUFFIX) |
| todo_confirm_delete | bool | true | Confirm при удалении TODO |
| todo_confirm_promote_on_drag | bool | false | Confirm при drag TODO→Open |

Все поля помечены #[serde(default)] — гарантирует backward-compat с частичным/пустым JSON.

## Backend

### Модуль tmux-web/src/user_settings.rs

- UserSettings struct (Serialize/Deserialize, Clone).
- PatchUserSettingsReq (все поля Option<T>) — DTO для PATCH-эндпоинта.
- UserSettingsStore: Arc<RwLock<Inner>>.
  - new(path) — lazy load: при отсутствии файла создаёт store с UserSettings::default(); файл НЕ создаётся на диске.
  - get() -> UserSettings — клон под read-lock.
  - patch(payload) -> Result<UserSettings> — применяет Some-поля, clamp priority 0..=4, atomic save (tmp + rename), возвращает обновлённый снапшот.
- save_locked: атомарная запись через tmp + fs::rename (стратегия идентична todos::save_locked).
- 4 юнит-теста (test_default_when_no_file, test_create_patch_reload, test_priority_clamp, test_suffix_not_trimmed).

### REST endpoints (main.rs, P1.3)

- GET /api/user-settings → JSON UserSettings (текущий снапшот через store.get()).
- PATCH /api/user-settings, body = JSON PatchUserSettingsReq → обновлённый UserSettings; на 500 при IO-ошибке save.

### Интеграция в promote_todo (P1.4)

При плановой задаче (todo.plan_mode == true) при формировании notify-текста:
1. suffix = state.user_settings.get().todo_plan_mode_suffix.
2. Если suffix.trim().is_empty() — используется константа PLAN_MODE_SUFFIX = 'Создай план для этой задачи' (legacy).
3. Иначе — кастомный suffix.

Это гарантирует backward-compat: пустой/отсутствующий config → поведение 1:1 как до фичи.

### AppState (P1.2)

Добавлено поле user_settings: UserSettingsStore. Инициализируется в main() через UserSettingsStore::new(forge_dir.join('user_settings.json')).

## Frontend

### state.userSettings (P2.1)

Глобальное поле в tmux-web/static/js/core/state.js: null | UserSettings-object. null до первого успешного fetch.

### API-клиент tmux-web/static/js/settings/user-settings-api.js (P2.2)

- fetchUserSettings() — GET → state.userSettings. На ошибке: state.userSettings не меняется (null остаётся null), функция возвращает null.
- updateUserSettings(payload) — PATCH с optimistic update + rollback.
  1. prev = deepClone(state.userSettings).
  2. state.userSettings = merge(current, payload) до запроса.
  3. На !r.ok или throw: rollback к prev, throw Error.

### Preload в bootstrap.js (P2.3)

Best-effort preload через fetchUserSettings().catch(() => {}) — не блокирует UI.

### Settings UI: tab TODO behavior (P2.4 + P2.5)

- settings/todo-tab.js: buildTodoBehaviorForm(settings, onSaved) — фабрика fieldset с 6-ю контролами. Использует CSS-классы из notifications-tab (.notify-fieldset, .notify-field, .notify-hint, .notify-error, .notify-actions, .modal-check) — без правок styles.css.
- settings/modal.js: добавлен таб 'TODO behavior' с lazy-render паттерном (форма строится один раз при первом клике на таб).

### Tasks UI integration (Phase 3)

- openCreateModal (modals.js, P3.1): для status='todo' initial values plan_mode/priority/issue_type из state.userSettings с fallback к legacy-дефолтам.
- openTodoEditModal (modals.js, P3.2): Delete-кнопка показывает confirm только если state.userSettings.todo_confirm_delete !== false (default true).
- renderColumn / drop-handler (render.js, P3.3): drag TODO→Open показывает confirm если state.userSettings.todo_confirm_promote_on_drag === true (default false).

## Дефолты и инвариант legacy-поведения

КРИТИЧЕСКИЙ ИНВАРИАНТ: при отсутствующем или пустом ~/.forge/user_settings.json поведение системы побитово идентично состоянию ДО введения этой фичи.

Дефолты UserSettings::default спроектированы так, чтобы воспроизводить legacy:
- todo_default_plan_mode = false → создание TODO с выключенным Plan Mode (как было).
- todo_default_priority = 2 → P2 / medium (как было).
- todo_default_issue_type = 'task' → task (как было).
- todo_plan_mode_suffix = '' → fallback к константе PLAN_MODE_SUFFIX в promote_todo (как было).
- todo_confirm_delete = true → confirm всегда показывался (как было).
- todo_confirm_promote_on_drag = false → drop без confirm (как было).

## Backward compat

1. #[serde(default)] на всех полях UserSettings — миграция не нужна. Старый JSON с подмножеством полей грузится корректно.
2. Lazy file creation — отсутствие файла не отличается от UserSettings::default(). UserSettingsStore::new() читает файл только если он есть, без создания.
3. fetchUserSettings глотает ошибки и оставляет state.userSettings = null — fallback к локальным дефолтам в UI.
4. Frontend дефолты ВЕЗДЕ совпадают с backend Default — это инвариант проверки кода.

## Acceptance criteria

- [x] UserSettings struct с 6 полями + serde(default).
- [x] UserSettingsStore с new/get/patch и atomic save.
- [x] REST GET/PATCH /api/user-settings.
- [x] promote_todo читает suffix через store.get() с fallback.
- [x] state.userSettings поле в state.js.
- [x] fetchUserSettings + updateUserSettings (optimistic + rollback).
- [x] preload в bootstrap.
- [x] вкладка 'TODO behavior' в settings modal с 6 контролами.
- [x] openCreateModal использует state.userSettings для дефолтов TODO.
- [x] openTodoEditModal Delete учитывает todo_confirm_delete.
- [x] renderColumn drop TODO→Open учитывает todo_confirm_promote_on_drag.
- [x] Юнит-тесты UserSettingsStore (4 шт.).
- [x] cargo check + cargo test green.
- [x] node --check на всех изменённых JS green.
- [x] Regression sanity: ~/.forge/user_settings.json отсутствует → поведение 1:1 как до фичи.

## Связанные элементы

- tmux-web/src/user_settings.rs — backend модуль.
- tmux-web/static/js/settings/user-settings-api.js — API-клиент.
- tmux-web/static/js/settings/todo-tab.js — фабрика формы.
- tmux-web/static/js/settings/modal.js — settings modal (новый таб).
- tmux-web/static/js/tasks/modals.js — openCreateModal, openTodoEditModal.
- tmux-web/static/js/tasks/render.js — renderColumn / drop-handler.
- tmux-web/static/js/core/state.js — state.userSettings.
- tmux-web/static/js/core/bootstrap.js — preload.