# reply_forward_json

plugins/vk/src/api/methods.rs::reply_forward_json(peer_id: i64, cmid: i64) -> String — параметр forward метода messages.send для ответа, адресованного conversation_message_id.

ЗАЧЕМ. У VK две нумерации сообщений: сквозная по аккаунту (message.id) и внутренняя для беседы (conversation_message_id). Параметр reply_to метода messages.send понимает ТОЛЬКО первую, а фронт адресует ответ второй — потому что именно cmid приезжает в reply_message каждой цитаты и одинаков у всех участников беседы. Мост между ними — forward с is_reply.

ФОРМА: {"peer_id":P,"conversation_message_ids":[C],"is_reply":1}. conversation_message_ids обязан содержать РОВНО ОДИН идентификатор (с is_reply список из двух VK отвергает — это уже пересылка). peer_id дублирует адресата намеренно: без него VK ищет оригинал не там и отвечает «сообщение не найдено». Строка собирается format!-ом, а не serde_json::json!, ради буквальности: набор и порядок полей — часть контракта с VK.

ИСПОЛЬЗУЕТСЯ: routes/dialogs.rs::post_message — при заданном SendMessageReq.reply_to_cmid строит forward и НЕ отправляет reply_to вовсе (обе формы одновременно VK не принимает); при отсутствии cmid остаётся прежний путь через reply_to = reply_to_id.

ТЕСТЫ: api::methods::tests::reply_forward_has_exactly_one_id_and_the_is_reply_flag (форма строки, валидность JSON, отрицательный peer_id сообщества) и стенд vk_stub::reply_is_addressed_by_cmid_not_by_message_id (при обоих полях в теле побеждает cmid; мутация «слать cmid в reply_to» краснеет).
