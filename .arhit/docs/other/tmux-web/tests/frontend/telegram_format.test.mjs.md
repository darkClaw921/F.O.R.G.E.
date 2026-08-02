# tmux-web/tests/frontend/telegram_format.test.mjs

Регресс-тест чистой логики вкладки Telegram (Фаза 3), 67 ассертов. Standalone Node-test без jest/vitest: node tmux-web/tests/frontend/telegram_format.test.mjs (0 — прошло, 1 — есть [FAIL]).

Грузит РЕАЛЬНЫЙ static/js/telegram/format.js через await import() — модуль намеренно не импортирует DOM и core/*, поэтому дрейф контракта ловится автоматически.

Покрывает: слаги (username/пробелы/кириллица/спецсимволы/пустое/длина/XSS-вектор), relativeAge, статус-строки (онлайн/last Nh/офлайн/группа/канал; отсутствие счётчика msgs у пользователя), splitDialogs+nodesTitle, unreadTotal с muted и 99+, formatTime/dayKey/разделители дат на границе суток, keyHex+sessionHeadText, nickOf, truncate, insertMessage (порядок + ДЕДУПЛИКАЦИЯ по id + нечисловой id), prependMessages без пересечений, oldestId, isAtBottom/shouldLoadOlder, toastAllowed, latencyFromPingDrift.
