# js/vk/format.js

tmux-web/static/js/vk/format.js — чистые функции форматирования панели VK (Ф4.6). Грузится в Node как есть, покрыт tests/frontend/vk_format.test.mjs (196 проверок).

КЛЮЧЕВОЕ ОТЛИЧИЕ ОТ telegram/format.js: mediaKey(dialogId, messageId, INDEX, kind). У сообщения VK до десяти вложений, и телеграмный ключ без индекса не уникален — второе фото затирало бы первое и в mediaCache, и в учёте objectURL. Тот же индекс попадает в mediaFileName.

ПЕРЕПИСАНО: slugName (суффиксы user->node, chat->chat, community->club), dialogStatusLine ([CHAT] N members / [CLUB] / [ONLINE]|[IDLE] last 3h|[OFFLINE]) — счётчика «N msgs» НЕТ и быть не может: у VK message.id сквозной по аккаунту, и «74212 msgs» в новой беседе это ложь; splitDialogs (сообщества = peer_id<0); mediaKindLabel/mediaGlyph/MIME_EXT/KIND_EXT (виды VK, включая graffiti и unsupported); MAX_UPLOAD_BYTES стал картой UPLOAD_LIMITS по видам загрузки + uploadRejectReason(file, kind, limits).

ПОЯВИЛОСЬ ВПЕРВЫЕ: peerKind(peerId) (тип выводится из САМОГО id: <0 сообщество, >=CHAT_PEER_BASE=2e9 беседа), dialogKind (перевод лексики DTO group -> UI community), cmidOf/oldestCmid, captchaFromError (адрес картинки ВСЕГДА через прокси /api/vk/captcha/:sid), captchaRetryBody, vkErrorLabel (17 кодов).

УБРАНО: весь блок тем (переехал в js/messenger/theme-core.js) и вся папочная логика — у VK папок нет, их место заняли 4 СЕРВЕРНЫХ фильтра, а серверный фильтр невозможно посчитать на фронте.

ПАГИНАЦИЯ ПО message.id (oldestId), а не по cmid: параметр VK называется start_message_id и принимает глобальный id; cmid адресует ответы (Ф6).
