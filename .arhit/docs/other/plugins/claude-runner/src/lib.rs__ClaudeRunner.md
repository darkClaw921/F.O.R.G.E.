# plugins/claude-runner/src/lib.rs::ClaudeRunner

Фасад над Claude CLI: stream/one_shot/cancel/shutdown. ПЕРЕЕХАЛ из plugins/echo/src/claude/mod.rs в общий листовой крейт forge-claude-runner (Фаза 0 фичи «Анализ чата через Claude в VK»), потому что forge-vk не может зависеть от forge-echo. Echo видит его по старому пути crate::claude::ClaudeRunner через re-export.

Поля: cli_path (PathBuf), semaphore (Arc<Semaphore>, ограничивает ОДНОВРЕМЕННЫЕ запуски), running (Arc<Mutex<HashMap<RunId, AbortHandle>>>), run_timeout (Duration; ZERO = отключён, дефолт DEFAULT_RUN_TIMEOUT_SECS = 300 c).

new(cli_path, max_parallel) — НЕ падает, если бинаря нет: только warn-log (важно для healthz, тестов и dev-машин без Claude CLI). with_run_timeout(secs) — билдер таймаута (Echo прокидывает EchoConfig::run_timeout_secs). available_permits()/active_runs() — для тестов и метрик.

stream(req) -> mpsc::Receiver<ClaudeEvent>: спавнит claude -p --output-format stream-json --include-partial-messages --verbose [--model] [--append-system-prompt] [prompt], читает stdout построчно (BufReader::lines) и парсит events::parse_line. Permit берётся ВНУТРИ spawned-таски, чтобы caller сразу получил Receiver. Чтение обёрнуто в tokio::time::timeout. stderr читается отдельной таской в лог + кольцевой буфер на 20 строк, который прикладывается к ClaudeEvent::Error, если stdout не дал ни одного события или сработал таймаут. После read-loop — безусловные child.kill() + child.wait().

Инварианты: drop-guard RunGuard снимает регистрацию run'а на ЛЮБОМ выходе из таски (return/таймаут/panic/abort); abort-handle вставляется в running под тем же Mutex'ом, что и удаление, поэтому гонка «remove до insert» невозможна; tracing-target раннера — claude_runner.

one_shot(req) -> anyhow::Result<RunResult>: агрегирует TextDelta в text + финальный usage; is_error финального result-event'а прокидывается в RunResult. cancel(run_id) -> bool и shutdown() abort'ят задачи; дочерние процессы умирают через kill_on_drop.
