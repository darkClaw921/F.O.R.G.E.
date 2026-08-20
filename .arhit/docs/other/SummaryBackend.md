# SummaryBackend

Перечисление конфига forge-meet: где считается САММАРИ. Варианты cloud (polza.ai chat/completions со стримом SSE) и local (Claude CLI); по умолчанию Cloud по той же причине, что и у SttBackend. parse принимает синонимы polza/claude. Значение читает summary::summarize (развилка внутри неё, чтобы слот очереди, промпт, запись файла и finish остались одной веткой) и routes::meetings::summary_blocker (облако без ключа = cloud_not_configured). Файл: plugins/meet/src/config.rs.
