# attr

`tmux-web/static/js/messenger/voice-transcript-core.js` — `setAttribute`, если документ его умеет.

Сигнатура: `attr(node, name, value)`; молча пропускает вызов на узлах фейкового документа
без `setAttribute`. Тот же довод, что у `setClass`: общий модуль обязан пережить самый
бедный из трёх фейковых документов.
