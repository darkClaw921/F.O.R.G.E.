# plugins/vk/src/longpoll.rs

LongPoll-воркер плагина VK — единственный источник realtime.

## Зачем модуль существует
У VK нет постоянного соединения: события забираются long-polling'ом с отдельного
хоста, адрес и ключ которого выдаёт messages.getLongPollServer. Модуль превращает
этот поток в кадры VkServerMsg, которые уходят в broadcast VkState и дальше в /ws/vk.

## Константы
- BACKOFF_SECS = [1,2,5,10,30,60] — лестница пауз (дословно телеграмная);
- AUTH_POLL = 2 c — тик перепроверки авторизации (страховка к authorized_notify);
- LP_WAIT_SECS = 25 — штатное ожидание на LongPoll-сервере;
- LP_VERSION = 3, LP_MODE = 2|8|32|64|128 = 234. Бит 32 (pts) ОБЯЗАТЕЛЕН: без него
  a_check не отдаёт pts, и messages.getLongPollHistory вызвать нечем;
- MAX_FALLBACK_GETBYID = 3 — потолок деградации;
- CURSOR_FILE = longpoll.json, CURSOR_PERSIST_EVERY = 20, CURSOR_MAX_AGE_SECS = 86400;
- FLAG_UNREAD=1, FLAG_OUTBOX=2, FLAG_DELETED=128;
- ACTION_TYPING/ACTION_RECORD_VOICE, DIRECTION_IN/DIRECTION_OUT.

## Чистое ядро (тестируется без сети)
- LpPoll { Updates{ts,pts,updates} | Failed(LpFailed) | Malformed(String) };
- LpFailed { StaleTs{new_ts} | ExpiredKey | InfoLost{new_ts} };
- LpRecovery { UseTs(u64) | RenewKeyKeepTs | RenewAll };
- parse_poll(&Value) -> LpPoll. Пустой updates — ШТАТНЫЙ выход по тайм-ауту, а не
  ошибка. ts принимается и числом, и строкой. failed:1 БЕЗ ts даёт Malformed:
  сервер объявил наш ts устаревшим, но не сказал, куда встать;
- recovery_for(&LpFailed) -> LpRecovery. failed:1 -> UseTs (ключ прежний);
  failed:2 -> RenewKeyKeepTs (ts из ответа НЕ берётся — пока ключ протухал, на
  сервере копились события, и свежий ts указывает на «сейчас»); failed:3 ->
  RenewAll (курсор сбрасывается целиком + кадр Resync);
- parse_event(&[Value]) -> Option<LpEvent>. Событие LongPoll это МАССИВ, а не
  объект. Коды 1/2/3/4/5/6/7/8/9/10-12/13/18/51/52/61/62/63/64/70/80/114,
  неизвестный -> LpEvent::Unknown(code) + warn (forward-compat). У 8/9 id друга
  приходит со знаком минус — снимается. У 51/62 второе поле это НОМЕР беседы,
  к нему прибавляется CHAT_PEER_BASE. У 63/64 порядок from_ids и peer_id в
  разных версиях документации VK описан по-разному, поэтому разбор идёт по ТИПУ
  элемента, а не по позиции;
- needs_enrichment(&[LpEvent]) -> bool — только 4/5/18;
- enrichment_ids(&[LpEvent]) -> Vec<i64> — дедуплицированные id для деградации;
- MapCtx { messages: HashMap<i64,MessageDto>, muted: HashMap<i64,bool> } с
  empty()/from_history(hist,names,now)/absorb_messages()/message()/is_muted().
  Незнакомая беседа считается ЗВЕНЯЩЕЙ: молчаливое true спрятало бы сообщение;
- map_event(&LpEvent,&MapCtx) -> Vec<VkServerMsg>. Вектор, а не Option, из-за
  63/64: «печатают трое» = три кадра. События 4/5/18 без обогащения дают Resync,
  события 2/3 без обогащения — ПУСТО (resync на каждую смену флага был бы штормом);
