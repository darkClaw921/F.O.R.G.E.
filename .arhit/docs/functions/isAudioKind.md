# isAudioKind

`tmux-web/static/js/messenger/voice-transcript-core.js` — бывает ли у вложения такого вида расшифровка.

Сигнатура: `isAudioKind(kind) -> boolean`, проверка вхождения в `AUDIO_KINDS`.
Вызывается при сборке карточки: не звук — блока расшифровки нет вовсе, кнопка не рисуется.
