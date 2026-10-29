//! # forge-claude-runner
//!
//! Общий для плагинов F.O.R.G.E. стриминговый раннер LLM-запросов:
//! локальный `claude -p`, локальный `kimi -p` или облачный OpenAI-совместимый
//! провайдер — выбор движка делает конфиг плагина через [`backend::LlmBackend`].
//!
//! Крейт выделен из `plugins/echo/src/claude/mod.rs` без изменения поведения:
//! плагины изолированы друг от друга, поэтому `forge-vk` не может зависеть от
//! `forge-echo`, а дублировать процессный код (spawn/kill/таймаут/семафор) в
//! каждом плагине — гарантированный дрейф. Крейт ЛИСТОВОЙ: у него нет
//! зависимостей ни на один плагин и ни на `devforge` (единственная
//! внешняя зависимость-ядро — `forge-voice`, облачный транспорт).
//!
//! Sub-модули:
//! - [`events`] — NDJSON-парсеры `claude -p` и `kimi -p`
//!   (`--output-format stream-json`);
//! - [`backend`] — выбор движка ([`backend::CliKind`], [`backend::LlmBackend`],
//!   [`backend::Transport`]) и единая точка резолва [`backend::runner_for`].
//!
//! Сборка самого prompt'а остаётся на стороне вызывающего плагина
//! (`forge_echo::claude::prompt_builder` для Echo) — она предметная и в общий
//! крейт не переезжает.
//!
//! Главный фасад — [`ClaudeRunner`].
//!
//! ## Архитектура запуска
//!
//! `ClaudeRunner` исполняет run одним из двух транспортов ([`backend::Transport`]):
//!
//! - `Cli` — спавнит дочерний процесс `claude -p --output-format stream-json
//!   --include-partial-messages --verbose [--model <m>] <prompt>` (или
//!   `kimi --print --output-format stream-json [-m <m>] -p <prompt>`) и
//!   построчно читает stdout, парся каждую строку в [`events::ClaudeEvent`];
//! - `Cloud` — POST `{base_url}/chat/completions` со `stream: true` и разбор
//!   ответа по SSE-кадрам (`data:` … `[DONE]`), поверх клиента
//!   `forge_voice::cloud::CloudClient`.
//!
//! - Параллелизм ограничен `Semaphore` (по умолчанию 4 одновременных run'а).
//!   Слишком много параллельных CLI-запусков убивает квоту и плодит зомби.
//! - Каждый активный run регистрируется в `running: HashMap<RunId, AbortHandle>`,
//!   что даёт `cancel(run_id)` без зависания.
//! - Не падаем, если CLI-бинарь не существует на момент `new()` — только
//!   warn-log. Это позволяет `EchoState::init` собирать runner даже когда
//!   CLI не установлен (для healthz/тестов).
//! - Запуски **по умолчанию выключены**: runner созданный через
//!   [`ClaudeRunner::new`], не запустит ни одного run'а, пока его явно не
//!   включат через [`ClaudeRunner::with_enabled`]. Плагины прокидывают туда
//!   свой `claude_enabled`-конфиг ИЛИ глобальный [`env_runs_enabled`]
//!   (`FORGE_CLAUDE_RUNNER_ENABLED=1` включает все плагины разом — это и
//!   есть «настройка, включающая всё»).

pub mod backend;
pub mod events;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::task::AbortHandle;

pub use backend::{runner_for, CliKind, CloudTransportCfg, LlmBackend, Transport};
pub use events::{ClaudeEvent, Usage};

/// Дефолтный таймаут на один Claude-run (чтение стрима stdout), в секундах.
/// Зависший CLI держит permit семафора и без таймаута выедает все
/// `max_parallel` слотов, останавливая все пути run'а разом. 300s = 5 минут —
/// с запасом для длинных агентных прогонов, но гарантированно освобождает
/// слот при зависании. 0 → таймаут отключён.
///
/// Значение перенесено из `forge_echo::config::DEFAULT_RUN_TIMEOUT_SECS` —
/// та константа остаётся на своём месте как дефолт конфига Echo, здесь же
/// живёт дефолт самого раннера (крейт не имеет доступа к конфигу плагина).
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 300;

/// Имя env-переменной — ГЛОБАЛЬНОГО выключателя запусков `claude -p` для всего
/// процесса devforge. Заданная в «истинное» значение (`1`, `true`, `yes`,
/// `on`, регистронезависимо) включает запуски во ВСЕХ плагинах разом —
/// поверх их собственных `claude_enabled`-конфигов. Любое другое значение
/// (включая `0`/`false` и нераспознанный мусор) трактуется как «выключено».
///
/// Это «настройка, включающая всё»: один экспорт включает Echo, Meet,
/// Bitrix24, VK, Telegram и chat-analysis без правки их конфигов.
pub const ENV_RUNS_ENABLED: &str = "FORGE_CLAUDE_RUNNER_ENABLED";

/// Включены ли запуски `claude -p` глобально через [`ENV_RUNS_ENABLED`].
/// Не задана или нераспознанное значение → `false` (дефолт — выключено).
pub fn env_runs_enabled() -> bool {
    std::env::var(ENV_RUNS_ENABLED)
        .ok()
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Уникальный идентификатор run'а. Совпадает по типу с `String` для
/// тривиальной сериализации в WS-протокол.
pub type RunId = String;

/// Запрос на запуск Claude CLI.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Полный prompt (уже собранный вызывающей стороной).
    pub prompt: String,
    /// Модель Claude (`--model`). `None` → дефолт CLI.
    pub model: Option<String>,
    /// Дополнительный system-prompt (`--append-system-prompt`).
    pub system: Option<String>,
    /// ID run'а — генерируется callee. Используется для cancel.
    pub run_id: RunId,
    /// Куда подавать prompt: `false` (дефолт) — последним positional-аргументом
    /// argv, как исторически делал Echo; `true` — в stdin дочернего процесса
    /// (в argv промпта нет вообще).
    ///
    /// Зачем: длинный prompt в argv виден в `ps aux` ЛЮБОМУ локальному
    /// пользователю и упирается в `ARG_MAX` (на macOS ~256 KiB на всё
    /// окружение + argv). Для чат-транскриптов на десятки тысяч символов это
    /// и утечка содержимого переписки, и риск `E2BIG` при spawn. stdin от
    /// обеих проблем свободен.
    ///
    /// Дефолт оставлен `false` намеренно: вызовы Echo не должны менять
    /// поведение при выносе раннера в общий крейт.
    #[doc(alias = "stdin")]
    pub prompt_via_stdin: bool,
}

