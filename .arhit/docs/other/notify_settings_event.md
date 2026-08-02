# notify_settings_event

plugins/telegram/src/updates.rs — преобразование updateNotifySettings в WS-кадр без побочных
эффектов.

Сигнатура (ИЗМЕНИЛАСЬ, было (u, now)):
notify_settings_event(u: &tl::types::UpdateNotifySettings, now: i64,
defaults: NotifyDefaults) -> Option<TgServerMsg>.

Маппинг: NotifyOutcome::Defaults -> TgServerMsg::NotifyDefaultsChanged { users, chats, broadcasts };
Dialog -> TgServerMsg::NotifySettings; Ignored -> None.

ПОЧЕМУ СИГНАТУРА ИЗМЕНИЛАСЬ: без defaults per-peer muted нельзя разрешить (наследование), а
глобальные апдейты раньше просто отдавали None и терялись. Теперь глобальные апдейты — не None.

Связанное изменение: ветка U::NotifySettings УБРАНА из map_raw_update (там она даёт None), потому
что уведомления разбираются раньше, в map_update, где доступны и дефолты, и кэши состояния.