- invalidations(&[VkServerMsg]) -> Vec<(String,i32)> — что выбросить из медиа-кэша
  (правки и удаления, с дедупом).

## Обогащение: ОДИН запрос на ПАЧКУ
Событие 4 приезжает урезанным. Отвергнуты: (а) разбор плоских полей attach1_* —
второй парсер второго недокументированного формата, физически не отдающий
fwd_messages/reply_message/action/keyboard; (б) getById на каждое событие — один
запрос на СООБЩЕНИЕ при бюджете 3 rps. Выбран (в) messages.getLongPollHistory(ts,
pts): один запрос на пачку, полноценные объекты сообщений плюс profiles/groups
(бесплатно закрывает peers.rs), new_pts как курсор. Если событий 4/5/18 нет —
дозапроса нет вовсе (typing/online/read стоят НОЛЬ запросов). Деградация: код 10
или иная ошибка -> getById не более чем на MAX_FALLBACK_GETBYID сообщений,
остаток закрывается кадром Resync.

## Холодного catch-up НЕТ
getLongPollHistory с ts старше суток отвечает кодом 10. Прямого аналога
телеграмному catch_up VK не даёт, и изображать его нельзя. На старте берётся
свежий ts и поток идёт «с сейчас»; история беседы подтягивается обычным
messages.getHistory при её открытии. Сохранённый курсор нужен только для тёплого
догона ВНУТРИ суток.

## Курсор
PersistedCursor { cursor: LpCursor, saved_at: i64 } в <cache_dir>/longpoll.json с
правами 0600 (config::write_secret_file). saved_at обязателен: ts LongPoll — это
ВНУТРЕННИЙ счётчик событий VK, а не unix-время, и без отметки нельзя понять,
протух ли курсор. load_cursor выбрасывает файл старше CURSOR_MAX_AGE_SECS, битый,
урезанный и «из будущего» (скачок часов). obtain_cursor зовёт
messages.getLongPollServer(need_pts=1, lp_version=3).

## Supervisor (структурная копия телеграмного updates.rs)
- spawn(&Arc<VkState>) — вызывается ВСЕГДА из lib.rs::spawn_workers, даже без
  токена: воркер сам ждёт авторизации, поэтому вставка токена в работающем
  процессе поднимает realtime БЕЗ рестарта devforge;
- wait_authorized — notified().enable() ДО проверки состояния (иначе гонка
  «токен вставили между проверкой и подпиской» подвешивает воркера до
  следующего тика) + biased select на shutdown;
- start_cursor — память -> диск -> сеть;
- run_worker — biased select с shutdown вокруг a_check; курсор персистится ДО
  return на ЛЮБОЙ ветке;
- backoff_sleep — прерывается shutdown;
- WorkerExit: Shutdown | AuthInvalid (код 5, login::on_auth_invalidated,
  РЕКОННЕКТА НЕТ) | ScopeLost (код 15, on_scope_lost, реконнекта нет) |
  Captcha{sid,img} (код 14 с картинкой, on_captcha, пауза до нового Authorized) |
  Failed (backoff, step = min(step+1, last)). После успешного цикла step = 0.

## Тайминги
LpTimings { auth_poll, backoff, wait_secs, persist_every } живут в VkState (поле
lp, конструктор VkState::with_timings) — данные, а не константы, чтобы
интеграционный тест не спал боевую лестницу backoff'а.

## Связи
crate::api (VkClient::longpoll_check без троттлера + get_longpoll_server /
get_longpoll_history / get_by_id через троттлер), crate::dto (message_from_raw,
dialog_id_string, CHAT_PEER_BASE), crate::peers (absorb_extended), crate::login
(три терминальные ветки), crate::state (LpCursor, broadcast), crate::ws::protocol
(VkServerMsg). Тесты: 30 юнитов в самом модуле + 9 сценариев в
plugins/vk/tests/vk_stub.rs.