impl Default for RunRequest {
    /// Пустой запрос с новым UUID. Нужен ради `..Default::default()` в
    /// struct-литералах вызывающих плагинов: так добавление нового поля в
    /// `RunRequest` (как `prompt_via_stdin`) не ломает их сборку.
    fn default() -> Self {
        Self {
            prompt: String::new(),
            model: None,
            system: None,
            run_id: uuid::Uuid::new_v4().to_string(),
            prompt_via_stdin: false,
        }
    }
}

impl RunRequest {
    /// Утилита для тестов и быстрых запусков: только prompt + новый UUID run_id.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Default::default()
        }
    }

    /// Builder-переключатель на подачу prompt'а через stdin.
    pub fn with_prompt_via_stdin(mut self, via_stdin: bool) -> Self {
        self.prompt_via_stdin = via_stdin;
        self
    }
}

/// Собирает argv дочернего процесса (без argv[0]) под конкретный вид CLI.
///
/// Вынесено из [`ClaudeRunner::stream`] отдельной чистой функцией ровно затем,
/// чтобы тест мог проверить главный инвариант приватности: при
/// `prompt_via_stdin == true` текста промпта в argv нет ни в каком виде.
///
/// ## Kimi: свои флаги и свои ограничения
///
/// Kimi CLI (print-режим) имеет другой интерфейс: prompt — значение флага
/// `-p` (positional print-режим не поддерживает), стрим-json —
/// `--output-format stream-json`, модель — `-m <model>`. System-prompt'
/// отдельного флага у print-режима нет, поэтому он склеивается в начало
/// prompt'а (`"system\n\nprompt"`).
///
/// **stdin Kimi в print-режиме не читает**, поэтому `prompt_via_stdin`
/// игнорируется: prompt всегда идёт в argv. Это осознанный компромисс —
/// для огромных транскриптов (сотни тысяч символов) есть риск упереться в
/// ARG_MAX; тогда выбирайте `claude`-бэкенд с `prompt_via_stdin` или облако.
fn build_args(kind: CliKind, req: &RunRequest) -> Vec<String> {
    match kind {
        CliKind::Claude => {
            let mut args = vec![
                "-p".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--include-partial-messages".to_string(),
                "--verbose".to_string(),
            ];
            if let Some(model) = &req.model {
                args.push("--model".to_string());
                args.push(model.clone());
            }
            if let Some(sys) = &req.system {
                args.push("--append-system-prompt".to_string());
                args.push(sys.clone());
            }
            if !req.prompt_via_stdin {
                // Prompt — последний positional аргумент.
                args.push(req.prompt.clone());
            }
            args
        }
        CliKind::Kimi => {
            // Порядок: служебные флаги → -p <prompt>. Prompt в argv всегда
            // (см. doc-комментарий выше про stdin).
            let mut args = vec![
                "--print".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ];
            if let Some(model) = &req.model {
                args.push("-m".to_string());
                args.push(model.clone());
            }
            let prompt = match &req.system {
                Some(sys) => format!("{sys}\n\n{}", req.prompt),
                None => req.prompt.clone(),
            };
            args.push("-p".to_string());
            args.push(prompt);
            args
        }
    }
}

/// Результат `one_shot` — собранный текст + итоговый usage + сырой json для аудита.
///
/// `is_error` — финальный `result`-event пришёл с `is_error:true`/`subtype:error_*`.
/// Caller'ы (scheduler::runner) должны трактовать такой run как неуспешный
/// (status=error), даже если текст частично собран.
#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub text: String,
    pub usage: Usage,
    pub raw: serde_json::Value,
    pub is_error: bool,
}

/// Фасад над LLM-запуском: spawn/stream/cancel поверх выбранного
/// [`Transport`] (локальный CLI или облако).
///
/// Cheap-clonable через `Arc`. Один процесс devforge → один runner; внутри
/// runner может быть несколько одновременных stream'ов до значения semaphore.
pub struct ClaudeRunner {
    transport: Transport,
    semaphore: Arc<Semaphore>,
    running: Arc<Mutex<HashMap<RunId, AbortHandle>>>,
    /// Таймаут на один run (чтение стрима stdout/SSE). `Duration::ZERO` →
    /// отключён. При срабатывании spawned-таска убивает дочерний процесс
    /// (CLI) или рвёт HTTP-стрим (Cloud) и закрывает канал, освобождая permit
    /// семафора (см. [`ClaudeRunner::stream`]). Это закрывает разом все пути
    /// run'а: чат, автономные задачи, next_step, memory.
    run_timeout: Duration,
    /// Разрешены ли запуски. **Дефолт — `false` (выключено)**: запуски дорогие
    /// и не должны происходить без явного согласия. Включение — через
    /// [`ClaudeRunner::with_enabled`] (плагины прокидывают сюда свой
    /// `claude_enabled`-конфиг или [`env_runs_enabled`]). Выключенный runner НЕ
    /// исполняет run: `stream` сразу отдаёт `ClaudeEvent::Error` с инструкцией,
    /// как включить.
    enabled: bool,
}

impl ClaudeRunner {
    /// Создаёт runner с локальным `claude` CLI по указанному пути. **Не
    /// падает**, если бинарь не существует — только warn-log. Это намеренно:
    /// `init` плагина не должен падать в окружении без CLI (тесты, dev-машины
    /// без CLI).
    ///
    /// Запуски созданного runner **выключены** — включать нужно явно через
    /// [`ClaudeRunner::with_enabled`].
    pub fn new(cli_path: PathBuf, max_parallel: usize) -> Self {
        Self::from_transport(Transport::claude_cli(cli_path), max_parallel)
    }

    /// Создаёт runner с явным транспортом ([`Transport::Cli`] любого вида или
    /// [`Transport::Cloud`]). Вся логика warn'ов живёт здесь.
    pub fn from_transport(transport: Transport, max_parallel: usize) -> Self {
        match &transport {
            Transport::Cli { kind, path } => {
                if !path.exists() {
                    tracing::warn!(
                        cli = %path.display(),
                        kind = kind.as_str(),
                        "ClaudeRunner: cli binary not found at startup — run requests will fail at exec time"
                    );
                }
            }
            Transport::Cloud(cfg) => {
                if !cfg.configured() {
                    tracing::warn!(
                        base_url = %cfg.base_url,
                        "ClaudeRunner: cloud transport not configured (no api_key in config/env/meet.toml) — cloud runs will fail"
                    );
                }
            }
        }
        let permits = max_parallel.max(1);
        Self {
            transport,
            semaphore: Arc::new(Semaphore::new(permits)),
            running: Arc::new(Mutex::new(HashMap::new())),
            run_timeout: Duration::from_secs(DEFAULT_RUN_TIMEOUT_SECS),
            enabled: false,
        }
    }

