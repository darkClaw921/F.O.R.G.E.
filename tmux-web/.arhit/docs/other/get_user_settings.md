# get_user_settings

REST-handler GET /api/user-settings в tmux-web/src/main.rs.

Возвращает текущий снимок UserSettings из state.user_settings.get(). Если файл ~/.forge/user_settings.json отсутствует или повреждён — возвращаются дефолтные значения (см. UserSettings::default).

Используется фронтендом для инициализации экрана настроек.
