# tmux-web/static/js/settings/modal.js

Settings modal — родительский модальный диалог с вкладками: Notifications, Themes, TODO behavior (Phase 2), Remotes (опционально).

# Вкладки

1. Notifications — список проектов, per-project notify-конфиг (delay, wait_previous, template). Используется по умолчанию (active при открытии).
2. Themes — список доступных тем + апply (см. styles.css / themes).
3. TODO behavior (Phase 2) — форма пользовательских настроек поведения TODO. Содержит fieldset из todo-tab.js (buildTodoBehaviorForm), 6 контролов.
4. Remotes (опционально) — управление remote-серверами.

# Phase 2 (новое): TODO behavior tab

Добавлен новый таб-button с data-tab='todo' (label 'TODO behavior') и панель #ps-panel-todo с контейнером #ps-todo-content. Импорты:
- buildTodoBehaviorForm из './todo-tab.js' — фабрика формы.
- fetchUserSettings из './user-settings-api.js' — lazy preload (если bootstrap не успел).

# Lazy-render паттерн

todoState.loaded — boolean флаг, чтобы рендер формы происходил один раз при первом клике на таб 'TODO behavior'. Это экономит и DOM-узлы, и время fetch при открытии модалки на другой вкладке.

Алгоритм renderTodoPanel:
1. Если уже loaded — выход.
2. todoState.loaded = true.
3. Показать 'Loading settings…' placeholder в контейнере.
4. Если state.userSettings === null — попробовать fetchUserSettings() (с защитой от throw — функция сама глотает ошибки).
5. Очистить контейнер.
6. settingsArg = state.userSettings || {} — если backend down, передаём пустой объект (buildTodoBehaviorForm подставит дефолты).
7. Вставить результат buildTodoBehaviorForm(settingsArg, onSaved). onSaved сохраняет updated в state.userSettings.

# Связи

- todo-tab.js: buildTodoBehaviorForm — фабрика.
- user-settings-api.js: fetchUserSettings — preload.
- state.js: state.userSettings — кэш.

# Файл

tmux-web/static/js/settings/modal.js.