    /// Транспорт runner'а (для healthz-подобных ответов и CLI-путей плагинов).
    #[must_use]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Заменяет транспорт, сохраняя таймаут и флаг `enabled`. Плагины зовут
    /// это после чтения конфига, когда бэкенд выбран строкой `llm_backend`:
    /// `ClaudeRunner::new(...)` собирает дефолтный claude-транспорт, а здесь
    /// он переключается на kimi/cloud без потери настроек билдера.
    #[must_use]
    pub fn with_transport(mut self, transport: Transport) -> Self {
        match &transport {
            Transport::Cli { kind, path } => {
                if !path.exists() {
                    tracing::warn!(
                        cli = %path.display(),
                        kind = kind.as_str(),
                        "ClaudeRunner: cli binary not found — run requests will fail at exec time"
                    );
                }
            }
            Transport::Cloud(cfg) => {
                if !cfg.configured() {
                    tracing::warn!(
                        base_url = %cfg.base_url,
                        "ClaudeRunner: cloud transport not configured (no api_key in config/env/meet.toml) — cloud runs will fail"
                    );
                }
            }
        }
        self.transport = transport;
        self
    }

    /// Переопределяет таймаут на один run. `secs == 0` → таймаут отключён.
    /// Используется в `forge_echo::init_with_config` для проброса
    /// `EchoConfig::run_timeout_secs`.
    pub fn with_run_timeout(mut self, secs: u64) -> Self {
        self.run_timeout = Duration::from_secs(secs);
        self
    }

    /// Разрешает (или запрещает) запуски `claude -p`. Плагины вызывают это с
    /// результатом «свой конфиг ИЛИ [`env_runs_enabled`]» — глобальная
    /// переменная `FORGE_CLAUDE_RUNNER_ENABLED=1` так включает ВСЕ плагины
    /// разом, не трогая их конфиги.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Разрешены ли сейчас запуски (для логов и healthz-подобных ответов).
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Сколько свободных permit'ов сейчас (для тестов и метрик).
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Сколько активных run'ов сейчас (для тестов и /api/echo/runs).
    pub async fn active_runs(&self) -> usize {
        self.running.lock().await.len()
    }

    /// Запускает Claude CLI и возвращает receiver-сторону канала с
    /// событиями [`ClaudeEvent`]. Канал закрывается, когда CLI завершился
    /// (EOF на stdout) или run был отменён.
    ///
    /// Если запуски выключены ([`ClaudeRunner::with_enabled`]), процесс НЕ
    /// спавнится: канал отдаёт ровно один `ClaudeEvent::Error` с инструкцией,
    /// как включить, и закрывается.
    ///
    /// Не блокирует caller'а на permit'е — permit берётся уже внутри
    /// spawned-таски, чтобы caller сразу получил `Receiver` и подписался на
    /// первые события.
    pub async fn stream(&self, mut req: RunRequest) -> mpsc::Receiver<ClaudeEvent> {
        let (tx, rx) = mpsc::channel::<ClaudeEvent>(64);

        // Глобальный выключатель: дорогие запуски `claude -p` не должны
        // происходить без явного согласия. Раньше spawn'а — чтобы не тратить
        // ни процесс, ни permit.
        if !self.enabled {
            tracing::warn!(
                target: "claude_runner",
                run_id = %req.run_id,
                "claude run rejected: runner disabled \
                 (по умолчанию выключено; включить: FORGE_CLAUDE_RUNNER_ENABLED=1 \
                 или claude_enabled = true в конфиге плагина)"
            );
            let _ = tx
                .send(ClaudeEvent::Error {
                    message: "claude runner disabled: запуски `claude -p` выключены. \
                              Включить: FORGE_CLAUDE_RUNNER_ENABLED=1 (все плагины) \
                              или claude_enabled = true в конфиге плагина."
                        .into(),
                })
                .await;
            return rx;
        }
        let transport = self.transport.clone();
        let semaphore = self.semaphore.clone();
        let run_timeout = self.run_timeout;
        let run_id = req.run_id.clone();
        let run_id_for_map = run_id.clone();

        // Снятие регистрации делаем через drop-guard: он срабатывает на ЛЮБОМ
        // выходе из таски — нормальное завершение, ранний `return` (spawn/pipe
        // ошибки), таймаут, panic, abort (cancel/shutdown). Это закрывает течь,
        // когда run завершался раньше, чем мы успевали удалить запись.
        struct RunGuard {
            running: Arc<Mutex<HashMap<RunId, AbortHandle>>>,
            run_id: RunId,
        }
        impl Drop for RunGuard {
            fn drop(&mut self) {
                let running = self.running.clone();
                let run_id = std::mem::take(&mut self.run_id);
                // Lock — async, в Drop недоступен .await: снимаем регистрацию в
                // detached-таске. try_lock покрывает быстрый путь без spawn.
                let removed = match running.try_lock() {
                    Ok(mut map) => {
                        map.remove(&run_id);
                        true
                    }
                    Err(_) => false,
                };
                if !removed {
                    tokio::spawn(async move {
                        running.lock().await.remove(&run_id);
                    });
                }
            }
        }

        let guard_running = self.running.clone();
        let guard_run_id = run_id.clone();
        let handle = tokio::spawn(async move {
            let _guard = RunGuard {
                running: guard_running,
                run_id: guard_run_id,
            };
            // Permit удерживаем на всё время run'а — Semaphore ограничивает
            // ОДНОВРЕМЕННЫЕ запуски, что и требовалось.
            let _permit = match semaphore.acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "semaphore closed unexpectedly");
                    let _ = tx
                        .send(ClaudeEvent::Error {
                            message: "semaphore closed".into(),
                        })
                        .await;
                    return;
                }
            };

