//! tmux-web — web-viewer для активных tmux-сессий.
//!
//! Phase 6.B: multi-project. AppState держит `Arc<RwLock<ProjectStore>>`,
//! все эндпоинты получают активный проект (path + tmux_prefix) и работают
//! в его контексте: tasks читаются из `active.path`, сессии фильтруются и
//! префиксуются по `active.tmux_prefix`, новые сессии стартуют в
//! `active.path` (`tmux new-session -c`).

mod attention;
#[allow(dead_code)] // публичный API используется в Phase 3 (POST /api/todos/:id/promote)
mod notifier;
mod projects;
mod pty;
mod tasks;
mod tasks_watcher;
mod themes;
mod tmux;
mod todos;
mod ws;
mod ws_tasks;
mod ws_todos;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;
use tokio::sync::{broadcast, watch, RwLock};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::projects::{ensure_prefixed, Project, ProjectStore};
use crate::tasks::TaskEvent;
use crate::themes::{Theme, ThemesState};
use crate::tmux::SessionInfo;
use crate::todos::TodoStore;
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Инициализация логирования. По умолчанию: info для всего + debug для tmux_web.
    // Переопределяется переменной окружения RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tmux_web=debug")),
        )
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

    // Каталог со статикой. По умолчанию ./static относительно cwd, но мы ищем
    // также рядом с бинарём — это упрощает запуск из произвольной директории.
    let static_dir = resolve_static_dir();
    tracing::info!(path = %static_dir.display(), "serving static files");

    let app = Router::new()
        .route("/healthz", get(healthz))
        // Sessions API.
        .route("/api/sessions", get(get_sessions).post(create_session))
        .route("/api/sessions/:name", delete(delete_session))
        // Tasks API.
        .route("/api/tasks", get(get_tasks).post(create_task))
        .route("/api/tasks/:id", patch(patch_task).delete(close_task))
        .route("/api/tasks/:id/reopen", post(reopen_task))
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
        // WebSocket-attach в tmux-сессию.
        .route("/ws/attach", get(ws::attach))
        // WebSocket — lazygit TUI в браузере (по cwd проекта).
        .route("/ws/lazygit", get(ws::lazygit_attach))
        // Phase 6.D — WS-стрим realtime событий из beads watcher'а.
        .route("/ws/tasks", get(ws_tasks::tasks_ws))
        // Phase 3 — WS-стрим TODO-карточек.
        .route("/ws/todos", get(ws_todos::todos_ws))
        .with_state(app_state)
        // ServeDir отдаёт index.html на запрос "/" автоматически.
        .fallback_service(ServeDir::new(&static_dir).append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = "127.0.0.1:7331".parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!(%addr, "listening on http://{addr}");

    axum::serve(listener, app)
        .await
        .context("axum server error")?;

    Ok(())
}

