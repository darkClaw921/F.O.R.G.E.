# describeAnswer

tmux-web/static/js/telegram/botkb.js — разбор ответа бота на нажатие callback-кнопки.

Сигнатура: describeAnswer(resp) -> {kind: 'none'|'toast'|'alert', message: string, url: ?string,
cacheTime: ?number}. Вход — тело 200 POST /api/telegram/dialogs/:id/messages/:mid/callback.

Правила Telegram (messages.botCallbackAnswer):
- message + alert == false → неблокирующее уведомление (обычный тост);
- message + alert == true  → пользователь ОБЯЗАН заметить: «громкий» тост BOT ALERT с ttl 15 с;
- url → открыть БЕЗ подтверждения (сервер гарантирует deep-link самого бота или game-URL).
  Белый список схем safeUrl применяется ВСЁ РАВНО: доверять «гарантии» чужой стороны на клиенте
  нельзя.

message прогоняется через cleanAnswerText, cacheTime берётся только если Number.isFinite.
Пустой message ⇒ kind 'none' (бот ответил молча — это нормальный сценарий, тост показывать не за
что). Применение результата — applyAnswer.
