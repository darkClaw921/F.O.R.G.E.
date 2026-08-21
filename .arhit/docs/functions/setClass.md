# setClass

`tmux-web/static/js/messenger/voice-transcript-core.js` — ставит/снимает класс БЕЗ `classList`.

Сигнатура: `setClass(node, cls, on)`; работает через `className`.

# Зачем так

`classList` есть не во всех фейковых документах панелей — карточки Битрикса строятся на
документе из десяти строк (`bitrix_dom_stub.mjs`). Расходиться с ними модулю нельзя:
тогда одна из трёх панелей осталась бы без теста разметки. Тот же довод, что у параметра
`doc` в `buildWave`.
