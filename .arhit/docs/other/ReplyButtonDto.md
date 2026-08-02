# ReplyButtonDto

plugins/telegram/src/dto.rs — DTO одной кнопки клавиатуры бота (элемент ReplyMarkupDto::rows).

Контракт (порядок ключей зафиксирован, все Option → явный null):
{
  "text": string,
  "kind": "callback"|"url"|"switch_inline"|"switch_inline_current"|"text"|"request_phone"
         |"request_location"|"request_poll"|"web_view"|"buy"|"game"|"login_url"
         |"user_profile"|"other",
  "data": string|null,   // base64 (standard alphabet, С padding) от callback-data;
                         // ТОЛЬКО при kind == "callback"
  "url": string|null,    // url / login_url / web_view / user_profile
  "query": string|null,  // switch_inline / switch_inline_current
  "requires_password": bool
}

ПОЧЕМУ data — base64. callback_data у Telegram это произвольные БАЙТЫ (до 64), а не UTF-8:
класть их в JSON-строку как есть нельзя. Фронт трактует значение как НЕПРОЗРАЧНЫЙ токен и не
декодирует его — просто возвращает обратно в POST .../callback.

kind == "other" — это НЕ ошибка, а страховка forward-compat: маппер reply_button_dto
заканчивается ветвью `_ => {}`, поэтому новый конструктор кнопки из будущего layer'а не ломает
сборку и не теряет кнопку — она приедет с СОХРАНЁННЫМ текстом и будет нарисована визуально
неактивной с честной причиной.

requires_password == true (keyboardButtonCallback.requires_password) фронт рисует неактивной:
плагин не поддерживает 2FA-подтверждение нажатия, а Telegram на такой запрос без пароля отвечает
PASSWORD_MISSING ⇒ честный 400.

keyboardButtonUserProfile отдаётся как kind="user_profile" + url="tg://user?id=<id>", чтобы
фронту не приходилось знать про схему ссылок на профиль.

derive: Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize.
Строится функцией reply_button_dto. Потребители: botkb.js::normalizeButton / buttonAction.
