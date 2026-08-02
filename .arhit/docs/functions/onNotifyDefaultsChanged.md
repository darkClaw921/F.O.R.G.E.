# onNotifyDefaultsChanged

tmux-web/static/js/telegram/main.js — обработчик нового WS-кадра notify_defaults_changed
({"type","users","chats","broadcasts"}).

Сигнатура: onNotifyDefaultsChanged(msg) -> boolean (true, если перезагрузка списка запущена;
false — уже идёт).

Что делает: запоминает дефолты в tgState.notifyDefaults, СБРАСЫВАЕТ производные карты
(local.flagsExtra, local.pendingUnread — они посчитаны по СТАРЫМ дефолтам) и ПЕРЕЧИТЫВАЕТ список
диалогов.

ПОЧЕМУ ПЕРЕЧИТЫВАЕТ, А НЕ ПЕРЕСЧИТЫВАЕТ: пересчитать mute поимённо на фронте НЕЛЬЗЯ — признака
«настройки унаследованы» в DialogDto нет (там уже разрешённый bool), а перечислить все диалоги
категории сервер не может (их тысячи, и он их все не знает). Поэтому кадр несёт только новые
дефолты, а список приезжает заново.

Флаг defaultsReloading не даёт пачке кадров (пользователь щёлкает настройки на телефоне)
наплодить параллельные запросы.

Смежное состояние: tgState.notifyDefaults {users, chats, broadcasts} в state.js — сбрасывается в
clearTgAccountData() вместе с остальными данными аккаунта.
