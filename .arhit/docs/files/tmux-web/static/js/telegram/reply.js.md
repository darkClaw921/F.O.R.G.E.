# tmux-web/static/js/telegram/reply.js

ЧИСТЫЙ модуль ответов на сообщения (R1 «нельзя ответить на выбранное сообщение» + R2 «не видно, на
какое сообщение ответил собеседник»). Ни DOM, ни сети: зависит только от format.js, поэтому
целиком грузится в Node и покрыт тестами tests/frontend/telegram_reply.test.mjs. Всё, что требует
DOM (полоса ответа, кнопка «ответить», цитата в строке, переход к оригиналу), живёт в chat.js.

## Три задачи модуля

1. НОРМАЛИЗАЦИЯ `MessageDto.reply_to` во всех трёх формах, в которых ответ доезжает до фронта:
   полный `reply_to` с превью (история / ответ на POST), `reply_to` с одним `message_id` (поток
   апдейтов — там сеть запрещена), голое `reply_to_id` (старый сервер или закешированный фронт).
   `reply_to` СИЛЬНЕЕ `reply_to_id`: серверное поле сознательно расходится со старым на
   форум-топиках, где Telegram кладёт id топика в reply_to_msg_id КАЖДОГО сообщения, и старое поле
   нарисовало бы цитату под всей супергруппой.
2. ПРЕВЬЮ для цитаты: приоритет `quote` (фрагмент) → `unavailable` → `text` (+ пометка вида для
   медиа с подписью: `[video] смотри`) → `[photo]` → `…`. Обрезка по code points, многоточие внутри
   лимита REPLY_PREVIEW_CHARS = 140 (зеркало серверного dto::reply_preview_text).
3. СОСТОЯНИЕ «на что отвечаем» — единственная цель в модульном `state.target`, читается только по
   диалогу (replyTarget/replyTargetId). Сверка диалога и есть автосброс при переключении чата:
   ответ физически не может уйти в чужой чат.

## Ключевые решения

- Автор цитаты опирается на `out`, а не только на `sender`: у своих сообщений в личке Telegram
  отправителя не присылает вовсе (с layer 119 from_id в личных чатах отсутствует), иначе
  собственная цитата осталась бы безымянной. У недоступного оригинала имя ПУСТОЕ — «unknown» рядом
  с «сообщение недоступно» сообщал бы одно и то же дважды.
- mergeReplyFromMessage досбирает превью из уже загруженной ленты: типовой «ответили соседу в
  открытом чате» закрывается бесплатно, без запроса. Оригинал с чужим id игнорируется — защита от
  подстановки чужого текста (то же правило, что на сервере в enrich_reply_preview).
- classifyRowClick — единственная точка, где решается «двойной клик = ответить»; от DOM нужен
  только `closest`, поэтому правило покрыто тестами без браузера.

## Экспорт

Константы: REPLY_PREVIEW_CHARS (140), REPLY_GONE_TEXT ('сообщение недоступно'), REPLY_PENDING_TEXT
('…'), REPLY_CLICK_IGNORE (стоп-лист селекторов).
Функции: truncatePreview, replyMediaMark, normalizeReplyTo, replyNeedsPreview, replyAuthorLabel,
replyPreviewText, replyFromMessage, mergeReplyFromMessage, replySourceLine, buildReplyTarget,
replyDtoFromTarget, setReplyTarget, replyTarget, replyTargetId, clearReplyTarget, classifyRowClick,
_debugReplyState.

## Потребители

chat.js (цитата, кнопка, полоса, переход к оригиналу, оптимистичная отправка),
upload.js (replyTargetId на этапе enqueue — медиа уходит с reply_to_id).
Модуль — лист графа импортов: цикла нет, chat.js/upload.js тянут его, он их — нет.
