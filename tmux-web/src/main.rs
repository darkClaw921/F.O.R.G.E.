//! tmux-web — web-viewer для активных tmux-сессий.
//!
//! После Phase 4 (`remove-projects-concept`) понятие «проект» удалено
//! полностью: концепция `ProjectStore`, REST-эндпоинты `/api/projects/*`,
//! поля `SessionDto.project_id/project_name` и фильтрация по
//! `tmux_prefix` сняты. Источник истины — cwd активной сессии; tasks/todos
//! привязаны к корню (`paths::resolve_root` от cwd сессии). Группировка
//! сессий — только folder-headers (`SessionDto.folder_id/folder_label`).

mod attention;
mod auth;
mod cli;
mod daemon;
// Phase 1 Echo plugin — адаптер AppState → echo_host_api::HostApi.
// Регистрируется в main() через forge_echo::register_routes.
mod echo_host;
#[allow(dead_code)] // публичный API используется в Phase 3 (POST /api/todos/:id/promote)
mod notifier;
// Phase 3 — глобальный конфиг notifier'а (template/delay/wait_previous/session).
// Снимает привязку notify-настроек к Project (см. план remove-projects-concept.md).
mod notifier_config;
// Резолв «корня» (.beads/ → .git/ → cwd) для cwd-only архитектуры
// (см. план remove-projects-concept.md). Используется TodoStore (Phase 1+),
// REST/WS API в Phase 2 и notifier в Phase 3.
#[allow(dead_code)] // API будет вызываться из main.rs хендлеров в Phase 2
mod paths;
mod pty;
mod qr_print;
// Phase 3 — модуль HTTP-прокси на удалённые devforge. Публичные функции
// (`proxy_request`, `enrich_with_origin`) используются в handler'ах
// resource-routes (task forge-v5x9.4), но до их подключения компилятор
// видит как dead_code. Аллов точечно на уровне модуля.
#[allow(dead_code)]
mod remote_proxy;
mod remotes;
mod server_config;
mod session_history;
mod static_embed;
mod tasks;
mod tasks_watcher;
mod themes;
mod tmux;
mod todos;
mod user_settings;
mod ws;
mod ws_tasks;
mod ws_todos;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap as AxumHeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch, RwLock};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::notifier_config::{NotifierConfig, NotifierConfigStore, PatchNotifierConfigReq};
use crate::tasks::TaskEvent;
use crate::themes::{Theme, ThemesState};
use crate::tmux::SessionInfo;
use crate::todos::TodoStore;
use crate::user_settings::{PatchUserSettingsReq, UserSettings, UserSettingsStore};
use crate::ws_todos::TodoEvent;

