# tmux-web/static/js/core/dom.js

DOM-references singletons для tmux-web frontend (Phase 0 ES Modules refactor).

## Назначение
1:1 копия блока top-level `const $... = document.getElementById(...)` из IIFE `tmux-web/static/app.js` (строки 173-233, 674). Каждый ref вычисляется один раз при первом импорте модуля и экспортируется как named export.

## Контракт корректности
DOM-refs валидны только если модуль импортируется ПОСЛЕ полной загрузки HTML. В Phase 1 main.js будет подключён как `<script type="module">` — это даёт implicit defer, т.е. модуль выполнится после parse HTML (по HTML-спецификации). До этого ref'ы будут null.

## Экспорты (по группам)

### Layout / sidebar
- $layout (#layout), $btnSidebarToggle (#btn-sidebar-toggle), $sidebar (#session-list), $btnNew (#btn-new), $sidebarOverlay (#sidebar-overlay).

### Terminal / window-bar / status
- $terminalEl (#terminal), $placeholder (#placeholder), $windowBar (#window-bar), $windowTabs (#window-tabs), $windowNewBtn (#window-new), $statusDot (#status-dot), $statusText (#status-text).

### Tasks UI (Phase 6.A)
- $tabTerminal (#tab-terminal), $tabTasks (#tab-tasks), $tasksStatus, $tasksEl (#tasks), $tasksReload, $tasksNew, $tasksMeta, $tasksBoard.

### Git / lazygit tab
- $tabGit (#tab-git), $gitEl (#git), $gitTermEl (#git-term), $gitPlaceholder, $gitError, $gitErrorText, $gitErrorRetry, $gitErrorClose, $gitInstallHelp, $gitInstallList.

### Docker / lazydocker tab
- $tabDocker (#tab-docker), $dockerEl (#docker), $dockerTermEl, $dockerPlaceholder, $dockerError, $dockerErrorText, $dockerErrorRetry, $dockerErrorClose, $dockerInstallHelp, $dockerInstallList.

### Telescope / tv tab
- $tabTelescope (#tab-telescope), $telescopeEl (#telescope), $telescopeTermEl, $telescopePlaceholder, $telescopeError, $telescopeErrorText, $telescopeErrorRetry, $telescopeErrorClose, $telescopeInstallHelp, $telescopeInstallList, $telescopeChannelBar.

### Project bar (Phase 6.B)
- $projectSelect (#project-select), $projectNew (#project-new), $projectSettings (#project-settings).

### Origin-табы (Phase 5)
- $originTabs (#origin-tabs) — скрыты при remote_mode=false.

## Зависимости
НЕТ — только браузерный DOM. Pure leaf-module.

## Ограничения
- Имена экспортов сохраняют исторические $-имена из app.js (один-в-один).
- В Phase 0 модуль ещё не подключен; legacy app.js работает как раньше.
- НЕ кэшировать элементы, которые создаются динамически (например $panel внутри settings modal — он остаётся локальной переменной в feature-модуле).
