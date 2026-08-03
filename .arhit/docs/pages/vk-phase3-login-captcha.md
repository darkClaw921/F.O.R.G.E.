Фаза 3 плагина VK: логин и captcha (эпик forge-vk-f3-4mmp).

ЧТО ПОЯВИЛОСЬ
Бэкенд: plugins/vk/src/login.rs получил сетевую половину (parse_token_input, LoginError, trait LoginBackend, HttpBackend, verify_token, on_auth_invalidated / on_scope_lost / on_captcha); новый plugins/vk/src/routes/login.rs (5 роутов); VkState получил api_client() (один клиент = один троттлер на токен), реестр captcha_imgs и forget_captchas; VkConfig получил config_path (тесты пишут в tempdir, а не в реальный ~/.config/forge/vk.toml); StatusDto получил hint; spawn_workers запускает стартовую проверку токена.
Фронт: новый общий слой js/messenger/{http-envelope,ws-core}.js; новые js/vk/{api,ws,login,captcha}.js; main.js переведён на них и поднимает постоянный WS в гейте.

ФОРМАТ ВВОДА ТОКЕНА — главное решение фазы
Пользователь копирует адресную строку redirect-URL ЦЕЛИКОМ (https://oauth.vk.ru/blank.html#access_token=...&expires_in=0&user_id=...&email=...). Поле принимает три формы: полный URL, голый фрагмент, просто токен. Разбор — ОДНА чистая функция и ТОЛЬКО на бэкенде: вторая копия парсера на фронте была бы вторым местом, где можно ошибиться с секретом. email отбрасывается сразу (поля под него в структуре нет), user_id берётся как подсказка (источник истины — users.get), expires_in=0 означает бессрочный токен.

СЕМЬ СОСТОЯНИЙ И ПОЧЕМУ ИХ СЕМЬ
NotConfigured | Checking | Disconnected | InvalidToken | NoMessagesScope | CaptchaRequired | Authorized. Два добавлены против Telegram и оба не косметика: NoMessagesScope (VK 15) — отдельный ЭКРАН, потому что VK не выдаёт право messages обычным standalone-приложениям, и пользователь, увидевший вместо внятного объяснения общее «что-то пошло не так», будет вставлять один и тот же рабочий токен по кругу; Checking — потому что проверка токена это сетевой вызов из spawn_workers, и без него UI мигал бы «отключено» на каждом старте.
Порядок в verify_token: users.get (покажет код 5) -> messages.getConversations count=1 (покажет 15). ОБРАТНЫЙ ПОРЯДОК ДАЛ БЫ «нет scope» НА ПРОТУХШЕМ ТОКЕНЕ.

CAPTCHA
Оверлей #vk-captcha живёт ВНЕ #vk-login (z-index 7): код 14 прилетает на любой метод, а не только на логин. Картинка грузится через прокси GET /api/vk/captcha/:sid — браузер на серверы VK не ходит; прокси принимает только sid и ищет адрес в реестре, наполняемом исключительно ответами VK, поэтому открытым прокси стать не может. Повтор ровно один. Пока оверлей открыт, isCaptchaOpen() замораживает фоновые запросы (иначе они породят вторую captcha).

ИНВАРИАНТЫ УТЕЧКИ (4 проверки)
status_never_leaks_token_in_any_state (все 7 состояний, живой ответ /status + оба DTO), debug_of_config_is_redacted, «канарейка и email не появляются ни в логах, ни в vk.toml, ни в теле ответа» на полном пути REST->сеть->код 5, и построчный grep по src/**.rs, запрещающий .expose() рядом с tracing-макросом. Все четыре проверены мутациями.