/// Глобальное состояние axum-приложения.
///
/// После Phase 4 (`remove-projects-concept`) поле `projects: ProjectStore`
/// удалено — концепция проектов снята целиком. Tasks/todos берут cwd либо
/// из явного query-параметра `?path=`, либо из `active_path_tx`
/// (extension-point — обновляется хост-кодом, например при переключении
/// активной сессии). Дефолтное начальное значение — cwd процесса.
///
/// - `tasks_tx` — broadcast-sender, в который [`tasks_watcher::run_watcher`]
///   пушит [`TaskEvent`] при изменениях `.beads/issues.jsonl`. WS-handler
///   `/ws/tasks` делает `subscribe()` на каждое соединение.
/// - `active_path_tx` — watch-sender для пересоздания watcher'а при смене
///   активного пути. Sender держится в state, изменение пути — опциональный
///   extension-point (см. doc-string `tasks_watcher::run_watcher`).
#[derive(Clone)]
struct AppState {
    /// broadcast-канал глобальных task-событий активного пути.
    /// Используется `notifier.rs` (subscribe в `main()` до создания AppState).
    /// `ws_tasks` больше не подписывается — у каждого WS свой per-conn watcher.
    /// Поле остаётся в state, чтобы newer endpoints могли получить subscribe()
    /// при необходимости.
    #[allow(dead_code)]
    tasks_tx: broadcast::Sender<TaskEvent>,
    active_path_tx: Arc<watch::Sender<PathBuf>>,
    /// Shared state of «session needs attention» flags. Background
    /// [`attention::watcher_loop`] writes here every 1.5s; HTTP-handler
    /// [`get_sessions`] reads a snapshot and exposes it as
    /// `SessionDto.needs_attention`.
    attention: Arc<attention::AttentionState>,
    /// Phase 2 — handle к фоновому notifier_loop (см. `notifier.rs`).
    /// Используется в Phase 3 эндпоинтом `POST /api/todos/:id/promote`,
    /// который ставит [`notifier::NotifyJob`] в очередь через
    /// `notify.enqueue(job).await`. Cheap-clonable (`mpsc::Sender` внутри).
    notify: notifier::NotifyHandle,
    /// Phase 3 — TODO-карточки активного проекта. Cheap-clonable
    /// (`Arc<RwLock<...>>` внутри `TodoStore`). Используется CRUD-роутами
    /// `/api/todos*` и WS-handler'ом `/ws/todos`.
    todos: TodoStore,
    /// Phase 3 — broadcast-канал событий TODO. Sender'ы — REST-handler'ы
    /// `/api/todos*`; subscribers — WS-handler `/ws/todos`. При мутации
    /// (create/update/delete/promote) handler шлёт `TodoEvent::Upsert` /
    /// `TodoEvent::Removed`. Buffer = 64 — достаточно для коротких burst'ов.
    todos_tx: tokio::sync::broadcast::Sender<TodoEvent>,
    /// Phase wk7 — состояние тем (active id + custom themes). Live-state в
    /// памяти под `RwLock`, персистится в `<data_dir>/themes.json` через
    /// [`themes::save`]. Чтение GET /api/themes* — read-lock; мутирующие
    /// PATCH/POST/PUT/DELETE — write-lock.
    themes: Arc<RwLock<ThemesState>>,
    /// Phase wk7 — каталог для `themes.json` (типично `~/.config/forge/`).
    /// Хранится в state, чтобы не пересчитывать его в каждом handler'е.
    themes_dir: PathBuf,
    /// Phase 1 — флаг remote-mode. True ⇒ сервер запущен с `--remote`
    /// (или с server_config.json, подразумевающим remote). Используется
    /// `/healthz` для информирования frontend'а и (в будущем) для
    /// активации `?server=<id>` веток и `/api/remote-servers` маршрутов.
    remote_mode: bool,
    /// Phase 1 — Bearer-token, ожидаемый middleware'ом. `None` в legacy
    /// localhost-режиме (middleware не подключается). `Some` в remote-mode.
    /// `Arc<Option<String>>` чтобы клонировать AppState дёшево.
    #[allow(dead_code)]
    auth_token: Arc<Option<String>>,
    /// Phase 2 — реестр remote-серверов (другие devforge-инстансы, к которым
    /// этот локальный клиент может подключаться). Store независим от
    /// `remote_mode`: реестр редактируется CLI всегда; REST CRUD маршруты
    /// `/api/remote-servers/...` регистрируются ТОЛЬКО при `remote_mode=true`.
    /// Доступ через `Arc<RwLock<...>>` — параллельные чтения, сериализованные
    /// записи.
    remotes: Arc<RwLock<remotes::RemoteServerStore>>,
    /// Phase 3 — общий HTTP-клиент для proxy-запросов на удалённые devforge.
    /// `reqwest::Client` внутри — Arc-обёртка над пулом TCP-соединений
    /// (cheap-clonable, потокобезопасен). Один клиент на сервер — это идиома
    /// reqwest (а не `Client::new()` в каждом handler'е). Используется в
    /// `remote_proxy::proxy_request` (handler'ы подключают в Phase 3.4).
    #[allow(dead_code)]
    http: reqwest::Client,
    /// User-level настройки (`~/.forge/user_settings.json`). Cheap-clonable
    /// (Arc<RwLock> внутри). Используется REST-handler'ами
    /// `/api/user-settings` и `promote_todo` (кастомный plan_mode_suffix).
    user_settings: UserSettingsStore,
    /// Phase 3 — глобальный конфиг notifier'а
    /// (`~/.config/forge/notifier.json`). Cheap-clonable (Arc<RwLock> внутри).
    /// Используется `promote_todo` (template/delay/wait_previous/session) и
    /// REST-эндпоинтами `/api/notifier-config`. Заменяет соответствующие
    /// поля старого `Project` (см. план `remove-projects-concept.md`).
    notifier_config: NotifierConfigStore,
    /// История tmux-сессий (`~/.config/forge/session_history.json`).
    /// Персистентный журнал когда-либо виденных сессий: фундамент для
    /// «главной»/«недавних» сессий и их восстановления. Cheap-clonable
    /// (`Arc<RwLock>` внутри [`session_history::HistoryStore`]). Наполняется
    /// периодическим воркером (раз в час) и shutdown-хуком через
    /// [`session_history::capture_now`]; читается/мутируется REST-роутами
    /// `/api/sessions/history*`.
    history: session_history::HistoryStore,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --help / -h — обрабатывается до парсинга и до тяжёлой инициализации,
    // чтобы Homebrew `test do` блок (`devforge --help`) не упирался в
    // отсутствие tmux/config/etc.
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{}", cli::help_text());
        std::process::exit(0);
    }

    // CLI: подкоманды run/start/stop/status + флаг --port. На ошибки парсинга
    // печатаем человекочитаемое сообщение в stderr с exit 2 (стандарт для usage
    // errors).
    let mode = match cli::parse() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
    };

    // Подкоманды-менеджеры daemon'а не требуют ни tokio-runtime, ни
    // инициализации сервера — обрабатываем и выходим. (Сам runtime уже поднят
    // атрибутом #[tokio::main], но это дешёвый no-op для start/stop/status.)
    let run_opts = match mode {
        cli::Mode::Start(opts) => {
            daemon::start(&opts)?;
            return Ok(());
        }
        cli::Mode::Stop => {
            daemon::stop()?;
            return Ok(());
        }
        cli::Mode::Status => {
            daemon::status()?;
            return Ok(());
        }
        cli::Mode::Pair(opts) => {
            cli::run_pair(&opts)?;
            return Ok(());
        }
        cli::Mode::Remote(cmd) => {
            cli::run_remote(&cmd)?;
            return Ok(());
        }
        cli::Mode::Run(opts) => opts,
    };

    // Phase 1 — резолвинг эффективной конфигурации сервера.
    //
    // Источники (в порядке приоритета): CLI > server_config.json > env > default.
    // Env DEVFORGE_AUTH_TOKEN уже подмешан в run_opts.token парсером cli::parse,
    // поэтому здесь видим CLI+env как одно целое.
    //
    // - Если файла нет — load() вернёт Ok(None), resolve() работает с CLI only.
    // - Если файл повреждён — печатаем warning и игнорируем (legacy localhost
    //   fall-back). Не fail-fast, чтобы битый конфиг не блокировал работу.
    let file_cfg = match server_config::load() {
        Ok(opt) => opt,
        Err(e) => {
            eprintln!(
                "[devforge] WARNING: failed to load server_config.json: {e:#}. \
                 Falling back to CLI/env only."
            );
            None
        }
    };
    let effective = server_config::resolve(&run_opts, file_cfg.as_ref());
    // Финализация токена: в remote-mode без явного токена генерируем 64-hex
    // и сохраняем в server_config.json. Печатает банер при auto-gen.
    let auth_token_value = server_config::finalize_token(&effective);
    let port = effective.port;
    let bind_host = effective.bind.clone();
    let remote_mode = effective.remote_mode;

    // Инициализация логирования. По умолчанию: info для всего + debug для tmux_web.
    // Переопределяется переменной окружения RUST_LOG.
    //
    // Phase 6 (Echo) — если FORGE_ECHO_DEBUG=1 и RUST_LOG не задан явно,
    // дополнительно включаем `forge_echo=debug` чтобы пользователь видел
    // streaming-события Echo (prompt/usage/scheduler tick).
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            let echo_debug = std::env::var("FORGE_ECHO_DEBUG")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if echo_debug {
                EnvFilter::new("info,tmux_web=debug,forge_echo=debug")
            } else {
                EnvFilter::new("info,tmux_web=debug")
            }
        }))
        .init();

    // Каналы для realtime task-watcher.
    //
    // tasks_tx: broadcast(64) — глубина 64 сообщений достаточна для
    // короткого burst'а (`br sync` обычно даёт 1-3 события). При лагающем
    // подписчике broadcast::Sender::send просто дропает старые, что для
    // нашего случая ОК — UI всё равно может выпасть снимок при reconnect.
    //
    // active_path_tx: watch — последняя value-семантика, идеально для
    // «активный путь сейчас X». Watcher подписывается через .changed().
    //
    // После Phase 4 (`remove-projects-concept`) initial_path берётся из
    // cwd процесса — раньше это был `store.active().path`. Если cwd
    // получить не удалось (теоретический edge-case), используем `/`.
    let (tasks_tx, _) = broadcast::channel::<TaskEvent>(64);
    let initial_path = std::env::current_dir().unwrap_or_else(|e| {
        tracing::warn!(error = ?e, "failed to read current_dir; falling back to /");
        PathBuf::from("/")
    });
    let (active_path_tx, active_path_rx) = watch::channel(initial_path.clone());
    let active_path_tx = Arc::new(active_path_tx);

    // Phase 2 — Notifier подсистема. Subscribe ДО передачи tasks_tx в watcher,
    // иначе ранние closed-events могут быть пропущены (broadcast хранит только
    // 64 last). NotifyHandle — cheap-clonable, кладём в AppState для
    // эндпоинтов promote (Phase 3).
    let notify_handle = notifier::start(initial_path.clone(), tasks_tx.subscribe());
    tracing::info!(
        path = %initial_path.display(),
        "notifier subsystem started"
    );

    // Phase 3 — TodoStore + broadcast-канал TODO-событий.
    // TodoStore привязывается к корню активного проекта (.forge/todos.json).
    // При смене активного проекта store *не* пересоздаётся — todos.json
    // остаётся в исходной папке. Если нужен per-project switching на лету —
    // это отдельная задача (см. план Phase будущего refactor).
    let todos_store = TodoStore::new(initial_path.clone())
        .with_context(|| format!("failed to init TodoStore at {}", initial_path.display()))?;
    let (todos_tx, _) = broadcast::channel::<TodoEvent>(64);

    // Phase wk7 — Themes state. Каталог `~/.config/forge/` — стандартное
    // место для всех глобальных конфигов F.O.R.G.E. После удаления
    // ProjectStore (Phase 4 remove-projects-concept) computed напрямую из
    // HOME. Если файла нет или он повреждён — `themes::load` вернёт
    // ThemesState::default() (active="default", custom=[]) без паники.
    let themes_dir = match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(".config").join("forge"),
        Err(_) => PathBuf::from("."),
    };
    if let Err(e) = std::fs::create_dir_all(&themes_dir) {
        tracing::warn!(
            path = %themes_dir.display(),
            error = ?e,
            "failed to create themes parent dir; first theme save may fail"
        );
    }
    let themes_state = themes::load(&themes_dir);
    tracing::info!(
        dir = %themes_dir.display(),
        active = %themes_state.active,
        custom = themes_state.custom.len(),
        "loaded themes state"
    );

    // Phase 2 — реестр remote-серверов. Файл создаётся лениво только при
    // первом save() — пустой store не создаёт файл. Если загрузка упала
    // (повреждённый JSON), печатаем warning и стартуем с пустым реестром —
    // не fail-fast, чтобы битый файл не блокировал работу devforge.
    let remotes_path = remotes::default_remotes_path()?;
    let remotes_store = match remotes::RemoteServerStore::load(remotes_path.clone()) {
        Ok(s) => {
            tracing::info!(
                path = %remotes_path.display(),
                count = s.list().len(),
                "loaded remote servers registry"
            );
            s
        }
        Err(e) => {
            eprintln!(
                "[devforge] WARNING: failed to load remote_servers.json: {e:#}. \
                 Starting with empty registry."
            );
            // Загружаем по пустому пути — но если файл проблемный, новый
            // load() с тем же путём упадёт снова. Возвращаем пустой store
            // через manual init: load на несуществующий путь точно работает.
            // Если path-каталог недоступен — пробрасываем оригинальную ошибку.
            remotes::RemoteServerStore::load(remotes_path.clone())
                .unwrap_or_else(|_| {
                    // Последний резорт — создать в /tmp, чтобы AppState
                    // имел валидный store. На практике это почти невозможный
                    // путь, но статическая гарантия для типов важна.
                    remotes::RemoteServerStore::load(
                        std::env::temp_dir().join("devforge_remotes_fallback.json"),
                    )
                    .expect("tmp fallback for remotes store")
                })
        }
    };

    // User-level настройки: ~/.forge/user_settings.json. Каталог создаём
    // eagerly через create_dir_all (no-op если уже есть) — Store сам не
    // создаёт parent при отсутствии файла, а нам нужно, чтобы первый patch
    // не падал из-за отсутствия `.forge/`. Файл данных создаётся lazy.
    let user_settings_path = match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(".forge").join("user_settings.json"),
        Err(e) => {
            tracing::warn!(error = ?e, "HOME env var not set; user_settings persistence disabled");
            // Фолбэк в temp dir — Store будет работать на чтение/запись,
            // но настройки не переживут перезапуск. Сохраняем работоспособность.
            std::env::temp_dir().join("devforge_user_settings.json")
        }
    };
    if let Some(parent) = user_settings_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                error = ?e,
                "failed to create user_settings parent dir; first patch may fail"
            );
        }
    }
    let user_settings_store = UserSettingsStore::new(user_settings_path.clone());
    tracing::info!(
        path = %user_settings_path.display(),
        "user_settings store initialized"
    );

    // Phase 3 — глобальный notifier-config: `~/.config/forge/notifier.json`.
    // Файл создаётся лениво при первом PATCH/PUT. parent-каталог создаём
    // eagerly, чтобы первая запись не падала из-за отсутствия `~/.config/forge/`.
    let notifier_cfg_path = notifier_config::default_config_path();
    if let Some(parent) = notifier_cfg_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                error = ?e,
                "failed to create notifier-config parent dir; first patch may fail"
            );
        }
    }
    let notifier_config_store = NotifierConfigStore::new(notifier_cfg_path.clone());
    tracing::info!(
        path = %notifier_cfg_path.display(),
        "notifier-config store initialized"
    );

    // История сессий — `<themes_dir>/session_history.json` (тот же data-каталог
    // `~/.config/forge/`, что и themes/notifier). Отсутствующий/битый файл →
    // пустой стор без паники (см. `HistoryStore::load`).
    let history_store = session_history::HistoryStore::load(&themes_dir);
    tracing::info!(
        dir = %themes_dir.display(),
        count = history_store.list().len(),
        "session history store loaded"
    );

    let app_state = AppState {
        tasks_tx: tasks_tx.clone(),
        active_path_tx: active_path_tx.clone(),
        attention: Arc::new(attention::AttentionState::new()),
        notify: notify_handle,
        todos: todos_store,
        todos_tx,
        themes: Arc::new(RwLock::new(themes_state)),
        themes_dir,
        remote_mode,
        auth_token: Arc::new(auth_token_value.clone()),
        remotes: Arc::new(RwLock::new(remotes_store)),
        // Phase 3 — общий reqwest-клиент. Дефолтные настройки (без таймаутов
        // выставленных явно — это TODO для Phase 7 robustness; пока полагаемся
        // на дефолты reqwest 0.12, где connect timeout не задан, общий — 30s).
        http: reqwest::Client::new(),
        user_settings: user_settings_store,
        notifier_config: notifier_config_store,
        history: history_store,
    };

    // Spawn фоновый watcher. Живёт всю жизнь процесса; завершится только
    // если active_path_tx будет дропнут (а он живёт в AppState — т.е.
    // фактически до shutdown'а).
    tokio::spawn(async move {
        tasks_watcher::run_watcher(active_path_rx, tasks_tx).await;
        tracing::info!("tasks_watcher exited");
    });

    // Spawn attention-watcher: каждые 1.5с обходит ВСЕ tmux-сессии,
    // дёргает `tmux capture-pane` и обновляет `app_state.attention` флагом
    // «Claude prompt открыт». Живёт всю жизнь процесса (внутренний loop без
    // условий выхода). Передаём только `attention` (а не AppState целиком),
    // чтобы избежать циклических импортов между main.rs и attention.rs.
    // Фильтрация по проекту не нужна: фронтенду требуются флаги для всех
    // сессий (cross-project visibility).
    tokio::spawn(attention::watcher_loop(app_state.attention.clone()));

    // Spawn периодический snapshot-воркер истории сессий. Раз в час (а также
    // сразу на первом тике — `tokio::time::interval` по умолчанию срабатывает
    // немедленно) дёргает `session_history::capture_now`, который опрашивает
    // tmux и upsert'ит снимок в стор с атомарной записью на диск. В spawn
    // передаём только cheap-clone `HistoryStore` (Arc внутри), а не весь
    // AppState — воркеру не нужны остальные поля состояния.
    {
        let history_for_worker = app_state.history.clone();
        tokio::spawn(async move {
            tracing::info!("session_history snapshot worker started (interval 3600s)");
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                session_history::capture_now(&history_for_worker).await;
            }
        });
    }

    // Static-ассеты встроены в бинарь через rust-embed (см. mod static_embed).
    // Никакой зависимости от cwd или каталога рядом с бинарём — это позволяет
    // ставить devforge через Homebrew/`cargo install` и запускать откуда угодно.
    tracing::info!("serving embedded static assets");

    let mut app = Router::new()
        .route("/healthz", get(healthz))
        // Sessions API.
        .route("/api/sessions", get(get_sessions).post(create_session))
        .route("/api/sessions/:name", delete(delete_session).patch(rename_session))
        .route(
            "/api/sessions/:name/windows",
            get(list_windows).post(create_window),
        )
        .route(
            "/api/sessions/:name/windows/:index",
            delete(delete_window).patch(patch_window),
        )
        .route(
            "/api/sessions/:name/windows/:index/select",
            post(select_window),
        )
        // Session history API — журнал ранее виденных сессий + восстановление.
        .route(
            "/api/sessions/history",
            get(get_session_history).delete(delete_session_history),
        )
        .route("/api/sessions/history/restore", post(restore_session_history))
        .route(
            "/api/sessions/history/restore-all",
            post(restore_all_session_history),
        )
        // Tasks API.
        .route("/api/tasks", get(get_tasks).post(create_task))
        .route("/api/tasks/:id", patch(patch_task).delete(close_task))
        .route("/api/tasks/:id/reopen", post(reopen_task))
        .route("/api/tasks/:id/purge", post(purge_task))
        // Todos API (Phase 3).
        .route("/api/todos", get(get_todos).post(create_todo))
        .route("/api/todos/:id", patch(patch_todo).delete(delete_todo))
        .route("/api/todos/:id/promote", post(promote_todo))
        // Themes API (Phase wk7).
        .route("/api/themes", get(get_themes))
        .route("/api/themes/active", get(get_active_theme).patch(patch_active_theme))
        .route("/api/themes/custom", post(create_custom_theme))
        .route(
            "/api/themes/custom/:id",
            put(put_custom_theme).delete(delete_custom_theme),
        )
        // User-level settings: глобальные пользовательские настройки,
        // персистятся в ~/.forge/user_settings.json. GET возвращает текущий
        // снимок (или дефолты, если файл отсутствует); PATCH применяет
        // частичный апдейт и атомарно сохраняет на диск.
        .route(
            "/api/user-settings",
            get(get_user_settings).patch(patch_user_settings),
        )
        // Phase 3 — глобальный конфиг notifier'а
        // (`~/.config/forge/notifier.json`). PUT — полная замена;
        // PATCH — частичный update. Используется фронтом Phase 5 (Settings
        // modal — глобальный notifier-template без project-вкладки).
        .route(
            "/api/notifier-config",
            get(get_notifier_config)
                .put(put_notifier_config)
                .patch(patch_notifier_config),
        )
        // WebSocket-attach в tmux-сессию.
        .route("/ws/attach", get(ws::attach))
        // WebSocket — lazygit TUI в браузере (по cwd проекта).
        .route("/ws/lazygit", get(ws::lazygit_attach))
        // WebSocket — lazydocker TUI (Docker manager) в браузере.
        .route("/ws/lazydocker", get(ws::lazydocker_attach))
        // WebSocket — television (tv) TUI fuzzy-finder в браузере.
        .route("/ws/telescope", get(ws::telescope_attach))
        // Phase 6.D — WS-стрим realtime событий из beads watcher'а.
        .route("/ws/tasks", get(ws_tasks::tasks_ws))
        // Phase 3 — WS-стрим TODO-карточек.
        .route("/ws/todos", get(ws_todos::todos_ws))
        .with_state(app_state.clone());

    // Phase 2 — REST-эндпоинты реестра remote-серверов. Регистрируются ТОЛЬКО
    // в remote-mode. В обычном (localhost) режиме обращение к
    // /api/remote-servers вернёт 404 (fallback на статику). Это сознательное
    // решение: реестр редактируется через CLI `devforge remote ...` всегда,
    // но веб-UI настроек remote-серверов имеет смысл только в публичном
    // режиме, где сам сервер может быть удалённым agregator'ом.
    if remote_mode {
        let remotes_router: Router<AppState> = Router::new()
            .route(
                "/api/remote-servers",
                get(list_remote_servers).post(create_remote_server),
            )
            .route(
                "/api/remote-servers/:id",
                delete(delete_remote_server).patch(patch_remote_server),
            )
            .route(
                "/api/remote-servers/:id/healthz",
                get(remote_server_healthz),
            );
        app = app.merge(remotes_router.with_state(app_state.clone()));
        tracing::info!("registered /api/remote-servers routes (remote mode)");
    }

    // Phase 1 (Echo) — регистрация плагина forge-echo.
    //
    // ВАЖНО: register_routes вызывается ДО .layer(...) ниже, чтобы
    // bearer_auth middleware покрыл /api/echo/* и (в будущем) /ws/echo
    // автоматически в remote-mode. Static-fallback и trace-layer
    // применяются уже к мердженному роутеру.
    let echo_cfg = forge_echo::EchoConfigStub::default();
    let echo_state = forge_echo::init(echo_cfg)
        .await
        .context("forge_echo::init failed")?;
    let echo_host: Arc<dyn echo_host_api::HostApi> = Arc::new(echo_host::EchoHostAdapter {
        state: app_state.clone(),
    });
    let app = forge_echo::register_routes(app, echo_state.clone(), echo_host.clone());
    tracing::info!("forge-echo: plugin registered");
    // Phase 4 — поднимаем background scheduler автономных задач.
    forge_echo::spawn_workers(&echo_state, echo_host.clone());
    let _ = (&echo_state, &echo_host); // удерживаем для будущих фаз (см. план)

    let mut app = app
        // Embedded static fallback: serve_static резолвит "/" → index.html и
        // отдаёт остальные ассеты с правильным Content-Type из bytes-секции
        // бинаря. Используется .fallback() (Handler), а не .fallback_service().
        .fallback(static_embed::serve_static)
        .layer(TraceLayer::new_for_http());

    // Phase 1 — Bearer-auth middleware (только в remote-mode с заданным
    // токеном). В legacy localhost mode middleware вообще не подключается,
    // что побитово сохраняет старое поведение.
    //
    // .layer применяется после .with_state/.fallback/.layer(Trace) — это
    // axum-идиома: внешний слой выполняется первым на входящий запрос.
    // Path-exclusion для /healthz и статики делает само middleware
    // (см. `auth::is_path_excluded`).
    if let Some(token) = auth_token_value.clone() {
        let auth_state = auth::AuthState::new(Some(token));
        app = app.layer(from_fn_with_state(auth_state, auth::bearer_auth));
        tracing::info!("Bearer-auth middleware enabled (remote mode)");
    }

    // Phase 1 — финальный bind:
    // - remote_mode=false → 127.0.0.1:<port> (legacy, hardcoded — гарант
    //   что без --remote ничего наружу не торчит).
    // - remote_mode=true → <bind_host>:<port>, где bind_host резолвится из
    //   CLI/файла, или 0.0.0.0 по умолчанию.
    let addr: SocketAddr = if remote_mode {
        format!("{bind_host}:{port}")
            .parse()
            .with_context(|| format!("invalid bind address {bind_host}:{port}"))?
    } else {
        SocketAddr::from(([127, 0, 0, 1], port))
    };
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!(
        %addr,
        remote_mode,
        auth = auth_token_value.is_some(),
        "listening on http://{addr}"
    );

    // Phase 7 — public-bind warning. Печатается ВСЕГДА при remote_mode и
    // non-loopback bind, чтобы пользователь увидел отсутствие TLS даже если
    // токен был явно передан через CLI/env (а не авто-сгенерён). На loopback
    // — no-op.
    if remote_mode {
        server_config::print_public_bind_warning(
            &bind_host,
            port,
            auth_token_value.as_deref(),
        );
    }

    // QR-баннер для подключения с телефона. Печатается всегда: на loopback
    // даёт LAN-IP с подсказкой запустить с --remote, на remote-mode — QR
    // с реальным bind/LAN-URL. В remote-mode URL включает токен в hash
    // (#token=...), чтобы клиент мог авторизоваться без ручного ввода.
    qr_print::print_startup_qr(&bind_host, port, remote_mode, auth_token_value.as_deref());

    // Phase 6 (Echo) — graceful shutdown по Ctrl-C / SIGTERM. axum::serve
    // принимает фьючер shutdown-signal'а; внутри сначала ждём ctrl_c (или
    // SIGTERM на unix), потом завершаем Echo (kill дочерние claude + abort
    // worker'ов + закрытие соединений). После возврата future axum закрывает
    // listener и грейсфулли разлогинивает все активные соединения.
    let echo_for_shutdown = echo_state.clone();
    let history_for_shutdown = app_state.history.clone();
    let shutdown_signal = async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received; running Echo graceful shutdown");
        forge_echo::shutdown(&echo_for_shutdown).await;
        // Финальный снимок истории сессий перед выходом — чтобы последнее
        // состояние активных сессий/окон попало в журнал, даже если до
        // следующего часового тика воркера дело не дошло.
        tracing::info!("capturing final session history snapshot before shutdown");
        session_history::capture_now(&history_for_shutdown).await;
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .context("axum server error")?;

    Ok(())
}

