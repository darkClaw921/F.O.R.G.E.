# message_exists

plugins/telegram/src/dto.rs

`pub fn message_exists(msg: &grammers_client::message::Message) -> bool`

Существует ли сообщение на самом деле: `matches!(msg.raw, tl::enums::Message::Message(_) |
tl::enums::Message::Service(_))`.

ЗАЧЕМ. `messages.getMessages` отдаёт на удалённый id не пустоту, а `messageEmpty`, и grammers
оборачивает его в обычный `Message`. Поэтому `Option::is_some` тут ничего не доказывает: `text()`
вернёт "", `date()` — эпоху. Без этой проверки вместо честного 404 / «сообщение удалено» клиент
получил бы пустое сообщение и пустую цитату (что читается как баг вёрстки).

Проверять `Option::is_some` вручную вызывающему не надо: reply_originals_index фильтрует сам,
а роут get_message использует предикат как `.filter(dto::message_exists)`.

СВЯЗИ: reply_originals_index, dialogs.rs::get_message, dialogs.rs::enrich_single_reply.
