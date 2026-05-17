//! tmux-web — web-viewer для активных tmux-сессий.
//!
//! Phase 6.B: multi-project. AppState держит `Arc<RwLock<ProjectStore>>`,
//! все эндпоинты получают активный проект (path + tmux_prefix) и работают
//! в его контексте: tasks читаются из `active.path`, сессии фильтруются и
//! префиксуются по `active.tmux_prefix`, новые сессии стартуют в
//! `active.path` (`tmux new-session -c`).

mod attention;
mod auth;
mod cli;
mod daemon;
// Phase 1 Echo plugin — адаптер AppState → echo_host_api::HostApi.
// Регистрируется в main() через forge_echo::register_routes.
mod echo_host;
#[allow(dead_code)] // публичный API используется в Phase 3 (POST /api/todos/:id/promote)
mod notifier;
mod projects;
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
use std::path::{Path, PathBuf};
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
use tokio::process::Command as TokioCommand;
use tokio::sync::{broadcast, watch, RwLock};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::projects::{ensure_prefixed, Project, ProjectStore};
use crate::tasks::TaskEvent;
use crate::themes::{Theme, ThemesState};
use crate::tmux::SessionInfo;
use crate::todos::TodoStore;
use crate::user_settings::{PatchUserSettingsReq, UserSettings, UserSettingsStore};
use crate::ws_todos::TodoEvent;

/// Глобальное состояние axum-приложения.
///
/// `projects` под `Arc<RwLock<…>>` — чтения (GET /api/projects, GET
/// /api/sessions, GET /api/tasks) идут параллельно, write (POST/DELETE)
/// сериализуются. Disk persistence — через `ProjectStore::save()` под
/// write-lock'ом.
///
/// Phase 6.D:
/// - `tasks_tx` — broadcast-sender, в который [`tasks_watcher::run_watcher`]
///   пушит [`TaskEvent`] при изменениях `.beads/issues.jsonl`. WS-handler
///   `/ws/tasks` делает `subscribe()` на каждое соединение.
/// - `active_path_tx` — watch-sender для пересоздания watcher'а при смене
///   активного проекта. Любой эндпоинт, меняющий active project, должен
///   `state.active_path_tx.send(new_path)` сразу после `store.save()`.
#[derive(Clone)]
struct AppState {
    projects: Arc<RwLock<ProjectStore>>,
    /// Phase 6.D — broadcast-канал глобальных task-событий активного проекта.
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
    /// Phase wk7 — каталог для `themes.json` (тот же `~/.config/forge/`,
    /// что и `projects.json`). Хранится в state, чтобы не пересчитывать
    /// `default_registry_path` в каждом handler'е.
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

    // Загрузка реестра проектов. При первом старте создастся
    // ~/.config/forge/projects.json с дефолтным проектом forge.
    let registry_path = projects::default_registry_path()?;
    let store = ProjectStore::load(registry_path.clone())
        .with_context(|| format!("failed to load project registry from {}", registry_path.display()))?;
    tracing::info!(
        path = %registry_path.display(),
        active = %store.active().id,
        count = store.list().len(),
        "loaded project registry"
    );
    // Phase 6.D — каналы для realtime task-watcher.
    //
    // tasks_tx: broadcast(64) — глубина 64 сообщений достаточна для
    // короткого burst'а (`br sync` обычно даёт 1-3 события). При лагающем
    // подписчике broadcast::Sender::send просто дропает старые, что для
    // нашего случая ОК — UI всё равно может выпасть снимок при reconnect.
    //
    // active_path_tx: watch — последняя value-семантика, идеально для
    // «активный путь сейчас X». Watcher подписывается через .changed().
    let (tasks_tx, _) = broadcast::channel::<TaskEvent>(64);
    let initial_path = store.active().path.clone();
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

    // Phase wk7 — Themes state. Каталог = parent registry_path (типично
    // `~/.config/forge/`). Если файла нет или он повреждён — `themes::load`
    // вернёт ThemesState::default() (active="default", custom=[]) без паники.
    let themes_dir = registry_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
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

