# Telegram Фаза 3 — диалоги, текст, realtime (эпик forge-tg-phase3-pp0l)

Полноценная текстовая переписка в реальном времени во вкладке ✈ (плагин forge-telegram, opt-in `devforge --telegram`). Фаза опирается на Фазу 1 (каркас) и Фазу 2 (логин MTProto, FileSession).

## Backend (plugins/telegram/src/)

### dto.rs — контракт данных
DialogDto{id,kind,title,username,unread_count,pinned,last_message}, MessageDto{id,dialog_id,out,date,sender,text,media,reply_to_id,edit_date}, MediaDto{kind,mime,size,file_name,width,height,duration_secs,has_thumb}.
ДВА ИНВАРИАНТА, нарушать нельзя:
1. Все 64-битные id — СТРОКИ в Bot API-формате (`777000`, `-100…`); JS теряет точность выше 2^53. Исключение — message_id (i32 в Telegram).
2. Option-поля сериализуются ЯВНЫМ null, `skip_serializing_if` НЕ используется: фронт отличает 'поля нет в контракте' от 'значения нет'.
Мапперы from_grammers_dialog/from_grammers_message/media_dto + чистые хелперы dialog_id_string/parse_dialog_id/kind_of/peer_kind/peer_title/sender_dto/document_kind (классификация документа по АТРИБУТАМ, MIME только фолбэк: у голосового MIME audio/ogg, различает только флаг voice).

### peers.rs — резолв dialog_id -> PeerRef
Три эшелона: кэш в памяти (state.peers, ключ PeerId) -> FileSession::peer_ref (диск) -> ограниченный iter_dialogs(200). Логика вынесена в resolve_peer_with(state,id,&dyn PeerSource); trait PeerSource подменяется в тестах считающим фейком — 'кэш экономит вызовы' проверяется счётчиком.
Контракт: мусорный/недоступный id -> 404 unknown_dialog (форма id проверяется ДО состояния соединения), нет рантайма -> 503 not_connected, закэшированный peer отдаётся и без соединения.
ЛОВУШКА: `Dialog::peer_ref()` внутри делает `auth().unwrap()` и ПАНИКУЕТ на min-peer'ах — везде используется `Peer::to_ref()`, который в том же случае отдаёт Ok(None).
RwLock-guard'ы никогда не живут через await.

### routes/dialogs.rs — пять эндпоинтов
GET /api/telegram/dialogs?limit (<=500, default 100) -> {dialogs}; побочно наполняет peers-кэш и reset_unread.
GET /dialogs/:id/messages?limit(<=100,default 50)&before_id -> {messages, has_more}; before_id = offset_id Telegram (ЭКСКЛЮЗИВЕН, страницы не пересекаются); порядок наружу — СТРОГО по возрастанию id (sort_by_key, не reverse).
POST /dialogs/:id/messages {text,reply_to_id} -> {message} с реальными id/date/out=true (на них строится reconcile). upload_id -> 400 (медиа в Фазе 5). id==0 (grammers не нашёл сообщение в апдейтах) — не ошибка: warn + DTO как есть.
POST /dialogs/:id/read {max_id?} — messages.readHistory/channels.readHistory по PeerKind (client.mark_as_read зашивает max_id=0); побочно set_unread(0) + broadcast Read{in}.
POST /dialogs/:id/typing {action?} — parse_typing (закрытый набор; опечатка -> 400, а не молчаливое 'печатает') -> client.action(peer).oneshot/cancel.
Порядок отказов везде: 409 not_authorized -> 503 not_connected -> 404 unknown_dialog. map_invocation — единый маппинг RPC-ошибок (420 -> 429+retry_after_secs, 401 -> 409, PEER_ID_INVALID/CHANNEL_INVALID/CHAT_ID_INVALID/USER_ID_INVALID -> 404, прочие 400 -> 400, остальное -> 503).

