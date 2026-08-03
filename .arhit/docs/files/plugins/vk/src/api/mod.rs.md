# plugins/vk/src/api/mod.rs

Транспортный слой плагина VK: VkClient — ЕДИНСТВЕННАЯ точка выхода в сеть.

НАЗНАЧЕНИЕ. Конвейер: methods.rs -> VkClient::call -> Throttle::acquire -> POST form -> parse_envelope -> ретрай-политика -> ConnHealth.

СОСТАВ. VkClient { http: reqwest::Client, base_url, token: SecretToken, api_version, throttle: Arc<Throttle>, health: Arc<RwLock<ConnHealth>>, longpoll_wait_secs, network_backoff, flood_retry_delay }. Params = Vec<(&'static str, String)>.

МЕТОДЫ.
- new(&VkConfig, Arc<RwLock<ConnHealth>>) -> anyhow::Result<Self>: timeout 30 c, connect 10 c, честный User-Agent devforge-vk/<version>. Ячейка здоровья приходит СНАРУЖИ (VkState::health_handle), чтобы 503 not_connected в data-роутах опирался на реальные вызовы.
- with_retry_delays(network_backoff, flood): подмена пауз для тестов, как VkState::with_ws_timings. Прод всегда идёт через new().
- call<T: DeserializeOwned>(method, params) и call_value(method, params): троттлинг, POST формы, parse_envelope, не более ОДНОГО повтора по error::retry_for. Повтор использует ТЕ ЖЕ params — условие идемпотентности messages.send по random_id.
- post_once(method, &params): приватная, ЕДИНСТВЕННОЕ место подстановки access_token и v. Токен едет в ТЕЛЕ (form-urlencoded), а не в query — иначе оседал бы в логах прокси и в тексте ошибок reqwest.
- longpoll_check(server, key, ts, wait_secs): a_check, НЕ троттлится, таймаут wait + LONGPOLL_TIMEOUT_SLACK (10 c). Схема к server дописывается, если её нет — значение со схемой (http://127.0.0.1:PORT/lp) используется как есть, ради стенда без TLS. Возвращает СЫРОЙ JSON: у LongPoll свой протокол, не конверт API.
- upload_multipart(upload_url, field, file_name, size, stream): НЕ троттлится, stream_with_length ставит Content-Length (без него reqwest ушёл бы в chunked, который upload-сервер VK не принимает), таймаут UPLOAD_TIMEOUT (1 час, предел 2 GiB). Разбирается parse_upload_response, а не parse_envelope: upload-сервер отвечает голым объектом без конверта.
- get_stream(url): поток байтов вложения, НЕ троттлится, таймаут DOWNLOAD_TIMEOUT (10 минут). Тело НЕ вычитывается — Ф7 перекладывает его в файл кэша потоком.
- note_ok/note_down: здоровье. Повторный отказ НЕ сдвигает since (иначе окно 503 не кончалось бы никогда).

ТАЙМАУТЫ раздельные, потому что таймаут reqwest считается до конца чтения ТЕЛА: HTTP_TIMEOUT 30 c, CONNECT_TIMEOUT 10 c, LONGPOLL_TIMEOUT_SLACK 10 c, DOWNLOAD_TIMEOUT 600 c, UPLOAD_TIMEOUT 3600 c.

БЕЗОПАСНОСТЬ. Debug реализован вручную и печатает <redacted>. transport_error() зовёт reqwest::Error::without_url(): в query LongPoll лежит key — производный от токена секрет. В логи уходят метод, длительность, попытка и код ошибки, НИКОГДА не params. jitter_ms() берёт псевдослучайность из Uuid v4 — крейт rand не заводится.

СВЯЗИ: api::error (конверт и таблица), api::throttle (лимит), api::methods (обёртки), api::schema (формы), state::ConnHealth, config::VkConfig/SecretToken. Проверяется plugins/vk/tests/vk_stub.rs.
