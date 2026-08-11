AnalysisHub — реестр активных анализов чата VK (plugins/vk/src/analysis/hub.rs, АЧ-Ф3).

НАЗНАЧЕНИЕ. Тонкий слой поверх forge_claude_runner::ClaudeRunner, решающий три задачи, которых у раннера нет.

1. ДЕДУП ПО БЕСЕДЕ. Второй запуск для того же dialog_id не порождает второго процесса claude: start() возвращает StartOutcome::AlreadyRunning(run_id). Причина — broadcast вещает без фильтрации, поэтому вторая вкладка (или повторный клик) уже видит идущий поток кадров. 409 здесь был бы враньём: run существует, и клиенту нужен именно он.
2. ЯВНЫЙ ПОТОЛОК. MAX_PARALLEL_ANALYSIS = 2 анализа по РАЗНЫМ беседам; третий получает HubBusy(n) немедленно, роут превращает это в 409 invalid_state. Семафор раннера дал бы вместо отказа молчаливое ожидание — открытый попап без единого кадра.
3. КОАЛЕСЦИНГ ДЕЛЬТ (функция pump). claude -p --include-partial-messages даёт сотни TextDelta в секунду; один кадр на дельту переполнил бы broadcast (BROADCAST_CAPACITY = 1024), а переполнение это RecvError::Lagged -> кадр resync -> фронт перечитывает весь список бесед -> новый шторм. Поэтому текст копится в буфере и уходит кадром analysis_delta не чаще DELTA_FLUSH_MS = 120 мс либо по накоплению DELTA_FLUSH_CHARS = 400 символов. События Thinking и ToolUse не вещаются вовсе. Хвост буфера сливается ОБЯЗАТЕЛЬНО перед analysis_done.

КОНСТАНТЫ: MAX_PARALLEL_ANALYSIS=2, DEFAULT_ANALYSIS_TIMEOUT_SECS=300, DELTA_FLUSH_MS=120, DELTA_FLUSH_CHARS=400, KIND_ANALYZE/KIND_ASK, REASON_CANCELLED/REASON_TIMEOUT/REASON_ERROR.

API. AnalysisHub::new(cli_path, model, timeout_secs) / from_config(&VkConfig); cli_path() и cli_available() (для 409 claude_unavailable); active_count(); attached_run(dialog_id) -> Option<ActiveRun>; run(run_id); has_free_slot() -> Result<(), HubBusy>; start(broadcast_sender, StartRequest) -> Result<StartOutcome, HubBusy>; cancel(run_id) -> Option<dialog_id>; cancel_dialog(dialog_id) -> Option<run_id>; shutdown().

ЗАПУСК. RunRequest { prompt_via_stdin: TRUE, model = cfg.analysis_model (default sonnet), system = Some(analysis::prompt::SYSTEM_PROMPT), run_id }. prompt_via_stdin обязателен: транскрипт на десятки тысяч символов в argv виден в ps aux любому локальному пользователю и упирается в ARG_MAX.

КТО ЭМИТИТ analysis_done. Штатное завершение (EOF стрима, ошибка CLI, таймаут) — пампер. Отмена — НЕТ: cancel() abort'ит таску раннера вместе с пампером, поэтому кадр analysis_done{ok:false,reason:cancelled} шлёт РОУТ отмены. Признак отмены пампер видит через CancellationToken в записи ActiveRun; без него обрыв канала после abort'а он принял бы за нормальный конец и отправил бы ok:true — фронт получил бы два финальных кадра, один с враньём.

ИНВАРИАНТЫ. Дедуп, проверка лимита и вставка записи — под ОДНИМ lock'ом (иначе два одновременных POST по одной беседе запустили бы два процесса). Запись снимается при ЛЮБОМ исходе (успех, ошибка, отмена, таймаут, shutdown) — утечки слотов нет. Ошибка раннера прокидывается в analysis_error ЦЕЛИКОМ: к ней уже приклеен хвост stderr, и это единственный способ увидеть 'claude не залогинен'. В логи уходят только РАЗМЕРЫ (prompt_chars, messages_used) — ни промпта, ни транскрипта, ни вопроса; на это есть тест the_prompt_never_reaches_the_logs.

СВЯЗИ. Живёт в VkState.analysis (создаётся безусловно, даже без установленного CLI). forge_vk::shutdown вызывает state.analysis.shutdown() ПЕРЕД shutdown_workers — иначе процесс claude пережил бы остановку devforge. Кадры описаны в crate::ws::protocol.