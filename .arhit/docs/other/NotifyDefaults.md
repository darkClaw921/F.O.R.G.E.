# NotifyDefaults

plugins/telegram/src/dto.rs — глобальные дефолты уведомлений по трём категориям Telegram.

ЗАЧЕМ. Telegram НЕ дублирует настройки в каждый диалог. Если пользователь заглушил «все личные
чаты» / «все группы» / «все каналы» с телефона, у самих диалогов PeerNotifySettings приезжает
ПУСТЫМ (silent=None, mute_until=None), и решение «звенит или нет» принимается по дефолту
категории. Плагин раньше этого дефолта не знал, поэтому такие диалоги считались незаглушёнными —
это и была первая половина жалобы «уведомления продолжают приходить от ненужных чатов» (D1).

ПОЛЯ (все — уже РАЗРЕШЁННЫЕ на момент now булевы «заглушено», а не сырые mute_until; срок
глобального mute проверяется один раз при загрузке дефолтов, а не в каждом диалоге):
- users: bool — личные чаты, ВКЛЮЧАЯ ботов и Saved Messages;
- chats: bool — группы, И малые, И мегагруппы (у Telegram это одна категория);
- broadcasts: bool — broadcast-каналы.

derive: Debug, Clone, Copy, Default, PartialEq, Eq. Copy — потому что структура таскается через
все мапперы диалогов по значению; Default (все false) — «пока не загрузили, ничего не заглушено».

МЕТОДЫ:
- from_settings(users, chats, broadcasts: &tl::types::PeerNotifySettings, now: i64) -> Self —
  собирает из трёх ответов account.getNotifySettings (InputNotifyUsers / InputNotifyChats /
  InputNotifyBroadcasts). Каждый разрешается обычным notify_muted: у ГЛОБАЛЬНЫХ настроек
  наследовать уже не от кого, поэтому пустые настройки = «не заглушено».
- for_kind(&self, kind: &str) -> bool — дефолт по DialogDto::kind ("user" | "group" | "channel").
  Неизвестный kind даёт false СОЗНАТЕЛЬНО: молчать по ошибке хуже, чем показать лишнее —
  пропавшее сообщение пользователь не увидит вовсе, а лишнее хотя бы заметит.
- for_peer_id(&self, id: PeerId) -> bool — дефолт по одному лишь id. ОГРАНИЧЕНИЕ: по тегу
  PeerId мегагруппа неотличима от broadcast-канала (обе PeerKind::Channel), поэтому мегагруппа
  получит дефолт broadcasts. Там, где настоящий kind известен (RawPeerIndex, клиентский Peer),
  надо звать for_kind.

СВЯЗИ: хранится в TelegramState::notify_defaults (RwLock), загружается updates::load_notify_defaults
перед стартом потока апдейтов, обновляется по updateNotifySettings с категорийным NotifyPeer
(см. updates::notify_outcome), уезжает во фронт WS-кадром notify_defaults_changed. Потребители:
notify_muted_with_default, dialog_muted_with_defaults(_for_kind), from_raw_dialog_with_defaults,
from_grammers_dialog_with_defaults, updates::resolve_dialog_flags, routes/dialogs.rs.