/// Phase 6 (Echo) — ждёт Ctrl-C, либо SIGTERM (на unix). Возвращается, как
/// только пришёл первый из сигналов. Используется в graceful-shutdown
/// future для axum::serve.
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = ?e, "failed to install ctrl_c handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = ?e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// Phase 1 — структура ответа `GET /healthz`. Используется frontend'ом ДО
/// получения токена (см. контракт auth::is_path_excluded), чтобы прочитать
/// `remote_mode` и решить, рисовать ли UI логина. Поле `version` берётся из
/// `CARGO_PKG_VERSION` и пригодится для отображения в footer'е/about.
#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: &'static str,
    remote_mode: bool,
    version: &'static str,
}

/// Health-check endpoint.
///
/// Возвращает `application/json` (axum::Json делает это автоматически):
/// ```json
/// { "status": "ok", "remote_mode": <bool>, "version": "<x.y.z>" }
/// ```
///
/// Доступен без Bearer-токена (см. `auth::EXCLUDED_EXACT`). Это сознательное
/// решение: frontend в remote-mode должен иметь возможность проверить факт
/// доступности сервера и режим до того, как пользователь введёт токен.
async fn healthz(State(state): State<AppState>) -> Json<HealthzResponse> {
    Json(HealthzResponse {
        status: "ok",
        remote_mode: state.remote_mode,
        version: env!("CARGO_PKG_VERSION"),
    })
}

// =============================================================================
// Phase 3 — общий хелпер для ?server=<id> ветки в handler'ах
// =============================================================================

/// Извлекает `server` из query-параметров.
///
/// Поведение: query — `HashMap<String, String>`, парсится axum'ом из строки
/// типа `?server=office&foo=bar`. Возвращаем `Some(id)` только если значение
/// непустое после trim. Это упрощает контракт: `?server=` (пустой) трактуется
/// как «нет server».
///
/// `?server=local` — зарезервированное значение: воспринимается как «нет
/// server» (passthrough к локальной логике). Это позволяет фронтенду явно
/// указывать local-источник в URL без специальной ветки на стороне сервера.
fn extract_server_id(q: &HashMap<String, String>) -> Option<String> {
    q.get("server")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| s != "local")
}

/// Результат решения о dispatch'е запроса с возможным `?server=<id>`.
///
/// Pure helper, выделен для unit-тестов try_proxy_to_remote-логики (Phase 8 .3).
/// Не зависит от tokio/AppState — принимает только две детали: server_id
/// (после extract_server_id) и флаг remote_mode.
#[derive(Debug, PartialEq, Eq)]
enum DispatchDecision {
    /// Локальная обработка (без проксирования). Происходит когда `?server=`
    /// отсутствует, пустой или равен зарезервированному `local`.
    Local,
    /// Проксировать на удалённый сервер с указанным id. Гарантирует, что
    /// `remote_mode == true`.
    Proxy(String),
    /// `?server=<id>` указан, но сервер запущен в legacy-режиме
    /// (без `--remote`). Должен вернуться `400 Bad Request`.
    LegacyRejection,
}

/// Чистый dispatcher для прокси-логики.
///
/// Контракт (зафиксирован в `Phase 8 .3` интеграционных тестах):
/// - `q[server]` отсутствует / `""` / `"   "` → [`DispatchDecision::Local`].
/// - `q[server] == "local"` → [`DispatchDecision::Local`] (зарезервированное имя).
/// - `q[server] == "<id>"` && `remote_mode == false` → [`DispatchDecision::LegacyRejection`].
/// - `q[server] == "<id>"` && `remote_mode == true` → [`DispatchDecision::Proxy(id)`].
///
/// Multiple `?server=a&server=b` — axum при парсинге в HashMap оставляет
/// одно значение (последнее, как правило, но это implementation-detail
/// serde_urlencoded). Тест-as-spec фиксирует: dispatcher работает с тем,
/// что отдал axum.
fn resolve_dispatch(q: &HashMap<String, String>, remote_mode: bool) -> DispatchDecision {
    match extract_server_id(q) {
        None => DispatchDecision::Local,
        Some(id) if !remote_mode => {
            tracing::trace!(server_id = %id, "resolve_dispatch: legacy mode rejects ?server");
            DispatchDecision::LegacyRejection
        }
        Some(id) => DispatchDecision::Proxy(id),
    }
}

/// Сериализует пары `HashMap<String, String>` обратно в query-строку для
/// проксирования. Параметр `server` исключается (он адресован локальному
/// прокси, а не remote'у). Возвращает строку БЕЗ ведущего `?`.
///
/// Порядок ключей не гарантирован (`HashMap` — unordered). Это допустимо,
/// потому что HTTP query семантически — мульти-множество, а remote handler'ы
/// читают по имени.
fn rebuild_query_without_server(q: &HashMap<String, String>) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(q.len());
    for (k, v) in q.iter() {
        if k == "server" {
            continue;
        }
        // Минимальный url-encoding для значений: replace & и = (рядом с этими
        // спецсимволами axum/reqwest сами обрабатывают остальное; у нас же
        // на практике передаются ids/строки без таких символов).
        let kv = format!("{}={}", urlencode_minimal(k), urlencode_minimal(v));
        parts.push(kv);
    }
    parts.join("&")
}

/// Минимальный url-encoder для query-значений: экранирует `&`, `=`, `?`,
/// `#`, ` ` (space) в `%XX`. Для типичных id-значений (alnum, `-`, `_`)
/// никаких изменений. Полноценный URL-encoder не тянем, чтобы не добавлять
/// зависимость percent-encoding в Cargo.toml.
fn urlencode_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            '+' => out.push_str("%2B"),
            c => out.push(c),
        }
    }
    out
}

/// Превращает ответ от `remote_proxy::proxy_request` в `axum::Response`.
///
/// Если `enrich_array` == `true`, тело пытается распарситься как JSON, и при
/// успехе — массивные item'ы обогащаются полем `origin = <server_id>` через
/// [`remote_proxy::enrich_with_origin`]. Если тело не JSON — отдаём как есть.
///
/// При `enrich_array == false` тело пробрасывается дословно (для DELETE/
/// 204 No Content, где тело пустое).
fn proxy_response_to_axum(
    status: reqwest::StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Bytes,
    server_id: &str,
    enrich_array: bool,
) -> Response {
    // axum::StatusCode переиспользует http::StatusCode, как и reqwest:
    // .as_u16() сохраняет точное значение.
    let axum_status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    // Прокидываем headers, переводя их в axum::http::HeaderMap (по факту это
    // тоже http::HeaderMap из http crate, но reqwest и axum могут иметь разные
    // версии). Делаем через имя+байты значения — безопасно для любой версии.
    let mut axum_headers = AxumHeaderMap::new();
    for (k, v) in headers.iter() {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(k.as_str().as_bytes()),
            axum::http::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            axum_headers.insert(name, value);
        }
    }

    // Решаем, обогащать ли тело. Только если включён enrich_array И
    // content-type указывает на JSON.
    let is_json = axum_headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("application/json"))
        .unwrap_or(false);

    let final_body: Bytes = if enrich_array && is_json && !body.is_empty() {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(mut v) => {
                remote_proxy::enrich_with_origin(&mut v, server_id);
                match serde_json::to_vec(&v) {
                    Ok(bytes) => {
                        // Content-Length перепишется ниже только если он был в headers.
                        // Иначе hyper посчитает сам по body.
                        axum_headers.remove(axum::http::header::CONTENT_LENGTH);
                        Bytes::from(bytes)
                    }
                    Err(_) => body,
                }
            }
            Err(_) => body, // не JSON — отдаём как есть
        }
    } else {
        body
    };

    (axum_status, axum_headers, final_body).into_response()
}

/// Высокоуровневый helper для прокси-ветки в handler'ах.
///
/// Возвращает `Some(Result)` если прокси должен сработать (есть `?server=<id>`),
/// иначе `None` — handler продолжает локальную логику.
///
/// Логика:
/// - `?server=` отсутствует / пустой → `None` (продолжать локально).
/// - `?server=<id>` есть, но `state.remote_mode == false` → `Some(Err(400))`.
/// - Иначе → `Some(Ok(proxy_response))` или `Some(Err(error))`.
///
/// `path` — путь без query, например `/api/sessions`. Query прокидывается
/// автоматически (минус `server`).
///
/// `enrich_array` — нужно ли обогащать JSON-ответ полем `origin = <server_id>`.
/// Для GET-ресурсов это `true`, для DELETE/204 — `false`.
async fn try_proxy_to_remote(
    state: &AppState,
    raw_query: &HashMap<String, String>,
    method: reqwest::Method,
    path: &str,
    content_type: Option<&str>,
    body: Option<Bytes>,
    enrich_array: bool,
) -> Option<Result<Response, (StatusCode, String)>> {
    let server_id = match resolve_dispatch(raw_query, state.remote_mode) {
        DispatchDecision::Local => return None,
        DispatchDecision::LegacyRejection => {
            return Some(Err((
                StatusCode::BAD_REQUEST,
                "remote mode disabled — cannot use ?server=<id>".to_string(),
            )))
        }
        DispatchDecision::Proxy(id) => id,
    };

    let query = rebuild_query_without_server(raw_query);
    let store = state.remotes.read().await;
    let result = remote_proxy::proxy_request(
        &store,
        &state.http,
        &server_id,
        method,
        path,
        &query,
        content_type,
        body,
    )
    .await;

    match result {
        Ok((status, headers, bytes)) => Some(Ok(proxy_response_to_axum(
            status,
            headers,
            bytes,
            &server_id,
            enrich_array,
        ))),
        Err(e) => Some(Err(e.into_response())),
    }
}

// =============================================================================
// Sessions endpoints
// =============================================================================

