# forge-claude-runner (plugins/claude-runner)

Общий крейт Cargo workspace со стриминговым раннером Claude CLI
(`claude -p --output-format stream-json --include-partial-messages --verbose`).

## Зачем выделен

Раннер жил в `plugins/echo/src/claude/mod.rs`. Плагин VK получает фичу
«анализ чата через Claude», но зависимость `forge-vk -> forge-echo` запрещена
(плагины изолированы друг от друга), а вторая копия процессного кода
(spawn/kill/таймаут/семафор/парсер NDJSON) гарантированно разъедется.
Поэтому раннер вынесен в ЛИСТОВОЙ крейт `forge-claude-runner`: у него нет
зависимостей ни на один плагин, ни на `devforge`. Любая такая зависимость
вернёт цикл, из-за которого вынос затевался.

Перенос сделан без изменения поведения Echo: `plugins/echo/src/claude/mod.rs`
схлопнут до `pub mod prompt_builder;` + полного re-export'а, поэтому все
исторические пути `crate::claude::*` (включая `crate::claude::events::*`)
резолвятся по-прежнему и вызывающие файлы Echo не менялись.

## Состав

* `src/lib.rs` — `ClaudeRunner`, `RunRequest`, `RunResult`, `RunId`,
  `DEFAULT_RUN_TIMEOUT_SECS` (300 c), приватная `build_args`.
* `src/events.rs` — `ClaudeEvent` (TextDelta / Thinking / ToolUse / Result /
  Error), `Usage`, `parse_line` — NDJSON-парсер стрима, вместе со своим
  модулем тестов (15 тестов формата stream-json).

## ClaudeRunner

Фасад над Claude CLI: spawn, stream, cancel.

* `new(cli_path, max_parallel)` — НЕ падает, если бинаря нет: только warn-log,
  чтобы `init` плагина поднимался в окружении без Claude CLI (тесты, healthz).
* `with_run_timeout(secs)` — таймаут на один run; `0` отключает. Дефолт —
  `DEFAULT_RUN_TIMEOUT_SECS`. Зависший CLI без таймаута держит permit семафора
  и выедает все слоты `max_parallel`.
* `stream(req) -> mpsc::Receiver<ClaudeEvent>` — построчное чтение stdout с
  парсингом в события. Permit семафора берётся ВНУТРИ spawned-таски, чтобы
  caller сразу получил Receiver.
* `one_shot(req) -> anyhow::Result<RunResult>` — агрегирует текст + финальный
  usage; `is_error` из финального `result`-event'а прокидывается в RunResult.
* `cancel(run_id) -> bool`, `shutdown()` — abort активных задач; дочерние
  процессы умирают через `kill_on_drop`.
* `available_permits()`, `active_runs()` — для тестов и метрик.

Инварианты, которые нельзя терять при правках:
* drop-guard `RunGuard` снимает регистрацию run'а на ЛЮБОМ выходе из таски
  (return, таймаут, panic, abort) — иначе течёт `running`-map;
* abort-handle вставляется в `running` под тем же Mutex'ом, что и удаление, —
  гонка «remove до insert» исключена;
* хвост stderr (последние 20 строк) прикладывается к `ClaudeEvent::Error`,
  когда stdout не дал ни одного события или сработал таймаут;
* после read-loop идёт безусловный `child.kill()` + `child.wait()`;
* tracing-target раннера — `claude_runner` (в Echo был `forge_echo`).

## RunRequest.prompt_via_stdin

Единственное функциональное отличие от кода, жившего в Echo.

* `false` (дефолт, поведение Echo не изменилось) — prompt уходит ПОСЛЕДНИМ
  positional-аргументом argv, stdin процесса = `Stdio::null()`.
* `true` — в argv промпта нет вообще, stdin = `Stdio::piped()`, prompt
  пишется туда целиком и пишущий конец закрывается через `shutdown()`.

Мотивация: чат-транскрипт на десятки тысяч символов в argv виден в `ps aux`
ЛЮБОМУ локальному пользователю (утечка переписки) и упирается в ARG_MAX
(риск `E2BIG` при spawn).

Две ловушки, зафиксированные тестами:
1. Писать в stdin ОБЯЗАТЕЛЬНО отдельной таской. На промпте больше pipe-буфера
   (8—64 KiB) `write_all` заблокируется до вычитывания дочерним процессом, а
   тот в это время пишет в stdout, который мы ещё не читаем — взаимная
   блокировка. Тест `large_prompt_via_stdin_does_not_deadlock` гоняет 60 000
   символов через mock-CLI, который сначала пишет в stdout и лишь потом читает
   stdin (худший порядок).
2. `shutdown()` обязателен: без EOF на stdin `claude -p` считает ввод
   незавершённым и не начинает генерацию.

Сборка argv вынесена в чистую `build_args(&RunRequest) -> Vec<String>` именно
затем, чтобы тест мог проверить инвариант приватности напрямую.

## Тесты

27 тестов: 15 — парсер `events`, 12 — раннер (mock-CLI как shell-скрипт во
временном каталоге): события из стрима, stderr-хвост на пустом stdout,
агрегация one_shot, cancel, shutdown, потолок семафора, argv в обоих режимах,
доставка через stdin, пустой stdin по умолчанию, 60k без дедлока.

Живая проверка на настоящем `claude` (гейт фазы 0) подтвердила: argv-путь
стримит чанками, stdin-путь работает на реальном CLI, после CANCEL
`pgrep -f 'claude -p --output-format stream-json'` пуст.

## Потребители

* `forge-echo` — через фасад `plugins/echo/src/claude/mod.rs` (чат, автономные
  задачи, next_step, memory, daily_report).
* `forge-vk` — фазы 1+ фичи «Анализ чата через Claude».
