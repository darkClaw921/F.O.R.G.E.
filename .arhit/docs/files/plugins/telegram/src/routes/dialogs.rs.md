# plugins/telegram/src/routes/dialogs.rs

REST-роуты данных Telegram (Фаза 3, до этого — честные 409-заглушки).

GET /api/telegram/dialogs?limit — clamp(1..500, default 100), iter_dialogs; побочно наполняет peers-кэш (следующие messages-роуты не резолвят по сети) и reset_unread по данным Telegram. Порядок — как отдал Telegram (закреплённые сверху), фронт не сортирует.
GET /dialogs/:id/messages?limit&before_id — clamp(1..100, default 50); before_id = offset_id (ЭКСКЛЮЗИВЕН, страницы не пересекаются и не теряют сообщений); ответ {messages,has_more} СТРОГО по возрастанию id (sort_by_key, а не reverse — контракт не должен зависеть от порядка чужой реализации); пустой диалог — не ошибка.
POST /dialogs/:id/messages {text,reply_to_id} — {message: MessageDto} с реальными id/date/out=true для reconcile; пустой text -> 400; upload_id -> 400 (медиа в Фазе 5); id==0 (grammers не нашёл сообщение в ответных апдейтах) — warn, но не ошибка: сообщение доставлено.
POST /dialogs/:id/read {max_id?} — messages.readHistory / channels.readHistory по PeerKind через raw-invoke (client.mark_as_read жёстко зашивает max_id=0); побочно set_unread(0) + broadcast Read{direction:in} для остальных вкладок.
POST /dialogs/:id/typing {action?} — parse_typing (закрытый набор значений; опечатка -> 400 с перечислением, а не молчаливое 'печатает') -> client.action(peer).oneshot/cancel.

Порядок проверок везде: авторизация (409 not_authorized) -> соединение (503 not_connected) -> резолв peer'а (404 unknown_dialog). map_invocation — единый маппинг ошибок RPC: 420 -> 429+retry_after_secs (авто-ретрая нет, но в отличие от логина это не опасно), 401 -> 409, *_ID_INVALID/CHANNEL_INVALID -> 404, прочие 400 -> 400, остальное -> 503.
