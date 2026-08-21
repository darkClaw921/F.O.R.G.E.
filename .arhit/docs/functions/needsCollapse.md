# needsCollapse

`tmux-web/static/js/messenger/voice-transcript-core.js` — надо ли сворачивать этот текст.

Сигнатура: `needsCollapse(text) -> boolean`, сравнение длины с `COLLAPSE_CHARS = 320`.
От результата зависит, появится ли кнопка «Показать полностью» и класс сворачивания на
блоке текста.
