# DialogDto

plugins/telegram/src/dto.rs — DTO диалога, единственный формат списка чатов для фронта.

ПОЛЯ:
- id: String — Bot API-формат (ИНВАРИАНТ проекта: все id peer'ов в JSON — строки, иначе
  JS-число теряет точность на больших id каналов);
- kind: String — "user" | "group" | "channel"; ВАЖНО: мегагруппа здесь "group", а не "channel";
- title: String, username: Option<String>;
- bot: bool — собеседник является ботом; ТОЛЬКО для kind=="user", иначе всегда false (D3);
- contact: bool — пользователь в контактах (User.contact || User.mutual_contact); для не-user
  всегда false (D3);
- unread_count: i32, pinned: bool;
- muted: bool — С УЧЁТОМ НАСЛЕДОВАНИЯ глобальных дефолтов категории (D1), см. NotifyDefaults;
- archived: bool, folder_id: Option<i32>;
- last_message: Option<MessageDto>.

ПОЛЯ bot/contact ДОБАВЛЕНЫ ПОД D3. Без них фронтовый dialogInFolder не мог классифицировать
диалог в ровно одну категорию папки Telegram (bots / contacts / non_contacts) и вынужден был
делать OR по всем трём — папка «Боты» показывала людей, а чат бота мог не показаться вовсе.
Старый DTO (без полей) фронт распознаёт как «сервер не сказал» и деградирует в прежний OR —
показать лишнее в папке безопаснее, чем спрятать переписку.

ИЗВЕСТНОЕ РАСХОЖДЕНИЕ С ОФИЦИАЛЬНЫМ КЛИЕНТОМ: Saved Messages. TDLib относит диалог с самим собой
к contacts (user_id == my_id || is_user_contact), а в DTO признака self нет и contact заполняется
только из u.contact || u.mutual_contact — свой чат попадёт в non_contacts. Лечится добавлением
поля self в DTO; на исходную жалобу не влияет.

Строится: from_raw_dialog / from_raw_dialog_with_defaults (сырой путь, через RawPeerIndex) и
from_grammers_dialog / from_grammers_dialog_with_defaults (клиентский путь).
Потребители: GET /api/telegram/dialogs, TelegramState::remember_dialogs (засев кэшей muted/archived),
фронтовые format.js::dialogCategory / dialogInFolder / dialogsForDir, dialogs.js.
