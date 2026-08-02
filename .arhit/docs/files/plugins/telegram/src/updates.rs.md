# plugins/telegram/src/updates.rs

Supervisor приёма апдейтов MTProto и их трансляции в WS-broadcast. Фаза 3.

spawn() регистрируется в spawn_workers ВСЕГДА, даже без кредов: supervisor сам ждёт Authorized, поэтому логин через wizard в работающем процессе поднимает realtime без рестарта devforge.

Цикл: wait_authorized (подписка на Notify ДО проверки состояния + страховочный тик 2с — notify_waiters не оставляет permit, и авторизация между проверкой и await была бы потеряна) -> take_stream (updates_rx ОДНОРАЗОВЫЙ, принадлежит SenderPool) -> run_worker(client.stream_updates{catch_up,update_queue_limit}) -> при падении backoff 1/2/5/10/30/60с и ПЕРЕСБОРКА рантайма (clear_runtime + restore_session): переподписаться на апдейты нельзя. Успешный старт потока сбрасывает backoff.

Маппинг (чистые, тестируемые без сети функции): map_update -> NewMessage (+bump_unread для !out; 'активность диалога' — понятие фронта, счётчик гасит POST /read), MessageEdited, MessageDeleted через deleted_event (канал адресуется точно, в ЛС Telegram НЕ сообщает диалог -> пустой dialog_id, фронт ищет по id). map_raw_update -> Typing (UserTyping/ChatUserTyping/ChannelUserTyping), Online (UserStatus; Recently/LastWeek/LastMonth -> online=false без времени), Read (ReadHistoryInbox/Outbox + ReadChannelInbox/Outbox — последние сверх плана, без них бейдж канала не гас бы при чтении с телефона). action_name — snake_case-контракт с фронтом на все варианты SendMessageAction. Неинтересный апдейт -> None, поток не падает.

РИСК №4 плана: is_auth_error (код 401 ИЛИ имена AUTH_KEY_UNREGISTERED/AUTH_KEY_DUPLICATED/SESSION_REVOKED/SESSION_EXPIRED/USER_DEACTIVATED — Telegram шлёт их и с другими кодами) -> WorkerExit::AuthInvalid -> login::on_auth_invalidated + clear_runtime, БЕЗ реконнекта: ключ мёртв.

sync_update_state вызывается на shutdown (select! biased) и перед уходом на реконнект — иначе catch_up после рестарта дублирует или теряет сообщения. Ради graceful-выхода state.shutdown_workers ждёт WORKER_STOP_GRACE=2с перед abort.
