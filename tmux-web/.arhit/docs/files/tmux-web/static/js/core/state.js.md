# tmux-web/static/js/core/state.js

Глобальный singleton state — обычный JS-объект (не reactive), мутируется напрямую модулями приложения. Импортируется как именованный экспорт: import { state } from '../core/state.js'.

# Ключевые поля

## Терминал
- ws, encoder, lastResizeKey: WebSocket клиент PTY, anti-loop logic.

## Sessions / Sidebar
- sessions, currentSession, activeProjectId, projects: массивы и id'шники для tmux-сессий и проектов.
- projectFilter: '__all__' или конкретный project.id — UI-фильтр сайдбара (cross-project visibility).

## Tasks
- activeTab: 'terminal' | 'tasks' | 'git'.
- tasksPollTimer, tasksData: poll handle + последний JSON snapshot.

## User Settings (Phase 2 — TODO behavior)
- userSettings: null | object.
  - Кэш пользовательских настроек, загружается через GET /api/user-settings на bootstrap (best-effort) и обновляется через PATCH в settings/user-settings-api.js.
  - null до первого успешного fetch; при ошибке остаётся null.
  - Tasks UI ДОЛЖЕН проверять на null и применять локальные дефолты — это критично для инварианта legacy-поведения.
  - Структура: { todo_default_plan_mode, todo_default_priority, todo_default_issue_type, todo_plan_mode_suffix, todo_confirm_delete, todo_confirm_promote_on_drag }.

## Прочее
- attention, telescopeTerm, dockerTerm, gitWs и др. — см. соответствующие модули.

# Соглашения

- Прямая мутация (state.foo = bar) — единственный способ изменения. Никаких setters / proxy.
- Не сериализовать целиком — большая часть полей (WS, timers, term-instances) непригодна для JSON.
- При добавлении поля — комментировать назначение и инициализацию.

# Файл

tmux-web/static/js/core/state.js.
