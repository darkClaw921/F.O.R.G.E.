# tmux-web/static/css/git-tab.css

Вкладка Git (#git) — pane для встроенного lazygit-xterm. #git { display: none; flex 1 1 auto } — переключается JS на 'flex' при активации таба. .git-* стили для xterm-контейнера: full-width/full-height с !important, фиксированные размеры через JS resize. .git-placeholder когда нет проекта. 204 строки (2147-2350). Связан с tabs/git.js и xterm-инстансом.
