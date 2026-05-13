# static/index.html#git-pane

Контейнер вкладки Git: <div id='git' hidden> в static/index.html (строки 54-79). Скрыт по умолчанию (атрибут hidden), активируется JS через switchTab() (Phase 4).

Структура:
- #git-toolbar (строки 55-60): кнопка #git-reload (↻ перезагрузить), #git-branch (имя текущей ветки), #git-ahead-behind (счётчик коммитов впереди/позади upstream), #git-meta (резервный span для дополнительной информации).
- #git-body (строки 61-78): grid 3-pane layout (1fr 1fr 1.4fr) с тремя секциями.

Секции:
1. #git-files-pane (строки 62-67): два списка файлов — #git-staged-list (заголовок 'Изменения', staged-файлы) и #git-unstaged-list ('Не отслежено / изменено', unstaged + untracked). Заполняется renderGitFiles (Phase 4 tw-lkn) с чекбоксами и .git-badge.
2. #git-commit-pane (строки 68-73): форма коммита — textarea #git-commit-msg (rows=6, plain plaintext, плейсхолдер 'Сообщение коммита (первая строка — subject)'), button #git-commit-btn (class='primary' disabled по умолчанию), p#git-commit-error (class='error' hidden, отображает ошибки commit/stage).
3. #git-graph-pane (строки 74-77): canvas #git-graph-canvas для рендеринга графа коммитов (Phase 5 tw-eez renderGitGraph).

Phase 3 — tw-ezf. JS-логика — Phase 4-5.