    let app_state = AppState {
        projects: Arc::new(RwLock::new(store)),
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
        // Tasks API.
        .route("/api/tasks", get(get_tasks).post(create_task))
        .route("/api/tasks/:id", patch(patch_task).delete(close_task))
        .route("/api/tasks/:id/reopen", post(reopen_task))
        .route("/api/tasks/:id/purge", post(purge_task))
        // Projects API (Phase 6.B).
        .route("/api/projects", get(get_projects).post(create_project))
        .route("/api/projects/:id", delete(delete_project))
        .route("/api/projects/:id/settings", patch(patch_project_settings))
        .route("/api/projects/active", post(set_active_project))
        .route("/api/projects/init", post(init_project))
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
    let shutdown_signal = async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received; running Echo graceful shutdown");
        forge_echo::shutdown(&echo_for_shutdown).await;
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
    project_id: Option<String>,
    project_name: Option<String>,
    /// Идентификатор папочно-ориентированной группы для sidebar-группировки.
    /// Формат: `"__folder:<absolute_path>"`. Префикс `__folder:` гарантирует
    /// отсутствие коллизий с `project_id` (формы `<uuid>` / `__path__:<cwd>` /
    /// tmux-префикс). `None` только для сессий с пустым или некорректным
    /// `path` (file_name отсутствует). Сериализуется всегда — фронт ожидает
    /// унифицированный формат.
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

/// `GET /api/sessions` — JSON-массив ВСЕХ активных tmux-сессий (без фильтра
/// по активному проекту). Каждая сессия обогащается `project_id`/`project_name`
/// — id и имя проекта, чей `tmux_prefix` матчит имя сессии через
/// [`projects::session_belongs`]. Если совпадений нет — оба поля `None`
/// (orphan-сессия, созданная вне tmux-web).
///
/// Каждая сессия отдаётся как [`SessionDto`] = `SessionInfo` + флаг
/// `needs_attention` из snapshot'а `state.attention` + `project_id` /
/// `project_name`. Snapshot attention снимается один раз на запрос
/// (под коротким read-lock'ом) и не блокирует watcher. Snapshot проектов
/// тоже снимается один раз (через `ProjectStore::list`) — это копия
/// `Vec<Project>`, по которой матчинг идёт без удержания lock'а.
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

    let projects_snap = state.projects.read().await.list();
    match tmux::list_sessions().await {
        Ok(list) => {
            let attention = state.attention.snapshot().await;
            let dtos: Vec<SessionDto> = list
                .into_iter()
                .map(|s| {
                    let needs_attention = attention.get(&s.name).copied().unwrap_or(false);
                    let (project_id, project_name) = resolve_project(&s, &projects_snap);
                    let (folder_id, folder_label) = resolve_folder(&s);
                    SessionDto {
                        needs_attention,
                        project_id,
                        project_name,
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

/// `POST /api/sessions` — создаёт detached-сессию в cwd активного проекта.
/// Имя автопрефиксуется по `active.tmux_prefix` если ещё не префиксовано.
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

    let (name, cwd) = {
        let store = state.projects.read().await;
        let active = store.active();
        let prefixed = ensure_prefixed(&active.tmux_prefix, &req.name);
        (prefixed, active.path.clone())
    };

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
/// Новое имя автопрефиксуется через `active.tmux_prefix` (если ещё не префиксовано),
/// как и при `POST /api/sessions`.
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

    let new_name = {
        let store = state.projects.read().await;
        let active = store.active();
        ensure_prefixed(&active.tmux_prefix, req.name.trim())
    };

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

    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
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

    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };

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

    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };

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

    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };

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

    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };
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

    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };
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
// Projects endpoints (Phase 6.B)
// =============================================================================

/// JSON-форма проекта во фронтенд. Дополнительно к полям `Project` —
/// `active: bool` для быстрого рендера в `<select>`.
///
/// Phase 3: добавлены `notify_template`, `notify_delay_minutes`,
/// `notify_wait_previous`, `notify_session` — для секции «Notifications»
/// в Settings modal на фронтенде. Эти же поля принимаются роутом
/// `PATCH /api/projects/:id/settings`.
#[derive(Debug, Serialize)]
struct ProjectDto {
    id: String,
    name: String,
    path: String,
    tmux_prefix: String,
    active: bool,
    notify_template: String,
    notify_delay_minutes: u32,
    notify_wait_previous: bool,
    notify_session: Option<String>,
    /// Phase 3 — источник записи. Для локального проекта — `"local"`. См.
    /// комментарий у [`SessionDto::origin`] про унификацию формата.
    origin: String,
}

impl ProjectDto {
    fn new(p: &Project, active_id: &str) -> Self {
        Self {
            id: p.id.clone(),
            name: folder_name(p),
            path: p.path.display().to_string(),
            tmux_prefix: p.tmux_prefix.clone(),
            active: p.id == active_id,
            notify_template: p.notify_template.clone(),
            notify_delay_minutes: p.notify_delay_minutes,
            notify_wait_previous: p.notify_wait_previous,
            notify_session: p.notify_session.clone(),
            origin: "local".to_string(),
        }
    }
}

/// Имя проекта = имя последней папки в `project.path`.
/// Fallback на `Project::name` если path не содержит file_name (root, пустой и т.п.).
fn folder_name(p: &Project) -> String {
    p.path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.name.clone())
}

/// Резолвит проект для сессии. Стратегия в порядке приоритета:
/// 1. Зарегистрированный проект, чей `path` совпадает или является префиксом
///    `session.path` (берём самое длинное совпадение для вложенных путей).
/// 2. Зарегистрированный проект по `tmux_prefix` имени сессии.
/// 3. Авто-группа из basename(`session.path`) — даёт стабильное имя для
///    несзарегистрированных папок.
/// Возвращает `(None, None)` только если `session.path` пуст и ничего не нашли.
fn resolve_project(
    s: &tmux::SessionInfo,
    projects: &[Project],
) -> (Option<String>, Option<String>) {
    let sess_path = std::path::Path::new(&s.path);

    let by_path = projects
        .iter()
        .filter(|p| sess_path.starts_with(&p.path))
        .max_by_key(|p| p.path.as_os_str().len());
    if let Some(p) = by_path {
        return (Some(p.id.clone()), Some(folder_name(p)));
    }

    let by_prefix = projects
        .iter()
        .find(|p| projects::session_belongs(&p.tmux_prefix, &s.name));
    if let Some(p) = by_prefix {
        return (Some(p.id.clone()), Some(folder_name(p)));
    }

    if let Some(name) = sess_path.file_name() {
        let folder = name.to_string_lossy().into_owned();
        let id = format!("__path__:{}", s.path);
        return (Some(id), Some(folder));
    }

    (None, None)
}

/// Резолвит папочно-ориентированную группу для сессии.
///
/// Возвращает кортеж `(folder_id, folder_label)`:
/// - `folder_id` — стабильный ключ группы вида `"__folder:<absolute_path>"`.
///   Префикс `__folder:` исключает коллизии с `project_id` (формы registered-uuid,
///   `__path__:<cwd>`, tmux-префикс), используемыми в `switchActiveProject`
///   и фильтрах TODO/`.beads`.
/// - `folder_label` — basename последней папки `session.path` для отображения
///   в заголовке группы sidebar.
///
/// Если `session.path` пустой или равен `/` (нет `file_name`), либо basename
/// пустая строка — оба значения `None` (orphan-ветка sidebar отрисует через
/// `ORPHAN_KEY`).
///
/// В отличие от [`resolve_project`], НЕ учитывает зарегистрированные проекты
/// и tmux-префиксы — это чисто файловая группировка для UI, независимая от
/// семантики `project_id`.
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

/// `GET /api/projects` — массив всех проектов с пометкой `active`.
async fn get_projects(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if let Some(result) = try_proxy_to_remote(
        &state,
        &q,
        reqwest::Method::GET,
        "/api/projects",
        None,
        None,
        true,
    )
    .await
    {
        return result;
    }

    let store = state.projects.read().await;
    let active = store.active_id().to_string();
    let dtos: Vec<ProjectDto> = store
        .list()
        .iter()
        .map(|p| ProjectDto::new(p, &active))
        .collect();
    Ok(Json(dtos).into_response())
}

/// Тело запроса `POST /api/projects`.
#[derive(Debug, Deserialize)]
struct CreateProjectReq {
    name: String,
    path: String,
    #[serde(default)]
    tmux_prefix: Option<String>,
}

/// `POST /api/projects` — добавляет проект в реестр (без mkdir / git init).
///
/// - 201 + Project DTO.
/// - 400 при дубликате id или пустом имени.
async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Result<(StatusCode, Json<ProjectDto>), (StatusCode, String)> {
    let mut store = state.projects.write().await;
    let path = PathBuf::from(&req.path);
    match store.add(req.name, path, req.tmux_prefix) {
        Ok(p) => {
            if let Err(e) = store.save() {
                tracing::error!(error = ?e, "projects save failed");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("save: {e:#}")));
            }
            let active = store.active_id().to_string();
            tracing::info!(id = %p.id, "project added");
            Ok((StatusCode::CREATED, Json(ProjectDto::new(&p, &active))))
        }
        Err(e) => {
            tracing::warn!(error = ?e, "create_project failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `DELETE /api/projects/:id` — удаление. Активный — 409.
async fn delete_project(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut store = state.projects.write().await;
    // Сравниваем с registered active (не transient), иначе пока активен
    // synthetic transient-проект, можно случайно удалить тот registered,
    // что станет активным после clear_transient.
    if id == store.registered_active_id() {
        return Err((
            StatusCode::CONFLICT,
            format!("cannot remove the active project `{id}`"),
        ));
    }
    match store.remove(&id) {
        Ok(true) => {
            if let Err(e) = store.save() {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("save: {e:#}")));
            }
            tracing::info!(%id, "project removed");
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            format!("no project with id `{id}`"),
        )),
        Err(e) => Err((StatusCode::CONFLICT, format!("{e:#}"))),
    }
}