/// JSON-форма tmux-сессии для эндпоинта `GET /api/sessions`.
///
/// Все поля исходной [`SessionInfo`] выносятся на верхний уровень JSON через
/// `#[serde(flatten)]`, что сохраняет полную backward-совместимость с
/// фронтендом. Дополнительно добавляется флаг `needs_attention` (default
/// `false`), обновляемый фоновым [`attention::watcher_loop`].
///
/// Семантика `needs_attention`:
/// - `true` — в панели сессии обнаружен Claude permission prompt
///   (см. [`attention::detect_claude_prompt`]); фронтенд должен подсветить
///   вкладку оранжевым;
/// - `false` (или отсутствие записи в snapshot'е) — нормальное состояние.
#[derive(Debug, Serialize)]
struct SessionDto {
    #[serde(flatten)]
    info: SessionInfo,
    needs_attention: bool,
    /// `true` если за прошедший тик watcher'а (1.5с) содержимое последних
    /// 50 строк pane изменилось (prev≠current по `gen_hash50`) — индикатор
    /// активной генерации Claude или любого другого процесса, рисующего в pane.
    ///
    /// Дополнительно применяется per-tick дедупликация
    /// ([`attention::deduplicate_generating`]): среди сессий с одинаковым
    /// `session_group`+`gen_hash50` `true` остаётся только у одной primary
    /// (правило выбора зеркалит [`attention::pick_primary`] для
    /// `needs_attention`). Это устраняет ложные подсветки на «зрителях»
    /// `attach`нутого pane (например, при `switch-client` / `resize`).
    ///
    /// Сигнал независим от `needs_attention`: оба флага могут гореть
    /// одновременно. См. [`attention::AttentionState::update_generation`] и
    /// [`attention::AttentionState::set_generating`].
    is_generating: bool,
    /// Сколько секунд длится ТЕКУЩАЯ непрерывная серия генерации (та, что
    /// сейчас зажигает `is_generating`). `None` когда сессия не генерирует.
    ///
    /// Источник — [`attention::AttentionState::generating_age_snapshot`]:
    /// отсчёт ведётся от фронта `false→true` финального флага. Поле чисто
    /// информационное — фронтенд использует его в tooltip синего индикатора
    /// работы («терминал обновляется уже N с»), на дедуп/детект не влияет.
    #[serde(skip_serializing_if = "Option::is_none")]
    generating_since_secs: Option<u64>,
    /// Идентификатор папочно-ориентированной группы для sidebar-группировки.
    /// Формат: `"__folder:<absolute_path>"`. Префикс `__folder:` —
    /// стабильный namespace для UI-группировки. `None` только для сессий
    /// с пустым или некорректным `path` (file_name отсутствует).
    /// Сериализуется всегда — фронт ожидает унифицированный формат.
    folder_id: Option<String>,
    /// Человекочитаемая метка папочной группы — basename последней папки
    /// `session.path`. Отображается в group-header sidebar. `None` зеркалит
    /// `folder_id == None` (orphan-ветка sidebar).
    folder_label: Option<String>,
    /// Phase 3 — источник записи. Для локально-сгенерированных сессий — всегда
    /// `"local"`. Прокси через `?server=<id>` НЕ создаёт SessionDto на этой
    /// стороне (там прокидывается уже готовый JSON remote'а, обогащённый
    /// `remote_proxy::enrich_with_origin`). Поле сериализуется ВСЕГДА,
    /// независимо от `remote_mode`, чтобы фронт получал унифицированный формат.
    origin: String,
}

/// `GET /api/sessions` — JSON-массив ВСЕХ активных tmux-сессий. Каждая
/// сессия отдаётся как [`SessionDto`] = `SessionInfo` + флаг
/// `needs_attention` из snapshot'а `state.attention` + folder-группировка.
///
/// Snapshot attention снимается один раз на запрос (под коротким
/// read-lock'ом) и не блокирует watcher.
///
/// После Phase 4 (`remove-projects-concept`) поля `project_id`/`project_name`
/// и фильтрация по `tmux_prefix` удалены — все сессии всегда возвращаются
/// одинаково. Группировка в UI — через `folder_id`/`folder_label`.
///
/// Если tmux-сервер не запущен — возвращает `[]` (а не 500).
async fn get_sessions(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::GET,
        "/api/sessions",
        None,
        None,
        true,
    )
    .await
    {
        return result;
    }

    match tmux::list_sessions().await {
        Ok(list) => {
            let attention = state.attention.snapshot().await;
            let generating = state.attention.generating_snapshot().await;
            let gen_ages = state.attention.generating_age_snapshot().await;
            let dtos: Vec<SessionDto> = list
                .into_iter()
                .map(|s| {
                    let needs_attention = attention.get(&s.name).copied().unwrap_or(false);
                    let is_generating = generating.get(&s.name).copied().unwrap_or(false);
                    let generating_since_secs = if is_generating {
                        gen_ages.get(&s.name).copied()
                    } else {
                        None
                    };
                    let (folder_id, folder_label) = resolve_folder(&s);
                    SessionDto {
                        needs_attention,
                        is_generating,
                        generating_since_secs,
                        folder_id,
                        folder_label,
                        info: s,
                        origin: "local".to_string(),
                    }
                })
                .collect();
            Ok(Json(dtos).into_response())
        }
        Err(e) => {
            tracing::error!(error = ?e, "list_sessions failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
        }
    }
}

/// Тело запроса `POST /api/sessions`.
#[derive(Debug, Deserialize)]
struct CreateSessionReq {
    name: String,
}

