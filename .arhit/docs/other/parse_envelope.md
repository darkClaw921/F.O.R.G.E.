# parse_envelope

plugins/vk/src/api/error.rs. ЕДИНСТВЕННОЕ место, где JSON от VK превращается в Result. Сигнатура: parse_envelope(status: u16, body: &[u8]) -> Result<serde_json::Value, VkError>. Чистая: ни логов, ни состояния, ни сети.

Пять веток В ПОРЯДКЕ ПРОВЕРКИ: status != 200 -> VkError::Http (трактуем как транспорт); тело не JSON -> Decode; ключ error -> Api; ключ response -> Ok(value); иначе -> Decode. Ошибка проверяется ПЕРЕД успехом: у VK бывают ответы с обоими ключами.

КРИТИЧНО: перед построением VkError::Api из объекта ошибки ЦЕЛИКОМ удаляется поле request_params — VK возвращает там ваши же параметры запроса, включая access_token. Это самая вероятная утечка токена во всём плагине. Тест error_request_params_are_stripped.
