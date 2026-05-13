# renderGitToolbar

Функция в static/app.js (Phase 4, ~строка 1258). Рендерит верхний toolbar внутри #git-pane: имя ветки, ahead/behind chip, upstream-метку.

Логика:
- if (!state.gitStatus) return — первый paint до fetch, toolbar остаётся пустым.
- if (!s.repo) — показывает плейсхолдер 'Не git-репозиторий' в $gitBranch, очищает $gitAheadBehind и $gitMeta. (renderGitFiles отдельно очистит файлы.)
- Branch name: если s.branch есть — текст = s.branch; иначе detached HEAD → '(detached) ' + abbrev (первые 7 символов s.head, или '?' для пустого репо).
- Ahead/behind chip: показывается только если s.upstream И (s.ahead || s.behind) > 0. Формат: '↑5 ↓2'. Без upstream chip всегда пуст.
- $gitMeta: текст s.upstream или ''.

DOM-элементы кешируются как module-level $gitBranch/$gitAheadBehind/$gitMeta при init. Вызывается из fetchGitStatus после успешного fetch.

См. также: fetchGitStatus, renderGitFiles.