            match transport {
                Transport::Cli { kind, path } => {
                    run_cli(tx, kind, path, req, run_timeout, run_id).await
                }
                Transport::Cloud(cfg) => run_cloud(tx, cfg, req, run_timeout, run_id).await,
            }
        });

        // Регистрируем abort-handle ДО того как таска сможет снять регистрацию:
        // держим lock на `running` весь insert. Drop-guard внутри таски лочит
        // тот же Mutex для удаления, поэтому пока мы держим guard здесь, таска
        // не может выполнить cleanup раньше insert'а — гонка «remove до insert»
        // исключена. После выхода из этого scope lock освобождается.
        {
            let mut map = self.running.lock().await;
            map.insert(run_id_for_map, handle.abort_handle());
        }

        rx
    }

    /// Запускает CLI и собирает все события до завершения. Возвращает
    /// агрегированный текст и финальный usage.
    pub async fn one_shot(&self, req: RunRequest) -> anyhow::Result<RunResult> {
        let mut rx = self.stream(req).await;
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut raw = serde_json::Value::Null;
        let mut last_error: Option<String> = None;
        let mut result_is_error = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                ClaudeEvent::TextDelta { text: t } => text.push_str(&t),
                ClaudeEvent::Thinking { .. } => {}
                ClaudeEvent::ToolUse { .. } => {}
                ClaudeEvent::Result { usage: u, is_error, raw_json } => {
                    usage = u;
                    raw = raw_json;
                    result_is_error = is_error;
                }
                ClaudeEvent::Error { message } => {
                    last_error = Some(message);
                }
            }
        }
        if let Some(msg) = last_error {
            // Если был ошибочный event, но ничего полезного не собрали — Err.
            if text.is_empty() {
                anyhow::bail!("claude run error: {msg}");
            }
        }
        // Финальный result с is_error: текст мог собраться частично, но run
        // неуспешен. Если текста нет — это явный провал, bail; иначе вернём
        // RunResult с is_error=true, чтобы caller записал status=error.
        if result_is_error && text.is_empty() {
            anyhow::bail!("claude run finished with error result");
        }
        Ok(RunResult { text, usage, raw, is_error: result_is_error })
    }

    /// Прерывает run по id. Возвращает `true` если run был найден и aborted.
    pub async fn cancel(&self, run_id: &str) -> bool {
        let mut map = self.running.lock().await;
        if let Some(h) = map.remove(run_id) {
            h.abort();
            tracing::info!(target: "claude_runner", run_id, "ClaudeRunner: cancelled");
            true
        } else {
            false
        }
    }

    /// Phase 6 hardening — abort'нуть все активные run-задачи. Дочерние
    /// процессы CLI убиваются автоматически благодаря `kill_on_drop`
    /// в [`tokio::process::Command`], потому что drop spawned tokio-task'и
    /// приводит к drop'у `child` внутри неё; облачные run'ы рвутся drop'ом
    /// HTTP-ответа.
    pub async fn shutdown(&self) {
        let mut map = self.running.lock().await;
        let n = map.len();
        for (_id, h) in map.drain() {
            h.abort();
        }
        if n > 0 {
            tracing::info!(target: "claude_runner", killed = n, "ClaudeRunner: shutdown — aborted active runs");
        } else {
            tracing::debug!(target: "claude_runner", "ClaudeRunner: shutdown — no active runs");
        }
    }
}

// ===========================================================================
// Транспортные run-функции (вызываются из spawned-таски ClaudeRunner::stream)
// ===========================================================================

/// Диспетчер парсера построчных событий под вид локального CLI.
fn parse_cli_line(kind: CliKind, line: &str) -> Option<ClaudeEvent> {
    match kind {
        CliKind::Claude => events::parse_line(line),
        CliKind::Kimi => events::parse_line_kimi(line),
    }
}

/// Исполняет run через локальный CLI (`claude -p` или `kimi -p`).
///
/// Вся процессная механика (spawn, stdin-пайп, stderr-хвост, таймаут, kill)
/// — ровно та, что была в `ClaudeRunner::stream` до появления облачного
/// транспорта; вынесена сюда, чтобы `stream()` остался диспетчером транспортов.
async fn run_cli(
    tx: mpsc::Sender<ClaudeEvent>,
    kind: CliKind,
    path: PathBuf,
    mut req: RunRequest,
    run_timeout: Duration,
    run_id: RunId,
) {
    let mut cmd = Command::new(&path);
    cmd.args(build_args(kind, &req));

    // stdin открываем пайпом только когда через него идёт prompt (Claude).
    // В остальных случаях сохраняем прежнее `Stdio::null()`: CLI сразу видит
    // EOF и не ждёт ввода. Kimi print-режим stdin не читает — prompt всегда
    // в argv (см. [`build_args`]).
    cmd.stdin(if req.prompt_via_stdin && kind == CliKind::Claude {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                error = %e,
                cli = %path.display(),
                kind = kind.as_str(),
                "failed to spawn LLM CLI"
            );
            let _ = tx
                .send(ClaudeEvent::Error {
                    message: format!("spawn failed: {e}"),
                })
                .await;
            return;
        }
    };

    // Подача prompt'а в stdin — ОБЯЗАТЕЛЬНО в отдельной таске и ОБЯЗАТЕЛЬНО с
    // `shutdown()` в конце:
    //   * отдельная таска — иначе на промпте больше размера pipe-буфера
    //     (64 KiB на Linux, 8—64 KiB на macOS) `write_all` заблокируется,
    //     пока CLI не вычитает, а CLI в это время пишет в stdout, который
    //     мы ещё не начали читать → взаимная блокировка;
    //   * `shutdown()` закрывает пишущий конец: без EOF на stdin CLI считает
    //     ввод незавершённым и не начинает генерацию.
    if req.prompt_via_stdin && kind == CliKind::Claude {
        match child.stdin.take() {
            Some(mut sin) => {
                let payload = std::mem::take(&mut req.prompt);
                let run_id_for_stdin = run_id.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    if let Err(e) = sin.write_all(payload.as_bytes()).await {
                        // Штатный случай: CLI завершился/убит раньше, чем
                        // мы дописали (EPIPE). Диагностика — в лог, run
                        // всё равно свалится по пустому stdout.
                        tracing::debug!(
                            target: "claude_runner",
                            run_id = %run_id_for_stdin,
                            error = %e,
                            "failed to write prompt to cli stdin"
                        );
                    }
                    let _ = sin.shutdown().await;
                });
            }
            None => {
                tracing::error!(
                    target: "claude_runner",
                    "LLM CLI: stdin pipe not captured"
                );
                let _ = tx
                    .send(ClaudeEvent::Error {
                        message: "stdin pipe missing".into(),
                    })
                    .await;
                let _ = child.kill().await;
                return;
            }
        }
    }

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            tracing::error!("LLM CLI: stdout pipe not captured");
            let _ = tx
                .send(ClaudeEvent::Error {
                    message: "stdout pipe missing".into(),
                })
                .await;
            let _ = child.kill().await;
            return;
        }
    };

    // Stderr читаем в отдельной таске для логирования (не emit'им как
    // ClaudeEvent::Error — там бывают noisy warnings). Дополнительно
    // храним последние строки в кольцевом буфере: если CLI упал, ничего
    // не написав в stdout (пустой результат), мы приложим этот хвост к
    // финальному Error — раньше stderr тихо терялся в debug-логе.
    const STDERR_TAIL_MAX: usize = 20;
    let stderr_tail: Arc<Mutex<std::collections::VecDeque<String>>> =
        Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(
            STDERR_TAIL_MAX,
        )));
    let stderr_task = child.stderr.take().map(|stderr| {
        let run_id_for_log = run_id.clone();
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(run_id = %run_id_for_log, stderr = %line, "cli stderr");
                let mut buf = tail.lock().await;
                if buf.len() == STDERR_TAIL_MAX {
                    buf.pop_front();
                }
                buf.push_back(line);
            }
        })
    });

    let mut reader = BufReader::new(stdout).lines();
    // Был ли получен хоть один полезный event из stdout. Если нет —
    // run явно провалился, и stderr-хвост станет телом Error.
    let mut emitted_any = false;
    let mut timed_out = false;
    // Чтение всего стрима оборачиваем в общий таймаут: зависший CLI
    // (нет EOF, нет новых строк) иначе держал бы permit семафора
    // навсегда и выедал все слоты max_parallel_runs. При срабатывании
    // таймаута убиваем дочерний процесс и эмитим Error.
    let read_loop = async {
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    if let Some(ev) = parse_cli_line(kind, &line) {
                        emitted_any = true;
                        if tx.send(ev).await.is_err() {
                            // Потребитель ушёл — убиваем процесс.
                            tracing::debug!(run_id = %run_id, "receiver dropped, killing child");
                            break;
                        }
                    }
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    tracing::warn!(error = %e, "cli stdout read error");
                    break;
                }
            }
        }
    };

    if run_timeout.is_zero() {
        read_loop.await;
    } else {
        match tokio::time::timeout(run_timeout, read_loop).await {
            Ok(()) => {}
            Err(_elapsed) => {
                timed_out = true;
                let tail = {
                    let buf = stderr_tail.lock().await;
                    buf.iter().cloned().collect::<Vec<_>>().join("\n")
                };
                tracing::warn!(
                    run_id = %run_id,
                    timeout_secs = run_timeout.as_secs(),
                    kind = kind.as_str(),
                    "cli run exceeded run_timeout, killing child"
                );
                let mut message = format!(
                    "run timed out after {}s",
                    run_timeout.as_secs()
                );
                if !tail.is_empty() {
                    message.push_str("; stderr: ");
                    message.push_str(&tail);
                }
                let _ = tx.send(ClaudeEvent::Error { message }).await;
            }
        }
    }

    // Run завершился (EOF), но stdout не дал ни одного event'а —
    // CLI почти наверняка упал. Прикладываем хвост stderr к Error,
    // иначе caller получит пустой результат без диагностики.
    if !emitted_any && !timed_out {
        // Дождаться, пока stderr-таска дочитает буфер (CLI уже на EOF
        // stdout, stderr вот-вот закроется). Короткий таймаут чтобы не
        // зависнуть, если stderr почему-то не закрывается.
        if let Some(t) = stderr_task {
            let _ = tokio::time::timeout(Duration::from_secs(2), t).await;
        }
        let tail = {
            let buf = stderr_tail.lock().await;
            buf.iter().cloned().collect::<Vec<_>>().join("\n")
        };
        let message = if tail.is_empty() {
            "cli run produced no output (empty result, no stderr)".to_string()
        } else {
            format!("cli run produced no output; stderr: {tail}")
        };
        tracing::warn!(run_id = %run_id, kind = kind.as_str(), "cli run empty; emitting stderr tail");
        let _ = tx.send(ClaudeEvent::Error { message }).await;
    }

    // Убиваем процесс безусловно после выхода из read-loop: либо он уже
    // завершился (kill — no-op), либо завис/потребитель ушёл/таймаут.
    let _ = child.kill().await;
    // Дождаться завершения процесса (или kill); игнорируем код выхода —
    // для callee важны только события.
    let _ = child.wait().await;
}

