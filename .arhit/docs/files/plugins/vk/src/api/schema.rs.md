# plugins/vk/src/api/schema.rs

Сырые serde-структуры ответов VK — форма провода. Проекцию для UI (dto.rs) собирает Ф4.

ИНВАРИАНТ УСТОЙЧИВОСТИ РАЗБОРА (формы VK документированы неполно, особенно attachments/action/keyboard/reply_message):
1. Все поля объявлены #[serde(default)], кроме опознавательных id/peer_id/date у MessageRaw и id у ProfileRaw/GroupRaw. Подставить туда 0 значило бы испортить ключ медиа-кэша (peer_id + message_id + индекс).
2. deny_unknown_fields не используется НИГДЕ: новое поле VK не должно ронять страницу переписки.
3. Неизвестный attachment.type даёт kind unsupported, а не ошибку — AttachmentRaw::normalized_kind().
4. Неизвестный action.type сохраняется как есть: ActionRaw не перечисляет виды вовсе.
5. Ни одна структура НЕ выводит Serialize: слой ближайший к проводу, производная сериализация означала бы, что любую можно случайно отдать наружу вместе с access_key вложений.

ОТДЕЛЬНЫЙ ТИП ДЛЯ ВЛОЖЕННЫХ СООБЩЕНИЙ. fwd_messages и reply_message у VK НЕ несут id и peer_id: пересланное сообщение живёт в чужой беседе и адреса в текущей не имеет. Требовать у них ту же тройку означало бы ронять разбор ЦЕЛОЙ страницы истории из-за одной пересылки, поэтому существует NestedMessageRaw без обязательных полей вовсе (с рекурсией fwd_messages и reply_message внутри себя).

LongPollHistoryRaw.history — Vec<Vec<serde_json::Value>>, а не Vec<Vec<i64>>: события LongPoll разнородны внутри одного массива ([4, msg_id, flags, peer_id, ts, "текст", {…}]), строгий числовой тип ронял бы разбор на первом же новом сообщении.

СОСТАВ. Общие: ItemsRaw<T>, ConversationsRaw, ConversationWithMessageRaw, ConversationRaw, PeerRaw, SortIdRaw, CanWriteRaw, PushSettingsRaw, ChatSettingsRaw, ChatPhotoRaw. Сообщения: MessageRaw, NestedMessageRaw, ActionRaw, GeoRaw + Coordinates/Place. Клавиатура: KeyboardRaw, KeyboardButtonRaw, KeyboardActionRaw. Вложения: AttachmentRaw + PhotoRaw/PhotoSizeRaw/VideoRaw/AudioRaw/DocRaw/DocPreviewRaw/AudioMessageRaw/StickerRaw/GraffitiRaw/LinkRaw. Люди: ProfileRaw, LastSeenRaw, GroupRaw. LongPoll: LongPollServerRaw, LongPollHistoryRaw. Загрузка: UploadServerRaw, DocSaveRaw.

Словарь видов: KIND_PHOTO/VIDEO/AUDIO/VOICE/DOCUMENT/STICKER/GRAFFITI/OTHER/UNSUPPORTED. audio_message -> voice, doc -> document, link/wall/poll/market/story/call/... -> other, незнакомое -> unsupported.

ФИКСТУРЫ: plugins/vk/tests/fixtures/{get_conversations,get_history,get_by_id,get_longpoll_server,get_longpoll_history,error_auth_failed}.json, подключаются через include_str!.

Тесты: a_page_of_fifty_survives_an_unknown_attachment (страница из 50 сообщений с вложением из будущего разбирается ЦЕЛИКОМ), bare_minimum_message_falls_back_to_defaults (и обратное: без id/peer_id/date разбор ОБЯЗАН отказать), unknown_action_kind_is_preserved_verbatim, attachment_kind_table.
