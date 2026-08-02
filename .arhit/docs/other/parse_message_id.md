# parse_message_id

plugins/telegram/src/routes/dialogs.rs

`pub fn parse_message_id(mid: &str) -> Result<i32, ApiError>`

Разбор `:mid` из пути в id сообщения. Trim'ит, поэтому "007" и "+7" дают 7.

Не число либо не положительное ⇒ `404 unknown_message` (не 400!). Причина та же, что в
validate_callback: id сообщений у Telegram начинаются с единицы, «нулевого» сообщения не существует
ни в одном диалоге, и для фронта это тот же исход, что «сообщение удалили» — надо показать
«сообщение недоступно», а не ошибку ввода.

СВЯЗИ: get_message, validate_callback, ApiError::unknown_message.
