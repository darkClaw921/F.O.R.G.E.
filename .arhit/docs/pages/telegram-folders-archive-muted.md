Бэкенд Telegram-плагина: папки, архив и mute (дефекты 1–2 живой проверки).

## DialogDto (plugins/telegram/src/dto.rs)
Новые поля (после pinned, до last_message): muted: bool (notify_settings сырого Dialog: silent==true ИЛИ mute_until>now; mute_until сравнивается в i64 — «навсегда» i32::MAX без переполнения), archived: bool (folder_id==1), folder_id: number|null (0/null — основной список, 1 — архив). Option — явный null (инвариант модуля).

## GET /api/telegram/folders (routes/dialogs.rs)
Ответ {"folders":[FolderDto]} из raw messages.getDialogFilters. FolderDto: id,title,emoticon|null,pinned_ids[],include_ids[],exclude_ids[] (строки Bot API-формата, как DialogDto.id),contacts,non_contacts,groups,broadcasts,bots,exclude_muted,exclude_read,exclude_archived. Вариант Default пропускается (не папка), Chatlist — include-флаги/исключения false/пустые. Без папок — пустой массив.

## GET /api/telegram/dialogs?folder=main|archive|all
folder отсутствует/all — прежний путь iter_dialogs (grammers 0.10 не даёт задать folder_id: поле request приватно). main/archive — raw messages.getDialogs{folder_id:0|1} с пагинацией как в DialogIter (offset_date/offset_id/offset_peer, exclude_pinned со 2-й страницы); маппинг через dto::RawPeerIndex + dto::from_raw_dialog/from_raw_message (зеркала клиентских мапперов; Media::from_raw публичен и переиспользует media_dto). Кэш peer'ов пополняется из сырых users/chats (PeerAuth::from_hash), кэш непрочитанного при частичной выборке НЕ пересевается.

## WS-события (ws/protocol.rs, updates.rs)
notify_settings {dialog_id, muted} — из UpdateNotifySettings (только NotifyPeer::Peer; класс-wide NotifyUsers/Chats/Broadcasts и форумные топики пропускаются). folders_changed (без полей) — из UpdateDialogFilter | UpdateDialogFilterOrder | UpdateDialogFilters; фронт перезапрашивает /folders. Чистая часть — updates::notify_settings_event(u, now).

Тесты: +20 в cargo test -p forge-telegram --lib (всего 377 зелёных).