### updates.rs — supervisor приёма апдейтов
spawn() регистрируется в spawn_workers ВСЕГДА (даже без кредов): supervisor сам ждёт Authorized, поэтому вход через wizard поднимает realtime без рестарта devforge.
Цикл: wait_authorized (подписка на Notify ДО проверки + тик 2с — иначе авторизация между проверкой и await теряется) -> take_stream (updates_rx ОДНОРАЗОВЫЙ) -> run_worker(stream_updates) -> при падении backoff 1/2/5/10/30/60с + ПЕРЕСБОРКА рантайма (clear_runtime + restore_session): 'переподписаться' на апдейты нельзя, приёмник принадлежит SenderPool.
Маппинг: NewMessage -> broadcast + bump_unread (для !out), MessageEdited, MessageDeleted -> deleted_event (канал адресуется точно; в ЛС Telegram НЕ говорит диалог -> dialog_id пустой, фронт ищет по id у себя). Raw: UserTyping/ChatUserTyping/ChannelUserTyping/UserStatus/ReadHistoryInbox|Outbox + СВЕРХ ПЛАНА ReadChannelInbox|Outbox (без них бейдж канала не гас бы при чтении с телефона).
РИСК №4 (сессию отозвали): is_auth_error (401 ИЛИ имена AUTH_KEY_UNREGISTERED/AUTH_KEY_DUPLICATED/SESSION_REVOKED/SESSION_EXPIRED/USER_DEACTIVATED) -> login::on_auth_invalidated + clear_runtime, БЕЗ реконнекта.
sync_update_state вызывается и на shutdown, и перед реконнектом; ради этого state.shutdown_workers получил WORKER_STOP_GRACE=2с перед abort (abort в произвольной await-точке не дал бы сохранить pts, и catch_up после рестарта дублировал бы сообщения).

## Фронт (tmux-web/static/js/telegram/)

- format.js — ЧИСТАЯ логика без DOM/core-импортов (грузится в Node): слаги, статус-строки, разделители дат, keyHex, insertMessage (дедуп по message_id), prependMessages, stick-to-bottom, троттлинг тостов, оценка задержки. Регресс-тест tmux-web/tests/frontend/telegram_format.test.mjs (67 ассертов).
- ws.js — постоянное соединение /ws/telegram (backoff [1,2,5,10]с, мгновенный pong, LAT по дрейфу ping'ов; полноценный RTT недостижим — сервер не отвечает эхом). Живёт независимо от активной вкладки, гасится только visibilitychange:hidden и beforeunload.
- dialogs.js — DIRECTORIES/NODES/CHANNELS, точечные апдейты присутствия и unread без перерисовки списка.
- chat.js — renderHistory + date-separators, prependHistory с ЯКОРЕМ скролла, optimistic send + reconcile, edited/deleted, bindPrompt.
- main.js — buildHandlers (13 типов), recomputeUnread, тосты, постоянный WS из initTelegramGate.
- api.js — getDialogs/getMessages/sendMessage/markRead/sendTyping.

XSS: во всех модулях только createElement+textContent, ни одного innerHTML (title и текст приходят из Telegram).

## Проверено
cargo test --workspace зелёный (537 devforge + 133 forge-telegram + 195 forge-echo + 8 telegram_smoke), clippy -p forge-telegram чист, cargo tree -i openssl-sys/libsql пуст, e2e echo-ui 16/16, node telegram_format.test.mjs 67/67, рантайм на изолированном инстансе (assets 200, контракт ошибок, XSS-вектор в title обезврежен, дедуп/edited/deleted в браузере, обе темы, graceful shutdown supervisor'а).

## Требует ручной проверки пользователем (нужен реальный аккаунт)
Список диалогов и история с живого аккаунта, отправка/ответ, пагинация вверх без пересечений, read/typing на телефоне, realtime <1с, реконнект после обрыва сети, отзыв сессии из Telegram -> disconnected, рестарт без дублей.