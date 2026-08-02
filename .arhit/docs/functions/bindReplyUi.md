# bindReplyUi

tmux-web/static/js/telegram/chat.js (private)

`bindReplyUi()` — навешивает обработчики ответов один раз (флаг `local.replyBound`).

- `#tg-log` dblclick → onLogDblClick (начать ответ, если classifyRowClick вернул 'reply');
- `#tg-log` click → onLogClick (клик по цитате — переход к оригиналу);
- `#tg-log` keydown → onLogKeyDown (Enter/Space на цитате, с preventDefault: пробел иначе
  прокрутил бы ленту на экран);
- `document` keydown → onReplyEscape (Esc снимает выбранный ответ).

ДЕЛЕГИРОВАНИЕ, а не построчные слушатели: лента перерисовывается целиком при каждой подгрузке
истории, и построчные слушатели пришлось бы снимать вручную (а забытые держали бы в памяти мёртвые
узлы). Исключение — кнопка «ответить»: у неё всё равно есть собственный DOM-узел, и свой слушатель
там дешевле.

Вызывается строго ПОСЛЕ bindPrompt (у полосы свой флаг, и порядок вставки в DOM зависит от
существования формы `#tg-prompt`).
