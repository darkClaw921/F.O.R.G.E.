# reply_markup_from_raw

plugins/vk/src/dto.rs — разбор message.keyboard ВКонтакте в ReplyMarkupDto (Ф9).

ЧТО ДЕЛАЕТ. Принимает KeyboardRaw (api/schema.rs: {one_time, inline, author_id, buttons: Vec<Vec<KeyboardButtonRaw>>}) и возвращает Option<ReplyMarkupDto>. None — когда рисовать нечего (пустая матрица buttons): пустая рамка под сообщением читалась бы как сломанная клавиатура. Зовётся из dto::message_from_raw через msg.keyboard.as_ref().and_then(reply_markup_from_raw).

ФОРМА РЕЗУЛЬТАТА.
  ReplyMarkupDto { kind: 'inline'|'keyboard', rows: Vec<Vec<ReplyButtonDto>>, single_use: bool }
  ReplyButtonDto { row: u8, col: u8, text: String, kind: String, color: Option<String>, url: Option<String>, supported: bool }
kind клавиатуры — это ровно флаг inline (у VK нет ни hide, ни force_reply, в отличие от Telegram); single_use — это one_time.

КООРДИНАТЫ ЕДУТ ЯВНО (row/col), а не выводятся из позиции в rows: фронт (js/vk/botkb.js::normalizeButton) выбрасывает кнопки без подписи и на этом сдвинул бы собственную нумерацию, а сервер (routes/dialogs.rs::post_button) ищет кнопку по координатам в СЫРОЙ клавиатуре, перечитанной из VK.

PAYLOAD НАРУЖУ НЕ ОТДАЁТСЯ ВОВСЕ. Нажатие адресуется координатами {row, col}, а label+payload сервер достаёт из сообщения сам — подделать полезную нагрузку с фронта нечем. Инвариант закрыт тестом dto::tests::button_payload_never_reaches_the_wire (сериализованный MessageDto не содержит ни ключа payload у кнопки, ни его значения).

ГРАНИЦЫ (совпадают с фронтовыми, чтобы обрезка случалась на одной стороне):
  MAX_KB_ROWS = 32, MAX_KB_ROW_BUTTONS = 12 — разметка приезжает от ЧУЖОГО сообщества, и «клавиатура» из десяти тысяч рядов подвесила бы вкладку;
  BUTTON_LABEL_CHARS = 64 — потолок подписи (видимую обрезку до 48 делает UI);
  BUTTON_KIND_CHARS = 32 — потолок строки action.type.
Кнопки без подписи НЕ выбрасываются: их отбросит фронт, а координаты соседей обязаны остаться сырыми. Пустые ряды отбрасываются уже ПОСЛЕ нумерации.

FORWARD-COMPAT: незнакомый action.type доезжает до UI строкой как есть и получает supported:false — сообщение с новым видом кнопки обязано остаться читаемым. Пустой action.type превращается в 'other'. Цвет отдаётся только из закрытого словаря primary/secondary/negative/positive.

Связанные: button_is_supported (решение о поддержке), SUPPORTED_BUTTON_KINDS, ReplyButtonDto, vk-button-support (страница про ограничения платформы), routes::dialogs::post_button.
