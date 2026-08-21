# TELEGRAM_VOICE_CONFIG

`tmux-web/static/js/settings/voice-tab.js` — конфиг секции «Голосовые сообщения» для панели Telegram.

Поля: `key: 'telegram'`, `enabledKey: 'telegram_voice_autotranscribe'`,
`toggleClass: 'tg-voice-auto'`, `hint`.

Подсказка отличается от двух других по существу: у Telegram автоматически разбираются
ТОЛЬКО голосовые, а пересланная музыка и подкасты — по кнопке
(`forge_telegram::voice_auto::AUTO_KIND`). Иначе один пересланный час аудио съедал бы GPU
или деньги без единого нажатия.
