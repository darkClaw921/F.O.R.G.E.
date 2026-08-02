# onNotifySettings

Обработчик WS notify_settings (static/js/telegram/dialogs.js): точечно патчит DialogDto.muted, переключает класс .tg-muted и метку [M] (mutedMark) на строке списка без полной перерисовки, пересчитывает счётчики папок renderDirs(). Бейдж ✈ пересчитывает main.js: recomputeUnread() пересобирает local.muted из mutedIds(tgState.dialogs) — единый источник истины про mute это поле muted в DialogDto. folders_changed → dialogs.loadFolders() (перезапрос GET /folders + полная перерисовка).
