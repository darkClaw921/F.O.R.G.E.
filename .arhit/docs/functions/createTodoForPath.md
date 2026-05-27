# createTodoForPath

createTodoForPath(path, title, description) — создаёт TODO-задачу в проекте по абсолютному пути (tmux-web/static/js/tasks/crud.js, экспортируется).

Назначение: добавить задачу в TODO нужного проекта, идентифицируемого абсолютным путём (git-корнем). Используется блоком «Предлагаемые задачи» в «Сводке дня» (renderSuggestions) для добавления выбранных LLM-предложений в TODO проекта.

Сигнатура: async function createTodoForPath(path, title, description). path — абсолютный путь проекта (project_path из предложения = ProjectActivity.path = git-корень); сервер сам делает resolve_root(path) для привязки к проекту. description опционально (пустая строка по умолчанию).

Поведение: POST /api/todos с JSON { path, title, description }. При не-2xx ответе бросает Error (текст тела или 'HTTP <код>'). При успехе возвращает r.json() (созданная задача).

Связи: вызывается из обработчика кнопки «Добавить выбранные в TODO» в renderSuggestions для каждой выбранной карточки; общий REST-эндпоинт POST /api/todos того же бэкенда что и TODO-канбан.
