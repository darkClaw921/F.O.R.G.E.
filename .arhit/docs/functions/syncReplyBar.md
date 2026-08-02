# syncReplyBar

tmux-web/static/js/telegram/botkb.js — привести полосу reply-клавиатуры в соответствие с
состоянием активного диалога.

Сигнатура: syncReplyBar() -> boolean (видима ли полоса после синхронизации).

Рисует заголовок (barTitle), ряды кнопок, кнопку закрытия .tg-kb-bar-close и — в свёрнутом
состоянии — подсказку .tg-kb-bar-hint с кнопкой «показать» .tg-kb-bar-show
(модификатор .tg-kb-bar-collapsed). Обрабатывает single_use: после нажатия клавиатура сворачивается,
persistent — остаётся. placeholder разметки подставляется в поле ввода.

Мобильная вёрстка полосы: flex-wrap + flex: 1 1 40% + min-width: 0 + text-overflow — ряд из 4-5
кнопок на 320px не должен давать горизонтальный скролл панели (та же болезнь, что лечили у
.tg-media-card).
