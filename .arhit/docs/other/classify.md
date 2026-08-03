# classify

plugins/vk/src/api/error.rs. classify(code: i64) -> ApiError — чистая таблица кода ошибки VK в контракт REST плагина: HTTP-статус, машинный code и retry_after_secs. Сообщение каноническое (текст VK подмешивает to_api_error), captcha_sid/redirect_uri не заполняются — из одного кода их не вывести. Неизвестный код даёт 502 vk_error без паники. Полная таблица и её тест — full_vk_code_contract_table.
