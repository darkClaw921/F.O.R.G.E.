# load_notify_defaults

plugins/telegram/src/updates.rs — загрузка глобальных дефолтов уведомлений с сервера Telegram.

Сигнатура: load_notify_defaults(client: &Client, now: i64) -> Option<NotifyDefaults> (async).

Делает ТРИ RPC account.getNotifySettings — с InputNotifyUsers, InputNotifyChats и
InputNotifyBroadcasts — и складывает ответы через NotifyDefaults::from_settings.

None означает, что ХОТЯ БЫ ОДИН вызов не удался. Частичный результат НЕ отдаётся сознательно:
две категории из трёх дали бы кэш, в котором третья молча считается «не заглушена», то есть
частично воспроизводила бы исходный дефект — и отладить это было бы почти невозможно
(«иногда каналы звенят»).

Таймаут на всю операцию — NOTIFY_DEFAULTS_TIMEOUT = 10 секунд (pub const).
