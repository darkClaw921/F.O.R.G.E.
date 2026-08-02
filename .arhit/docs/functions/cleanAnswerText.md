# cleanAnswerText

tmux-web/static/js/telegram/botkb.js — вычистка текста ответа бота на нажатие кнопки
(поле message из 200 POST .../callback).

Сигнатура: cleanAnswerText(raw) -> string, потолок ANSWER_TEXT_MAX = 400 символов.

Отличие от cleanButtonText: переводы строк СОХРАНЯЮТСЯ — ответ бота часто многострочный
(«готово: 3 из 5»), и склеивать его в одну строку значит терять смысл. Остальное то же:
управляющие символы и bidi-override вырезаются, обрезка по code points.
