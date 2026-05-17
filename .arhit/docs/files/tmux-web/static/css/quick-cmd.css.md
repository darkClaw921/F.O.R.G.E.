# tmux-web/static/css/quick-cmd.css

Quick-command bar (Phase B mobile): #quick-cmd-bar + #tui-quick-bar — горизонтальный бар с топ-N команд + spec-клавиш (Esc, Tab, ^C, стрелки), absolute-позиционирование внизу #terminal. Показывается JS только на mobile (matchMedia max-width:768px). Также Quick-cmd Edit-UI модалка (.quick-cmd-edit-*) для управления списком команд. @media (min-width: 769px) { display: none } — гарантия что бар не появится на desktop. 222 строки (3198-3419). Связан с tmux-web/static/quick-cmd.js.