/// Тело запроса `PATCH /api/projects/:id/settings`.
///
/// Все поля опциональны. Семантика `notify_session`:
/// - отсутствие поля → не трогать;
/// - `null` → стереть (записать None);
/// - строка → записать значение.
///
/// Текущая deserialize-стратегия: serde с `Option<Option<...>>` не различает
/// `null` и отсутствие поля без custom-кода. Здесь используем тот же
/// `deserialize_optional_optional_string`, что и в [`PatchTodoReq`].
#[derive(Debug, Deserialize)]
struct PatchProjectSettingsReq {
    #[serde(default)]
    notify_template: Option<String>,
    #[serde(default)]
    notify_delay_minutes: Option<u32>,
    #[serde(default)]
    notify_wait_previous: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    notify_session: Option<Option<String>>,
}

/// `PATCH /api/projects/:id/settings` — обновляет notify-настройки проекта.
///
/// 404 если проекта нет. После апдейта — atomic save через `ProjectStore::save`.
/// Поля, отсутствующие в body, не перезаписываются (см. doc-string на
/// [`ProjectStore::update_settings`]).
async fn patch_project_settings(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<PatchProjectSettingsReq>,
) -> Result<Json<ProjectDto>, (StatusCode, String)> {
    let mut store = state.projects.write().await;
    let updated = match store.update_settings(
        &id,
        req.notify_template,
        req.notify_delay_minutes,
        req.notify_wait_previous,
        req.notify_session,
    ) {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no project with id `{id}`"),
            ));
        }
    };
    if let Err(e) = store.save() {
        tracing::error!(error = ?e, "projects save failed");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("save: {e:#}")));
    }
    let active = store.active_id().to_string();
    tracing::info!(%id, "project settings updated");
    Ok(Json(ProjectDto::new(&updated, &active)))
}

