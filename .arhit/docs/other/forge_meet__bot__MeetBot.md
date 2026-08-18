# forge_meet::bot::MeetBot

Объектно-безопасный трейт бота встречи (async-trait): start, stop, cancel, показать окно, снять скриншот. Единственная точка подмены Chrome — BotFactory; MissingBotFactory отвечает 409 chrome_unavailable, поэтому вся логика записи тестируется без браузера (ScriptedBot проигрывает сценарий из BotScript). Живая реализация — ChromeBotFactory поверх forge-cdp.