/// Разбирает одну строку SSE-ответа облачного chat/completions.
///
/// Возвращает:
/// - `None` — строка не является `data:`-кадром (пустые, `event:`, `id:`…);
/// - `Some(None)` — кадр `[DONE]` — штатный конец стрима;
/// - `Some(Some(ev))` — событие из JSON-полезной нагрузки кадра.
fn parse_sse_data_line(line: &str) -> Option<Option<ClaudeEvent>> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload.is_empty() {
        return None;
    }
    if payload == "[DONE]" {
        return Some(None);
    }
    Some(events::parse_line_kimi(payload))
}

/// Исполняет run через облачный OpenAI-совместимый провайдер
/// (`POST {base_url}/chat/completions` со `stream: true`, ответ — SSE).
///
/// HTTP-клиент и разбор отказов — [`forge_voice::cloud`]: тот же polza.ai-
/// совместимый слой, что у STT/саммари встреч. Поле `stt_*` его конфига здесь
/// не используется — берём base_url, ключ и модель.
async fn run_cloud(
    tx: mpsc::Sender<ClaudeEvent>,
    cfg: CloudTransportCfg,
    req: RunRequest,
    run_timeout: Duration,
    run_id: RunId,
) {
    use forge_voice::cloud::{CloudClient, CloudConfig, SecretKey};

    let api_key = cfg.resolve_api_key();
    if api_key.is_empty() {
        let _ = tx
            .send(ClaudeEvent::Error {
                message: "cloud LLM backend not configured: задайте llm_cloud_api_key в конфиге \
                          плагина или env FORGE_LLM_CLOUD_API_KEY / FORGE_MEET_POLZA_KEY, \
                          либо polza_api_key в ~/.config/forge/meet.toml"
                    .into(),
            })
            .await;
        return;
    }
    let model = cfg.resolve_model();

    let cloud_cfg = CloudConfig {
        base_url: if cfg.base_url.trim().is_empty() {
            forge_voice::cloud::DEFAULT_BASE_URL.to_string()
        } else {
            cfg.base_url.clone()
        },
        api_key: SecretKey::new(&api_key),
        summary_model: model.clone(),
        ..CloudConfig::default()
    };
    let client = match CloudClient::new(&cloud_cfg) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx
                .send(ClaudeEvent::Error { message: e.message() })
                .await;
            return;
        }
    };

    // Тело OpenAI-совместимого запроса: system — отдельным сообщением,
    // prompt — пользовательским.
    let mut messages = Vec::new();
    if let Some(sys) = &req.system {
        messages.push(serde_json::json!({"role": "system", "content": sys}));
    }
    messages.push(serde_json::json!({"role": "user", "content": req.prompt}));
    let body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": messages,
    });

    let response = match client.post("chat/completions").json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            let err = forge_voice::cloud::CloudError::from_reqwest(e);
            tracing::warn!(run_id = %run_id, error = %err, "cloud LLM request failed");
            let _ = tx
                .send(ClaudeEvent::Error { message: err.message() })
                .await;
            return;
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // Читаем тело целиком (там текст отказа) — через status_error из
        // forge-voice, чтобы форма совпадала с остальными облачными путями.
        let text_body = response.text().await.unwrap_or_default();
        let err = forge_voice::cloud::status_error(status, &text_body);
        tracing::warn!(run_id = %run_id, error = %err, "cloud LLM rejected request");
        let _ = tx
            .send(ClaudeEvent::Error { message: err.message() })
            .await;
        return;
    }

    let mut emitted_any = false;
    let mut usage_seen = false;
    let mut timed_out = false;

    // Читаем SSE-кадры чанками, строки накапливаем в буфере. Общий таймаут —
    // тот же контракт, что у CLI-ветки: зависший стрим не должен держать
    // permit семафора вечно. При срабатывании future рвётся, response
    // дропается, соединение закрывается.
    let sse_loop = async {
        let mut response = response;
        let mut buf = String::new();
        'chunks: loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    // Вырезаем и разбираем все полные строки из буфера.
                    while let Some(pos) = buf.find('\n') {
                        let raw_line: String = buf.drain(..=pos).collect();
                        let line = raw_line.trim_end();
                        match parse_sse_data_line(line) {
                            Some(None) => break 'chunks, // [DONE]
                            Some(Some(ev)) => {
                                if matches!(ev, ClaudeEvent::Result { .. }) {
                                    usage_seen = true;
                                }
                                emitted_any = true;
                                if tx.send(ev).await.is_err() {
                                    break 'chunks;
                                }
                            }
                            None => {}
                        }
                    }
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    tracing::warn!(run_id = %run_id, error = %e, "cloud LLM stream read error");
                    break;
                }
            }
        }
        // Хвост буфера без завершающего '\n' (некоторые серверы не шлют
        // последний перевод строки).
        let line = buf.trim();
        if let Some(Some(ev)) = parse_sse_data_line(line) {
            if matches!(ev, ClaudeEvent::Result { .. }) {
                usage_seen = true;
            }
            emitted_any = true;
            let _ = tx.send(ev).await;
        }
    };

    if run_timeout.is_zero() {
        sse_loop.await;
    } else {
        match tokio::time::timeout(run_timeout, sse_loop).await {
            Ok(()) => {}
            Err(_elapsed) => {
                timed_out = true;
                tracing::warn!(
                    run_id = %run_id,
                    timeout_secs = run_timeout.as_secs(),
                    "cloud run exceeded run_timeout, dropping stream"
                );
                let _ = tx
                    .send(ClaudeEvent::Error {
                        message: format!("run timed out after {}s", run_timeout.as_secs()),
                    })
                    .await;
            }
        }
    }

    if !emitted_any && !timed_out {
        tracing::warn!(run_id = %run_id, "cloud run produced no events");
        let _ = tx
            .send(ClaudeEvent::Error {
                message: "cloud run produced no output (empty stream)".to_string(),
            })
            .await;
    } else if emitted_any && !usage_seen && !timed_out {
        // Паритет с CLI-веткой: стрим завершился штатно, но usage-кадр не
        // приехал (провайдер не отдаёт usage при стриминге) — эмитим пустой
        // Result, чтобы у агрегаторов был явный финал run'а.
        let _ = tx
            .send(ClaudeEvent::Result {
                usage: Usage::default(),
                is_error: false,
                raw_json: serde_json::Value::Null,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write_mock_cli(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[tokio::test]
    async fn new_warns_on_missing_cli_but_does_not_panic() {
        // Не должно паниковать.
        let r = ClaudeRunner::new(PathBuf::from("/does/not/exist"), 2);
        assert_eq!(r.available_permits(), 2);
    }

    /// Дефолт — выключено: runner из `new()` не запускает процесс и отдаёт
    /// Error с инструкцией, как включить.
    #[tokio::test]
    async fn stream_rejects_runs_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        // Mock CLI пишет маркер-файл при любом запуске — его НЕ должно быть.
        let marker = dir.path().join("was-run");
        let script = format!(
            "#!/bin/sh\ntouch '{}'\nprintf '%s\\n' '{{\"type\":\"result\",\"usage\":{{}}}}'\n",
            marker.display()
        );
        let cli = write_mock_cli(&dir, &script);
        let runner = ClaudeRunner::new(cli, 4);
        assert!(!runner.enabled(), "дефолт runner'а — выключено");

        let mut rx = runner.stream(RunRequest::new("test")).await;
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            ClaudeEvent::Error { message } => {
                assert!(message.contains("disabled"), "нет диагностики: {message}");
                assert!(
                    message.contains(ENV_RUNS_ENABLED),
                    "инструкция по включению отсутствует: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(
            !marker.exists(),
            "процесс НЕ должен спавниться при выключенном runner'е"
        );
    }

    /// `with_enabled(true)` включает запуски независимо от дефолта.
    #[tokio::test]
    async fn with_enabled_turns_runner_on() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        assert!(runner.enabled());
        let res = runner.one_shot(RunRequest::new("hi")).await.unwrap();
        assert_eq!(res.usage.input_tokens, 1);
    }

    /// Разбор глобального env-флага: истинные значения, ложные и нераспознанный
    /// мусор (он трактуется как «выключено» — безопасная сторона по умолчанию).
    #[test]
    fn env_runs_enabled_accepts_only_truthy_values() {
        let saved = std::env::var(ENV_RUNS_ENABLED).ok();
        for (v, want) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("yes", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("no", false),
            ("off", false),
            ("мусор", false),
            ("", false),
        ] {
            std::env::set_var(ENV_RUNS_ENABLED, v);
            assert_eq!(env_runs_enabled(), want, "value {v:?}");
        }
        let was_set = saved.is_some();
        match saved {
            Some(v) => std::env::set_var(ENV_RUNS_ENABLED, v),
            None => std::env::remove_var(ENV_RUNS_ENABLED),
        }
        // Не задана → выключено (дефолт поведения проекта).
        if !was_set {
            assert!(!env_runs_enabled());
        }
    }

    #[tokio::test]
    async fn stream_yields_events_from_mock_cli() {
        let dir = tempfile::tempdir().unwrap();
        // Mock CLI: игнорит args, печатает 2 text_delta + result, выходит 0.
        // Игнорим аргументы через "$@" в shebang-free скрипте.
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":10,"output_tokens":2}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        let mut rx = runner
            .stream(RunRequest::new("test"))
            .await;

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events.len(), 3, "got {events:?}");
        match &events[0] {
            ClaudeEvent::TextDelta { text } => assert_eq!(text, "Hello "),
            other => panic!("unexpected: {other:?}"),
        }
        match &events[2] {
            ClaudeEvent::Result { usage, .. } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_stdout_emits_error_with_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        // CLI пишет диагностику в stderr и выходит, ничего не дав в stdout.
        let script = r#"#!/bin/sh
echo "fatal: model overloaded" 1>&2
echo "retry later" 1>&2
exit 1
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        let mut rx = runner.stream(RunRequest::new("x")).await;
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events.len(), 1, "expected single Error, got {events:?}");
        match &events[0] {
            ClaudeEvent::Error { message } => {
                assert!(
                    message.contains("stderr") && message.contains("model overloaded"),
                    "stderr tail missing: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_shot_aggregates_text_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"foo"}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"bar"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":7,"output_tokens":3}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        let res = runner.one_shot(RunRequest::new("hi")).await.unwrap();
        assert_eq!(res.text, "foobar");
        assert_eq!(res.usage.input_tokens, 7);
        assert_eq!(res.usage.output_tokens, 3);
    }

    #[tokio::test]
    async fn cancel_aborts_running_stream() {
        let dir = tempfile::tempdir().unwrap();
        // Mock CLI: спит 30 секунд между чанками — даём время отменить.
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"start"}}'
sleep 30
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 4).with_enabled(true));
        let req = RunRequest::new("slow");
        let run_id = req.run_id.clone();
        let mut rx = runner.stream(req).await;

        // Получаем первый чанк.
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("first chunk timed out")
            .expect("first chunk None");
        assert!(matches!(first, ClaudeEvent::TextDelta { .. }));

        // Cancel — должен прервать stream быстро.
        assert!(runner.cancel(&run_id).await, "cancel returned false");

        // Канал должен закрыться (либо новых событий не будет за разумный
        // тайм-аут — мы убиваем процесс через kill_on_drop).
        let next =
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
        // Либо None (закрыт), либо тайм-аут — оба означают, что long sleep не
        // продолжился. Не должны получить второй текстовый чанк.
        match next {
            Ok(None) => { /* ok — закрылся */ }
            Ok(Some(ev)) => match ev {
                ClaudeEvent::Result { .. } => panic!("got Result after cancel"),
                ClaudeEvent::TextDelta { .. } => panic!("got more text after cancel"),
                _ => {}
            },
            Err(_) => { /* тайм-аут — тоже ок, abort произошёл */ }
        }
    }

    #[tokio::test]
    async fn shutdown_aborts_running_streams() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}'
