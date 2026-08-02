# LoginFlow

plugins/telegram/src/login.rs — конечный автомат авторизации личного аккаунта Telegram (Фаза 2, полная реализация поверх grammers 0.10).

СТРУКТУРА МОДУЛЯ. Разделён на чистую и сетевую половины:
- чистая: enum LoginFlow (NotConfigured | Disconnected{error} | CodeSent{token: Option<LoginToken>, phone} | PasswordRequired{token: Option<PasswordToken>, hint} | Authorized{user}) + функции-переходы request_code/submit_code/submit_password/sign_out/on_auth_invalidated. Работают через trait LoginBackend и ничего не знают о сети, поэтому матрица переходов покрыта unit-тестами без единого сетевого вызова;
- сетевая: MtBackend — тонкая обёртка над grammers_client::Client, переводящая его типы и ошибки в термины модуля.
Каждый успешный переход идёт через TelegramState::set_login, то есть broadcast'ит login_state всем WS-подписчикам под тем же lock'ом, что и запись.

ТОКЕНЫ. LoginToken/PasswordToken — собственные обёртки над типами grammers: Debug печатается как <redacted>, наружу уходит только LoginStateDto без токенов (тест dto_never_leaks_tokens). Конструктор MtLoginToken в grammers приватен, поэтому у обёрток есть cfg(test)-заглушки stub()/stub_tag() — по ним тесты отличают 'тот же токен' от 'новый токен, пришедший вместе с ошибкой'. Токены живут только в памяти: рестарт посреди логина означает начать заново (осознанный размен — хранить полузавершённый логин на диске опаснее).

ГРАБЛИ GRAMMERS 0.10, РАДИ КОТОРЫХ ТОКЕНЫ — Option:
- Client::sign_in берёт &LoginToken и НЕ расходует его: после неверного кода тот же токен годится для повтора, поэтому submit_code кладёт токен обратно в CodeSent и пользователь вводит код заново без перезапуска wizard'а;
- Client::check_password ПОГЛОЩАЕТ PasswordToken, а при неверном пароле возвращает НОВЫЙ токен внутри SignInError::InvalidPassword. Его обязательно нужно положить обратно (PasswordStep::Invalid -> PasswordRequired{token: Some(new)}), иначе одна опечатка стоила бы всего логина. Если check_password упал по другой причине (FLOOD_WAIT, обрыв) — токен потерян безвозвратно, в состоянии остаётся PasswordRequired{token: None}, и UI обязан начать логин заново.
- CodeSent.token тоже Option: токен физически изымается из состояния на время RPC, чтобы не держать Mutex логина через сетевой вызов (иначе GET /status и первый кадр WS вставали бы на всю длительность RPC). Параллельный login/code в этот момент честно получает invalid_state, а не гонку.

LoginError{InvalidState|BadInput|FloodWait{secs,message}|SignUpRequired|NotConnected} + from_invocation: 420 -> FloodWait, 400 -> BadInput, 401 -> InvalidState, прочее -> NotConnected. Маппинг в HTTP — единственный, живёт в routes/error.rs (impl From<LoginError> for ApiError), чтобы контракт нельзя было разъехать между четырьмя роутами.

on_auth_invalidated(state, reason): перевод в Disconnected{error} при AUTH_KEY_UNREGISTERED, разлогине с другого устройства, смене пароля (риск №4 плана). Вызывается updates-воркером (Фаза 3) и восстановлением сессии при старте. Идемпотентна. Источник истины про наличие кредов — state.has_credentials().await (а не config.has_credentials(), который знает только про старт процесса).

ТЕСТЫ: 12 кейсов матрицы переходов, включая 'неверный пароль дважды подряд (с проверкой, что в состояние легли именно новые токены pt-2/pt-3), затем верный -> Authorized', неверный код с сохранением токена, FLOOD_WAIT без смены состояния, потерю токена, шаги вне состояния, пустой ввод, broadcast каждого перехода в правильном порядке.
