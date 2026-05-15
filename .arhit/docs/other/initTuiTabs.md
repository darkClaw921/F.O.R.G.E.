# initTuiTabs

Инициализирует все TUI-вкладки (lazygit, lazydocker, telescope) через createTuiTab(). Расположен в tmux-web/static/app.js (≈строка 1905). Вызывается из bootstrap() после загрузки DOM (см. bootstrap → initTuiTabs()).

## Сигнатура

function initTuiTabs(): void

Не принимает аргументов и не возвращает значения. Побочный эффект — записывает three TuiTab-инстанции в state.

## Поведение

Создаёт три TuiTab через createTuiTab() и пишет их в state:
- state.gitTerm — createTuiTab({name:'lazygit', wsPath:'/ws/lazygit', activeTabName:'git', refs:{termEl:$gitTermEl,...}, installHelp:{binary:'lazygit', notFoundMsg:'lazygit not found...', entries: LAZYGIT_INSTALL_ENTRIES}}).
- state.dockerTerm — { name:'lazydocker', wsPath:'/ws/lazydocker', activeTabName:'docker', refs:{termEl:$dockerTermEl,...}, installHelp:{binary:'lazydocker', entries: LAZYDOCKER_INSTALL_ENTRIES}}.
- state.telescopeTerm — { name:'telescope', wsPath:'/ws/telescope', activeTabName:'telescope', refs:{termEl:$telescopeTermEl,...}, installHelp:{binary:'tv', entries: TELESCOPE_INSTALL_ENTRIES}}.

Refs берутся из top-level DOM-references ($gitTermEl/$gitPlaceholder/$gitError/..., $dockerTermEl/..., $telescopeTermEl/...), которые получены в начале app.js через document.getElementById.

## INSTALL_ENTRIES константы

Определены рядом в app.js (≈строки 1873-1897):
- LAZYGIT_INSTALL_ENTRIES — Homebrew, MacPorts, Debian/Ubuntu (script с GitHub releases API), Arch (pacman), Fedora (dnf+copr), Windows (winget/Scoop), Go.
- LAZYDOCKER_INSTALL_ENTRIES — Homebrew (jesseduffield/lazydocker/lazydocker), Linux script, Arch (yay AUR), Scoop, Go.
- TELESCOPE_INSTALL_ENTRIES — Homebrew, Arch (pacman), Fedora (copr), Cargo (cargo install --locked television).

Каждая запись: {id, label, cmd}. id используется для detectClientOS-сортировки.

## Вызов

Из bootstrap() (≈строка 5908): после loadHealthz/инициализации темы/initTerminal/sidebar/project-bar, перед регистрацией других listeners. После initTuiTabs() методы state.gitTerm.openForActiveProject() / state.dockerTerm.openForActiveProject() / state.telescopeTerm.openForActiveProject() становятся доступны и вызываются из switchTab() и project-change handler'ов.

## Жизненный цикл

- mount xterm Terminal происходит лениво — только когда openForActiveProject вызовется и проект есть.
- В beforeunload-handler вызываются state.dockerTerm.close('beforeunload') и state.telescopeTerm.close('beforeunload') (как и для gitTerm).

## Зависимости

- createTuiTab — factory.
- LAZYGIT_INSTALL_ENTRIES / LAZYDOCKER_INSTALL_ENTRIES / TELESCOPE_INSTALL_ENTRIES.
- Глобальные DOM-refs: $gitTermEl, $gitPlaceholder, $gitError, $gitErrorText, $gitErrorRetry, $gitErrorClose, $gitInstallHelp, $gitInstallList и зеркальные для docker/telescope.
