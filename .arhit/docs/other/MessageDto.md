# MessageDto

plugins/telegram/src/dto.rs — DTO сообщения.

ПОЛЯ: id: i32, dialog_id: String (Bot API-формат, строка), out: bool, date: i64,
sender: Option<TgUserDto>, text: String, media: Option<MediaDto>, reply_to_id: Option<i32>,
reply_to: Option<ReplyToDto>, edit_date: Option<i64>, reply_markup: Option<ReplyMarkupDto>.

## reply_to (добавлено под жалобу «не понятно, на какое сообщение ответил собеседник»)

Проекция ОРИГИНАЛА, на который отвечает сообщение: автор, превью текста, вид медиа,
процитированный фрагмент, признак недоступности (см. ReplyToDto). Именно из него UI рисует
приглушённую строку-цитату над текстом.

Старое поле reply_to_id СОХРАНЕНО без единой правки — на него опираются фронт и тесты.
Два поля СОЗНАТЕЛЬНО расходятся в двух местах (см. reply_to_from_header):
- на форумах Telegram кладёт id топика в reply_to_msg_id у каждого сообщения; reply_to_id это
  повторяет, а reply_to там `None` — иначе под каждой строкой висела бы цитата «сообщение топика»;
- у сервисных сообщений цитата ЕСТЬ в reply_to (grammers `reply_to_message_id()` читает только
  обычные), а reply_to_id пуст.

Заполняется в два приёма: reply_to_from_header (синхронно, из самого сообщения) +
enrich_reply_previews / enrich_reply_preview (превью оригиналов, добранных батчем). В потоке
апдейтов второй приём НЕ делается — там сеть в маппере запрещена, и наружу уходит цитата с одним
message_id; превью дотягивает фронт через GET /dialogs/:id/messages/:mid.

## reply_markup

ДОБАВЛЕНО ПОД D2 («нету поддержки ботов, ни кнопок которые появляются»). Это
клавиатура, присланная ботом: либо inline-клавиатура под конкретным сообщением, либо состояние
reply-клавиатуры диалога (см. ReplyMarkupDto). Заполняется:
- from_grammers_message — через msg.reply_markup();
- from_raw_message — ВСЕГДА null: у messageService поля reply_markup по схеме нет.

## Инвариант

Все поля обязательно присутствуют в JSON (null, а не отсутствие) — фронт различает «значения нет»
и «старый сервер» по typeof, и пропущенное поле означало бы регресс. Ни одного skip_serializing_if.

Потребители на фронте: chat.js::buildMessageRow (инлайн-клавиатура в li._tg.kb, цитата первым
потомком строки), chat.js::buildQuoteRow, reply.js::normalizeReplyTo,
botkb.js::buildMessageKeyboard / noteReplyMarkup / latestReplyMarkup, onEdited (пересборка строки
при появлении/смене разметки).
