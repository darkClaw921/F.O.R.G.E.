# createTuiTab

Generic-factory для xterm-вкладок tmux-web frontend, говорящих по WebSocket с PTY на бэкенде (lazygit / lazydocker / telescope). Расположен в tmux-web/static/app.js (≈строка 1512).

## Назначение

Унифицирует код всех TUI-табов: вместо трёх копий mountXxxTerm/connectXxxWs/closeXxxWs/xxxSwitchCwd/showXxxBanner — одна функция, конфигурируемая опциями. Phase 2 рефакторинг: lazygit-tab мигрирован на эту factory, lazydocker и telescope построены поверх неё.

## Сигнатура

createTuiTab(opts) → tabState

opts:
- name (string) — имя для логов ('lazygit'|'lazydocker'|'telescope').
- wsPath (string) — путь WS endpoint ('/ws/lazygit'|'/ws/lazydocker'|'/ws/telescope').
- activeTabName (string) — значение state.activeTab для этого таба ('git'|'docker'|'telescope'). Используется в ResizeObserver/window resize, чтобы fit вызывался только когда вкладка реально активна.
- refs (object) — DOM-ссылки таба:
  - termEl — контейнер xterm.js.
  - placeholderEl — текст-заглушка 'Select a project to open <tui>', виден когда нет активного проекта.
  - errorEl, errorTextEl — banner ошибки и его текстовый узел.
  - retryBtn, closeBtn — кнопки в banner (Retry / × dismiss).
  - installHelpEl, installListEl — install-help блок (показывается при binary-not-found).
- installHelp (object|null):
  - binary (string) — имя бинаря (для эвристики 'not found': если message содержит binary + 'not found'/'no such file' → показываем install-help).
  - notFoundMsg (string) — текст в banner вместо raw error.
  - entries (Array<{id,label,cmd}>) — список команд установки per-OS.

## Возвращаемое значение (tabState)

Объект, который пишется в state.gitTerm/dockerTerm/telescopeTerm. Поля:
- term, fit — инстанции xterm.js Terminal и FitAddon.
- ws — текущий WebSocket (null если закрыт).
- mounted (bool) — флаг, чтобы mount() не пере-открывал терминал.
- currentCwd (string|null) — какой cwd сейчас передан.
- errorSticky (bool) — banner показан и не должен быть стёрт обычным onclose (например, binary-not-found).
- resizeObserver (ResizeObserver|null) — наблюдатель за termEl.
- name, activeTabName — переданные опции (для интроспекции).

Методы (привязаны как поля tabState):
- mount() → Terminal|null — ленивая инициализация xterm.js (Terminal + FitAddon + term.open + onData/onResize + ResizeObserver + window resize-listener). Идемпотентна.
- connect(cwd) — открыть WS на wsPath?cwd=...&cols=...&rows=...[&server=<origin>]. binaryType='arraybuffer'. На onmessage Binary → term.write; Text JSON {type:'error'} → showBanner (+ install-help при notFound). На onclose с кодом ≠ 1000/1001 показывает 'Connection lost'. ?server=<origin> добавляется только в remote-mode (когда isRemoteMode() и state.activeOrigin не 'local'/'all').
- close(reason) — закрыть WS с code=1000, обнулить tabState.ws.
- switchCwd(newCwd) — если WS открыт: term.clear() + ws.send({type:'switch_cwd',cwd}). Если закрыт — fallback на connect(newCwd). При ошибке send'а делает close+connect.
- showBanner(message, {showInstall}) — раскрыть error-banner, опционально показать install-help.
- hideBanner() — скрыть banner (и установить errorSticky=false).
- retry() — hideBanner+close+currentCwd=null+openForActiveProject — обработчик retryBtn.
- openForActiveProject() — основная точка входа из switchTab/project-change: getActiveProject → mount → fit → connect.

## Контракт WebSocket

Идентичен для lazygit/lazydocker/telescope (общий handle_tui_socket на бэкенде):
- query: ?cwd=<path>&cols=<n>&rows=<n>[&server=<id>]
- input (frontend → backend): raw bytes (Uint8Array) → Binary frame.
- control (frontend → backend, Text JSON):
  - {type:'resize', cols, rows}
  - {type:'switch_cwd', cwd}
- output (backend → frontend):
  - Binary frame = pty stdout → term.write.
  - Text JSON {type:'error', message} → showBanner, errorSticky=true.

## install-help механизм

При получении Text frame {type:'error',message} проверяется эвристика not-found: message.toLowerCase() содержит installHelp.binary и одну из строк 'not found' / 'no such file'. Если совпадает — message заменяется на installHelp.notFoundMsg, и в banner раскрывается installHelpEl со списком команд установки. detectClientOS() (navigator.platform + userAgent) определяет текущую ОС, и подходящие entries сортируются наверх. Каждая команда снабжена кнопкой Copy (Clipboard API → fallback textarea+execCommand).

## Привязка к state

После init createTuiTab — tabState ложится в state.gitTerm/dockerTerm/telescopeTerm. Старый код, обращающийся к state.gitTerm.term/ws/mounted, продолжает работать без изменений: tabState содержит те же поля, что и pre-refactor.

## Зависимости

- xterm.js (window.Terminal) + FitAddon (window.FitAddon.FitAddon) — CDN/embedded.
- state (encoder/activeTheme/activeTab/activeOrigin/projects/activeProjectId).
- mapTermTheme(activeTheme) — для согласованной темы с основным терминалом.
- getActiveProject() — для openForActiveProject().
- isRemoteMode() — для добавления &server=<origin> к URL.
- detectClientOS, copyToClipboardSafe — utility-функции рядом в app.js.
