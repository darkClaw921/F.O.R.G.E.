# forge-meet-summary

plugins/meet/src/summary/ — саммари встречи через forge-claude-runner. Модуль НЕ импортирует axum и http.

СОСТАВ
- prompt.rs — чистая сборка промпта: build(SummaryInput) -> BuiltPrompt{text, truncated, used_chars}, escape(), truncate_middle(), константы SYSTEM_PROMPT, TASK, TRUNCATION_MARK.
- mod.rs — прогон: generate() поверх ClaudeRunner, summarize() на одну встречу, DeltaCoalescer, render(), read_summary(), константы SUMMARY_MD, DELTA_FLUSH_MS, DELTA_FLUSH_CHARS, TRUNCATION_NOTE.

ПРОМПТ СВОЙ. forge_chat_analysis::prompt НЕ переиспользуется: его SYSTEM_PROMPT буквально говорит «разбирает переписку из ВКонтакте», и подмешать туда встречу значит зашить ложь в системную инструкцию. Запрет структурный — forge-chat-analysis не значится в зависимостях плагина, тест the_chat_analysis_prompt_is_not_reused читает Cargo.toml. Дисциплина взята целиком: теговая форма <meeting>/<participants>/<transcript>/<task>, экранирование & < > в ДАННЫХ (амперсанд первым, иначе экранирование съело бы само себя), явная строка про ДАННЫЕ в системной инструкции, системная часть только через --append-system-prompt (то, что приезжает отдельным каналом, содержимым расшифровки подделать невозможно).

PROMPT_VIA_STDIN ОБЯЗАТЕЛЕН. 80 КБ расшифровки в argv видны в ps aux любому локальному пользователю и упираются в ARG_MAX. RunRequest заполняется литералом ЦЕЛИКОМ, без ..Default::default(): дефолт поля false, и новое поле раннера всосалось бы молча. Инвариант проверяется трижды: по исходнику (единственный RunRequest в крейте), на живом процессе (мок сбрасывает argv и stdin в файлы) и живой пробой ps на настоящем claude.

ОБРЕЗКА ИЗ СЕРЕДИНЫ. Транскрипт длиннее config.summary_max_chars (120 000 code points) режется по границам строк: 40 процентов лимита началу, 60 концу, между ними TRUNCATION_MARK. Оба конца встречи ценны — в начале повестка и состав, в конце решения и задачи; обрезка хвоста потеряла бы ровно то, ради чего саммари и делают. Тег несёт truncated=true, а в summary.md добавляется TRUNCATION_NOTE для человека. Map-reduce по получасовым окнам — версия 2.

ПРОВАЛ НЕ РОНЯЕТ ВСТРЕЧУ. summarize() при ЛЮБОМ исходе идёт через одну функцию finish(): журнал meeting_events, кадр MeetServerMsg::Error с хвостом stderr, repo::meetings::finish_summary(ok) -> ready плюс summary_status. Коды: summary_failed, summary_timeout (по префиксу сообщения раннера, что префикс тот — доказывает тест с зависшим моком), summary_empty, transcript_missing, summary_write_failed. Отмена — единственное исключение: статус трогает тот, кто отменял.

КОАЛЕСЦИНГ. DeltaCoalescer — чистая структура без таймеров (время приходит параметром), поэтому оба порога проверяются синхронно и без сна. DELTA_FLUSH_MS=120 и DELTA_FLUSH_CHARS=400 — намеренная КОПИЯ значений forge_chat_analysis::hub (плагины не зависят друг от друга), сверяется тестом по исходнику первоисточника. Кадры уезжают как MeetServerMsg::Progress{phase: Summarizing, done: символов}.

ОЧЕРЕДЬ. MeetHub::acquire_summary_queued, два слота, FIFO, позиция = ahead-(limit-1); при ожидании уходит кадр progress с queued=true.

ИЗВЕСТНОЕ ОГРАНИЧЕНИЕ: дочерний процесс получает полный набор инструментов пользователя (задача forge-8wuuw, блокируется forge-dh79). Защита от инъекции сейчас — экранирование плюс системная инструкция.
