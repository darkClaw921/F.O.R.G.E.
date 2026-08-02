# from_raw_dialog_with_defaults

plugins/telegram/src/dto.rs — сборка DialogDto из сырого tl::enums::Dialog с наследованием
глобальных дефолтов уведомлений.

Сигнатура: from_raw_dialog_with_defaults(raw: &tl::enums::Dialog, top: Option<&tl::enums::Message>,
index: &RawPeerIndex, now: i64, defaults: NotifyDefaults) -> DialogDto.

Отличие от старой from_raw_dialog (сохранена без изменений, эквивалент defaults=Default):
muted считается через dialog_muted_with_defaults_for_kind, причём kind берётся ИЗ ИНДЕКСА
peer'ов, а не из тега id. Это принципиально: в индексе мегагруппа честно помечена "group" и
получает дефолт категории chats, а по тегу PeerId она неотличима от broadcast-канала и получила
бы дефолт broadcasts.

Поля bot/contact берутся из RawPeerEntry (заполняются в RawPeerIndex::new из tl::types::User:
u.bot, u.contact || u.mutual_contact). Промах индекса даёт false/false.

Внутренняя деталь: приватный dto_parts возвращает СТРУКТУРУ PeerParts (kind/title/username/
bot/contact), а не кортеж — кортеж из пяти однотипных полей был бы миной (перепутанные bool
компилируются молча).

Используется в routes/dialogs.rs::folder_dialogs (постраничная догрузка папок).
