# NotifyOutcome

plugins/telegram/src/updates.rs — разбор апдейта updateNotifySettings в три исхода.

enum NotifyOutcome (Debug, Clone, PartialEq, Eq):
- Defaults(NotifyDefaults) — пришёл КАТЕГОРИЙНЫЙ NotifyPeer (NotifyUsers / NotifyChats /
  NotifyBroadcasts): пользователь заглушил/размьютил целую категорию с телефона. Внутри —
  готовое НОВОЕ состояние всех трёх категорий (не дельта), чтобы вызывающему не пришлось знать,
  какую именно ветку обновлять.
- Dialog { peer: PeerId, muted: bool } — per-peer настройка; muted УЖЕ разрешён с наследованием
  через dto::notify_muted_with_default.
- Ignored — NotifyForumTopic (настройка ветки форума).

ПОЧЕМУ ЭТО ВАЖНО: раньше код падал/выбрасывал на unit-вариантах NotifyPeer, то есть глобальный
mute с телефона вообще не доезжал до плагина. Именно поэтому «заглушил всё в телефоне — а тосты
идут» (D1).

Строится функцией notify_outcome(u, now, defaults). Потребители: notify_settings_event (чистое
преобразование в WS-кадр) и основной цикл апдейтов через CacheEffect.
