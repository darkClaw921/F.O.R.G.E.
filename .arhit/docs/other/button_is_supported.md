# button_is_supported

plugins/vk/src/dto.rs — ЕДИНСТВЕННОЕ место, где решается, можно ли нажать кнопку сообщества под ПОЛЬЗОВАТЕЛЬСКИМ токеном (Ф9).

SUPPORTED_BUTTON_KINDS = ["text", "open_link"] — и только они:
  text      — официальный клиент VK отправляет её как обычное сообщение с payload через messages.send; ровно это умеет api::methods::SendBody (поля message + payload);
  open_link — открывает браузер, API не участвует вовсе (фронт зовёт safeUrl + openExternal).

НЕ поддерживаются callback / vkpay / open_app / location. Это ограничение ПЛАТФОРМЫ, а не реализации: messages.sendMessageEventAnswer — метод СТОРОНЫ СООБЩЕСТВА (им бот ОТВЕЧАЕТ на уже полученный message_event) и требует ключа доступа сообщества; обратной, клиентской половины («отправить message_event от имени пользователя») в публичном API VK нет ни в документации, ни в клиентских библиотеках. Прямого аналога телеграмному messages.getBotCallbackAnswer у VK не существует. vkpay/open_app/location требуют официального приложения VK.

ПОЧЕМУ РЕШЕНИЕ ОДНО НА ВЕСЬ ПЛАГИН. UI рисует кнопку по флагу ReplyButtonDto.supported и сам ничего не классифицирует. Две таблицы видов разъехались бы, и пользователь получил бы либо активную кнопку, которая всегда отвечает 409 unsupported_button, либо погашенную кнопку, которая на самом деле работает.

Следствие в контракте ошибок: код bot_timeout (504) из телеграмного контракта в VK ОТСУТСТВУЕТ — таймаутиться нечему, нажатие либо превращается в обычную отправку, либо отвергается сразу.

Мутационный щит: dto::tests::button_kinds_map_to_the_support_table (пометка callback как supported краснеет там же).

Связанные: reply_markup_from_raw, routes::dialogs::post_button, ApiError::unsupported_button, vk-button-support.
