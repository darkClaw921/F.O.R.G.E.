# tmux-web/static/js/core/state.js

Singleton state для tmux-web frontend (Phase 0 ES Modules refactor).

## Назначение
1:1 копия объекта `state` из IIFE `tmux-web/static/app.js` (строки ~89-171). Экспортируется как named export `state` и используется как shared-singleton всеми feature-модулями через `import { state } from '../core/state.js'`. Семантика IIFE-замыкания эмулируется shared ES-модулем — один импорт даёт один и тот же объект во всех модулях, обеспечивая прозрачную миграцию без правок логики.

## Экспорты
- `export const state` — объект со всеми runtime-полями приложения.

## Зависимости
НЕТ — нижний слой архитектуры. Не импортирует ни одного модуля.

## Ключевые поля state
### Phase 5 — Remote mode
- `remoteMode: bool` — режим запуска бэкенда, из /healthz. Default false (legacy localhost).
- `serverVersion: string|null` — версия из /healthz.version.
- `healthzLoaded: bool` — true после первого /healthz.
- `remoteServers: Array` — список RemoteServerView { id, label, url }.
- `remoteProjects: Map<server_id, ProjectDto[]>` — lazy-load кэш.
- `remoteSessions: Map<server_id, SessionDto[]>` — lazy-load кэш.
- `activeOrigin: 'all'|'local'|server_id` — фильтр sidebar, в localStorage.

### Терминал / WS-attach
- `term, fitAddon, webLinksAddon, ws` — xterm.js handles.
- `attachWsBackoffStep, attachWsReconnectTimer, attachWsClosedByUs, attachWsOrigin` — Phase 7 auto-reconnect.
- `currentSession, sessions, currentWindows, windowsPollTimer, pollTimer` — sessions/windows.
- `encoder: TextEncoder` — pre-allocated для PTY-input.
- `lastResizeKey` — anti-loop защита от resize-петли.

### Phase 6 — Tasks / TODO
- `activeTab: 'terminal'|'tasks'|'git'` — активный таб.
- `tasksPollTimer, tasksData, projects, activeProjectId, projectFilter`.
- `tasksWs, tasksWsBackoffStep, tasksWsReconnectTimer, tasksWsClosedByUs` — Phase 6.D realtime.
- `todosData, todosWs, todosWsBackoffStep, todosWsReconnectTimer, todosWsClosedByUs, todosPollTimer` — Phase 4 TODO kanban.

### Phase 3 — Themes
- `activeTheme: {id, name, kind, ui, term}|null` — последняя применённая тема.

### TUI-tabs
- `gitTerm, dockerTerm, telescopeTerm: TuiTab.state|null` — инстансы lazygit/lazydocker/telescope.

## Ограничения / инварианты
- Поля переименовывать нельзя — на них завязан весь legacy app.js и публичный контракт window.ForgeApp.state.
- В Phase 0 модуль ещё не импортируется (index.html не меняется), но файл готов к импорту из main.js в Phase 1.
- Поля изменяются in-place — мутации видны во всех потребителях немедленно.

## Связи
- Источник: `tmux-web/static/app.js` строки 89-171 (внутри IIFE).
- Импортируется в: будущие модули core/*, ws/*, sessions/*, sidebar/*, tabs/*, tasks/*, projects/*, settings/*, themes/*, remote/* (Phase 1).
