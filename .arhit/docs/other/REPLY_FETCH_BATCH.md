# REPLY_FETCH_BATCH

plugins/telegram/src/dto.rs — `pub const REPLY_FETCH_BATCH: usize = 100;`

Сколько оригиналов можно запросить одним `messages.getMessages`. Ограничение Telegram; grammers
`get_messages_by_id` НЕ режет список сам, поэтому нарезка — забота вызывающего.

В роуте истории нарезку делает dialogs.rs::reply_fetch_plan, а недостижимость этой обрезки
сторожит компилятор: `const _: () = assert!(MAX_MESSAGES <= dto::REPLY_FETCH_BATCH);`. Поднимут
потолок страницы истории — упадёт сборка, а не потеряются цитаты.

СВЯЗИ: reply_ids_to_fetch, dialogs.rs::reply_fetch_plan, dialogs.rs::MAX_MESSAGES.
