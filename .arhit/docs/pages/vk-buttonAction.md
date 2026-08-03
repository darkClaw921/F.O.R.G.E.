# buttonAction (VK)

js/vk/botkb.js — решение «что делать по нажатию кнопки сообщества» (Ф9). ЧИСТАЯ функция, покрыта vk_botkb.test.mjs. Отдельная страница, потому что имя совпадает с телеграмным buttonAction (js/telegram/botkb.js) — записи arhit по имени затирают друг друга.

ВХОД: нормализованная кнопка (normalizeButton) либо сырой ReplyButtonDto.
ВЫХОД: {action: 'press'|'open-url'|'unsupported', text?, row?, col?, url?, reason?}.

ПОРЯДОК РЕШЕНИЙ.
1. supported !== true -> unsupported с человеческой причиной из UNSUPPORTED_REASON. Флаг ставит БЭКЕНД (dto.rs::button_is_supported) — он один знает, что умеет.
2. open_link -> safeUrl (js/vk/format.js): белый список схем http/https/vk ПОСЛЕ вычистки управляющих символов. Недопустимая или пустая ссылка -> unsupported.
3. text -> press с координатами {row, col} из DTO. Координат нет (кадр старой версии плагина) -> unsupported: «нажать первую кнопку вместо той, что просили» — худшая реакция на битую разметку.
4. Иначе -> unsupported. Это ветка «сервер объявил вид поддержанным, а фронт его исполнить не умеет»: флаг supported — условие НЕОБХОДИМОЕ, но не достаточное, и нажатие вслепую хуже честного пояснения.

ЧЕМ ОТЛИЧАЕТСЯ ОТ ТЕЛЕГРАМНОГО. Нет действий callback и inline-query: у VK клиентской половины message_event не существует вовсе, а switch_inline у него нет как механики. Действие press означает не «спросить бота», а «отправить подпись кнопки с её payload через роут нажатия».

ЧТО ЗНАЧИТ unsupported ДЛЯ UI: кнопка рисуется, но disabled + класс .vk-kb-btn-unsupported + title с причиной; обработчик клика к ней не привязывается вовсе.

Причины (UNSUPPORTED_REASON): callback — «механика message_event доступна только самому сообществу»; vkpay — VK Pay только в официальном приложении; open_app — мини-приложения только в клиенте VK; location — геопозиция только в приложении VK.

Связанные: [[vk-button-support]], js/vk/botkb.js, button_is_supported, post_button.