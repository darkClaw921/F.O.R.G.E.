# bitrix-recent-sync

plugins/bitrix/src/recent_sync.rs — фолбэк-воркер списка, счётчиков и присутствия (Ф7).

ЗАЧЕМ. Поток событий не покрывает четыре вещи: правки чужих сообщений (ONIMV2MESSAGEUPDATE документирован, но живьём НЕ ПРИХОДИТ), счётчики непрочитанного, состав/порядок списка диалогов, онлайн-оффлайн (realtime-присутствия платформа вебхуку не даёт). Прямое следствие: чужая правка появляется с задержкой до recent_sync_secs — это поведение по проекту, а не баг.

КЛЮЧЕВЫЕ ЭЛЕМЕНТЫ.
- spawn(state) / supervisor(state) — цикл раз в poll.recent_sync с побудкой sync_nudge (её дёргает ONIMV2JOINCHAT) и лестницей backoff при отказах; владеет окном LAST_SYNC_DATE.
- sync_once(state, client, since) -> SyncOutcome — один batch{im.recent.get{LAST_SYNC_DATE}, im.counters.get}; since=None означает ПОЛНУЮ перезагрузку (первый запуск, Р18, Р21).
- edit_frame() — ГЛАВНОЕ ПРАВИЛО: тот же message.id + другой текст = правка → кадр message_update с edited=true. Новое сообщение получило бы новый id; первый проход (previous=None) молчит.
- presence_frame() — кадр presence только на ИЗМЕНЕНИИ и только у личных диалогов.
- seen_of()/peer_of() — снимок строки (state::DialogSeen) для следующего диффа.
- atom_at() — дата в ATOM (UTC +00:00) с перекрытием SYNC_OVERLAP_SECS=5.

ОГРАНИЧЕНИЯ. Ловится правка ПОСЛЕДНЕГО сообщения диалога (правка в середине истории строку списка не двигает). Об исчезновении строк портал не сообщает — removed в dialogs_patch всегда пуст. LAST_SYNC_MAX_AGE_SECS=7 суток (Р21). Символ '+' в дате обязан уехать как %2B (страж the_sync_batch_encodes_the_plus_in_the_date).

СВЯЗИ. api::methods::recent_and_counters (общее тело с cold_start Ф4); dto::{dialog_from_raw, message_from_raw, CountersDto}; state::{DialogSeen, remember_dialogs, seen_dialog, set_counters}; вызывается также из events::full_resync.

ПОДРОБНОСТИ — страница bitrix-realtime-model.
