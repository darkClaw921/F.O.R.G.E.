# post_button

plugins/vk/src/routes/dialogs.rs — POST /api/vk/dialogs/:id/messages/:mid/button (Ф9): нажатие кнопки сообщества по КООРДИНАТАМ.

ТЕЛО: {row, col} (PressButtonReq) + необязательные captcha_sid/captcha_key. ОТВЕТ: {message: MessageDto} — отправленное сообщение, той же формы, что в ленте.

ПОЧЕМУ КООРДИНАТЫ, А НЕ PAYLOAD. payload кнопки уходит сообществу как есть; принимай роут его из тела, любая вкладка (или XSS в соседней панели) отправила бы боту произвольную команду от имени пользователя. Клиент называет только МЕСТО кнопки, а label и payload сервер достаёт из свеже перечитанного сообщения (fetch_message_raw -> pick_button). Побочная выгода: устаревшая клавиатура (бот успел переписать сообщение) даёт честный отказ вместо нажатия «не той» кнопки.

ПОЧЕМУ РОУТ НАЗЫВАЕТСЯ /button, А НЕ /callback. Нажать под пользовательским токеном можно ровно один вид — text, и нажатие это обычная отправка messages.send с подписью кнопки и её payload (так делает официальный клиент). Callback-механики у клиента VK нет: messages.sendMessageEventAnswer — метод СТОРОНЫ СООБЩЕСТВА (им бот отвечает на уже полученный message_event, нужен ключ сообщества), а обратной, клиентской половины в публичном API нет ни в документации, ни в библиотеках. Аналога messages.getBotCallbackAnswer у VK не существует. Поэтому из контракта ошибок выпал bot_timeout (504) — ждать ответа бота негде.

ЛЕСТНИЦА ПРОВЕРОК (дословно как в остальных data-роутах):
  1. require_ready -> 409 not_authorized / 503 not_connected
  2. resolve_peer  -> 404 unknown_dialog
  3. parse_message_id -> 404 unknown_message
  4. press_coords (тело) -> 400 invalid_input; координат нет или они за потолками MAX_KB_ROWS/MAX_KB_ROW_BUTTONS -> отказ ДО единого обращения к VK
  5. fetch_message_raw -> 404 unknown_message (пустой items либо чужая беседа)
  6. pick_button по сырой матрице -> 400 invalid_input («кнопки уже нет»)
  7. plan_for(kind):
       Send (text)        -> messages.send(message=label, payload=payload, random_id один на нажатие) -> fetch_message -> 200
       ClientSide (open_link) -> 409 unsupported_button «кнопка-ссылка открывается браузером на стороне клиента»
       Unsupported        -> 409 unsupported_button «ВКонтакте не позволяет нажать кнопку этого типа из стороннего клиента (тип «callback»)»
Кнопка text без подписи -> 400 invalid_input: отправлять VK нечего.

ЧИСТЫЕ ПОМОЩНИКИ РЯДОМ: ButtonPlan + plan_for (вид -> план, сверен тестом с dto::button_is_supported), press_coords (обязательность и границы координат), pick_button (индексация сырой матрицы без паник).

ТЕСТЫ: routes::dialogs::tests::{button_plan_matches_the_support_table, press_coordinates_are_required_and_bounded, button_lookup_is_bounded_by_the_raw_matrix}; стенд plugins/vk/tests/vk_stub.rs — the_keyboard_reaches_the_client_without_any_payload, pressing_a_text_button_sends_its_label_and_payload, pressing_a_callback_button_is_refused_with_an_explanation, coordinates_outside_the_keyboard_send_nothing, a_payload_in_the_request_body_is_ignored, a_stale_keyboard_never_turns_into_a_random_message, pressing_a_button_without_a_token_hits_authorization_first.

Связанные: dto::reply_markup_from_raw, dto::button_is_supported, ApiError::unsupported_button, fetch_message_raw, js/vk/botkb.js, vk-button-support.
