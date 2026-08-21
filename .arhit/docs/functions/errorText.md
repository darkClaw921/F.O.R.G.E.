# errorText

`tmux-web/static/js/messenger/voice-transcript-core.js` — человеческая причина отказа по коду.

Сигнатура: `errorText(code) -> string`: `ERROR_TEXT[code]`, иначе `ERROR_FALLBACK`.
Единственная точка, через которую отказ попадает человеку на глаза — см. `ERROR_TEXT`
про то, почему сообщение с бэкенда сюда не проходит.