/// `POST /api/sessions` — создаёт detached-сессию в cwd «активного пути»
/// (`state.active_path_tx.borrow()` — после Phase 4 это инициализированный
/// cwd процесса либо последнее значение, проставленное хост-кодом).
///
/// После удаления проектов авто-префикс по `tmux_prefix` больше не
/// применяется: имя используется как ввёл пользователь (с trim пробелов
/// и базовой валидацией внутри `tmux::new_session`).
///
/// - 201 Created при успехе.
/// - 400 Bad Request при невалидном имени или duplicate.
async fn create_session(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        "/api/sessions",
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: CreateSessionReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;

    let name = req.name.trim().to_string();
    let cwd = state.active_path_tx.borrow().clone();

    match tmux::new_session(&name, &cwd).await {
        Ok(()) => {
            tracing::info!(name = %name, cwd = %cwd.display(), "tmux session created");
            Ok(StatusCode::CREATED.into_response())
        }
        Err(e) => {
            tracing::warn!(name = %name, error = ?e, "new_session failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `DELETE /api/sessions/:name` — убивает существующую сессию.
///
/// - 204 No Content при успехе.
/// - 400 Bad Request при невалидном имени или если сессии нет.
async fn delete_session(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let path = format!("/api/sessions/{}", urlencode_minimal(&name));
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::DELETE,
        &path,
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    match tmux::kill_session(&name).await {
        Ok(()) => {
            tracing::info!(%name, "tmux session killed");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => {
            tracing::warn!(%name, error = ?e, "kill_session failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// Тело запроса `PATCH /api/sessions/:name`.
#[derive(Debug, Deserialize)]
struct RenameSessionReq {
    name: String,
}

/// `PATCH /api/sessions/:name` — переименовывает существующую сессию.
/// Имя используется **ровно как ввёл пользователь** (с trim пробелов).
/// После Phase 4 (`remove-projects-concept`) tmux-prefix auto-применение
/// снято и в `POST /api/sessions` — концепция проектов удалена.
///
/// - 200 OK + `{ "name": "<new>" }` при успехе.
/// - 400 Bad Request при невалидном теле/имени, если сессии нет или новое имя занято.
async fn rename_session(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let path = format!("/api/sessions/{}", urlencode_minimal(&name));
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::PATCH,
        &path,
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: RenameSessionReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;

    let new_name = req.name.trim().to_string();

    match tmux::rename_session(&name, &new_name).await {
        Ok(()) => {
            tracing::info!(old = %name, new = %new_name, "tmux session renamed");
            Ok(Json(serde_json::json!({ "name": new_name })).into_response())
        }
        Err(e) => {
            tracing::warn!(%name, new = %new_name, error = ?e, "rename_session failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

// =============================================================================
// Windows endpoints (внутри сессии)
// =============================================================================

/// `GET /api/sessions/:name/windows` — JSON-массив всех окон сессии.
async fn list_windows(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let path = format!("/api/sessions/{}/windows", urlencode_minimal(&name));
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::GET,
        &path,
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    match tmux::list_windows(&name).await {
        Ok(wins) => Ok(Json(wins).into_response()),
        Err(e) => {
            tracing::warn!(%name, error = ?e, "list_windows failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// Тело запроса `POST /api/sessions/:name/windows` (опциональное имя нового окна).
#[derive(Debug, Deserialize, Default)]
struct CreateWindowReq {
    #[serde(default)]
    name: Option<String>,
}

/// `POST /api/sessions/:name/windows` — создаёт новое окно в сессии.
/// Body опционален: `{ "name": "..." }`.
async fn create_window(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let path = format!("/api/sessions/{}/windows", urlencode_minimal(&name));
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        &path,
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: CreateWindowReq = if body.is_empty() {
        CreateWindowReq::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?
    };

    let win_name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match tmux::new_window(&name, win_name).await {
        Ok(()) => {
            tracing::info!(session = %name, win = ?win_name, "tmux window created");
            Ok(StatusCode::CREATED.into_response())
        }
        Err(e) => {
            tracing::warn!(session = %name, error = ?e, "new_window failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `POST /api/sessions/:name/windows/:index/select` — делает окно активным.
async fn select_window(
    State(state): State<AppState>,
    AxumPath((name, index)): AxumPath<(String, u32)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let path = format!(
        "/api/sessions/{}/windows/{}/select",
        urlencode_minimal(&name),
        index
    );
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        &path,
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    match tmux::select_window(&name, index).await {
        Ok(()) => {
            tracing::info!(session = %name, %index, "tmux window selected");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => {
            tracing::warn!(session = %name, %index, error = ?e, "select_window failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `DELETE /api/sessions/:name/windows/:index` — убивает окно.
async fn delete_window(
    State(state): State<AppState>,
    AxumPath((name, index)): AxumPath<(String, u32)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let path = format!(
        "/api/sessions/{}/windows/{}",
        urlencode_minimal(&name),
        index
    );
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::DELETE,
        &path,
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    match tmux::kill_window(&name, index).await {
        Ok(()) => {
            tracing::info!(session = %name, %index, "tmux window killed");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => {
            tracing::warn!(session = %name, %index, error = ?e, "kill_window failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// Тело запроса `PATCH /api/sessions/:name/windows/:index` — переименование.
#[derive(Debug, Deserialize)]
struct RenameWindowReq {
    name: String,
}

/// `PATCH /api/sessions/:name/windows/:index` — переименовывает окно.
async fn patch_window(
    State(state): State<AppState>,
    AxumPath((name, index)): AxumPath<(String, u32)>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let path = format!(
        "/api/sessions/{}/windows/{}",
        urlencode_minimal(&name),
        index
    );
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::PATCH,
        &path,
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: RenameWindowReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;
    let new_name = req.name.trim().to_string();
    if new_name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name must not be empty".to_string()));
    }

    match tmux::rename_window(&name, index, &new_name).await {
        Ok(()) => {
            tracing::info!(session = %name, %index, new = %new_name, "tmux window renamed");
            Ok(Json(serde_json::json!({ "name": new_name })).into_response())
        }
        Err(e) => {
            tracing::warn!(session = %name, %index, error = ?e, "rename_window failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

// =============================================================================
// Tasks endpoint
// =============================================================================

/// `GET /api/tasks` — read-only snapshot задач из beads активного проекта.
///
/// cwd для `br list --json --all --limit 0` берётся из active project.
async fn get_tasks(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    // Phase 3: enrich_array=false, потому что remote отдаёт {issues:[...]},
    // т.е. Object, не Array. enrich_with_origin поставит origin на корень,
    // а нам нужно на каждый item внутри `issues`. Делаем это вручную.
    if let Some(server_id) = extract_server_id(&q) {
        if !state.remote_mode {
            return Err((
                StatusCode::BAD_REQUEST,
                "remote mode disabled — cannot use ?server=<id>".to_string(),
            ));
        }
        let query = rebuild_query_without_server(&q);
        let store = state.remotes.read().await;
        return match remote_proxy::proxy_request(
            &store,
            &state.http,
            &server_id,
            reqwest::Method::GET,
            "/api/tasks",
            &query,
            None,
            None,
        )
        .await
        {
            Ok((status, headers, body)) => {
                // Парсим JSON, обогащаем `issues` каждого item полем origin.
                let mut value: serde_json::Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(_) => {
                        return Ok(proxy_response_to_axum(
                            status, headers, body, &server_id, false,
                        ))
                    }
                };
                if let Some(arr) = value.get_mut("issues").and_then(|v| v.as_array_mut()) {
                    for item in arr.iter_mut() {
                        if let Some(obj) = item.as_object_mut() {
                            obj.insert(
                                "origin".to_string(),
                                serde_json::Value::String(server_id.clone()),
                            );
                        }
                    }
                }
                let body_out = serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec());
                let axum_status =
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                Ok((
                    axum_status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body_out,
                )
                    .into_response())
            }
            Err(e) => Err(e.into_response()),
        };
    }

    // Tasks следуют за cwd текущей сессии (как git-вкладка): если фронт
    // передал ?path=<abs>, берём его как cwd для list_tasks, иначе fallback
    // на текущее значение state.active_path_tx.
    let cwd = if let Some(p) = q
        .get("path")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        PathBuf::from(p)
    } else {
        state.active_path_tx.borrow().clone()
    };
    match tasks::list_tasks(&cwd).await {
        Ok(mut value) => {
            // Phase 3 — каждый issue в response.issues получает origin="local"
            // для унификации формата с прокси-ответами `?server=<id>` (где
            // remote_proxy::enrich_with_origin ставит remote id).
            if let Some(arr) = value.get_mut("issues").and_then(|v| v.as_array_mut()) {
                for item in arr.iter_mut() {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert(
                            "origin".to_string(),
                            serde_json::Value::String("local".to_string()),
                        );
                    }
                }
            }
            Ok(Json(value).into_response())
        }
        Err(e) => {
            tracing::error!(error = ?e, "list_tasks failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
        }
    }
}

/// Тело запроса `POST /api/tasks` — создание новой задачи.
///
/// Все поля кроме `title` опциональны. Маппинг на CLI `br create --json`:
/// - `title` → `--title`;
/// - `description` → `-d`;
/// - `type` (с переименованием из `type` через `serde(rename)`) → `-t`;
/// - `priority` (0..=4) → `-p`;
/// - `labels` (csv) → `-l`;
/// - `parent` → `--parent`.
#[derive(Debug, Deserialize)]
struct CreateTaskReq {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "type")]
    issue_type: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    labels: Option<String>,
    #[serde(default)]
    parent: Option<String>,
}

/// `POST /api/tasks` — создаёт issue в beads активного проекта через
/// `br create --json`.
///
/// Возвращает 201 Created + распарсенный JSON созданного issue (его кладёт
/// `br create --json` одиночным объектом).
async fn create_task(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        "/api/tasks",
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: CreateTaskReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;

    let title = req.title.trim();
    if title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "title is required".to_string()));
    }

    let cwd = state.active_path_tx.borrow().clone();

    // Собираем аргументы динамически — у `br create` все флаги «ключ значение»,
    // удобно держать всё в `Vec<String>` и в конце передать как `Vec<&str>`.
    let mut args: Vec<String> = vec![
        "create".to_string(),
        "--json".to_string(),
        "--title".to_string(),
        title.to_string(),
    ];
    if let Some(t) = req.issue_type.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        args.push("-t".to_string());
        args.push(t.to_string());
    }
    if let Some(p) = req.priority {
        if p > 4 {
            return Err((StatusCode::BAD_REQUEST, "priority must be 0..=4".to_string()));
        }
        args.push("-p".to_string());
        args.push(p.to_string());
    }
    if let Some(d) = req.description.as_deref().filter(|s| !s.is_empty()) {
        args.push("-d".to_string());
        args.push(d.to_string());
    }
    if let Some(l) = req.labels.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        args.push("-l".to_string());
        args.push(l.to_string());
    }
    if let Some(p) = req.parent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        args.push("--parent".to_string());
        args.push(p.to_string());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match tasks::run_br(&arg_refs, &cwd).await {
        Ok(value) => {
            tracing::info!(title = %title, "task created");
            Ok((StatusCode::CREATED, Json(value)).into_response())
        }
        Err(e) => {
            tracing::warn!(error = ?e, "br create failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// Тело запроса `PATCH /api/tasks/:id`. Все поля опциональны.
///
/// - `status`: `open|in_progress|blocked|deferred|draft|closed` (для `closed`
///   обычно используется DELETE — но прокидываем `--status` если запросили).
/// - `title`/`description`/`priority` — прямые `--title`/`--description`/`-p`.
/// - `labels`: csv-строка → `--set-labels` (replace-семантика, не add).
#[derive(Debug, Deserialize)]
struct PatchTaskReq {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    labels: Option<String>,
}

/// `PATCH /api/tasks/:id` — обновляет поля issue через `br update --json`.
///
/// `br update --json` возвращает массив обновлённых issues — отдаём его как есть.
async fn patch_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::PATCH,
        &format!("/api/tasks/{}", urlencode_minimal(&id)),
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: PatchTaskReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;

    let cwd = state.active_path_tx.borrow().clone();

    let mut args: Vec<String> = vec!["update".to_string(), "--json".to_string(), id.clone()];
    let mut any = false;
    if let Some(s) = req.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        args.push("-s".to_string());
        args.push(s.to_string());
        any = true;
    }
    if let Some(t) = req.title.as_deref().filter(|s| !s.is_empty()) {
        args.push("--title".to_string());
        args.push(t.to_string());
        any = true;
    }
    if let Some(p) = req.priority {
        if p > 4 {
            return Err((StatusCode::BAD_REQUEST, "priority must be 0..=4".to_string()));
        }
        args.push("-p".to_string());
        args.push(p.to_string());
        any = true;
    }
    if let Some(d) = req.description.as_deref() {
        // Пустая строка — допустимо (стирает description через --description "").
        args.push("--description".to_string());
        args.push(d.to_string());
        any = true;
    }
    if let Some(l) = req.labels.as_deref() {
        args.push("--set-labels".to_string());
        args.push(l.to_string());
        any = true;
    }

    if !any {
        return Err((StatusCode::BAD_REQUEST, "no updatable fields provided".to_string()));
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match tasks::run_br(&arg_refs, &cwd).await {
        Ok(value) => {
            tracing::info!(%id, "task updated");
            Ok(Json(value).into_response())
        }
        Err(e) => {
            tracing::warn!(%id, error = ?e, "br update failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `DELETE /api/tasks/:id?reason=...` — закрывает issue через `br close --json -r`.
///
/// 204 No Content при успехе; ошибка `br` маппится в 400.
async fn close_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::DELETE,
        &format!("/api/tasks/{}", urlencode_minimal(&id)),
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    let cwd = state.active_path_tx.borrow().clone();

    let reason = q.get("reason").map(|s| s.as_str()).unwrap_or("");
    let mut args: Vec<String> = vec!["close".to_string(), "--json".to_string(), id.clone()];
    if !reason.is_empty() {
        args.push("-r".to_string());
        args.push(reason.to_string());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match tasks::run_br(&arg_refs, &cwd).await {
        Ok(_) => {
            tracing::info!(%id, "task closed");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("ISSUE_NOT_FOUND") || msg.contains("Issue not found") {
                tracing::info!(%id, "task already absent — treating close as idempotent success");
                return Ok(StatusCode::NO_CONTENT.into_response());
            }
            tracing::warn!(%id, error = ?e, "br close failed");
            Err((StatusCode::BAD_REQUEST, msg))
        }
    }
}

/// `POST /api/tasks/:id/reopen` — переводит закрытый issue обратно в `open`
/// через `br reopen --json`. Возвращает 200 + объект `{reopened: [...]}`.
async fn reopen_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        &format!("/api/tasks/{}/reopen", urlencode_minimal(&id)),
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    let cwd = state.active_path_tx.borrow().clone();
    let args = ["reopen", "--json", id.as_str()];
    match tasks::run_br(&args, &cwd).await {
        Ok(value) => {
            tracing::info!(%id, "task reopened");
            Ok(Json(value).into_response())
        }
        Err(e) => {
            tracing::warn!(%id, error = ?e, "br reopen failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `POST /api/tasks/:id/purge` — физически удаляет issue через
/// `br delete --hard --force --json` (используется bulk-clean кнопкой
/// фронта для колонки Closed). 204 No Content при успехе; ошибка `br`
/// маппится в 400.
async fn purge_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        &format!("/api/tasks/{}/purge", urlencode_minimal(&id)),
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    let cwd = state.active_path_tx.borrow().clone();
    let args = [
        "delete",
        "--hard",
        "--force",
        "--json",
        "--reason",
        "clean-column",
        id.as_str(),
    ];
    match tasks::run_br(&args, &cwd).await {
        Ok(_) => {
            tracing::info!(%id, "task purged");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("ISSUE_NOT_FOUND") || msg.contains("Issue not found") {
                tracing::info!(%id, "task already absent — treating purge as idempotent success");
                return Ok(StatusCode::NO_CONTENT.into_response());
            }
            tracing::warn!(%id, error = ?e, "br delete failed");
            Err((StatusCode::BAD_REQUEST, msg))
        }
    }
}

// =============================================================================
// Folder-resolution helper (Phase 4 — единственный остаток от удалённого
// project-блока, нужен для группировки сессий в sidebar).
// =============================================================================

/// Резолвит папочно-ориентированную группу для сессии.
///
/// Возвращает кортеж `(folder_id, folder_label)`:
/// - `folder_id` — стабильный ключ группы вида `"__folder:<absolute_path>"`.
///   Префикс `__folder:` — стабильный namespace для UI-группировки.
/// - `folder_label` — basename последней папки `session.path` для отображения
///   в заголовке группы sidebar.
///
/// Если `session.path` пустой или равен `/` (нет `file_name`), либо basename
/// пустая строка — оба значения `None` (orphan-ветка sidebar отрисует через
/// `ORPHAN_KEY`).
///
/// Это чисто файловая группировка для UI — единственный способ
/// группировать сессии после удаления проектов (Phase 4 remove-projects-concept).
fn resolve_folder(s: &tmux::SessionInfo) -> (Option<String>, Option<String>) {
    let p = std::path::Path::new(&s.path);
    match p.file_name().and_then(|os| os.to_str()) {
        Some(name) if !name.is_empty() => (
            Some(format!("__folder:{}", s.path)),
            Some(name.to_string()),
        ),
        _ => (None, None),
    }
}
// =============================================================================
// TODOs endpoints (Phase 3)
// =============================================================================

/// `GET /api/todos?path=<cwd>` — JSON-массив TODO-карточек корня.
///
/// Query-параметр `path` обязателен. На бэкенде значение прогоняется через
/// [`paths::resolve_root`] (`.beads/` → `.git/` → сам cwd), результат
/// используется как ключ в `TodoStore`. Возвращает пустой массив (не 404),
/// если для этого корня нет TODO.
///
/// Если `path` отсутствует или пустой — 400 Bad Request.
async fn get_todos(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::GET,
        "/api/todos",
        None,
        None,
        true,
    )
    .await
    {
        return result;
    }

    let cwd = match q.get("path").map(String::as_str) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "missing required query parameter `path`".to_string(),
            ));
        }
    };
    let root = paths::resolve_root(std::path::Path::new(&cwd));
    let root_key = root.to_string_lossy().to_string();
    Ok(Json(state.todos.list(&root_key)).into_response())
}

/// Тело запроса `POST /api/todos`.
///
/// `path` — обязателен, представляет cwd сессии-инициатора. Сервер делает
/// `paths::resolve_root(&path)` и привязывает TODO к получившемуся корню.
#[derive(Debug, Deserialize)]
struct CreateTodoReq {
    path: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    /// План-мод: если true → при promote к notify-тексту добавится
    /// суффикс PLAN_MODE_SUFFIX. Default false.
    #[serde(default)]
    plan_mode: bool,
}

/// `POST /api/todos` — создаёт новую TODO-карточку.
///
/// `path` в body обязателен — сервер резолвит из него корень через
/// [`paths::resolve_root`] и сохраняет TODO в этот корень.
/// Валидация: `path` и `title` после trim не должны быть пустыми → 400.
/// После создания → broadcast `TodoEvent::Upsert` всем подписчикам WS.
async fn create_todo(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        "/api/todos",
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: CreateTodoReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;
    let title = req.title.trim();
    if title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "title is required".to_string()));
    }
    let cwd = req.path.trim();
    if cwd.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path is required".to_string()));
    }
    let root = paths::resolve_root(std::path::Path::new(cwd));
    let root_key = root.to_string_lossy().to_string();
    match state.todos.create(&root_key, title, req.description.clone(), req.plan_mode) {
        Ok(t) => {
            // Broadcast — игнорируем Err (нет подписчиков, ОК).
            let _ = state.todos_tx.send(ws_todos::upsert(t.clone()));
            tracing::info!(id = %t.id, root = %root_key, "todo created");
            Ok((StatusCode::CREATED, Json(t)).into_response())
        }
        Err(e) => {
            tracing::warn!(error = ?e, "todo create failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
        }
    }
}

/// Тело запроса `PATCH /api/todos/:id`.
///
/// Семантика description (как в `TodoStore::update`):
/// - отсутствие поля → не трогать;
/// - `description: null` → стирает описание;
/// - `description: "..."` → записать строку.
///
/// Поле `path` опционально: если задано — TODO переезжает в корень,
/// получающийся из `paths::resolve_root(path)`. Это move между корнями.
#[derive(Debug, Deserialize)]
struct PatchTodoReq {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    description: Option<Option<String>>,
    /// `None` (поля нет) → не трогать. `Some(true|false)` → перезаписать.
    #[serde(default)]
    plan_mode: Option<bool>,
    /// Опциональный move TODO в другой корень. Резолвится через
    /// `paths::resolve_root`. Если пустая строка после trim — игнорируется.
    #[serde(default)]
    path: Option<String>,
}

/// Custom deserializer: различает «отсутствие поля» и «null».
///
/// Без него serde трактует и `{}` и `{description: null}` одинаково
/// (обе → None после `Option<Option<...>>`). Здесь же:
/// - поля нет → внешний Option = None (не трогать);
/// - `null` → Some(None) (стереть);
/// - строка → Some(Some(str)) (записать).
fn deserialize_optional_optional_string<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<String> = Option::deserialize(deserializer)?;
    Ok(Some(v))
}

/// `PATCH /api/todos/:id` — обновляет title/description.
///
/// 404 если id не найден. После успешного апдейта → broadcast Upsert.
async fn patch_todo(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::PATCH,
        &format!("/api/todos/{}", urlencode_minimal(&id)),
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    let req: PatchTodoReq = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;
    if let Some(t) = req.title.as_deref() {
        if t.trim().is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "title must not be empty".to_string(),
            ));
        }
    }
    // 1) Поля title/description/plan_mode — обычный update.
    let updated = match state
        .todos
        .update(&id, req.title.clone(), req.description.clone(), req.plan_mode)
    {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no todo with id `{id}`"),
            ));
        }
        Err(e) => {
            tracing::warn!(%id, error = ?e, "todo update failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")));
        }
    };
    // 2) Если в body есть path — выполнить move в новый корень.
    let final_todo = if let Some(raw_path) = req.path.as_deref() {
        let p = raw_path.trim();
        if p.is_empty() {
            updated
        } else {
            let new_root = paths::resolve_root(std::path::Path::new(p));
            let new_root_key = new_root.to_string_lossy().to_string();
            if new_root_key == updated.root_path {
                updated
            } else {
                let old_root = updated.root_path.clone();
                match state.todos.move_to_root(&id, &new_root_key) {
                    Ok(Some(t)) => {
                        // Уведомить старый корень об удалении и новый корень о появлении.
                        let _ = state
                            .todos_tx
                            .send(ws_todos::removed(old_root, id.clone()));
                        t
                    }
                    Ok(None) => {
                        // Гонка с delete — маловероятно. Логируем и возвращаем
                        // updated (он же бывший).
                        tracing::warn!(%id, "todo move_to_root: id disappeared between update and move");
                        updated
                    }
                    Err(e) => {
                        tracing::warn!(%id, error = ?e, "todo move_to_root failed");
                        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")));
                    }
                }
            }
        }
    } else {
        updated
    };
    let _ = state.todos_tx.send(ws_todos::upsert(final_todo.clone()));
    tracing::info!(%id, "todo updated");
    Ok(Json(final_todo).into_response())
}

/// `DELETE /api/todos/:id` — удаляет TODO.
///
/// 204 при успехе; 404 если id не найден. После удаления → broadcast Removed
/// с `project_id` исходной карточки (резолвится через `todos.get` ДО удаления).
async fn delete_todo(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::DELETE,
        &format!("/api/todos/{}", urlencode_minimal(&id)),
        None,
        None,
        false,
    )
    .await
    {
        return result;
    }

    let root_path = match state.todos.get(&id) {
        Some(t) => t.root_path,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no todo with id `{id}`"),
            ));
        }
    };
    match state.todos.delete(&id) {
        Ok(true) => {
            let _ = state.todos_tx.send(ws_todos::removed(root_path, id.clone()));
            tracing::info!(%id, "todo deleted");
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            format!("no todo with id `{id}`"),
        )),
        Err(e) => {
            tracing::warn!(%id, error = ?e, "todo delete failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
        }
    }
}

/// Дефолтный шаблон уведомления (fallback для UI-настроек на случай, если
/// фронт хочет показать «дефолтный текст»). В `promote_todo` Phase 3 НЕ
/// используется: если `NotifierConfig.template` пуст — notify-job просто
/// не планируется (см. P3.3).
///
/// Плейсхолдеры: `{id}`, `{title}`, `{description}`, `{priority}`, `{type}`.
#[allow(dead_code)]
const DEFAULT_NOTIFY_TEMPLATE: &str =
    "Новая задача [{id}]: {title} — нужно сделать";

/// Суффикс, добавляемый к notify-тексту, если у TODO `plan_mode == true`.
/// Прикрепляется через `\n` к отрендеренному template'у в `promote_todo`.
/// Точный текст согласован с UI: чекбокс "Включить план мод" в модалке TODO.
const PLAN_MODE_SUFFIX: &str = "Создай план для этой задачи";

/// Дефолтный notify-шаблон для promote, когда `NotifierConfig.template` пуст.
/// Сохраняет инвариант «zero-config = рабочий перенос»: в активную сессию
/// уходит только ID и заголовок задачи в формате `[{id}] {title}`. Полное
/// описание в текст НЕ включается — агент сам подтягивает его через
/// `br show <id>`. Плейсхолдеры — как в [`format_notify_template`].
const DEFAULT_PROMOTE_TEMPLATE: &str = "[{id}] {title}";

/// Подставляет значения TODO/issue в template-строку.
///
/// Поддерживаемые плейсхолдеры:
/// - `{id}` — id (для bd-issue после `br create` берём из вернувшегося JSON).
/// - `{title}`, `{description}` — поля карточки.
/// - `{priority}` — числовое значение.
/// - `{type}` — issue_type.
///
/// Неизвестные плейсхолдеры остаются как есть (UI-логика сама решает,
/// показывать ли их пользователю как ошибку).
fn format_notify_template(
    template: &str,
    id: &str,
    title: &str,
    description: &str,
    priority: u8,
    issue_type: &str,
) -> String {
    template
        .replace("{id}", id)
        .replace("{title}", title)
        .replace("{description}", description)
        .replace("{priority}", &priority.to_string())
        .replace("{type}", issue_type)
}

/// Тело запроса `POST /api/todos/:id/promote`.
///
/// Phase 3: `session` опционален. Если задан — переопределяет
/// `NotifierConfig.session`. Если не задан, и в global config session=None —
/// notify не отправляется (но bd-задача всё равно создаётся).
#[derive(Debug, Deserialize)]
struct PromoteTodoReq {
    #[serde(default)]
    session: Option<String>,
}

/// `POST /api/todos/:id/promote` — конвертирует TODO в bd-задачу и (если
/// заданы template + session) планирует notify-job в очередь.
///
/// Алгоритм (Phase 3 — global NotifierConfig, без Project):
/// 1. Найти TODO (`todos.get`) → 404 если нет.
/// 2. Создать bd-issue: `br create --json --title <todo.title> --description
///    <todo.description> -t <todo.issue_type> -p <todo.priority>` в
///    `todo.root_path`. Получаем JSON → извлекаем `.id` (string).
/// 3. Удалить TODO + broadcast Removed (с root_path).
/// 4. Прочитать `state.notifier_config.get()`:
///    - target session = `body.session` (если задан) else `cfg.session`.
///    - template из `cfg.template`; если пуст — `DEFAULT_PROMOTE_TEMPLATE`
///      (`[{id}] {title}` — описание агент берёт сам через `br show <id>`).
///    - mode из `cfg.delay_minutes` + `cfg.wait_previous`:
///        - wait_previous=true ⇒ NotifyMode::WaitPrevious
///        - delay_minutes>0  ⇒ NotifyMode::Delayed
///        - иначе            ⇒ NotifyMode::Immediate
/// 5. Если target session пуст ⇒ skip notify (200 OK, `notify_scheduled =
///    false`). Пустой `template` НЕ блокирует отправку — см. п.4 (fallback).
/// 6. Иначе — `notifier.enqueue(job).await`.
/// 7. Ответ 200 `{ promoted: true, task_id: "<bd-id>", notify_scheduled: bool }`.
async fn promote_todo(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::POST,
        &format!("/api/todos/{}/promote", urlencode_minimal(&id)),
        Some("application/json"),
        Some(body.clone()),
        false,
    )
    .await
    {
        return result;
    }

    // Body может быть пустым (фронт иногда шлёт `{}`) — обрабатываем оба случая.
    let req: PromoteTodoReq = if body.is_empty() {
        PromoteTodoReq { session: None }
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?
    };

    // 1) Загружаем TODO.
    let todo = match state.todos.get(&id) {
        Some(t) => t,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no todo with id `{id}`"),
            ));
        }
    };

    // 2) Phase 3: глобальный NotifierConfig — источник template/delay/
    //    wait_previous/session. body.session (если задан) переопределяет
    //    cfg.session. Если оба пустые — notify не планируется (bd-задача
    //    всё равно создаётся: пункт 3-4 ниже).
    let cfg = state.notifier_config.get();
    let target_session: Option<String> = req
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            cfg.session
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });

    // 3) Создаём bd-задачу. Рабочая директория для `br` = root_path TODO.
    let priority_str = todo.priority.to_string();
    let mut br_args: Vec<String> = vec![
        "create".to_string(),
        "--json".to_string(),
        "--title".to_string(),
        todo.title.clone(),
    ];
    if let Some(desc) = todo.description.as_deref().filter(|s| !s.is_empty()) {
        br_args.push("-d".to_string());
        br_args.push(desc.to_string());
    }
    if !todo.issue_type.is_empty() {
        br_args.push("-t".to_string());
        br_args.push(todo.issue_type.clone());
    }
    br_args.push("-p".to_string());
    br_args.push(priority_str);

    let arg_refs: Vec<&str> = br_args.iter().map(String::as_str).collect();
    let root_path_buf = std::path::PathBuf::from(&todo.root_path);
    let created = match tasks::run_br(&arg_refs, &root_path_buf).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, "br create failed during promote");
            return Err((StatusCode::BAD_REQUEST, format!("br create: {e:#}")));
        }
    };
    let task_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            // Некоторые версии br могут возвращать `{"created":[{...}]}` или массив.
            created
                .get("created")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|o| o.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if task_id.is_empty() {
        tracing::warn!(?created, "br create returned no id field");
    }

    // 4) Удаляем TODO + broadcast.
    if let Err(e) = state.todos.delete(&id) {
        tracing::warn!(%id, error = ?e, "todo delete after promote failed (bd task already created)");
    } else {
        let _ = state
            .todos_tx
            .send(ws_todos::removed(todo.root_path.clone(), id.clone()));
    }

    // 5) Если target_session не определён — notify не планируется (некуда
    //    слать). bd-задача уже создана и TODO удалён — возвращаем 200 с
    //    `notify_scheduled: false`. Пустой `template` НЕ блокирует отправку:
    //    в этом случае используется DEFAULT_PROMOTE_TEMPLATE (см. п.6).
    if target_session.is_none() {
        tracing::info!(
            todo_id = %id,
            task_id = %task_id,
            root = %todo.root_path,
            session_missing = true,
            "todo promoted (notify skipped)"
        );
        return Ok(Json(serde_json::json!({
            "promoted": true,
            "task_id": task_id,
            "notify_scheduled": false,
        }))
        .into_response());
    }
    let target_session = target_session.expect("checked is_none above");

    // 6) Формируем текст. Если cfg.template пуст — используем дефолтный
    //    fallback `[{id}] {title}` (без описания: агент подтянет его сам
    //    через `br show <id>`). Затем — plan-mode suffix (если применимо).
    //    `description` всё ещё нужен для плейсхолдера `{description}` в
    //    кастомных шаблонах.
    let description = todo.description.clone().unwrap_or_default();
    let template_empty = cfg.template.trim().is_empty();
    let template_str: &str = if template_empty {
        DEFAULT_PROMOTE_TEMPLATE
    } else {
        cfg.template.as_str()
    };
    let mut text = format_notify_template(
        template_str,
        &task_id,
        &todo.title,
        &description,
        todo.priority,
        &todo.issue_type,
    );
    if todo.plan_mode {
        // Кастомный suffix из user-level настроек (~/.forge/user_settings.json).
        // Если в настройках пусто (после trim — строки из одних пробелов тоже
        // считаются «пустыми»), используем константу PLAN_MODE_SUFFIX —
        // это сохраняет инвариант «нулевая конфигурация = поведение до фичи».
        let custom_suffix = state.user_settings.get().todo_plan_mode_suffix;
        let suffix_str: &str = if custom_suffix.trim().is_empty() {
            PLAN_MODE_SUFFIX
        } else {
            custom_suffix.as_str()
        };
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(suffix_str);
    }

    // 7) Резолвим NotifyMode по cfg.wait_previous / cfg.delay_minutes.
    //    Приоритет: wait_previous > delayed > immediate. previous_task_id =
    //    None — notifier сам подберёт last_promoted_open_id по root_path.
    let mode = if cfg.wait_previous {
        notifier::NotifyMode::WaitPrevious {
            previous_task_id: None,
        }
    } else if cfg.delay_minutes > 0 {
        let fire_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
            + (cfg.delay_minutes as u64) * 60_000;
        notifier::NotifyMode::Delayed {
            fire_at_unix_ms: fire_at,
        }
    } else {
        notifier::NotifyMode::Immediate
    };

    // 8) NotifyJob.root_path = todo.root_path (cwd-only ключ для wait_queues).
    let job = notifier::new_job(
        todo.root_path.clone(),
        task_id.clone(),
        target_session,
        text,
        mode,
    );

    if let Err(e) = state.notify.enqueue(job).await {
        tracing::error!(error = ?e, "notifier enqueue failed");
        // Тут уже задача создана и TODO удалено — не катастрофа,
        // вернём 200 + warning (notification не доставится).
        return Ok(Json(serde_json::json!({
            "promoted": true,
            "task_id": task_id,
            "notify_scheduled": false,
            "notify_warning": format!("{e:#}"),
        }))
        .into_response());
    }

    tracing::info!(
        todo_id = %id,
        task_id = %task_id,
        root = %todo.root_path,
        "todo promoted"
    );
    Ok(Json(serde_json::json!({
        "promoted": true,
        "task_id": task_id,
        "notify_scheduled": true,
    }))
    .into_response())
}

// =============================================================================
// Themes endpoints (Phase wk7)
// =============================================================================

/// Ответ `GET /api/themes` — встроенные пресеты + пользовательские темы +
/// id активной темы. Фронтенд использует это для рендера вкладки Themes
/// в settings modal.
#[derive(Debug, Serialize)]
struct ThemesListResp {
    presets: Vec<Theme>,
    custom: Vec<Theme>,
    active: String,
}

/// `GET /api/themes` — список пресетов + custom + id активной.
///
/// Пресеты возвращаются всегда из [`themes::built_in_presets`]; custom — из
/// in-memory state (то, что было загружено из `themes.json`).
async fn get_themes(State(state): State<AppState>) -> Json<ThemesListResp> {
    let s = state.themes.read().await;
    Json(ThemesListResp {
        presets: themes::built_in_presets(),
        custom: s.custom.clone(),
        active: s.active.clone(),
    })
}

/// `GET /api/themes/active` — полный объект активной темы.
///
/// Поиск активной идёт сначала по пресетам, затем по custom. Если id указывает
/// в пустоту (повреждённый state) — fallback на пресет `default` + warn в лог.
/// Endpoint всегда отвечает 200 (нет варианта «темы нет вообще»: пресет
/// `default` гарантированно существует, так как зашит в бинарь).
async fn get_active_theme(State(state): State<AppState>) -> Json<Theme> {
    let active_id = state.themes.read().await.active.clone();

    if let Some(t) = themes::find_preset(&active_id) {
        return Json(t);
    }
    {
        let s = state.themes.read().await;
        if let Some(t) = s.custom.iter().find(|t| t.id == active_id) {
            return Json(t.clone());
        }
    }

    tracing::warn!(active = %active_id, "active theme id not found; falling back to default");
    Json(
        themes::find_preset("default")
            .expect("default preset must exist in built_in_presets()"),
    )
}

/// Тело запроса `PATCH /api/themes/active`.
#[derive(Debug, Deserialize)]
struct PatchActiveThemeReq {
    id: String,
}

/// `PATCH /api/themes/active` — переключить активную тему.
///
/// Допустимые id:
/// - id любого пресета из [`themes::built_in_presets`];
/// - id любой темы из `state.themes.custom`.
///
/// 404 если id не найден; 200 с `{ active: id }` при успехе. После записи
/// в memory — атомарный `themes::save` на диск; при ошибке записи — 500.
async fn patch_active_theme(
    State(state): State<AppState>,
    Json(req): Json<PatchActiveThemeReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let id = req.id.trim().to_string();
    if id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "id is required".to_string()));
    }

    let preset_exists = themes::find_preset(&id).is_some();
    let mut s = state.themes.write().await;
    let custom_exists = s.custom.iter().any(|t| t.id == id);
    if !preset_exists && !custom_exists {
        return Err((StatusCode::NOT_FOUND, format!("theme `{id}` not found")));
    }
    s.active = id.clone();
    if let Err(e) = themes::save(&state.themes_dir, &s) {
        tracing::error!(error = ?e, "themes save failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save: {e}"),
        ));
    }
    tracing::info!(active = %id, "active theme switched");
    Ok(Json(serde_json::json!({ "active": id })))
}

