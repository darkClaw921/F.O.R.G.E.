# vk-LoginFlow

plugins/vk/src/login.rs::LoginFlow — конечный автомат авторизации плагина VK.
(Имя vk-* потому, что запись LoginFlow в базе документации принадлежит
одноимённому автомату плагина Telegram — это РАЗНЫЕ автоматы.)

СЕМЬ СОСТОЯНИЙ, и их ровно семь (тест status_never_leaks_token_in_any_state
перечисляет все):
  not_configured    — токена нет, UI показывает форму вставки;
  checking          — токен есть, сетевая проверка идёт (без него UI мигал бы «офлайн»);
  disconnected      — токен есть, связи нет (сеть, 5xx, таймаут); лечится повтором;
  invalid_token     — VK код 5: протух, отозван или скопирован с ошибкой;
  no_messages_scope — VK код 15: токен рабочий, но БЕЗ права messages. Отдельное
                      состояние, а не invalid_token: «вставьте новый токен» тут
                      бессмысленно, нужен токен с другим scope, и экран под этот
                      случай объясняет, что стандартным standalone-приложениям
                      VK право messages не выдаёт;
  captcha_required  — VK код 14: несёт sid и адрес картинки;
  authorized        — есть VkUserDto из users.get.

КОНТРАКТ С ФРОНТОМ. kind() даёт машинное имя состояния, to_dto() — LoginStateDto
{state, error, user, captcha_sid, captcha_img}. Одно и то же значение уезжает
клиенту ДВУМЯ путями: снапшотом GET /api/vk/status и кадром WS login_state,
поэтому проверка «токен не течёт» сделана на обоих.

ПЕРЕХОДЫ. from_config() — стартовое состояние (токен есть ⇒ disconnected, нет ⇒
not_configured). VkState::set_login() броадкастит login_state всем WS-подписчикам
(открытые вкладки показывают одинаковую картину) и будит LongPoll-супервизора
через authorized_notify — realtime поднимается без рестарта процесса.

ОГРАНИЧЕНИЕ. Токена в состоянии нет и быть не может: см. SecretToken.
