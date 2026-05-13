# switchTheme

Переключение активной темы на сервере без релоада страницы (tmux-web/static/app.js, Phase 3 wk7).

## Что делает
async switchTheme(id) — атомарная операция смены активной темы:
1. PATCH /api/themes/active с body {id} (Content-Type: application/json) — сервер сохраняет новый active в themes.json.
2. GET /api/themes/active — получает полный Theme (нужна term-секция для xterm).
3. applyTheme(theme) — применяет CSS + xterm runtime + сохраняет в state.activeTheme.

## Параметры
id: string — id темы. Может быть пресет ('default', 'dracula', 'nord', ...) или uuid кастомной темы.

## Обработка ошибок
- Если PATCH вернул не-2xx — window.alert('Failed to switch theme: ' + body|status). Тема не применяется.
- Если GET вернул не-2xx — window.alert. Тема не применяется (на сервере active уже сменился, при следующей загрузке подхватится).
- При network-сбое (catch) — window.alert(e.message).

window.alert используется как стандартный механизм нотификаций в этом app.js (см. createSessionPrompt, promoteTodo и др.).

## Связанные
- applyTheme — финальный шаг применения.
- /api/themes/active (PATCH) — themes.rs::patch_active_theme.
- /api/themes/active (GET) — themes.rs::get_active_theme.
- (будущее, Phase 4) — будет вызываться из вкладки Themes в settings modal при клике на карточку пресета или custom темы.

## Бизнес-логика
Без релоада: изменения видны сразу же. PATCH+GET — отдельные запросы, поскольку PATCH возвращает только {active: id}, а нам нужен полный Theme для applyTheme; решение допустимо т.к. /api/themes/active — read-lock, дешёвый.
