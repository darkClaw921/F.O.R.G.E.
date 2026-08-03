# plugins/vk/src/api/error.rs

Ошибки транспорта VK, разбор конверта и ПОЛНАЯ таблица кодов. Все функции чистые.

VkError: Transport{reason} | Http{status} | Decode{reason} | Api{code,msg,captcha_sid,captcha_img,redirect_uri}. Методы: code(), is_transport(), captcha().

parse_envelope(status: u16, body: &[u8]) -> Result<Value, VkError> — ЕДИНСТВЕННОЕ место, где JSON VK становится Result. Правила В ПОРЯДКЕ ПРОВЕРКИ: 1) status != 200 -> Http (трактуем как транспорт: сам VK так почти не отвечает, это прокси/captive portal); 2) не JSON -> Decode; 3) ключ error -> Api; 4) ключ response -> Ok; 5) иначе -> Decode. Пункт 3 ПЕРЕД 4 намеренно: у VK бывают ответы с обоими ключами (частичный успех execute), приоритет ошибки честнее.

ИНВАРИАНТ БЕЗОПАСНОСТИ ФАЗЫ. В error.request_params VK возвращает ваши же параметры запроса, ВКЛЮЧАЯ access_token — самая вероятная утечка токена во всём плагине. api_error_from_value() выбрасывает поле ЦЕЛИКОМ первым же действием, до чтения любого другого поля. Целиком, а не маской по имени access_token: состав ключей нигде не документирован и может пополниться. Тест error_request_params_are_stripped кормит 4 формы (плоский список пар, секрет под чужим ключом, вложенный объект, массив объектов) и проверяет Debug, Display и сериализованное тело HTTP-ответа.

parse_upload_response(status, body) — отдельная функция для upload-сервера: он НЕ API и отвечает голым объектом {photo,server,hash} без конверта response. Прогон через parse_envelope дал бы Decode на исправной загрузке. request_params вырезаются и здесь.

classify(code: i64) -> ApiError — чистая таблица кода VK в HTTP + code + retry_after_secs. 1/2/3/23 -> 502 vk_error; 5 и 27/28 -> 409 not_authorized; 6 -> 429 flood_wait/1; 9 -> 429 flood_wait/60; 10 -> 503 not_connected; 14 -> 409 captcha_required; 15 -> 409 no_messages_scope; 17 -> 409 validation_required; 18/30/113/917/936 -> 404 unknown_dialog; 29 -> 429 flood_wait/3600; 100/913/914/921 -> 400 invalid_input; 900/901/902 -> 409 send_forbidden; 909/925 -> 409 invalid_state; остальное -> 502 vk_error без паники.

to_api_error(&VkError) -> ApiError — полное отображение, включая транспортные ветки (503 not_connected) и подстановку captcha_sid/captcha_img/redirect_uri, которых из одного кода не вывести. Текст VK ДОБАВЛЯЕТСЯ к каноническому, не заменяет его.

Retry { Never, AfterNetworkBackoff, AfterOneSecond } + retry_for(&VkError). Транспорт, HTTP 5xx и код 10 — один повтор через NETWORK_BACKOFF (500 мс) + джиттер 0-250 мс. Код 6 — один повтор через FLOOD_RETRY_DELAY (1 c): это промах НАШЕГО троттлера, молча починить честнее, чем показать 429 за свою ошибку; при второй шестёрке отдаём 429 и пишем warn. Коды 9 и 29 — НИКОГДА: автоповтор продлевает бан (прямой аналог FLOOD_WAIT у Telegram). HTTP 4xx — тоже никогда: прокси так и будет отвечать.

ОТЛИЧИЕ ОТ TELEGRAM: ошибка VK приходит с HTTP 200, статус не значит ничего.

Тесты: full_vk_code_contract_table (28 кодов + 0/-7/4242/i64::MAX), transport_rows_of_the_contract_table, flood_codes_are_never_retried, 5 тестов веток parse_envelope.
