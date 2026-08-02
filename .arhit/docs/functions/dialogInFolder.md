# dialogInFolder

tmux-web/static/js/telegram/format.js — попадает ли диалог в пользовательскую папку Telegram
(dialogFilter). Сигнатура прежняя: dialogInFolder(d, folder) -> boolean; ПОВЕДЕНИЕ ИЗМЕНЕНО под D3.

ПОРЯДОК ПРАВИЛ — дословно по Telegram, менять нельзя:
1. exclude_ids — исключает БЕЗУСЛОВНО (перебивает всё, включая категорию);
2. pinned_ids / include_ids — включают БЕЗУСЛОВНО (поимённый список сильнее категории);
3. КАТЕГОРИЯ через dialogCategory: 'group' → folder.groups; 'channel' → folder.broadcasts;
   'bot' → folder.bots (и ТОЛЬКО он); 'contact' → folder.contacts;
   'non_contact' → folder.non_contacts; 'user' (старый DTO) → OR по contacts|non_contacts|bots;
4. exclude_muted / exclude_read / exclude_archived.

ЧТО БЫЛО СЛОМАНО (D3): категорийная ветка делала OR «любой user-флаг ловит любого user», потому
что в DialogDto не было признака бота. Папка «Боты» показывала людей, а чат бота мог не
показаться вовсе. Признак приехал полями DialogDto.bot / DialogDto.contact.

ВТОРАЯ ПОЛОВИНА ДЕФЕКТА (не в этой функции): чат бота мог физически отсутствовать в загруженной
странице диалогов. Лечится dialogs.js::ensureFullList + backend DEFAULT_FOLDER_DIALOGS=300.
