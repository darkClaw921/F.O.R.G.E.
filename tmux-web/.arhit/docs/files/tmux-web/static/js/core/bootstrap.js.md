# tmux-web/static/js/core/bootstrap.js

Точка входа фронтенда: инициализирует все подсистемы при загрузке страницы. Импортируется из main.js (script type=module).

# Phase 2 (новое): preload state.userSettings

Добавлен импорт fetchUserSettings из '../settings/user-settings-api.js'.

В bootstrap-фазе после fetchProjects() запускается best-effort preload:

  try {
      fetchUserSettings().catch(() => {});
  } catch (_) { /* never reached, defensive */ }

Особенности:
1. Не await — preload идёт параллельно с другими fetch и не блокирует первый paint.
2. .catch(() => {}) — игнорируем сетевые ошибки и HTTP non-2xx. fetchUserSettings сама glotает и возвращает null, но дополнительный catch — защита от unhandled rejection.
3. try/catch снаружи — defensive, на случай если fetchUserSettings бросит синхронно (теоретически невозможно).

При успехе state.userSettings заполняется к моменту, когда пользователь открывает Tasks UI или Settings modal. При ошибке state.userSettings остаётся null — Tasks UI применит дефолты (см. openCreateModal/openTodoEditModal/renderColumn).

# Полный workflow bootstrap

1. fetchProjects() (await).
2. .finally → fetchSessions, startPolling, connectTasksWs, fetchTodos, connectTodosWs.
3. fetchUserSettings() (fire-and-forget).
4. window beforeunload → stopPolling, stop*Polling, disconnect*Ws, term.close.

# Связи

- ../settings/user-settings-api.js: fetchUserSettings.
- state.js: state.userSettings.
- остальные модули: см. полный список импортов в файле.

# Файл

tmux-web/static/js/core/bootstrap.js.
