# TgServerMsg

plugins/telegram/src/ws/protocol.rs — кадры, которые сервер шлёт по GET /ws/telegram.
Формат: внешне-тегированный JSON с полем "type" (serde tag="type", snake_case).

КАДРЫ: login_state, dialogs_changed, new_message, message_edited, messages_deleted, read_state,
typing, notify_settings, notify_defaults_changed, folders_changed, upload_progress,
download_progress, resync, ping. Помощник tag_of(&self) -> &'static str даёт тег без сериализации
(логи, метрики, тесты); полный список тегов продублирован в тестах, поэтому новый вариант нельзя
добавить, забыв про таблицу в шапке модуля.

ИЗМЕНЕНО ПОД D1:
NewMessage { message: MessageDto, muted: bool, archived: bool }
JSON: {"type":"new_message","message":{…},"muted":false,"archived":false}
Поля УРОВНЯ КАДРА, а не внутри message: это свойства ДИАЛОГА на момент доставки, а не сообщения.
Оба поля ОБЯЗАТЕЛЬНЫ при разборе — отсутствие это ошибка, а не false. Сервер стал источником
истины по вопросу «звенит ли диалог»: фронт больше не гадает по неполной странице /dialogs.
Значения считает updates::resolve_dialog_flags (промах кэша ⇒ дефолт КАТЕГОРИИ, не false).

НОВЫЙ КАДР:
NotifyDefaultsChanged { users: bool, chats: bool, broadcasts: bool }
JSON: {"type":"notify_defaults_changed","users":true,"chats":false,"broadcasts":false}
Пользователь заглушил/размьютил целую категорию с телефона. Сделан ОТДЕЛЬНЫМ кадром, а не
FoldersChanged и не пачкой notify_settings: перечислить поимённо все диалоги категории сервер
не может (их могут быть тысячи, и он их все не знает). Фронт в ответ сбрасывает производные
карты и перечитывает список диалогов.

ОСТАЛЬНОЕ БЕЗ ИЗМЕНЕНИЙ: клиентские кадры TgClientMsg (ping/pong), Resync при broadcast Lagged,
отсутствие фильтрации по активному диалогу.
