# tmux-web/src/analytics/parse.rs

Построчный разбор одного транскрипта Claude Code (.jsonl) в SessionRollup. Ядро Фазы 1 фичи «Аналитика использования Claude Code»: без сети, без кэша, без обхода каталогов.

## Зачем
Один .jsonl = одна сессия (имя файла = sessionId), файл append-only. Внутри — записи с полями type, timestamp (ISO-мс), cwd, gitBranch, version, isSidechain, isMeta. Файл содержит больше десятка типов записей; аналитике нужны шесть: user, assistant, system, cost-state, agent-name, ai-title.

## Ключевые элементы
- Record — узкая serde-структура строки транскрипта (rename_all=camelCase, все поля Option/default). Явные rename для totalCostUSD / totalAPIDuration / totalToolDuration. Поля durationMs НЕТ намеренно.
- Msg / Content (untagged: Text | Blocks | Other) / Block / Usage / OutputTokensDetails / ModelUsage / ToolUseResult (untagged, отдаёт interrupted).
- Record::is_human_message() — ГЛАВНОЕ правило: type==user И isMeta!=true И isSidechain==false И content — строка ЛИБО массив БЕЗ блоков tool_result. Без фильтра по tool_result метрика завышена примерно в 10 раз (в живой сессии 81 настоящее сообщение против 881 tool_result).
- Record::human_text_len() — длина сообщения в СИМВОЛАХ (для массива — конкатенация text-блоков).
- parse_ts_ms() — ISO-8601 c миллисекундами → i64 Unix-мс через chrono::DateTime::parse_from_rfc3339.
- interesting_line() — дешёвый префильтр по подстроке "type":"…" ДО serde_json::from_str. В живых файлах больше половины строк — attachment и file-history-snapshot; их разбор съедал бы основное время индексации. Ложноположительные срабатывания безвредны (тип перепроверяется после разбора).
- parse_session(reader, session_id, start_offset) — полный разбор в новый SessionRollup.
- append_session(&mut SessionRollup, reader) — инкрементальное дочитывание: счётчики складываются, итоги cost-state перезаписываются, состояние автомата ходов живёт в самом роллапе (TurnState), поэтому ход, разрезанный границей оффсета, не теряется и не дублируется. В конце вызывает redistribute_totals().
- apply_record() — маршрутизация записи по типу в методы SessionRollup.
- AWAY_THRESHOLD_MS = 30 мин («отошёл»), QUICK_REPLY_MS = 10 с («ответил не читая»).

## Устойчивость
Чтение построчное (BufRead::read_line) — файл целиком в память не попадает (живые файлы до сотен МБ). Битая или усечённая строка не роняет разбор: skipped_lines++, а parse_offset остаётся на конце последней ПОЛНОЙ строки, чтобы хвост был дочитан следующим проходом. Строка без завершающего \n трактуется как недописанная.

## Зависимости
serde/serde_json, chrono. Итоги складывает в analytics::rollup. Тесты — tmux-web/tests/analytics_parse.rs.

## Фаза 7 — роли записи `user`
`Record::human_text()` (заменил `human_text_len`) отдаёт текст сообщения ровно на один шаг — классификацию. `apply_record` больше не считает всякую структурно подходящую запись `user` сообщением человека: роль определяет `analytics::text::classify_user_text` — реплика (`on_human_message`), прерывание (`on_interruption`, ход НЕ открывается и пауза не меряется) или вывод слэш-команды (игнорируется). Маркер `toolUseResult.interrupted` оставлен для транскриптов старых версий CLI: в живом архиве его нет ни разу.

