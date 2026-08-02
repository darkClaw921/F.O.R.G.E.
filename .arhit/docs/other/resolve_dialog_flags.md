# resolve_dialog_flags

plugins/telegram/src/updates.rs — разрешение флагов диалога для кадра new_message.

Сигнатура: resolve_dialog_flags(cached_muted: Option<bool>, archived: bool,
defaults: NotifyDefaults, kind: &str) -> DialogFlags.

DialogFlags { muted: bool, archived: bool } (Debug, Clone, Copy, Default, PartialEq, Eq) — ровно
те два поля УРОВНЯ КАДРА, которые уезжают во фронт в new_message.

ГЛАВНОЕ ПРАВИЛО: cached_muted == None (промах кэша notify_overrides) ⇒ берётся
defaults.for_kind(kind), а НЕ false. Слепой false здесь — это буквально исходный дефект:
сообщение из глобально заглушённой категории уехало бы во фронт как звенящее.

kind передаётся из msg.peer() (клиентский Peer), где мегагруппа честно "group", а не "channel" —
поэтому дефолт для мегагрупп берётся из категории chats.

Вызывается в тракте апдейтов при сборке TgServerMsg::NewMessage.
