# tmux-web/static/js/vk/chat.js

Лента переписки панели VK: рендер истории, отправка текста, optimistic-строка и
реакция на кадры realtime.

Ключевые экспорты: initChat({onRead, presenceOf}), openDialog(id, dialog),
loadOlder, renderHistory, send(text, replyToId, payload), systemLine, updateHead,
refreshHead, bindScroll, bindPrompt, noteTyping/cancelTyping, resetChat,
_debugChatState. Realtime: onNewMessage, onEdited, onDeleted, onRead,
onHistoryCleared.

onHistoryCleared({dialog_id, up_to_id}) — обработчик НОВОГО кадра VK
(событие LongPoll 13). Отдельный от message_deleted, потому что VK не присылает
список удалённых идентификаторов, только границу. Ставить на каждое сообщение
заглушку «message purged», как это делает onDeleted, здесь нельзя: заглушек
оказалось бы столько, сколько было сообщений, и лента превратилась бы в стену
надгробий вместо пустой переписки. up_to_id = 0 означает «удалено всё».
Возвращает число убранных из модели сообщений; активная лента перерисовывается
целиком через renderHistory + системная строка о границе.

Инварианты: поколение открытия (createGeneration) отменяет дорисовку брошенной
загрузки при быстром переключении бесед — при бюджете 3 rps очередь из десяти
историй это три секунды чужих данных на экране; дедупликация через
format.js::insertMessage; текст в DOM только через textContent.

ДОПОЛНЕНО В Ф11-Ф12. Лента — предмет клавиатурной навигации: курсор ходит по
cmid (а не по глобальному message_id, который в каждой беседе свой), поэтому
openDialogById обязан звать resetKeynav при смене беседы. Новая против Telegram
зона — ВЛОЖЕНИЯ: keynav берёт по ОДНОЙ кнопке OPEN на карточку (у карточки их
две, OPEN и SAVE), а activate() со строки жмёт первую неотключённую OPEN, а не
кликает по телу карточки — тело реагирует только у превью и проигрываемых видов,
и на документе такой клик был бы съеденной клавишей без следствия.
