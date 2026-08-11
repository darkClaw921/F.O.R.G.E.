# bitrix-events

plugins/bitrix/src/events.rs — воркер потока событий портала (Ф7).

ЧТО ДЕЛАЕТ. Единственный realtime-механизм, доступный входящему вебхуку: im.v2.Event.subscribe один раз при подъёме + цикл im.v2.Event.get {offset, limit}. Проецирует события в кадры WS-протокола и рассылает их через broadcast.

КЛЮЧЕВЫЕ ЭЛЕМЕНТЫ.
- spawn(state) / supervisor(state) — вечный цикл «дождаться подключения → подписаться → крутить опрос»; регистрируется в реестре воркеров state.
- wait_authorized() — ожидание на authorized_notify с notified().enable() ДО проверки состояния (иначе вебхук, вставленный в окне между проверкой и подпиской, разбудил бы никого).
- plan_start() → StoredCursor::{Fresh,Stale,Missing} — откуда начинать: память, диск или холодный старт.
- cold_start() — get{limit:1000} БЕЗ offset, берётся только nextOffset, события ОТБРАСЫВАЮТСЯ (ретроспективы у платформы нет).
- run_worker() — цикл: get → dispatch → сдвиг курсора ПОСЛЕ рассылки → персист каждые CURSOR_PERSIST_EVERY циклов → пауза по каденсу; курсор пишется на ЛЮБОЙ ветке выхода.
- dispatch() — поглощение имён из события в PeerCache, дедуп по message.id, mute из state, проекция в кадры.
- frames_for() — ЧИСТАЯ проекция события в Vec<B24ServerMsg>; тестируется на живой фикстуре tests/fixtures/ev.json без сети.
- dedup_key() — Some(message.id) ТОЛЬКО для ONIMV2MESSAGEADD.
- next_interval() — каденс Р4 (hasMore 2с / active 4с / idle 20с).
- sleep_or_nudge() — прерывает ТОЛЬКО idle-паузу по poll_nudge (появился WS-подписчик).
- load_cursor/stored_cursor/save_cursor — <cache_dir>/events.json, 0600, порог 6 часов (Р18).
- full_resync() — кадр Resync + полный recent_sync::sync_once.
- report_error() — троттлинг портала (Р16) уезжает кадром error{flood_wait}; обычные транспортные отказы в кадр не превращаются (воркер повторит их сам по лестнице backoff).

КОНСТАНТЫ. EVENTS_CURSOR_FILE, CURSOR_MAX_AGE_SECS (6ч), HAS_MORE_INTERVAL (2с), AUTH_POLL (2с), BACKOFF_SECS [1,2,5,10,30,60], CURSOR_PERSIST_EVERY (10), EV_MESSAGE_ADD/UPDATE/DELETE, EV_REACTION_CHANGE, EV_JOIN_CHAT.

СВЯЗИ. api::methods::{event_subscribe, events_get}; api::schema::{EventsPageRaw, EventRaw, EventDataRaw, EventChatRaw}; state::{EventCursor, EventDedup, PollTimings, poll_nudge}; recent_sync::sync_once (ветка Р18); ws::protocol::B24ServerMsg.

ПОДРОБНОСТИ И ОБОСНОВАНИЯ — страница bitrix-realtime-model.