/// `POST /api/themes/custom` — создать пользовательскую тему.
///
/// Тело: полный объект [`Theme`]. Семантика поля `id`:
/// - пустая строка → генерируется `uuid::Uuid::new_v4()`;
/// - указан и совпадает с id пресета → 409 (нельзя override пресет);
/// - указан и уже занят в custom → 409.
///
/// При успехе → 201 + полный объект сохранённой темы. Запись на диск через
/// `themes::save` атомарная.
async fn create_custom_theme(
    State(state): State<AppState>,
    Json(mut theme): Json<Theme>,
) -> Result<(StatusCode, Json<Theme>), (StatusCode, String)> {
    let trimmed_id = theme.id.trim().to_string();
    let final_id = if trimmed_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        trimmed_id
    };

    if themes::find_preset(&final_id).is_some() {
        return Err((
            StatusCode::CONFLICT,
            format!("cannot override preset id `{final_id}`"),
        ));
    }
    let mut s = state.themes.write().await;
    if s.custom.iter().any(|t| t.id == final_id) {
        return Err((
            StatusCode::CONFLICT,
            format!("custom theme `{final_id}` already exists"),
        ));
    }

    theme.id = final_id;
    s.custom.push(theme.clone());
    if let Err(e) = themes::save(&state.themes_dir, &s) {
        tracing::error!(error = ?e, "themes save failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save: {e}"),
        ));
    }
    tracing::info!(id = %theme.id, "custom theme created");
    Ok((StatusCode::CREATED, Json(theme)))
}

