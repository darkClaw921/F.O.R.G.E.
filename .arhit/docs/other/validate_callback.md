# validate_callback

plugins/telegram/src/routes/dialogs.rs — чистая валидация нажатия inline-кнопки (без сети).

Сигнатура: validate_callback(mid: &str, req: &CallbackReq) -> Result<CallbackPlan, ApiError>.
CallbackPlan { msg_id: i32, data: Option<Vec<u8>>, game: bool }.
CallbackReq { data: Option<String>, game: bool } (Deserialize, Default).

ПРАВИЛА:
- :mid обязан быть ПОЛОЖИТЕЛЬНЫМ i32, иначе 404 unknown_message. Именно 404, а не 400: для фронта
  это тот же исход, что «сообщение удалили» — погасить клавиатуру, а не просить исправить ввод.
- data декодируется dto::base64_decode; не-base64 ⇒ 400 invalid_input.
- Лимит MAX_CALLBACK_DATA = 64 считается по ДЕКОДИРОВАННЫМ байтам. Сверка по длине строки резала
  бы легальные payload'ы: base64 раздувает данные на треть.
- Строка из одних '=' приравнена к отсутствию поля (пустой payload после декодирования).
- «Ни data, ни game» ⇒ 400: такой запрос Telegram всё равно отвергнет, но круглый путь до серверов
  делать незачем.

Чистая и полностью покрыта unit-тестами — вся защита от мусора проверяется без единого RPC.