sleep 30
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 4).with_enabled(true));
        let _rx = runner.stream(RunRequest::new("slow")).await;
        // Дать таске взять permit и зарегистрироваться.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(runner.active_runs().await >= 1);
        runner.shutdown().await;
        // После shutdown — running map пуст.
        assert_eq!(runner.active_runs().await, 0);
    }

    // ---- prompt_via_stdin -------------------------------------------------

    /// Инвариант приватности: при подаче через stdin текста промпта нет ни в
    /// одном элементе argv (иначе он виден в `ps aux` любому локальному
    /// пользователю и упирается в ARG_MAX).
    #[test]
    fn build_args_omits_prompt_when_via_stdin() {
        let secret = "СЕКРЕТНЫЙ-ТРАНСКРИПТ-42";
        let req = RunRequest::new(secret)
            .with_prompt_via_stdin(true);
        let args = build_args(CliKind::Claude, &req);
        assert!(
            !args.iter().any(|a| a.contains(secret)),
            "prompt leaked into argv: {args:?}"
        );
        assert_eq!(
            args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
            ]
        );
    }

    /// Обратная сторона: при `false` argv побайтово совпадает с прежним —
    /// флаги в том же порядке, prompt последним positional-аргументом.
    #[test]
    fn build_args_keeps_positional_prompt_by_default() {
        let mut req = RunRequest::new("привет");
        req.model = Some("sonnet".into());
        req.system = Some("sys".into());
        assert!(!req.prompt_via_stdin, "default must stay false");
        assert_eq!(
            build_args(CliKind::Claude, &req),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--model",
                "sonnet",
                "--append-system-prompt",
                "sys",
                "привет",
            ]
        );
    }

    /// Kimi: свои флаги (`--print`, `-m`, `-p <prompt>`), system склеивается
    /// в начало prompt'а — отдельного флага system у print-режима нет.
    #[test]
    fn build_args_for_kimi() {
        let mut req = RunRequest::new("саммари встречи");
        req.model = Some("kimi-k2".into());
        req.system = Some("Ты аналитик".into());
        assert_eq!(
            build_args(CliKind::Kimi, &req),
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "-m",
                "kimi-k2",
                "-p",
                "Ты аналитик\n\nсаммари встречи",
            ]
        );
    }

    /// Kimi print-режим stdin не читает: prompt идёт в argv даже при
    /// `prompt_via_stdin = true` (флаг игнорируется, риск ARG_MAX отмечен
    /// в доке [`build_args`]).
    #[test]
    fn build_args_for_kimi_ignores_via_stdin() {
        let secret = "СЕКРЕТНЫЙ-ТРАНСКРИПТ-99";
        let req = RunRequest::new(secret).with_prompt_via_stdin(true);
        let args = build_args(CliKind::Kimi, &req);
        assert!(
            args.iter().any(|a| a.contains(secret)),
            "kimi print-режим без stdin: prompt обязан быть в argv: {args:?}"
        );
        // Через stdin prompt НЕ дублируется.
        assert_eq!(
            args.iter().filter(|a| a.contains(secret)).count(),
            1
        );
    }

    // ---- parse_sse_data_line ------------------------------------------------

    #[test]
    fn sse_data_line_parses_payload() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#;
        match parse_sse_data_line(line) {
            Some(Some(ClaudeEvent::TextDelta { text })) => assert_eq!(text, "Hi"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_done_line_signals_end() {
        assert_eq!(parse_sse_data_line("data: [DONE]"), Some(None));
        assert_eq!(parse_sse_data_line("data:[DONE]"), Some(None));
    }

    #[test]
    fn sse_non_data_lines_are_skipped() {
        assert_eq!(parse_sse_data_line(""), None);
        assert_eq!(parse_sse_data_line("event: message"), None);
        assert_eq!(parse_sse_data_line("id: 42"), None);
        assert_eq!(parse_sse_data_line("data:"), None);
        assert_eq!(parse_sse_data_line("data:   "), None);
    }

    #[test]
    fn sse_usage_line_maps_to_result() {
        let line = r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3}}"#;
        match parse_sse_data_line(line) {
            Some(Some(ClaudeEvent::Result { usage, .. })) => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Облачный прогон против локального stub: тот же polza-стиль, что и в
    /// `forge-meet` (tests/polza_stub.rs) — base_url подменяется на стенд.
    #[tokio::test]
    async fn cloud_run_against_local_stub() {
        // Стенд: один SSE-ответ с двумя дельтами, usage-кадром и [DONE].
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"При\"}}]}\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\"вет\"}}]}\n\
                    data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\
                    data: [DONE]\n";

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body_c = body;
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut req_bytes = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                req_bytes.extend_from_slice(&tmp[..n]);
                if req_bytes.windows(4).any(|w| w == b"\r\n\r\n")
                    && req_bytes.len() > 250
                {
                    break;
                }
            }
            let req_text = String::from_utf8_lossy(&req_bytes).to_string();
            let header = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body_c.len()
            );
            sock.write_all(header.as_bytes()).await.unwrap();
            sock.write_all(body_c.as_bytes()).await.unwrap();
            req_text
        });

        let cfg = CloudTransportCfg {
            base_url: format!("http://{addr}/api/v1"),
            api_key: "pza_test_key_long_enough_to_mask".to_string(),
            model: "google/gemini-3.5-flash".to_string(),
        };
        let runner = ClaudeRunner::from_transport(Transport::Cloud(cfg), 2).with_enabled(true);
        let res = runner
            .one_shot(RunRequest::new("привет"))
            .await
            .expect("cloud run failed");
        assert_eq!(res.text, "Привет");
        assert_eq!(res.usage.input_tokens, 5);
        assert_eq!(res.usage.output_tokens, 2);

        // Запрос дошёл до стенда с нужными частями: путь, ключ заголовком,
        // модель и stream:true в теле.
        let req_text = server.await.unwrap();
        assert!(req_text.contains("POST /api/v1/chat/completions"), "{req_text}");
        assert!(req_text.contains("authorization: Bearer pza_test_key_long_enough_to_mask"), "{req_text}");
        assert!(req_text.contains("\"stream\":true"), "{req_text}");
        assert!(req_text.contains("google/gemini-3.5-flash"), "{req_text}");

        // Ключ не светится в Debug транспорта.
        let cfg2 = CloudTransportCfg {
            api_key: "pza_test_key_long_enough_to_mask".into(),
            ..Default::default()
        };
        let shown = format!("{cfg2:?}");
        assert!(!shown.contains("pza_test_key_long"), "{shown}");
        assert!(shown.contains("pza_test"), "{shown}");
    }

    /// Облако без ключа: run отказывает понятной ошибкой, ничего не шлёт.
    ///
    /// Тест изолируется от машины разработчика: временно убирает env-ключи и
    /// подменяет HOME на пустой каталог — иначе fallback-цепочка ключа
    /// (FORGE_MEET_POLZA_KEY / ~/.config/forge/meet.toml) нашла бы реальный
    /// ключ и тест стукнулся бы в polza.ai от имени пользователя.
    #[tokio::test]
    async fn cloud_run_without_key_errors() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_llm = std::env::var_os("FORGE_LLM_CLOUD_API_KEY");
        let prev_meet = std::env::var_os("FORGE_MEET_POLZA_KEY");
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("FORGE_LLM_CLOUD_API_KEY");
        std::env::remove_var("FORGE_MEET_POLZA_KEY");

        let cfg = CloudTransportCfg::default();
        let runner = ClaudeRunner::from_transport(Transport::Cloud(cfg), 2).with_enabled(true);
        let err = runner.one_shot(RunRequest::new("hi")).await.unwrap_err();
        assert!(
            err.to_string().contains("not configured"),
            "недиагностичная ошибка: {err}"
        );

        restore_var("HOME", prev_home);
        restore_var("FORGE_LLM_CLOUD_API_KEY", prev_llm);
        restore_var("FORGE_MEET_POLZA_KEY", prev_meet);
    }

    fn restore_var(name: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }

    /// Mock-CLI считает байты на stdin и печатает их числом в text_delta.
    /// Так тест видит, что prompt реально дошёл до дочернего процесса.
    const STDIN_COUNTING_CLI: &str = r#"#!/bin/sh