/// Тело запроса `POST /api/projects/active`.
#[derive(Debug, Deserialize)]
struct SetActiveReq {
    id: String,
}

/// `POST /api/projects/active` — переключает активный проект.
///
/// Phase 6.D: после сохранения отправляет новый `active.path` в
/// `state.active_path_tx`, чтобы фоновый tasks-watcher пересоздал
/// notify-watcher на новый `.beads/`. Подписчики `/ws/tasks` дополнительно
/// получат от клиента `{kind:"reload"}`-инициативу через JS-логику.
async fn set_active_project(
    State(state): State<AppState>,
    Json(req): Json<SetActiveReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Transient form: id вида `__path__:<absolute-cwd>`. Устанавливаем
    // synthetic active project без записи в реестр. Используется для
    // auto-group сессий из нерегистрированных папок.
    if let Some(raw_path) = req.id.strip_prefix("__path__:") {
        let path = PathBuf::from(raw_path);
        if !path.is_absolute() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("transient project path must be absolute: {raw_path}"),
            ));
        }
        if !path.exists() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("path does not exist: {raw_path}"),
            ));
        }
        let new_path = {
            let mut store = state.projects.write().await;
            store.set_transient_active(path.clone());
            store.active().path.clone()
        };
        if let Err(e) = state.active_path_tx.send(new_path) {
            tracing::warn!(error = ?e, "active_path_tx.send failed; watcher may be dead");
        }
        tracing::info!(path = %path.display(), "active project switched (transient)");
        return Ok(StatusCode::NO_CONTENT);
    }

    let new_path = {
        let mut store = state.projects.write().await;
        // Сначала чистим transient — иначе set_active не отразится в active().
        store.clear_transient_active();
        if let Err(e) = store.set_active(&req.id) {
            return Err((StatusCode::BAD_REQUEST, format!("{e:#}")));
        }
        if let Err(e) = store.save() {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("save: {e:#}")));
        }
        store.active().path.clone()
    };
    if let Err(e) = state.active_path_tx.send(new_path) {
        tracing::warn!(error = ?e, "active_path_tx.send failed; watcher may be dead");
    }
    tracing::info!(id = %req.id, "active project switched");
    Ok(StatusCode::NO_CONTENT)
}

