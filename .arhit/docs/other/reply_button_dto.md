# reply_button_dto

plugins/telegram/src/dto.rs — маппер tl::enums::KeyboardButton -> ReplyButtonDto.

Сигнатура: reply_button_dto(button: &grammers_tl_types::enums::KeyboardButton) -> ReplyButtonDto.

Покрывает все 18 конструкторов кнопок layer 227 и ОБЕ формы switch_inline:
- KeyboardButton                  -> kind "text"          (обычная кнопка reply-клавиатуры: нажатие = отправка её подписи)
- KeyboardButtonCallback          -> kind "callback",     data = base64_encode(байты), requires_password переносится
- KeyboardButtonUrl               -> kind "url",          url
- KeyboardButtonUrlAuth           -> kind "login_url",    url
- KeyboardButtonSwitchInline      -> kind "switch_inline" | "switch_inline_current" (по флагу same_peer), query
- KeyboardButtonRequestPhone      -> kind "request_phone"
- KeyboardButtonRequestGeoLocation-> kind "request_location"
- KeyboardButtonRequestPoll       -> kind "request_poll"
- KeyboardButtonWebView / SimpleWebView -> kind "web_view", url
- KeyboardButtonBuy               -> kind "buy"
- KeyboardButtonGame              -> kind "game"
- KeyboardButtonUserProfile / RequestPeer / RequestUsers / RequestPeer* -> kind "user_profile"
  (у профиля url = "tg://user?id=<id>")
- прочее                          -> kind "other"

КЛЮЧЕВОЕ РЕШЕНИЕ: матч заканчивается ветвью `_ => {}`, а не исчерпывающим перебором. Bump layer'а
grammers добавит новые конструкторы — с исчерпывающим матчем это была бы ошибка компиляции у
пользователя на ровном месте, а так кнопка приезжает как "other" с СОХРАНЁННЫМ текстом: фронт
нарисует её неактивной и объяснит причину, но переписка не потеряет ряд кнопок и не поедет по
вёрстке.

ИНВАРИАНТ: data заполняется ТОЛЬКО у kind=="callback" — фронт по этому полю решает, слать ли
POST .../callback.
