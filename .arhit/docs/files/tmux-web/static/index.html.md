# tmux-web/static/index.html

HTML-шаблон tmux-web frontend. Phase 4: git-pane перестроен под xterm.js терминал, подключённый к /ws/lazygit. Содержит:
- #git-placeholder — текст-заглушка 'Select a project to open lazygit', виден когда нет активного проекта
- #git-error — banner-плашка с текстом ошибки, кнопками Retry и × (dismiss). Hidden по умолчанию
- #git-term — контейнер для второй инстанции xterm.js Terminal (отдельный от основного #terminal), к которому подключается /ws/lazygit
- #git-legacy — обёртка над старой git-разметкой (toolbar/files/commit/graph), оставлена hidden до Phase 5 cleanup. Нужна чтобы legacy DOM-refs ($gitBranch, $gitCommitMsg, $gitCanvas и т.д.) не были null при загрузке app.js.