/// Тело запроса `POST /api/projects/init`.
#[derive(Debug, Deserialize)]
struct InitProjectReq {
    name: String,
    path: String,
    #[serde(default)]
    tmux_prefix: Option<String>,
}

/// `POST /api/projects/init` — bootstrap новой папки + регистрация.
///
/// Что делает:
/// 1. `mkdir -p path`.
/// 2. `touch CLAUDE.md`, `TODO.md`.
/// 3. Создаёт `.gitignore` со стандартом (target/, .DS_Store, node_modules,
///    .beads/*.db, .beads/*.db-*).
/// 4. `git init` (если ещё нет `.git`).
/// 5. `br init` (если ещё нет `.beads`).
/// 6. Добавляет в реестр через `store.add()` + сохраняет на диск.
///
/// При любом фейле инициализации каталога — 500 + сообщение. При ошибке
/// `add` (например, дубликат id) — 400. Идемпотентность: если каталог уже
/// существует и в нём есть файлы — touch не перезатирает, git/br init —
/// пропускаются.
async fn init_project(
    State(state): State<AppState>,
    Json(req): Json<InitProjectReq>,
) -> Result<(StatusCode, Json<ProjectDto>), (StatusCode, String)> {
    let path = PathBuf::from(&req.path);

    // 1) mkdir -p
    if let Err(e) = std::fs::create_dir_all(&path) {
        tracing::error!(error = ?e, path = %path.display(), "mkdir failed");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("mkdir {}: {e}", path.display()),
        ));
    }

    // 2) touch CLAUDE.md / TODO.md (idempotent — не трогаем содержимое если есть).
    if let Err(e) = touch_if_missing(&path.join("CLAUDE.md"), b"") {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("CLAUDE.md: {e}")));
    }
    if let Err(e) = touch_if_missing(&path.join("TODO.md"), b"") {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("TODO.md: {e}")));
    }

    // 3) .gitignore — стандартный набор.
    let gitignore = b"target/\n.DS_Store\nnode_modules/\n.beads/*.db\n.beads/*.db-*\n";
    if let Err(e) = touch_if_missing(&path.join(".gitignore"), gitignore) {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!(".gitignore: {e}")));
    }

    // 4) git init если нет .git/.
    if !path.join(".git").exists() {
        if let Err(e) = run_in(&path, "git", &["init"]).await {
            tracing::error!(error = ?e, "git init failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("git init: {e}")));
        }
    }

    // 5) br init если нет .beads/.
    if !path.join(".beads").exists() {
        if let Err(e) = run_in(&path, "br", &["init"]).await {
            // Если br недоступен — это soft-fail: проект всё равно регистрируем,
            // но возвращаем 500 чтобы пользователь увидел проблему. Beads
            // нужен для tasks-таба, без него UI работать не будет.
            tracing::error!(error = ?e, "br init failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("br init: {e}")));
        }
    }

    // 6) registry add + save.
    let mut store = state.projects.write().await;
    let added = match store.add(req.name, path, req.tmux_prefix) {
        Ok(p) => p,
        Err(e) => {
            return Err((StatusCode::BAD_REQUEST, format!("{e:#}")));
        }
    };
    if let Err(e) = store.save() {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("save: {e:#}")));
    }
    let active = store.active_id().to_string();
    tracing::info!(id = %added.id, path = %added.path.display(), "project initialized");
    Ok((StatusCode::CREATED, Json(ProjectDto::new(&added, &active))))
}

