# reply_markup_dto

plugins/telegram/src/dto.rs — маппер tl::enums::ReplyMarkup -> ReplyMarkupDto.

Сигнатура: reply_markup_dto(markup: &grammers_tl_types::enums::ReplyMarkup) -> ReplyMarkupDto.

Покрывает ВСЕ четыре контейнера схемы:
- ReplyInlineMarkup   -> kind "inline",      rows из строк (KeyboardButtonRow -> Vec<ReplyButtonDto>);
- ReplyKeyboardMarkup -> kind "keyboard",    rows + resize/single_use/persistent/placeholder;
- ReplyKeyboardHide   -> kind "hide",        rows ПУСТЫЕ (по контракту с фронтом);
- ReplyKeyboardForceReply -> kind "force_reply", rows ПУСТЫЕ, placeholder переносится.

Поле selective не переносится СОЗНАТЕЛЬНО (адресность клавиатуры внутри группы; в личном UI
решения не меняет, а лишнее поле в контракте пришлось бы поддерживать вечно).

Вызывается из from_grammers_message (через msg.reply_markup()). В from_raw_message поле всегда
null: у messageService по схеме reply_markup нет вовсе.

Каждая кнопка проходит через reply_button_dto.
