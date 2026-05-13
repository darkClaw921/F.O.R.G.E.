# hotkeys.js

Frontend hotkeys module: tmux-web/static/hotkeys.js (подключается из index.html после app.js). Реализует две системы:

1) Vim-style навигация (срабатывает когда фокус НЕ в input/textarea/select/contentEditable/.xterm-helper-textarea и нет модификаторов):
   - 1/2/3 → переключение вкладок Terminal/Tasks/Git (click на #tab-terminal/#tab-tasks/#tab-git)
   - gt / gT → следующая/предыдущая вкладка (порядок: terminal→tasks→git)
   - j / k → следующая/предыдущая .session-item в #session-list (фокус + scrollIntoView)
   - h → фокус на сайдбар (первая активная или первая .session-item, fallback #project-select)
   - l → фокус на основную панель: терминал → .xterm-helper-textarea внутри #terminal; tasks → #tasks-new; git → .xterm-helper-textarea внутри #git-term
   - Enter на сфокусированной .session-item → el.click() (активация сессии)
   - ? → toggle оверлей #hotkey-help со справкой
   - Esc → закрыть help

2) Cmd-hold hint mode (vimium-style):
   - Удержание Meta/⌘ дольше CMD_HOLD_DELAY_MS (200мс) без других нажатий → showHints()
   - Все видимые интерактивные элементы (button/a/select/input/textarea/[tabindex>=0]/.session-item/.task-card/.todo-card) получают абсолютно-позиционированные .hotkey-hint метки 1-2 символами (alphabet 'asdfjklewcmprtyuiopghbvnxz') в #hotkey-hints-layer (z-index 99999)
   - Пользователь вводит буквы → applyTyped() сужает совпадения (.hotkey-hint-dim для несовпадающих, .hotkey-hint-partial для совпадающих префиксов); при полном совпадении кода → activate(el): focus + click (для input/textarea/select — только focus)
   - Backspace откатывает typed; Esc отменяет; keyup Meta скрывает hints
   - Если до активации hints пользователь нажал другую клавишу при удержании Cmd (cmdSawOther=true) — timer отменяется, чтобы не ломать системные Cmd+C/Cmd+R/Cmd+V

Listeners на document с capture=true (keydown/keyup) — перехватывают раньше xterm-helper-textarea, поэтому hint-mode работает даже при фокусе в xterm. window.blur/scroll/resize → hideHints (метки могли уехать).

isVisible() проверяет getBoundingClientRect (видим в viewport), а также рекурсивно вверх по дереву: hidden attr, display:none, visibility:hidden, opacity:0. generateCodes(n) выдаёт 1-буквенные коды при n≤26, иначе 2-буквенные комбинации.

CSS определён в style.css: блок 'Горячие клавиши: Cmd-hold подсказки + ?-help оверлей' — стили .hotkey-hint (жёлтый бейдж #ffd54f с тёмным текстом, border, box-shadow), .hotkey-hint-partial/.hotkey-hint-dim, #hotkey-help модал с .hotkey-help-card и .kbd-стилем, а также .session-item:focus подсветка (outline #ffd54f).

Не модифицирует app.js — полностью изолированный модуль. Никаких зависимостей кроме DOM. Совместим с xterm (capture-фаза + проверка xterm-helper-textarea для vim-режима).
