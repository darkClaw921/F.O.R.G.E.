# reply_to_from_header

plugins/telegram/src/dto.rs

`pub fn reply_to_from_header(header: &tl::enums::MessageReplyHeader) -> Option<ReplyToDto>`

Разбор reply-header'а сообщения в ReplyToDto. Чистая, синхронная, БЕЗ сети — поэтому её безопасно
звать в hot-path'е updates-воркера. `None` ⇒ сообщение ничему не отвечает.

Вызывается автоматически из ОБЕИХ веток маппинга: from_grammers_message (через msg.reply_header())
и from_raw_message (оба рукава — Message и Service). Вручную звать не нужно.

ТРИ СЛУЧАЯ, КОТОРЫЕ ОБЯЗАНЫ РАЗБИРАТЬСЯ ПО-РАЗНОМУ:

1. ФОРУМ-ТОПИК. Чтобы отправить сообщение в топик, клиент кладёт id топика в reply_to_msg_id
   (core.telegram.org/api/forum), поэтому у ОБЫЧНОГО сообщения форума header есть, а ответа нет.
   Отличаем по `forum_topic && reply_to_top_id.is_none()` ⇒ возвращаем None. Иначе под каждой
   строкой супергруппы-форума висела бы цитата «сообщение топика». Старое поле MessageDto.reply_to_id
   этой проверки не делает и делать не будет (часть уже работающего контракта), так что на форумах
   два поля СОЗНАТЕЛЬНО расходятся.

2. ОТВЕТ В ЧУЖОЙ ЧАТ (reply_to_peer_id.is_some()). Оригинал лежит в другом диалоге,
   get_messages_by_id текущего диалога его не найдёт никогда. Зато Telegram сам приложил автора
   (reply_from → reply_from_parts) и медиа (reply_media). Заполняем их локально, а message_id
   отдаём None — иначе переход по цитате увёл бы на постороннее сообщение с тем же номером в нашем
   диалоге. Такие цитаты автоматически не попадают в батч догрузки. Если Telegram не прислал ни
   автора, ни медиа ⇒ unavailable: true.

3. ОТВЕТ НА СТОРИС (messageReplyStoryHeader). Сообщения-оригинала не существует в природе (header
   несёт только story_id), достать текст неоткуда ⇒ message_id: None, unavailable: true, остальное
   null. Роут GET /messages/:mid тут неприменим, а выдумывать media_kind вне закрытого словаря
   контракта нельзя.

QUOTE. `quote_text` берётся из header'а САМОГО ответа и нормализуется тем же reply_preview_text,
что и текст оригинала; пустой/пробельный ⇒ None.

СВЯЗИ: ReplyToDto, reply_preview_text, reply_from_parts (private), MessageDto.reply_to,
from_grammers_message, from_raw_message.
