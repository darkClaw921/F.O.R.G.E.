# index.html

Frontend layout tmux-web/static/index.html. Phase 5 cleanup (P5.2): удалён весь блок #git-legacy. В пейне #git только новый lazygit UI: #git-placeholder, #git-error banner, #git-term. Структура: layout (#sidebar с project-bar+session-list+status-footer; #main с tab-bar Terminal/Tasks/Git и tabs #terminal/#tasks/#git). Подключает xterm.js@5.3.0 + FitAddon + WebLinksAddon с CDN; app.js — основной модуль; hotkeys.js — модуль горячих клавиш (vim-style nav + Cmd-hold hint mode), подключается после app.js.
