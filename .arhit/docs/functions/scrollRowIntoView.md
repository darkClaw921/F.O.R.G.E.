# scrollRowIntoView

tmux-web/static/js/telegram/chat.js (private)

`scrollRowIntoView(row)` — прокручивает `#tg-log` так, чтобы строка оказалась по центру.

Через `getBoundingClientRect`, а НЕ через `offsetTop`: у `#tg-log` нет своего `position`, и
смещения его детей считаются от `#telegram` — прямой `scrollTop = row.offsetTop` промахнулся бы на
высоту титульбара и шапки сессии. Фолбэк на `scrollIntoView`, если rect-методов нет (jsdom-тесты).
