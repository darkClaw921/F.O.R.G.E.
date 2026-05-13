# tmux-web/static/app.js

Frontend SPA tmux-web. Phase 4-5: lazygit-tab через xterm + WS /ws/lazygit. Phase 6+: error-banner с детектом OS и copy-to-clipboard командами установки lazygit.

## state.gitTerm
{ term, fit, ws, mounted, currentCwd, errorSticky } — изолированный xterm.Terminal+FitAddon для git-таба, отдельный от основного terminal-таба. Открывается WS на switchTab('git'), закрывается на уход с таба или beforeunload.

## Ключевые функции
- mountGitTerm() — лениво создаёт Terminal+FitAddon в #git-term, biz Binary I/O.
- openLazygitForActiveProject() — open WS /ws/lazygit?cwd=<path>&cols=&rows=. Если активного проекта нет — placeholder #git-placeholder.
- connectGitWs(cwd) — handshake WebSocket. onmessage(ArrayBuffer)→term.write; onmessage(text JSON {type:'error',message})→showGitBanner. term.onData→ws.send Binary. resize→ws Text {type:'resize',cols,rows}.
- gitSwitchCwd(newCwd) — отправляет {type:'switch_cwd',cwd} (control), fallback на reconnect.
- closeGitWs(reason) — graceful close.
- showGitBanner(message, {showInstall}) — красный banner поверх xterm. showInstall=true → renderInstallHelp(): рендер списка установочных команд для разных OS.
- renderInstallHelp() — заполняет #git-install-list карточками: macOS(Homebrew/MacPorts), Debian/Ubuntu (через curl tarball + install), Arch (pacman), Fedora (dnf copr), Windows (winget/Scoop), Go (go install). detectClientOS() через navigator.platform/userAgent помечает текущую OS классом .detected и сортирует её первой.
- copyToClipboardSafe(text) — Clipboard API с fallback на скрытый textarea+execCommand. Кнопка Copy получает класс .copied на 1400ms.
- detectClientOS() — возвращает 'mac' | 'linux' | 'windows' | null. Дистрибутив Linux точно не детектится (выводим все варианты apt/pacman/dnf).
- hideGitBanner() — скрывает и banner и install-help.
- retryGitConnection() — hideGitBanner + closeGitWs + openLazygitForActiveProject.

## Wire-протокол /ws/lazygit
- Binary in/out — сырые байты PTY.
- Text frames: {type:'resize',cols,rows} | {type:'switch_cwd',cwd} от клиента; {type:'error',message} от сервера.
- При получении error frame с lazygit-not-found / no-such-file → showGitBanner с showInstall=true.

## DOM
- #git-term — xterm container.
- #git-placeholder — 'Select a project to open lazygit'.
- #git-error — sticky red banner (order:-1 в flex).
- #git-error-text / -retry / -close — текст и контролы banner.
- #git-install-help — раскрывающийся блок только при lazygit-not-found, со списком команд.
- #git-install-list — ul, li с .os-label/.os-cmd/.os-copy.

## Удалено в Phase 5
Polling /api/git/*, fetchGitStatus/Log/Stage/Unstage/Commit, renderGitToolbar/Files/Graph, computeGitLanes, state.gitStatus/gitLog/gitPollTimer, legacy DOM-refs (///...). Custom git UI полностью заменён на lazygit TUI.
