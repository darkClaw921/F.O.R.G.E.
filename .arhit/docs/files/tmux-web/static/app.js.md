# tmux-web/static/app.js

Frontend tmux-web (xterm + WS-attach + tasks/git/projects). Phase 4 добавил отдельную инстанцию xterm.js для git-таба, подключённую к /ws/lazygit:

Ключевые элементы Phase 4:
- state.gitTerm = { term, fit, ws, mounted, currentCwd, errorSticky } — изолированный xterm-контекст git-таба
- DOM-refs: $gitTermEl (#git-term), $gitPlaceholder (#git-placeholder), $gitError (#git-error), $gitErrorText, $gitErrorRetry, $gitErrorClose
- mountGitTerm() — ленивая инициализация Terminal+FitAddon в #git-term (опции совпадают с основным term, тема из state.activeTheme через mapTermTheme)
- openLazygitForActiveProject() — точка входа: проверяет наличие активного проекта (getActiveProject), показывает/скрывает placeholder/term, дергает mountGitTerm и connectGitWs
- connectGitWs(cwd) — открывает WS ws://host/ws/lazygit?cwd=<encoded>&cols=&rows=, binaryType=arraybuffer
  - onmessage: ArrayBuffer → term.write(Uint8Array); string → JSON.parse, при type=error показывает banner (lazygit-not-found распознаётся и заменяется подсказкой про brew install)
  - term.onData → ws.send(encoder.encode(data)) как Binary
  - term.onResize → ws.send JSON {type:'resize',cols,rows}
  - onclose с code!=1000/1001 и errorSticky=false → banner 'Connection lost'
- closeGitWs(reason) — снимает обработчики, ws.close(1000,...) без banner
- gitSwitchCwd(newCwd) — ws.send {type:'switch_cwd',cwd}, term.clear() перед, fallback на close+reconnect при failure
- showGitBanner/hideGitBanner/retryGitConnection — UI ошибок
- getActiveProject() — возвращает ProjectDto с .path из state.projects по state.activeProjectId, null для transient ids

Lifecycle:
- switchTab('git') → openLazygitForActiveProject()
- switchTab(прочее) → closeGitWs() (term остаётся mounted для быстрого re-attach)
- switchActiveProject() при activeTab==='git' → gitSwitchCwd(newPath)
- beforeunload → closeGitWs()

Файл по-прежнему содержит legacy git-UI код (fetchGitAll, renderGitGraph, commitNow, polling) — будет удалён в Phase 5.