/// Health-check endpoint.
async fn healthz() -> &'static str {
    "ok"
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
) -> Result<Json<Vec<SessionDto>>, (StatusCode, String)> {
    let projects_snap = state.projects.read().await.list();
    match tmux::list_sessions().await {
        Ok(list) => {
            let attention = state.attention.snapshot().await;
            let dtos: Vec<SessionDto> = list
                .into_iter()
                .map(|s| {
                    let needs_attention = attention.get(&s.name).copied().unwrap_or(false);
                    let (project_id, project_name) = resolve_project(&s, &projects_snap);
                    SessionDto {
                        needs_attention,
                        project_id,
                        project_name,
                        info: s,
                    }
                })
                .collect();
            Ok(Json(dtos))
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
    Json(req): Json<CreateSessionReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (name, cwd) = {
        let store = state.projects.read().await;
        let active = store.active();
        let prefixed = ensure_prefixed(&active.tmux_prefix, &req.name);
        (prefixed, active.path.clone())
    };

    match tmux::new_session(&name, &cwd).await {
        Ok(()) => {
            tracing::info!(name = %name, cwd = %cwd.display(), "tmux session created");
            Ok(StatusCode::CREATED)
        }
        Err(e) => {
            tracing::warn!(name = %name, error = ?e, "new_session failed");
            // Любая ошибка от tmux/валидатора — 400 (клиент дал плохой ввод
            // либо состояние tmux-сервера противоречит запросу).
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `DELETE /api/sessions/:name` — убивает существующую сессию.
///
/// - 204 No Content при успехе.
/// - 400 Bad Request при невалидном имени или если сессии нет.
async fn delete_session(
    AxumPath(name): AxumPath<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    match tmux::kill_session(&name).await {
        Ok(()) => {
            tracing::info!(%name, "tmux session killed");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            tracing::warn!(%name, error = ?e, "kill_session failed");
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };
    match tasks::list_tasks(&cwd).await {
        Ok(value) => Ok(Json(value)),
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
    Json(req): Json<CreateTaskReq>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
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
            Ok((StatusCode::CREATED, Json(value)))
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
    Json(req): Json<PatchTaskReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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
            Ok(Json(value))
        }
        Err(e) => {
            tracing::warn!(%id, error = ?e, "br update failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// Query-параметры для `DELETE /api/tasks/:id`.
#[derive(Debug, Deserialize)]
struct CloseTaskQuery {
    #[serde(default)]
    reason: Option<String>,
}

/// `DELETE /api/tasks/:id?reason=...` — закрывает issue через `br close --json -r`.
///
/// 204 No Content при успехе; ошибка `br` маппится в 400.
async fn close_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<CloseTaskQuery>,
) -> Result<StatusCode, (StatusCode, String)> {
    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };

    let mut args: Vec<String> = vec!["close".to_string(), "--json".to_string(), id.clone()];
    if let Some(r) = q.reason.as_deref().filter(|s| !s.is_empty()) {
        args.push("-r".to_string());
        args.push(r.to_string());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match tasks::run_br(&arg_refs, &cwd).await {
        Ok(_) => {
            tracing::info!(%id, "task closed");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            tracing::warn!(%id, error = ?e, "br close failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}

/// `POST /api/tasks/:id/reopen` — переводит закрытый issue обратно в `open`
/// через `br reopen --json`. Возвращает 200 + объект `{reopened: [...]}`.
async fn reopen_task(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cwd = {
        let store = state.projects.read().await;
        store.active().path.clone()
    };
    let args = ["reopen", "--json", id.as_str()];
    match tasks::run_br(&args, &cwd).await {
        Ok(value) => {
            tracing::info!(%id, "task reopened");
            Ok(Json(value))
        }
        Err(e) => {
            tracing::warn!(%id, error = ?e, "br reopen failed");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
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

/// `GET /api/projects` — массив всех проектов с пометкой `active`.
async fn get_projects(State(state): State<AppState>) -> Json<Vec<ProjectDto>> {
    let store = state.projects.read().await;
    let active = store.active_id().to_string();
    let dtos = store
        .list()
        .iter()
        .map(|p| ProjectDto::new(p, &active))
        .collect();
    Json(dtos)
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

/// Query-параметры `GET /api/todos`. Если `project_id` не задан — берём
/// активный проект из `state.projects`.
#[derive(Debug, Deserialize)]
struct TodosQuery {
    #[serde(default)]
    project_id: Option<String>,
}

/// `GET /api/todos?project_id=...` — JSON-массив TODO-карточек проекта.
///
/// Если `project_id` не задан — используем активный проект. Возвращает
/// пустой массив (не 404), если в проекте нет TODO.
async fn get_todos(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<TodosQuery>,
) -> Json<Vec<crate::todos::Todo>> {
    let pid = match q.project_id.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => state.projects.read().await.active().id.clone(),
    };
    Json(state.todos.list(&pid))
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
    Json(req): Json<CreateTodoReq>,
) -> Result<(StatusCode, Json<crate::todos::Todo>), (StatusCode, String)> {
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
            Ok((StatusCode::CREATED, Json(t)))
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
    Json(req): Json<PatchTodoReq>,
) -> Result<Json<crate::todos::Todo>, (StatusCode, String)> {
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
            Ok(Json(t))
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
) -> Result<StatusCode, (StatusCode, String)> {
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
            Ok(StatusCode::NO_CONTENT)
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
    Json(req): Json<PromoteTodoReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(PLAN_MODE_SUFFIX);
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
        })));
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
    })))
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

/// Определяет путь к каталогу `static/`.
///
/// Стратегия:
/// 1. `./static` относительно текущей рабочей директории (типичный `cargo run`).
/// 2. `<binary_dir>/static` — рядом с исполняемым файлом.
/// 3. Если ни одного нет — возвращаем `./static` (axum/ServeDir отдаст 404,
///    но процесс стартует и ошибка будет видна в логах запросов).
fn resolve_static_dir() -> PathBuf {
    let cwd_static = PathBuf::from("static");
    if cwd_static.is_dir() {
        return cwd_static;
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let near_exe = dir.join("static");
            if near_exe.is_dir() {
                return near_exe;
            }
        }
    }

    cwd_static
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

