# ReplyMarkupDto

plugins/telegram/src/dto.rs — DTO клавиатуры бота (поле MessageDto::reply_markup, D2).

Дословный контракт с фронтом (порядок ключей в JSON зафиксирован, все Option сериализуются
явным null, ни одного skip_serializing_if):
{
  "kind": "inline" | "keyboard" | "hide" | "force_reply",
  "rows": [[ReplyButtonDto]],
  "resize": bool,
  "single_use": bool,
  "persistent": bool,
  "placeholder": string|null
}

СЕМАНТИКА kind (это ДВЕ РАЗНЫЕ сущности Telegram, и фронт рисует их по-разному):
- "inline"  — replyInlineMarkup: клавиатура ПОД конкретным сообщением, живёт вместе с ним,
              кнопки нажимаются через POST .../callback;
- "keyboard" — replyKeyboardMarkup: СОСТОЯНИЕ ДИАЛОГА, заменяет системную клавиатуру; фронт
              рисует её полосой над полем ввода (botkb.js), а не под сообщением;
- "hide"    — replyKeyboardHide: снять клавиатуру диалога. rows ВСЕГДА пусты;
- "force_reply" — replyKeyboardForceReply: «ответь на это сообщение». rows ВСЕГДА пусты.

Флаги resize / single_use / persistent / placeholder осмысленны только у "keyboard"; у остальных
kind они приезжают в дефолтных значениях. Поле selective СОЗНАТЕЛЬНО не переносится: оно про
адресность клавиатуры внутри группы, а в личном UI решения не меняет.

derive: Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize (Deserialize нужен тестам
и фронтовым фикстурам).

Строится функцией reply_markup_dto. Потребители на фронте: botkb.js::normalizeMarkup,
buildMessageKeyboard, latestReplyMarkup, syncReplyBar.
