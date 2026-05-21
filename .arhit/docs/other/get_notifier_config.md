# get_notifier_config

GET/PUT/PATCH /api/notifier-config — REST endpoints для глобального NotifierConfig (~/.config/forge/notifier.json).

## Handlers (tmux-web/src/main.rs)

### get_notifier_config(State) -> Json<NotifierConfig>
Возвращает текущий снимок. При отсутствии файла или ошибке парсинга — defaults (template="", delay=0, wait_previous=false, session=None) — это zero-config состояние.

### put_notifier_config(State, Json<NotifierConfig>) -> Json<NotifierConfig>
Полная замена. Все поля обязательны в body. Atomic save. 500 при ошибке записи.

### patch_notifier_config(State, Json<PatchNotifierConfigReq>) -> Json<NotifierConfig>
Частичный update. Только Some-поля применяются. session="" ⇒ сброс в None.

## Route registration
Регистрируется в main.rs до auth middleware (защищается bearer-auth в remote mode, как и /api/user-settings).
