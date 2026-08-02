# notify_outcome

plugins/telegram/src/updates.rs — чистый разбор tl::types::UpdateNotifySettings.

Сигнатура: notify_outcome(u: &tl::types::UpdateNotifySettings, now: i64,
defaults: NotifyDefaults) -> NotifyOutcome.

Логика по варианту NotifyPeer:
- NotifyUsers / NotifyChats / NotifyBroadcasts -> Defaults(новое состояние всех трёх категорий:
  берётся текущий defaults и заменяется ОДНО поле, разрешённое обычным dto::notify_muted —
  у глобальных настроек наследовать не от кого);
- NotifyPeer(peer) -> Dialog { peer, muted: dto::notify_muted_with_default(settings, now,
  defaults.for_peer_id(peer)) };
- NotifyForumTopic -> Ignored.

ОГРАНИЧЕНИЕ (задокументировано и в коде): в per-peer ветке категория выводится по ТЕГУ PeerId,
который не отличает мегагруппу от broadcast-канала. Промах возможен только у диалога с ПОЛНОСТЬЮ
унаследованными настройками при разных дефолтах chats/broadcasts, и ближайший листинг диалогов
перезапишет запись точным значением через remember_dialogs.

Функция намеренно ЧИСТАЯ (никаких кэшей и broadcast внутри) — побочные эффекты вынесены в
CacheEffect и применяются apply_effects. Поэтому весь разбор покрыт unit-тестами без сети.