/// `PUT /api/themes/custom/:id` — заменить пользовательскую тему.
///
/// `id` в URL — каноничный. Поле `id` в теле игнорируется (перезаписывается
/// path-параметром), что позволяет фронтенду не дублировать его.
///
/// 404 если темы с таким `id` нет; 200 + обновлённый объект при успехе.
async fn put_custom_theme(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(mut theme): Json<Theme>,
) -> Result<Json<Theme>, (StatusCode, String)> {
    if themes::find_preset(&id).is_some() {
        return Err((
            StatusCode::CONFLICT,
            format!("cannot override preset id `{id}`"),
        ));
    }
    theme.id = id.clone();
    let mut s = state.themes.write().await;
    let slot = match s.custom.iter_mut().find(|t| t.id == id) {
        Some(t) => t,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("custom theme `{id}` not found"),
            ));
        }
    };
    *slot = theme.clone();
    if let Err(e) = themes::save(&state.themes_dir, &s) {
        tracing::error!(error = ?e, "themes save failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save: {e}"),
        ));
    }
    tracing::info!(%id, "custom theme updated");
    Ok(Json(theme))
}

/// `DELETE /api/themes/custom/:id` — удалить пользовательскую тему.
///
/// - 409 если тема активна (`state.active == id`).
/// - 404 если темы нет.
/// - 204 No Content при успехе.
async fn delete_custom_theme(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut s = state.themes.write().await;
    if s.active == id {
        return Err((
            StatusCode::CONFLICT,
            format!("cannot delete active theme `{id}`"),
        ));
    }
    let before = s.custom.len();
    s.custom.retain(|t| t.id != id);
    if s.custom.len() == before {
        return Err((
            StatusCode::NOT_FOUND,
            format!("custom theme `{id}` not found"),
        ));
    }
    if let Err(e) = themes::save(&state.themes_dir, &s) {
        tracing::error!(error = ?e, "themes save failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save: {e}"),
        ));
    }
    tracing::info!(%id, "custom theme deleted");
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// User-settings endpoints (~/.forge/user_settings.json)
// =============================================================================

/// `GET /api/user-settings` — текущие настройки пользователя.
///
/// Если файл `~/.forge/user_settings.json` отсутствует или повреждён,
/// возвращаются дефолтные значения [`UserSettings::default`] (см.
/// инвариант «нулевая конфигурация = поведение до фичи»).
async fn get_user_settings(State(state): State<AppState>) -> Json<UserSettings> {
    Json(state.user_settings.get())
}

/// `PATCH /api/user-settings` — частичное обновление настроек.
///
/// Тело: [`PatchUserSettingsReq`] — все поля опциональные, применяются
/// только `Some(..)`-варианты. Валидация:
/// - `todo_default_priority` клампится в `0..=4` (значения >4 → 4);
/// - `todo_plan_mode_suffix` принимается **без trim** (даже строка
///   из одних пробелов сохраняется как есть; решение об «эффективной
///   пустоте» принимается на стороне `promote_todo`).
///
/// При ошибке записи на диск — 500 c сообщением. При успехе — 200 +
/// JSON c обновлённым `UserSettings`.
async fn patch_user_settings(
    State(state): State<AppState>,
    Json(payload): Json<PatchUserSettingsReq>,
) -> Result<Json<UserSettings>, (StatusCode, String)> {
    match state.user_settings.patch(payload) {
        Ok(updated) => {
            tracing::info!("user_settings patched");
            Ok(Json(updated))
        }
        Err(e) => {
            tracing::error!(error = ?e, "user_settings patch failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("user_settings patch failed: {e:#}"),
            ))
        }
    }
}

// =============================================================================
// Notifier-config endpoints (~/.config/forge/notifier.json) — Phase 3
// =============================================================================

/// `GET /api/notifier-config` — текущий глобальный конфиг notifier'а.
///
/// Если файл `~/.config/forge/notifier.json` отсутствует или повреждён,
/// возвращаются дефолты [`NotifierConfig::default`] (template="",
/// delay_minutes=0, wait_previous=false, session=None) — это нормальное
/// «zero-config» состояние: notify не отправляется до явного PATCH'а.
async fn get_notifier_config(State(state): State<AppState>) -> Json<NotifierConfig> {
    Json(state.notifier_config.get())
}

/// `PUT /api/notifier-config` — полная замена конфига.
///
/// Тело: целиком [`NotifierConfig`]. Все поля обязательны. После записи
/// возвращает финальный снимок. При ошибке записи на диск — 500.
async fn put_notifier_config(
    State(state): State<AppState>,
    Json(payload): Json<NotifierConfig>,
) -> Result<Json<NotifierConfig>, (StatusCode, String)> {
    match state.notifier_config.put(payload) {
        Ok(updated) => {
            tracing::info!(
                template_len = updated.template.len(),
                delay = updated.delay_minutes,
                wait_previous = updated.wait_previous,
                session = ?updated.session,
                "notifier_config replaced (PUT)"
            );
            Ok(Json(updated))
        }
        Err(e) => {
            tracing::error!(error = ?e, "notifier_config PUT failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("notifier_config put failed: {e:#}"),
            ))
        }
    }
}

/// `PATCH /api/notifier-config` — частичное обновление конфига.
///
/// Тело: [`PatchNotifierConfigReq`] — все поля опциональные, применяются
/// только `Some(..)`-варианты. Семантика `session`: пустая строка после
/// trim ⇒ сброс в `None` (sentinel «убрать дефолтную сессию»). При успехе —
/// 200 + JSON с обновлённым [`NotifierConfig`].
async fn patch_notifier_config(
    State(state): State<AppState>,
    Json(payload): Json<PatchNotifierConfigReq>,
) -> Result<Json<NotifierConfig>, (StatusCode, String)> {
    match state.notifier_config.patch(payload) {
        Ok(updated) => {
            tracing::info!("notifier_config patched (PATCH)");
            Ok(Json(updated))
        }
        Err(e) => {
            tracing::error!(error = ?e, "notifier_config PATCH failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("notifier_config patch failed: {e:#}"),
            ))
        }
    }
}

// =============================================================================
// Remote-servers endpoints (Phase 2)
// =============================================================================

/// Тело запроса `POST /api/remote-servers`.
///
/// Все три поля обязательны. URL валидируется на `http://` / `https://`,
/// label/token — на непустоту. Дубликаты id разрешаются авто-суффиксом
/// (см. `RemoteServerStore::add` → `allocate_id`).
#[derive(Debug, Deserialize)]
struct CreateRemoteServerReq {
    label: String,
    url: String,
    token: String,
}