/// Создаёт файл с заданным содержимым, если он ещё не существует. Если файл
/// уже есть — оставляет его как есть (touch-семантика без перезаписи).
fn touch_if_missing(p: &Path, contents: &[u8]) -> std::io::Result<()> {
    if p.exists() {
        return Ok(());
    }
    std::fs::write(p, contents)
}

/// Запускает `cmd args...` в `cwd` и возвращает Err при non-zero exit или
/// невозможности spawn (например, бинарь не в PATH).
async fn run_in(cwd: &Path, cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = TokioCommand::new(cmd)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .with_context(|| format!("failed to spawn `{cmd}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`{cmd} {}` failed (exit {:?}): {}",
            args.join(" "),
            output.status.code(),
            stderr.trim()
        );
    }
    Ok(())
}

// =============================================================================
// TODOs endpoints (Phase 3)
// =============================================================================

/// `GET /api/todos?project_id=...` — JSON-массив TODO-карточек проекта.
///
/// Если `project_id` не задан — используем активный проект. Возвращает
/// пустой массив (не 404), если в проекте нет TODO.
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

    let pid = match q.get("project_id").map(String::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => state.projects.read().await.active().id.clone(),
    };
    Ok(Json(state.todos.list(&pid)).into_response())
}

/// Тело запроса `POST /api/todos`.
#[derive(Debug, Deserialize)]
struct CreateTodoReq {
    #[serde(default)]
    project_id: Option<String>,
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
/// `project_id` опционален: если не задан — берём активный проект.
/// Валидация: title после trim не должен быть пустым → 400.
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
    let pid = match req.project_id.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => state.projects.read().await.active().id.clone(),
    };
    match state.todos.create(&pid, title, req.description.clone(), req.plan_mode) {
        Ok(t) => {
            // Broadcast — игнорируем Err (нет подписчиков, ОК).
            let _ = state.todos_tx.send(ws_todos::upsert(t.clone()));
            tracing::info!(id = %t.id, project = %pid, "todo created");
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
#[derive(Debug, Deserialize)]
struct PatchTodoReq {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    description: Option<Option<String>>,
    /// `None` (поля нет) → не трогать. `Some(true|false)` → перезаписать.
    #[serde(default)]
    plan_mode: Option<bool>,
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
    match state
        .todos
        .update(&id, req.title.clone(), req.description.clone(), req.plan_mode)
    {
        Ok(Some(t)) => {
            let _ = state.todos_tx.send(ws_todos::upsert(t.clone()));
            tracing::info!(%id, "todo updated");
            Ok(Json(t).into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            format!("no todo with id `{id}`"),
        )),
        Err(e) => {
            tracing::warn!(%id, error = ?e, "todo update failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
        }
    }
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

    let project_id = match state.todos.get(&id) {
        Some(t) => t.project_id,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no todo with id `{id}`"),
            ));
        }
    };
    match state.todos.delete(&id) {
        Ok(true) => {
            let _ = state.todos_tx.send(ws_todos::removed(project_id, id.clone()));
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

/// Дефолтный шаблон уведомления, если у проекта `notify_template` пуст.
/// Поддерживает плейсхолдеры `{id}`, `{title}`, `{description}`, `{priority}`,
/// `{type}`. Используется как в [`promote_todo`], так и для UI-настроек.
const DEFAULT_NOTIFY_TEMPLATE: &str =
    "Новая задача [{id}]: {title} — нужно сделать";

/// Суффикс, добавляемый к notify-тексту, если у TODO `plan_mode == true`.
/// Прикрепляется через `\n` к отрендеренному template'у в `promote_todo`.
/// Точный текст согласован с UI: чекбокс "Включить план мод" в модалке TODO.
const PLAN_MODE_SUFFIX: &str = "Создай план для этой задачи";

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
/// `session` — опциональный override tmux-сессии для уведомления. Если пусто —
/// берём `project.notify_session`, иначе fallback на `<tmux_prefix>-main`
/// или сам `tmux_prefix` (если префикс пуст — 400, нет куда слать).
#[derive(Debug, Deserialize)]
struct PromoteTodoReq {
    #[serde(default)]
    session: Option<String>,
}

/// `POST /api/todos/:id/promote` — конвертирует TODO в bd-задачу и ставит
/// уведомление в очередь.
///
/// Алгоритм:
/// 1. Найти TODO (`todos.get`) → 404 если нет.
/// 2. Найти проект (`projects.get`) → 500 если пропал (TODO ссылается на
///    несуществующий проект — состояние нарушено).
/// 3. Создать bd-issue: `br create --json --title <todo.title> --description
///    <todo.description> -t <todo.issue_type> -p <todo.priority>` в
///    `project.path`. Получаем JSON → извлекаем `.id` (string).
/// 4. Удалить TODO + broadcast Removed.
/// 5. Сформировать NotifyJob:
///    - text = format_notify_template(...)
///    - session: req.session || project.notify_session || <tmux_prefix>-main
///    - mode: WaitPrevious если notify_wait_previous; иначе Delayed если
///      notify_delay_minutes>0; иначе Immediate.
///    - валидация session: 400 при отсутствии (resolve_target_session возвр. None).
/// 6. `notifier.enqueue(job).await`.
/// 7. Ответ 200 `{ promoted: true, task_id: "<bd-id>" }`.
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

    // 2) Загружаем проект (snapshot полей под read-lock'ом).
    let project_snap = {
        let store = state.projects.read().await;
        match store.find_any(&todo.project_id) {
            Some(p) => p.clone(),
            None => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("project `{}` for todo `{}` is gone", todo.project_id, id),
                ));
            }
        }
    };

    // 3) Резолвим целевую tmux-сессию заранее — чтобы НЕ создавать
    //    bd-задачу, если кидать уведомление некуда. Иначе — orphan-задача.
    let target_session = match resolve_notify_session(&req.session, &project_snap) {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                "no tmux session configured (set project.notify_session or pass `session` in body)"
                    .to_string(),
            ));
        }
    };

    // 4) Создаём bd-задачу.
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
    let created = match tasks::run_br(&arg_refs, &project_snap.path).await {
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

    // 5) Удаляем TODO + broadcast.
    if let Err(e) = state.todos.delete(&id) {
        tracing::warn!(%id, error = ?e, "todo delete after promote failed (bd task already created)");
    } else {
        let _ = state
            .todos_tx
            .send(ws_todos::removed(todo.project_id.clone(), id.clone()));
    }

    // 6) Формируем NotifyJob и enqueue.
    let template = if project_snap.notify_template.trim().is_empty() {
        DEFAULT_NOTIFY_TEMPLATE
    } else {
        project_snap.notify_template.as_str()
    };
    let description = todo.description.clone().unwrap_or_default();
    let mut text = format_notify_template(
        template,
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

    let mode = if project_snap.notify_wait_previous {
        notifier::NotifyMode::WaitPrevious {
            previous_task_id: None,
        }
    } else if project_snap.notify_delay_minutes > 0 {
        let delay_ms = project_snap.notify_delay_minutes as u64 * 60_000;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        notifier::NotifyMode::Delayed {
            fire_at_unix_ms: now + delay_ms,
        }
    } else {
        notifier::NotifyMode::Immediate
    };

    let job = notifier::new_job(
        todo.project_id.clone(),
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
            "notify_warning": format!("{e:#}"),
        }))
        .into_response());
    }

    tracing::info!(
        todo_id = %id,
        task_id = %task_id,
        project = %todo.project_id,
        "todo promoted"
    );
    Ok(Json(serde_json::json!({
        "promoted": true,
        "task_id": task_id,
    }))
    .into_response())
}

/// Резолвит целевую tmux-сессию для уведомления.
///
/// Приоритет:
/// 1. Override из request body (если непустой trim).
/// 2. `project.notify_session` (если непустой).
/// 3. Fallback `<tmux_prefix>-main` если `tmux_prefix` непустой.
/// 4. Иначе `None` → 400 в caller'е.
fn resolve_notify_session(override_session: &Option<String>, project: &Project) -> Option<String> {
    if let Some(s) = override_session.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(s) = project
        .notify_session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(s.to_string());
    }
    let prefix = project.tmux_prefix.trim();
    if prefix.is_empty() {
        return None;
    }
    Some(format!("{prefix}-main"))
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

