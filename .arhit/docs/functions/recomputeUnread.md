# recomputeUnread

tmux-web/static/js/telegram/main.js — пересчёт бейджа Telegram. Сигнатура прежняя
(recomputeUnread() -> number), ПОВЕДЕНИЕ ИЗМЕНЕНО под D1.

Теперь: badgeUnread(tgState.dialogs, local.muted) + сумма local.pendingUnread, плюс чистка
производных карт по актуальному списку — local.flagsExtra и local.pendingUnread снимаются для
диалогов, которые появились в списке (загруженный DialogDto авторитетнее знания из кадра).

badgeUnread исключает и заглушённые, и АРХИВ — цифра на ✈ теперь совпадает со счётчиком ~/inbox.