/// Тело запроса `PATCH /api/remote-servers/:id`.
///
/// Все поля опциональны. `label` и `token` могут быть обновлены;
/// `url` намеренно не меняется (для смены URL — DELETE + POST с тем же
/// label, чтобы id остался плюс-минус тот же).
#[derive(Debug, Deserialize)]
struct PatchRemoteServerReq {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

/// `GET /api/remote-servers` — список всех зарегистрированных remote-серверов.
///
/// Возвращает `Json<Vec<RemoteServerView>>` — без поля `token`. Это
/// архитектурное решение: token хранится локально, никогда не утекает в API.
async fn list_remote_servers(
    State(state): State<AppState>,
) -> Json<Vec<remotes::RemoteServerView>> {
    let store = state.remotes.read().await;
    Json(store.list_views())
}

/// `POST /api/remote-servers` — добавляет remote-сервер в реестр.
///
/// Валидация: label/url/token непустые, url начинается с `http://` или
/// `https://`. ID авто-генерится из label через slugify с дедупликацией.
/// Pre-flight healthz-проверка пока не делается (это Phase 3, требует
/// reqwest). После успешного add — atomic save на диск.
///
/// Ответ: 201 Created + `RemoteServerView` (без token).
async fn create_remote_server(
    State(state): State<AppState>,
    Json(req): Json<CreateRemoteServerReq>,
) -> Result<(StatusCode, Json<remotes::RemoteServerView>), (StatusCode, String)> {
    let mut store = state.remotes.write().await;
    match store.add(req.label, req.url, req.token) {
        Ok(server) => {
            if let Err(e) = store.save() {
                tracing::error!(error = ?e, "remotes save failed");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("save: {e:#}"),
                ));
            }
            tracing::info!(id = %server.id, "remote server added");
            Ok((StatusCode::CREATED, Json(remotes::RemoteServerView::from(&server))))
        }
        Err(e) => {
            tracing::warn!(error = ?e, "create_remote_server failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `DELETE /api/remote-servers/:id` — удаляет запись по id.
///
/// - 204 No Content при успехе.
/// - 404 Not Found если id отсутствует.
async fn delete_remote_server(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut store = state.remotes.write().await;
    if !store.remove(&id) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no remote server with id `{id}`"),
        ));
    }
    if let Err(e) = store.save() {
        tracing::error!(error = ?e, "remotes save failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save: {e:#}"),
        ));
    }
    tracing::info!(%id, "remote server removed");
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/remote-servers/:id` — обновляет label/token.
///
/// - 200 + `RemoteServerView` при успехе.
/// - 404 если id неизвестен.
async fn patch_remote_server(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<PatchRemoteServerReq>,
) -> Result<Json<remotes::RemoteServerView>, (StatusCode, String)> {
    let mut store = state.remotes.write().await;
    let updated = match store.update(&id, req.label, req.token) {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no remote server with id `{id}`"),
            ));
        }
    };
    if let Err(e) = store.save() {
        tracing::error!(error = ?e, "remotes save failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("save: {e:#}"),
        ));
    }
    tracing::info!(%id, "remote server updated");
    Ok(Json(remotes::RemoteServerView::from(&updated)))
}

/// `GET /api/remote-servers/:id/healthz` — реальный pre-flight на удалённый
/// devforge.
///
/// Phase 3 (forge-v5x9.4) — заменяет заглушку из Phase 2. Делает
/// `GET <remote.url>/healthz` с Bearer-токеном и возвращает либо тело
/// remote'а (с добавленным полем `online: true`), либо JSON-описание ошибки
/// (`{ online: false, error: "..." }`) с подходящим HTTP-статусом.
///
/// HTTP-маппинг:
/// - 404 — `id` отсутствует в реестре;
/// - 502 BAD_GATEWAY — remote недоступен / TLS / refused;
/// - 200 — успех, тело — `{ online: true, status, remote_mode, version }`.
async fn remote_server_healthz(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let store = state.remotes.read().await;
    match remote_proxy::proxy_request(
        &store,
        &state.http,
        &id,
        reqwest::Method::GET,
        "/healthz",
        "",
        None,
        None,
    )
    .await
    {
        Ok((status, _headers, bytes)) => {
            // Парсим тело как JSON; если не JSON — обернём в {raw: <string>}.
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| {
                    serde_json::json!({
                        "raw": String::from_utf8_lossy(&bytes).to_string()
                    })
                });
            // Помечаем online на основании HTTP-статуса remote'а.
            let online = status.is_success();
            if let Some(obj) = value.as_object_mut() {
                obj.insert("online".to_string(), serde_json::Value::Bool(online));
            }
            Ok(Json(value))
        }
        Err(e) => match e {
            remote_proxy::ProxyError::UnknownServer(_) => Err((
                StatusCode::NOT_FOUND,
                format!("no remote server with id `{id}`"),
            )),
            remote_proxy::ProxyError::Network(net_err) => {
                tracing::warn!(error = ?net_err, %id, "remote healthz failed");
                Ok(Json(serde_json::json!({
                    "online": false,
                    "error": net_err.to_string(),
                })))
            }
            remote_proxy::ProxyError::InvalidUrl(msg) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("invalid proxy URL: {msg}"),
            )),
            remote_proxy::ProxyError::WebSocket(msg) => {
                // healthz всегда HTTP, не WS — этот вариант сюда попасть не должен,
                // но обрабатываем для exhaustiveness.
                tracing::warn!(error = %msg, %id, "remote healthz: websocket error in http path");
                Ok(Json(serde_json::json!({
                    "online": false,
                    "error": msg,
                })))
            }
        },
    }
}

// =============================================================================
// Session history endpoints (~/.config/forge/session_history.json)
// =============================================================================

/// `GET /api/sessions/history` — журнал ранее виденных сессий.
///
/// Возвращает JSON-массив [`session_history::HistorySession`], отсортированный
/// по `last_seen` убыв. (самые свежие — первыми; см. `HistoryStore::list`).
/// Включает как активные, так и закрытые сессии — фронт сам помечает, какие из
/// них уже запущены в tmux.
async fn get_session_history(
    State(state): State<AppState>,
) -> Json<Vec<session_history::HistorySession>> {
    Json(state.history.list())
}

/// Тело запроса restore/delete history — идентификатор сессии в истории
/// (`name + path`, тот же составной ключ, что и в `HistoryStore`).
#[derive(Debug, Deserialize)]
struct HistorySessionRef {
    name: String,
    path: String,
}

/// Восстанавливает одну сессию из истории: создаёт tmux-сессию `name` в `path`
/// и воссоздаёт её окна по записи в `store`.
///
/// Предполагается, что вызывающий уже проверил отсутствие сессии `name` среди
/// активных (см. [`restore_session_history`]). Логика восстановления окон:
/// - окно с индексом 0 уже существует у новой сессии — если в истории для него
///   есть запись, оно переименовывается через [`tmux::rename_window`];
/// - остальные окна (по порядку записи) создаются через [`tmux::new_window`]
///   с именем из истории.
///
/// Ошибки переименования/создания отдельных окон не фатальны: сессия уже
/// создана, поэтому они логируются (`warn`) и восстановление продолжается.
/// Возвращает `Err` только если не удалось создать саму сессию.
async fn restore_one_session(
    store: &session_history::HistoryStore,
    name: &str,
    path: &str,
) -> anyhow::Result<()> {
    tmux::new_session(name, std::path::Path::new(path)).await?;

    // Находим соответствующую запись истории, чтобы воссоздать окна.
    let entry = store
        .list()
        .into_iter()
        .find(|s| s.name == name && s.path == path);

    if let Some(entry) = entry {
        let mut windows = entry.windows;
        windows.sort_by_key(|w| w.index);
        for (pos, w) in windows.iter().enumerate() {
            if pos == 0 {
                // Окно 0 создаётся автоматически вместе с сессией — только
                // переименовываем под историческое имя.
                if let Err(e) = tmux::rename_window(name, 0, &w.name).await {
                    tracing::warn!(
                        session = %name, window = %w.name, error = ?e,
                        "restore: failed to rename initial window"
                    );
                }
            } else if let Err(e) = tmux::new_window(name, Some(&w.name)).await {
                tracing::warn!(
                    session = %name, window = %w.name, error = ?e,
                    "restore: failed to create window"
                );
            }
        }
    }

    Ok(())
}

/// `POST /api/sessions/history/restore` — восстановить одну сессию из истории.
///
/// Тело: [`HistorySessionRef`] (`{ name, path }`).
/// - 409 Conflict, если сессия с таким именем уже запущена в tmux.
/// - 201 Created + `{ "name": "<name>" }` при успешном восстановлении.
/// - 400 Bad Request при невалидном теле или ошибке создания сессии.
async fn restore_session_history(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let req: HistorySessionRef = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;

    let existing = tmux::list_sessions()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    if existing.iter().any(|s| s.name == req.name) {
        return Err((
            StatusCode::CONFLICT,
            format!("session `{}` is already running", req.name),
        ));
    }

    match restore_one_session(&state.history, &req.name, &req.path).await {
        Ok(()) => {
            tracing::info!(name = %req.name, path = %req.path, "session restored from history");
            Ok((StatusCode::CREATED, Json(serde_json::json!({ "name": req.name })))
                .into_response())
        }
        Err(e) => {
            tracing::warn!(name = %req.name, error = ?e, "restore_session_history failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `POST /api/sessions/history/restore-all` — восстановить все сессии из
/// истории, пропуская те, что уже запущены в tmux.
///
/// Возвращает `{ "restored": ["<name>", ...] }` — список имён реально
/// восстановленных сессий. Ошибки восстановления отдельных сессий логируются
/// и не прерывают обработку остальных.
async fn restore_all_session_history(
    State(state): State<AppState>,
) -> Result<Response, (StatusCode, String)> {
    let existing = tmux::list_sessions()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let existing_names: std::collections::HashSet<String> =
        existing.into_iter().map(|s| s.name).collect();

    let mut restored: Vec<String> = Vec::new();
    for entry in state.history.list() {
        if existing_names.contains(&entry.name) {
            continue;
        }
        match restore_one_session(&state.history, &entry.name, &entry.path).await {
            Ok(()) => {
                tracing::info!(name = %entry.name, "session restored from history (restore-all)");
                restored.push(entry.name);
            }
            Err(e) => {
                tracing::warn!(name = %entry.name, error = ?e, "restore-all: failed to restore session");
            }
        }
    }

    Ok(Json(serde_json::json!({ "restored": restored })).into_response())
}

/// `DELETE /api/sessions/history` — удалить запись из истории по `name + path`.
///
/// Тело: [`HistorySessionRef`]. Удаляет только запись в журнале (активную
/// tmux-сессию не трогает). Идемпотентно: отсутствие записи — не ошибка.
/// - 200 OK при успехе.
/// - 400 Bad Request при невалидном теле.
async fn delete_session_history(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, (StatusCode, String)> {
    let req: HistorySessionRef = serde_json::from_slice(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid body: {e}")))?;

    state.history.remove(&req.name, &req.path);
    tracing::info!(name = %req.name, path = %req.path, "session history entry removed");
    Ok(StatusCode::OK.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 1 — контракт ответа `GET /healthz`.
    ///
    /// Проверяет, что JSON-форма соответствует ожиданиям фронтенда:
    /// `status`, `remote_mode`, `version` присутствуют и имеют правильные типы.
    /// Версия читается из `CARGO_PKG_VERSION` (compile-time константа), значит
    /// в тесте можно завязаться на её непустоту (точное значение зависит от
    /// `Cargo.toml`).
    #[test]
    fn healthz_response_shape_remote_mode_off() {
        let resp = HealthzResponse {
            status: "ok",
            remote_mode: false,
            version: env!("CARGO_PKG_VERSION"),
        };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ok"));
        assert_eq!(v.get("remote_mode").and_then(|x| x.as_bool()), Some(false));
        assert!(
            v.get("version")
                .and_then(|x| x.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "version must be a non-empty string"
        );
    }

    #[test]
    fn healthz_response_shape_remote_mode_on() {
        let resp = HealthzResponse {
            status: "ok",
            remote_mode: true,
            version: env!("CARGO_PKG_VERSION"),
        };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert_eq!(v.get("remote_mode").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ok"));
    }

    // =========================================================================
    // Phase 3 — helpers для ?server=<id> и rebuild query.
    // =========================================================================

    #[test]
    fn extract_server_id_returns_none_when_absent() {
        let q: HashMap<String, String> =
            [("foo".to_string(), "bar".to_string())].into_iter().collect();
        assert_eq!(extract_server_id(&q), None);
    }

    #[test]
    fn extract_server_id_returns_none_for_empty_value() {
        let q: HashMap<String, String> =
            [("server".to_string(), "".to_string())].into_iter().collect();
        assert_eq!(extract_server_id(&q), None);
    }

    #[test]
    fn extract_server_id_trims_whitespace() {
        let q: HashMap<String, String> =
            [("server".to_string(), "   ".to_string())].into_iter().collect();
        assert_eq!(extract_server_id(&q), None);

        let q: HashMap<String, String> =
            [("server".to_string(), "  office  ".to_string())].into_iter().collect();
        assert_eq!(extract_server_id(&q), Some("office".to_string()));
    }

    #[test]
    fn extract_server_id_present_returns_value() {
        let q: HashMap<String, String> = [
            ("server".to_string(), "office-2".to_string()),
            ("project_id".to_string(), "forge".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(extract_server_id(&q), Some("office-2".to_string()));
    }

    #[test]
    fn rebuild_query_excludes_server_key() {
        let q: HashMap<String, String> = [
            ("server".to_string(), "office".to_string()),
            ("project_id".to_string(), "forge".to_string()),
        ]
        .into_iter()
        .collect();
        let s = rebuild_query_without_server(&q);
        assert!(!s.contains("server"));
        assert!(s.contains("project_id=forge"));
    }

    #[test]
    fn rebuild_query_empty_when_only_server() {
        let q: HashMap<String, String> =
            [("server".to_string(), "x".to_string())].into_iter().collect();
        let s = rebuild_query_without_server(&q);
        assert_eq!(s, "");
    }

    // =========================================================================
    // Phase 8 .3 — try_proxy_to_remote dispatch decision + reserved "local"
    // =========================================================================

    #[test]
    fn extract_server_id_local_is_reserved() {
        // ?server=local → None (passthrough к локальной логике).
        let q: HashMap<String, String> =
            [("server".to_string(), "local".to_string())].into_iter().collect();
        assert_eq!(extract_server_id(&q), None);
    }

    #[test]
    fn extract_server_id_local_with_whitespace_still_reserved() {
        let q: HashMap<String, String> =
            [("server".to_string(), "  local  ".to_string())].into_iter().collect();
        assert_eq!(
            extract_server_id(&q),
            None,
            "trim + reserved check: '  local  ' тоже passthrough"
        );
    }

    #[test]
    fn resolve_dispatch_no_server_param_is_local() {
        let q: HashMap<String, String> =
            [("foo".to_string(), "bar".to_string())].into_iter().collect();
        assert_eq!(resolve_dispatch(&q, true), DispatchDecision::Local);
        assert_eq!(resolve_dispatch(&q, false), DispatchDecision::Local);
    }

    #[test]
    fn resolve_dispatch_empty_server_param_is_local() {
        let q: HashMap<String, String> =
            [("server".to_string(), "".to_string())].into_iter().collect();
        assert_eq!(resolve_dispatch(&q, true), DispatchDecision::Local);
        let q: HashMap<String, String> =
            [("server".to_string(), "   ".to_string())].into_iter().collect();
        assert_eq!(resolve_dispatch(&q, true), DispatchDecision::Local);
    }

    #[test]
    fn resolve_dispatch_reserved_local_is_local() {
        let q: HashMap<String, String> =
            [("server".to_string(), "local".to_string())].into_iter().collect();
        assert_eq!(
            resolve_dispatch(&q, true),
            DispatchDecision::Local,
            "?server=local в remote-mode = passthrough"
        );
        assert_eq!(
            resolve_dispatch(&q, false),
            DispatchDecision::Local,
            "?server=local в legacy = тоже passthrough (не reject)"
        );
    }

    #[test]
    fn resolve_dispatch_unknown_server_in_remote_mode_returns_proxy() {
        // Сам dispatcher не проверяет существование server_id в store —
        // это делает remote_proxy::proxy_request, возвращая UnknownServer→404.
        let q: HashMap<String, String> =
            [("server".to_string(), "office-12".to_string())].into_iter().collect();
        assert_eq!(
            resolve_dispatch(&q, true),
            DispatchDecision::Proxy("office-12".to_string())
        );
    }

    #[test]
    fn resolve_dispatch_legacy_mode_with_server_returns_rejection() {
        let q: HashMap<String, String> =
            [("server".to_string(), "anything".to_string())].into_iter().collect();
        assert_eq!(
            resolve_dispatch(&q, false),
            DispatchDecision::LegacyRejection,
            "legacy (remote_mode=false) + ?server=any → 400"
        );
    }

    #[test]
    fn resolve_dispatch_server_with_project_param_proxies() {
        // Сочетание ?server=foo&project_id=bar: dispatcher всё равно решает по server.
        // project_id forward'ит rebuild_query_without_server.
        let q: HashMap<String, String> = [
            ("server".to_string(), "foo".to_string()),
            ("project_id".to_string(), "bar".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve_dispatch(&q, true),
            DispatchDecision::Proxy("foo".to_string())
        );
        // И project_id попадёт в proxied query:
        let qs = rebuild_query_without_server(&q);
        assert!(qs.contains("project_id=bar"));
        assert!(!qs.contains("server="));
    }

    #[test]
    fn resolve_dispatch_multi_server_uses_what_axum_parsed() {
        // axum/serde_urlencoded для HashMap<String,String> оставляет ОДНО
        // значение при дубликатах ключей (последнее или первое — зависит от
        // версии serde_urlencoded). Этот тест документирует контракт:
        // dispatcher работает с тем, что в HashMap, и не пытается «угадывать»
        // multiple values. То есть `?server=a&server=b` в продакшене
        // распарсится в `{server: 'b'}` (или 'a') — и dispatcher вернёт
        // Proxy с этим значением. Никаких 400 за «дубликат».
        let q: HashMap<String, String> =
            [("server".to_string(), "b".to_string())].into_iter().collect();
        assert_eq!(
            resolve_dispatch(&q, true),
            DispatchDecision::Proxy("b".to_string())
        );
    }

    #[test]
    fn urlencode_minimal_basic() {
        assert_eq!(urlencode_minimal("simple"), "simple");
        assert_eq!(urlencode_minimal("a&b"), "a%26b");
        assert_eq!(urlencode_minimal("a=b"), "a%3Db");
        assert_eq!(urlencode_minimal("a b"), "a%20b");
        assert_eq!(urlencode_minimal("a+b"), "a%2Bb");
        assert_eq!(urlencode_minimal("a?b#c"), "a%3Fb%23c");
        // alphanumerics, '-', '_', '.', '/' остаются как есть (наши id такие).
        assert_eq!(urlencode_minimal("office-2"), "office-2");
        assert_eq!(urlencode_minimal("a/b.c_d"), "a/b.c_d");
    }
}

