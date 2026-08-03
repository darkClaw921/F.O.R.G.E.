# plugins/vk/src/routes/login.rs

REST-поверхность логина и captcha плагина VK (Ф3).

Роуты (смонтированы в routes::build_router, пути совпадают с таблицей в шапке routes/mod.rs):
- POST /api/vk/login/token {access_token} -> LoginStateDto. Порядок: parse_token_input (мусор -> 400 invalid_input ДО касания диска) -> save_token (атомарно, 0600, read-modify-write чужих ключей vk.toml) -> set_token в состоянии (сбрасывает кэш клиента) -> verify_token. Сохранение ПЕРЕД проверкой намеренно: пользователь, закрывший вкладку на середине, остаётся с введённым токеном.
- POST /api/vk/login/verify {captcha_sid?, captcha_key?} -> LoginStateDto. Кнопка «повторить» на экране disconnected, ответ на captcha, ручная проверка после правки прав токена. Без токена — 409 not_authorized.
- POST /api/vk/logout -> LoginStateDto(not_configured). В VK НЕ ХОДИМ: у пользовательского токена нет метода «отозвать себя» (auth.logout работает по логину/паролю). Порядок обратный логину: сначала диск, потом память. Забывает запомненные captcha.
- POST /api/vk/captcha {sid, key} -> LoginStateDto. Перепроверяет токен с ответом; успех переводит в Authorized, а set_login будит LongPoll-воркера через authorized_notify.
- GET /api/vk/captcha/:sid -> байты картинки. ПРОКСИ: адрес берётся ТОЛЬКО из VkState::captcha_imgs (карта наполняется исключительно из ответов VK), поэтому открытым прокси (SSRF) роут стать не может. Тело читается кусками с потолком MAX_CAPTCHA_BYTES=512 КБ, требуется Content-Type image/*, ответ помечается Cache-Control: no-store (картинка одноразовая).

Константа TOKEN_HINT — текст подсказки «где взять токен», уходящий в StatusDto.hint. Живёт на бэкенде, потому что правила выдачи scope messages у VK меняются, а разметку браузер держит в кэше PWA.

Инвариант секрета: тело POST /login/token содержит секрет целиком; ни хендлер, ни parse_token_input не пишут его в лог. Проверяется тестом login_route_keeps_the_token_and_email_out_of_the_logs в plugins/vk/tests/vk_stub.rs и построчным grep-тестом no_source_line_logs_the_exposed_token в lib.rs.

Captcha: общего авто-решателя нет. Единственный корректный повтор — тот же запрос плюс captcha_sid+captcha_key, а «тот же запрос» на уровне VkClient не восстанавливается. Схема сквозная: VkError::Api{code:14} -> 409 captcha_required с полями -> фронт добавляет два поля к тому же вызову.
