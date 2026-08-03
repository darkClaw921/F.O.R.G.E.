# js/vk/chat.js

tmux-web/static/js/vk/chat.js — лента #vk-log и отправка (Ф4.10; на этой фазе только текст). Покрыт tests/frontend/vk_chat.test.mjs (57 проверок на фейковом DOM).

СКОПИРОВАНО ДОСЛОВНО из telegram/chat.js: PAGE_SIZE=50, спейсер .vk-log-spacer переживает очистку ленты, якорь скролла при подгрузке вверх, stick-to-bottom с замером ДО мутации DOM, optimistic-строка и её reconcile, дедупликация через insertMessage, весь textContent-режим, import * as vkApi.

ДОБАВЛЕНО ПРОТИВ TELEGRAM: открытие беседы поднимает ПОКОЛЕНИЕ и отменяет предыдущую загрузку через vkCoalescer('history'). Проверка по поколению, а не по activeDialogId: повторное открытие ТОЙ ЖЕ беседы ([retry]) тоже обязано отменить предыдущую загрузку. У Telegram запросов было не жаль, у VK бюджет 3 rps.

РАЗМЕТКА СТРОКИ ЗАЛОЖЕНА ПОД БУДУЩЕЕ: контейнер .vk-line-media (Ф7 положит N карточек — «слот на одно вложение» пришлось бы переделывать) и dataset.cmid (Ф6 адресует им ответы). Блок пересланных (.vk-fwd) с отступом по глубине и хвостом «…ещё N».

cancelTyping В VK НЕ ХОДИТ: у messages.setActivity нет вида «перестал печатать», статус гаснет сам за ~10 с, а запрос стоил бы трети секундного бюджета.

Пагинация по oldestId (message.id) — параметр VK называется start_message_id.

Функции: initChat({onRead, presenceOf}), openDialog, renderHistory, loadOlder, bindScroll/bindPrompt, send(text, replyToId, payload), noteTyping/cancelTyping, systemLine, updateHead/refreshHead, onNewMessage/onEdited/onDeleted/onRead, resetChat, _debugChatState.
