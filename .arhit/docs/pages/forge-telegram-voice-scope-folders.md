# Чекбокс авторазбора Telegram молчал при выбранной папке

## Симптом
В настройках Telegram включён «Автоматически разбирать новые аудиосообщения», но ни одно входящее голосовое не расшифровывается.

## Причина
В user_settings.json было:
- telegram_voice_autotranscribe = true
- telegram_voice_scope_folders = ["folder:10"]
- telegram_voice_scope_dialogs = []

Скоуп СЧИТАЕТСЯ ДЕЙСТВУЮЩИМ, как только хоть один из двух списков непуст (Prefs::scope_active — тумблера у скоупа нет по построению). А плагин Telegram собирал Candidate с peer_kind: None, потому что «принадлежность чата папке из одного dialog_id не следует». Дальше Prefs::peer_in_scope при действующем скоупе и неизвестной группе возвращает false — консервативная сторона отказа. Итог: false для ЛЮБОГО чата, ни одного разбора, в логе на каждое голосовое reason=out_of_scope.

То есть форма позволяла выбрать папки, а плагин физически не мог с ними сверяться: выбор папки в скоупе Telegram работал как «выключить авторазбор полностью». У VK и Bitrix такого нет — они передают вид беседы, выводимый из id.

## Что сделано
1. forge-voice: Prefs::peer_visible / peer_in_scope принимают СРЕЗ групп (&[&str]) вместо Option<&str> одного вида; совпадения любой группы достаточно (any_group). Candidate.peer_kind → Candidate.peer_groups. VK и Bitrix передают срез из одного элемента.
2. dto Telegram: dialog_category / dialog_in_folder / dialog_group_keys — зеркало фронтового format.js::dialogInFolder, плюс ключи inbox/archive/folder:<id> ровно того формата, что пишет форма.
3. voice_auto Telegram: dialog_groups — папки из кэша состояния (TelegramState::folders, один messages.getDialogFilters, сброс по updateDialogFilter* через CacheEffect::FoldersChanged) + сам чат одним messages.getPeerDialogs (updates::fetch_peer_dialog). Запрос только когда действует хоть один белый список и только для голосовых.
4. updates: peer_dialogs_flags разложен на чистый peer_dialogs_dto + сетевой fetch_peer_dialog, которым теперь пользуются и ленивый резолв флагов уведомлений, и авторазбор.

## Что осталось намеренно
- Сторона отказа консервативна: не смогли разрешить папки — группы считаются неизвестными, разбирается лишь чат, отмеченный поимённо.
- Пустой скоуп по-прежнему означает «разбирать везде».
- Видимость остаётся вышестоящим фильтром: скрытый чат не разбирается, даже попав в скоуп.