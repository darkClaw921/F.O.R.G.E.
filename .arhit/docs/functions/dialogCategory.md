# dialogCategory

tmux-web/static/js/telegram/format.js — категория диалога в терминах флагов пользовательской
папки Telegram. Ровно ОДНА категория на диалог (D3).

Сигнатура: dialogCategory(d) -> 'group'|'channel'|'bot'|'contact'|'non_contact'|'user'.

Правила:
- kind === 'group'   → 'group';
- kind === 'channel' → 'channel';
- kind === 'user' && d.bot === true → 'bot' — И ТОЛЬКО 'bot', даже если d.contact === true.
  Так ведёт себя официальный клиент: в TDLib is_user_bot проверяется РАНЬШЕ is_user_contact,
  и папка «Боты» показывает ботов-контактов тоже;
- kind === 'user' && !bot && contact === true → 'contact';
- kind === 'user' && !bot && contact === false → 'non_contact';
- поля bot/contact ОТСУТСТВУЮТ → 'user'.

'user' — это НЕ категория, а «сервер не сказал» (старый DialogDto без полей bot/contact).
Отличать отсутствие поля от false ОБЯЗАТЕЛЬНО: иначе на старом DTO каждый человек и каждый бот
уехали бы в non_contacts, и папка «Боты» осталась бы пустой — то есть дефект D3 просто сменил бы
форму. Для такого DTO dialogInFolder деградирует в прежний OR по contacts|non_contacts|bots:
показать лишнее в папке безопаснее, чем спрятать переписку.

Приватный помощник categoryMatches(category, folder) сопоставляет категорию с флагами папки.
Введена взамен прежней логики «любой user-флаг ловит любого user».
