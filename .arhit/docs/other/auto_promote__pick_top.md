# auto_promote::pick_top

Чистая функция выбора верхней карточки канбана (tmux-web/src/auto_promote.rs). Сигнатура: pub fn pick_top(mut todos: Vec<Todo>) -> Option<Todo>. Без async/IO — вынесена ради тестируемости барьерной логики воркера auto_promote::run.

Назначение: вернуть карточку, которая окажется ПЕРВОЙ в колонке UI после сортировки compareIssues (static/js/tasks/render.js стр.72-80). Повторяет фронтенд один-в-один: priority ASC (u8, меньше = выше, идёт первым), при равном priority — updated_at DESC (строки RFC3339 сравниваются лексикографически, новее = больше -> DESC значит больший updated_at первым). Реализация: todos.sort_by(|a,b| a.priority.cmp(&b.priority).then_with(|| b.updated_at.cmp(&a.updated_at))); first().

Возврат: Some(top) — клон верхней карточки, либо None если список пуст.

ВАЖНО: фильтрации по auto_promote здесь НЕТ. Барьер цепочки (стоп если у верхней карточки auto_promote==false) проверяется в handle_closed уже ПОСЛЕ выбора top — сознательно, чтобы цепочка реагировала на текущую верхнюю карточку, а не перепрыгивала через неё к следующей флагнутой.

Тесты (auto_promote::tests): pick_top_empty_is_none, pick_top_orders_by_priority_asc, pick_top_breaks_ties_by_updated_at_desc, pick_top_priority_dominates_updated_at, pick_top_ignores_auto_promote_flag_in_selection.

Связанные: auto_promote::run/handle_closed (caller), todos::Todo, frontend compareIssues.
