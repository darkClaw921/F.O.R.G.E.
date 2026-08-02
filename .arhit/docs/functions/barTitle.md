# barTitle

tmux-web/static/js/telegram/botkb.js — подпись полосы reply-клавиатуры над #tg-prompt.

Сигнатура: barTitle(markup) -> string.
- force_reply → '// REPLY REQUESTED';
- persistent  → '// KEYBOARD · PERSISTENT';
- иначе       → '// KEYBOARD';
- нет разметки → ''.

Формат '// ...' — в одном алфавите с остальными заголовками терминального UI панели.
