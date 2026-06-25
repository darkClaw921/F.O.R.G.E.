# tmux-web/static/css/command-dock.css

CSS Command dock (desktop) — нижняя панель команд вкладки терминала. Всё на переменных темы base.css.

## Layout
.cmd-dock — flex-сиблинг #terminal внутри #main (column), flex:0 0 auto, max-height 45vh; ужимает #terminal, не перекрывает сессию.
.cmd-dock-body — flex-wrap:wrap, overflow-y:auto, overflow-x:hidden. Содержит .cmd-dock-pinned (вручную добавленные chip'ы) и .cmd-dock-frequent (авто-частые, border-left разделитель).

## Перенос chip'ов (важно!)
.cmd-dock-pinned/.cmd-dock-frequent — вложенные flex-контейнеры с flex-wrap:wrap. ОБЯЗАТЕЛЬНЫ min-width:0 и flex-shrink:1 (т.е. flex:0 1 auto / 1 1 auto). Без них неужимаемый flex-item (flex:0 0 auto) раздувается до max-content — все chip'ы в одну строку, внутренний wrap не срабатывает, chip'ы уходят вправо за экран, а overflow-x:hidden на body обрезает их без прокрутки. С ужатием wrap работает; при избытке chip'ов включается вертикальный скролл body (max-height 45vh). Баг проявлялся при ручном добавлении множества команд через '+ Добавить'.

## Chip
.cmd-chip — inline-flex, max-width 280px, label с ellipsis. Inline-кнопки .cmd-chip-act (edit/del/pin) раскрываются на hover (max-width 0→1.4em). Drag-and-drop состояния: .dragging, .drop-before/.drop-after (box-shadow accent).

## Mobile
@media max-width:768px → .cmd-dock display:none (работает .quick-cmd-bar).
