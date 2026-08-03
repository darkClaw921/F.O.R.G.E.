# plugins/vk/tests/vk_stub.rs

Локальный стенд плагина VK: фейковые api.vk.com, LongPoll-сервер и upload-сервер
на axum + TcpListener("127.0.0.1:0"). Новых dev-зависимостей (wiremock/httpmock)
не заводит намеренно — ради этого VkConfig::base_url объявлен обязательным ещё
в Ф1. Секретов не требует, гоняет ВЕСЬ транспорт через настоящий reqwest.

Скриптование: StubState::script(method, Vec<Reply>) — очередь ответов на метод
(Response / Error с HTTP 200, как в жизни / Drop — оборванное тело / Status /
Garbage); StubState::lp_script(Vec<Value>) — очередь ответов LongPoll.
Журналы: calls_of(method) с параметрами формы, lp_calls() с query каждого
a_check, lp_param(key) — значения одного параметра по всем обращениям.

Обращения к LongPoll пишутся ЦЕЛИКОМ вместе с query (LpCall{at, params}):
инвариант «после failed:2 воркер не сбросил ts» проверяется именно тем, с каким
ts пришёл следующий a_check, — по кадрам его не увидеть. Когда скрипт доигран,
handle_longpoll ведёт себя как настоящий сервер: держит соединение wait секунд и
отвечает пустой пачкой с ТЕМ ЖЕ ts. И удержание, и сохранение ts обязательны —
мгновенный ответ превратил бы «сценарий доиграл» в busy-loop из тысяч запросов
в секунду, а ts:1 откатил бы курсор назад.

Сценарии Ф5 (LongPoll): expired_key_renews_the_key_without_losing_frames_or_
resetting_ts (updates -> failed:2 -> новый key -> updates; ts = 100,101,101 и
key = KEY1,KEY1,KEY2 при ts свежего getLongPollServer = 500; плюс проверка, что
восстановление не ушло в backoff — failed это часть протокола, а не ошибка);
stale_ts_is_taken_from_the_response_and_the_key_survives (failed:1);
lost_history_resets_the_cursor_and_broadcasts_resync (failed:3);
a_batch_of_ten_messages_costs_exactly_one_enrichment_request (десять событий 4 =
ОДИН getLongPollHistory, ноль getById, ноль users.get, new_pts стал курсором);
enrichment_failure_degrades_to_three_get_by_id_and_resync (код 10 -> один
getById на 3 id + 2 кадра resync); the_cursor_survives_a_process_restart;
a_token_inserted_at_runtime_brings_realtime_up_without_a_restart (20 попыток,
auth_poll = 30 с, чтобы путь был только через уведомление);
a_revoked_token_stops_the_worker_without_reconnecting (код 5);
a_token_without_the_messages_scope_stops_the_worker (код 15).

Хелперы: lp_state(stub) / lp_state_tuned(stub, auth_poll) — VkState с короткими
LpTimings и авторизованным логином; collect_frames(rx, want, within);
wait_for_frame(rx, within, predicate) — пропускает login_state; wait_until.
