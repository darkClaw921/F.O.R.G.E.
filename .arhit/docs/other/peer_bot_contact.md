# peer_bot_contact

plugins/telegram/src/dto.rs — извлечение флагов (bot, contact) из клиентского grammers Peer.

Сигнатура: peer_bot_contact(peer: &grammers_client::peer::Peer) -> (bool, bool).

Правило:
- Peer::User(u) -> (u.bot(), u.contact() || u.mutual_contact());
- всё остальное (чаты, каналы) -> (false, false).

Зачем: без признака «бот» категорийный фильтр пользовательской папки Telegram не может отличить
бота от человека, и папка «Боты» на фронте оказывалась пустой — это дефект D3 из жалобы
(«при включении папки где есть боты их чаты не отображаются»).

contact собирается из ДВУХ флагов: у Telegram contact = «я добавил его», mutual_contact = «мы
друг у друга»; для папки «Контакты» годится любой из них.

Симметричная логика для сырого пути живёт в RawPeerIndex (поля bot/contact в RawPeerEntry,
заполняются из tl::types::User). Промах индекса даёт (false, false) — «не выдумываем бота».

Вызывается из from_grammers_dialog_with_defaults.