n=$(wc -c | tr -d ' \n')
printf '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"%s"}}\n' "$n"
printf '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}\n'
"#;

    #[tokio::test]
    async fn prompt_via_stdin_reaches_child_process() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, STDIN_COUNTING_CLI);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        let prompt = "привет из stdin";
        let res = runner
            .one_shot(RunRequest::new(prompt).with_prompt_via_stdin(true))
            .await
            .unwrap();
        assert_eq!(
            res.text.trim(),
            prompt.len().to_string(),
            "child saw wrong byte count on stdin"
        );
    }

    /// При `false` stdin у процесса — `/dev/null`, то есть 0 байт: поведение
    /// прежнее, prompt ушёл в argv (что проверено выше чистым тестом).
    #[tokio::test]
    async fn without_flag_child_stdin_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, STDIN_COUNTING_CLI);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        let res = runner
            .one_shot(RunRequest::new("привет из argv"))
            .await
            .unwrap();
        assert_eq!(res.text.trim(), "0", "stdin must stay null by default");
    }

    /// 60 000 символов — размер реального чат-транскрипта. Проверяем именно
    /// отсутствие дедлока: запись в stdin идёт отдельной таской, а mock-CLI
    /// начинает вычитывать stdin только после того, как мы уже читаем stdout.
    #[tokio::test]
    async fn large_prompt_via_stdin_does_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        // CLI сначала пишет чанк в stdout (мы обязаны его прочитать), и лишь
        // потом вычитывает stdin — худший для дедлока порядок.
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"start "}}'
n=$(wc -c | tr -d ' \n')
printf '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"%s"}}\n' "$n"
printf '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}\n'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4).with_enabled(true);
        let prompt = "x".repeat(60_000);
        let res = tokio::time::timeout(
            Duration::from_secs(30),
            runner.one_shot(RunRequest::new(prompt).with_prompt_via_stdin(true)),
        )
        .await
        .expect("60k prompt via stdin deadlocked")
        .unwrap();
        assert_eq!(res.text, "start 60000");
    }

    #[tokio::test]
    async fn semaphore_caps_concurrent_streams() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
sleep 2
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 2).with_enabled(true));

        // Запускаем 2 stream'а — оба должны быстро взять permit.
        let _rx1 = runner.stream(RunRequest::new("a")).await;
        let _rx2 = runner.stream(RunRequest::new("b")).await;

        // Дать таскам время взять permit'ы.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // Permit'ов должно быть 0 (все 2 заняты).
        assert_eq!(
            runner.available_permits(),
            0,
            "expected 0 permits after 2 streams started"
        );
    }
}
