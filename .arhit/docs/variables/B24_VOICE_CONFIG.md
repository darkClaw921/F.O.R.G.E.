# B24_VOICE_CONFIG

`tmux-web/static/js/settings/voice-tab.js` — конфиг секции «Голосовые сообщения» для панели Битрикс24.

Поля: `key: 'bitrix'`, `enabledKey: 'bitrix_voice_autotranscribe'`,
`toggleClass: 'b24-voice-auto'`, `hint`.

Известное ограничение, которое стоит держать в голове при чтении подсказки про белый
список: чат ЗАДАЧИ у Битрикса по `DIALOG_ID` неотличим от обычного, поэтому белый список
из одного вида `task` авторазбор там не включит.
