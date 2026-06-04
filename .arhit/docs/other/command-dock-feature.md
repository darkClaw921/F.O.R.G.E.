# command-dock-feature

Минималистичная нижняя панель команд (Command dock) на вкладке терминала (desktop).

## Назначение
Док с двумя секциями:
1. **Pinned** — пользовательские chip'ы (команды и просто-текст). localStorage forge.cmdDock.items: [{id,label,text,kind}], kind ∈ cmd|text. Drag-and-drop переупорядочивание, edit (✎), delete (×).
2. **Frequent** — авто-топ часто набираемых команд из forge.quickCmd.freq (ведёт quick-cmd.js по stdin xterm). Клик отправляет; 📌 закрепляет в Pinned; × скрывает (forge.cmdDock.freqHidden).

## Клик
runItem: kind='cmd' → sendToActivePty(text+'\r') (выполнить), kind='text' → sendToActivePty(text) (вставить без Enter). Через window.ForgeApp.sendToActivePty.

## Тема
CSS целиком на переменных base.css (--bg-header, --bg-pill, --border-soft, --accent, --fg-mute …) → подстраивается под активную тему. Минимализм: прозрачные header-кнопки, action-кнопки chip'а раскрываются на hover.

## Layout — НЕ перекрывает сессию
#cmd-dock — flex-сиблинг #terminal внутри #main (column), flex:0 0 auto, max-height 45vh. Его появление/изменение высоты ужимает #terminal (flex:1); xterm refit делает существующий ResizeObserver в terminal/xterm.js. Ручного padding-резервирования НЕТ. Видимость: показывается только когда terminalActive() (т.е. #terminal не hidden) и не mobile; MutationObserver на hidden у #terminal синхронизирует видимость при смене вкладок. На mobile скрыт (CSS + isMobile), работает .quick-cmd-bar.

## Файлы
- tmux-web/static/command-dock.js — IIFE, window.CommandDock.refresh()
- tmux-web/static/css/command-dock.css — стили на переменных темы (import в style.css)
- tmux-web/static/index.html — #cmd-dock как сиблинг #terminal внутри #main; скрипт после quick-cmd.js

## Свёртка
Кнопка ▾ сворачивает body (forge.cmdDock.collapsed) — остаётся только header, #terminal расширяется.
