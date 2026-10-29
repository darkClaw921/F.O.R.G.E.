//! Auth-layer-compatible REST surface for local agent activity and history.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::{AgentConfig, AgentProvider, AgentQuery, AgentRoot};
use crate::AppState;

type RouteResult = Result<Response, (StatusCode, String)>;

/// Register before the host applies Bearer auth and the same-origin guard.
pub(crate) fn register_routes(app: Router<AppState>) -> Router<AppState> {
    app.route("/api/agents/status", get(status))
        .route("/api/agents/live", get(live))
        .route("/api/agents/summary", get(summary))
        .route("/api/agents/projects", get(projects))
        .route("/api/agents/sessions", get(sessions))
        .route("/api/agents/rescan", post(rescan))
        .route("/api/agents/config", get(config).put(put_config))
        .route(
            "/api/agents/bindings",
            get(bindings).post(bind).delete(unbind),
        )
        .route("/api/agents/inventory", get(inventory))
        .route("/api/agents/prices/refresh", post(refresh_prices))
}

async fn proxy(
    state: &AppState,
    query: &HashMap<String, String>,
    method: reqwest::Method,
    path: &str,
    body: Option<Bytes>,
) -> Option<RouteResult> {
    crate::try_proxy_to_remote(
        state,
        query,
        method,
        path,
        body.as_ref().map(|_| "application/json"),
        body,
        false,
    )
    .await
}
fn bad(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, error.to_string())
}
fn failed(message: &str) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, message.to_owned())
}
fn json_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, (StatusCode, String)> {
    serde_json::from_slice(body).map_err(bad)
}

fn agent_query(raw: &HashMap<String, String>) -> Result<AgentQuery, (StatusCode, String)> {
    let provider = raw
        .get("provider")
        .filter(|value| !value.is_empty())
        .map(|value| match value.as_str() {
            "codex" => Ok(AgentProvider::Codex),
            "claude" => Ok(AgentProvider::Claude),
            "opencode" => Ok(AgentProvider::Opencode),
            "copilot" => Ok(AgentProvider::Copilot),
            _ => Err(bad("unknown agent provider")),
        })
        .transpose()?;
    let parse_date = |key: &str| -> Result<Option<NaiveDate>, (StatusCode, String)> {
        raw.get(key)
            .map(|value| {
                if value.len() != 10 {
                    return Err(bad("dates must use YYYY-MM-DD"));
                }
                NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .map_err(|_| bad("invalid date; expected YYYY-MM-DD"))
            })
            .transpose()
    };
    let from = parse_date("from")?;
    let to = parse_date("to")?;
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err(bad("from must not be after to"));
        }
        if to.signed_duration_since(from).num_days() >= 3660 {
            return Err(bad("date range exceeds 3660 days"));
        }
    }
    let number = |key: &str| -> Result<Option<usize>, (StatusCode, String)> {
        raw.get(key)
            .map(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| bad(format!("{key} must be a nonnegative integer")))
            })
            .transpose()
    };
    let limit = number("limit")?;
    let offset = number("offset")?;
    if limit.is_some_and(|limit| limit == 0 || limit > 500) {
        return Err(bad("limit must be between 1 and 500"));
    }
    if offset
        .unwrap_or(0)
        .checked_add(limit.unwrap_or(200))
        .is_none()
    {
        return Err(bad("pagination offset is out of range"));
    }
    if raw
        .get("bucket")
        .is_some_and(|bucket| !matches!(bucket.as_str(), "day" | "week"))
    {
        return Err(bad("bucket must be day or week"));
    }
    Ok(AgentQuery {
        from: raw.get("from").cloned(),
        to: raw.get("to").cloned(),
        provider,
        profile: raw.get("profile").cloned(),
        project: raw.get("project").cloned(),
        limit,
        offset,
        bucket: raw.get("bucket").cloned(),
    })
}

async fn status(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/status",
        None,
    )
    .await
    {
        return response;
    }
    Ok(Json(state.agents.status().await).into_response())
}
async fn live(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) =
        proxy(&state, &raw, reqwest::Method::GET, "/api/agents/live", None).await
    {
        return response;
    }
    let query = agent_query(&raw)?;
    let mut snapshot = state.agents.snapshot().await;
    let matches = |task: &super::AgentTask| {
        query
            .provider
            .is_none_or(|provider| task.provider == provider)
            && query
                .profile
                .as_deref()
                .is_none_or(|profile| profile.is_empty() || task.profile == profile)
            && query
                .project
                .as_deref()
                .is_none_or(|project| project.is_empty() || task.project == project)
    };
    snapshot.live.retain(matches);
    snapshot.waiting.retain(matches);
    snapshot.unknown.retain(matches);
    snapshot.working_count = snapshot.live.len();
    snapshot.by_provider = BTreeMap::new();
    for task in &snapshot.live {
        *snapshot.by_provider.entry(task.provider).or_default() += 1;
    }
    Ok(Json(snapshot).into_response())
}
async fn summary(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/summary",
        None,
    )
    .await
    {
        return response;
    }
    Ok(Json(
        state
            .agents
            .summary(agent_query(&raw)?)
            .await
            .map_err(bad)?,
    )
    .into_response())
}
async fn projects(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/projects",
        None,
    )
    .await
    {
        return response;
    }
    Ok(Json(
        state
            .agents
            .projects(agent_query(&raw)?)
            .await
            .map_err(bad)?,
    )
    .into_response())
}
async fn sessions(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/sessions",
        None,
    )
    .await
    {
        return response;
    }
    Ok(Json(
        state
            .agents
            .sessions(agent_query(&raw)?)
            .await
            .map_err(bad)?,
    )
    .into_response())
}
async fn rescan(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    body: Bytes,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::POST,
        "/api/agents/rescan",
        Some(body),
    )
    .await
    {
        return response;
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(state.agents.request_rescan().await),
    )
        .into_response())
}

/// The wire/config editor intentionally cannot choose the host's write paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigDto {
    enabled: bool,
    discover_defaults: bool,
    roots: Vec<RootDto>,
    history_days: i64,
    price_updates: bool,
    price_update_url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootDto {
    provider: AgentProvider,
    path: String,
    profile: String,
}
impl From<&AgentConfig> for ConfigDto {
    fn from(config: &AgentConfig) -> Self {
        Self {
            enabled: config.enabled,
            discover_defaults: config.discover_defaults,
            roots: config
                .roots
                .iter()
                .map(|root| RootDto {
                    provider: root.provider,
                    path: root.path.to_string_lossy().into_owned(),
                    profile: root.profile.clone(),
                })
                .collect(),
            history_days: config.history_days,
            price_updates: config.price_updates,
            price_update_url: config.price_update_url.clone(),
        }
    }
}
fn absolute_path(value: &str) -> Result<PathBuf, (StatusCode, String)> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(bad("invalid agent source path"));
    }
    let path = if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or_else(|| bad("HOME is unavailable for path expansion"))?;
        if value == "~" {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(&value[2..])
        }
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err(bad("agent source paths must be absolute or start with ~/"));
    }
    Ok(path)
}
fn apply_config(
    body: ConfigDto,
    mut current: AgentConfig,
) -> Result<AgentConfig, (StatusCode, String)> {
    if !(1..=3660).contains(&body.history_days) {
        return Err(bad("historyDays must be between 1 and 3660"));
    }
    if body.roots.len() > 256 {
        return Err(bad("too many agent source roots"));
    }
    if let Some(value) = &body.price_update_url {
        if value.len() > 4096 {
            return Err(bad("invalid price catalogue URL"));
        }
        let url = reqwest::Url::parse(value).map_err(|_| bad("invalid price catalogue URL"))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(bad(
                "price catalogue URL must use HTTPS without credentials",
            ));
        }
    }
    let mut roots = Vec::new();
    for root in body.roots {
        roots.push(AgentRoot {
            provider: root.provider,
            path: absolute_path(&root.path)?,
            profile: absolute_path(&root.profile)?.to_string_lossy().into_owned(),
        });
    }
    current.enabled = body.enabled;
    current.discover_defaults = body.discover_defaults;
    current.roots = roots;
    current.history_days = body.history_days;
    current.price_updates = body.price_updates;
    current.price_update_url = body.price_update_url;
    Ok(current)
}
async fn config(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/config",
        None,
    )
    .await
    {
        return response;
    }
    Ok(Json(ConfigDto::from(&state.agents.config().await)).into_response())
}
async fn put_config(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    body: Bytes,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::PUT,
        "/api/agents/config",
        Some(body.clone()),
    )
    .await
    {
        return response;
    }
    let next = apply_config(json_body(&body)?, state.agents.config().await)?;
    state
        .agents
        .configure(next)
        .await
        .map_err(|_| failed("could not save agent configuration"))?;
    Ok(Json(ConfigDto::from(&state.agents.config().await)).into_response())
}

async fn inventory(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/inventory",
        None,
    )
    .await
    {
        return response;
    }
    Ok(Json(state.agent_runtime.inventory().await).into_response())
}
async fn bindings(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::GET,
        "/api/agents/bindings",
        None,
    )
    .await
    {
        return response;
    }
    let runtime = state.agent_runtime.state.lock().await;
    Ok(
        Json(serde_json::json!({"bindings":runtime.bindings,"inventory":runtime.inventory}))
            .into_response(),
    )
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindDto {
    session_id: String,
    pane_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnbindDto {
    session_id: String,
}
async fn bind(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    body: Bytes,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::POST,
        "/api/agents/bindings",
        Some(body.clone()),
    )
    .await
    {
        return response;
    }
    let body: BindDto = json_body(&body)?;
    Ok(Json(
        state
            .agent_runtime
            .bind(&body.session_id, &body.pane_id)
            .await
            .map_err(bad)?,
    )
    .into_response())
}
async fn unbind(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    body: Bytes,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::DELETE,
        "/api/agents/bindings",
        Some(body.clone()),
    )
    .await
    {
        return response;
    }
    let body: UnbindDto = json_body(&body)?;
    state
        .agent_runtime
        .unbind(&body.session_id)
        .await
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn refresh_prices(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    body: Bytes,
) -> RouteResult {
    if let Some(response) = proxy(
        &state,
        &raw,
        reqwest::Method::POST,
        "/api/agents/prices/refresh",
        Some(body),
    )
    .await
    {
        return response;
    }
    let status = state
        .agents
        .refresh_prices(true)
        .await
        .map_err(|_| failed("could not refresh the price catalogue"))?;
    Ok(Json(status).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request},
        middleware::{from_fn, from_fn_with_state},
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    fn fixture() -> (tempfile::TempDir, AppState) {
        let directory = tempfile::tempdir().unwrap();
        let mut state = crate::tests::test_app_state(None);
        state.agents = super::super::AgentStatsStore::new(AgentConfig {
            discover_defaults: false,
            cache_path: Some(directory.path().join("private-cache.json")),
            config_path: Some(directory.path().join("agents.toml")),
            price_path: Some(directory.path().join("private-prices.json")),
            ..Default::default()
        });
        (directory, state)
    }
    fn app(state: AppState) -> Router {
        register_routes(Router::new())
            .layer(from_fn_with_state(
                crate::auth::AuthState::new(Some("agent-test-token".into())),
                crate::auth::bearer_auth,
            ))
            .layer(from_fn(crate::auth::csrf_guard))
            .with_state(state)
    }
    fn request(method: Method, uri: &str, authorized: bool, body: Option<Value>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost:8123");
        if authorized {
            builder = builder.header("authorization", "Bearer agent-test-token");
        }
        if let Some(body) = body {
            let body = body.to_string();
            builder
                .header("content-type", "application/json")
                .header("content-length", body.len())
                .body(Body::from(body))
                .unwrap()
        } else {
            builder.body(Body::empty()).unwrap()
        }
    }
    async fn json_response(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    fn config_body() -> Value {
        json!({"enabled":true,"discoverDefaults":false,"roots":[],"historyDays":91,"priceUpdates":false,"priceUpdateUrl":null})
    }

    #[tokio::test]
    async fn every_route_group_is_protected_by_the_real_bearer_layer() {
        let (_directory, state) = fixture();
        let app = app(state);
        for (method, path, body) in [
            (Method::GET, "/api/agents/status", None),
            (Method::GET, "/api/agents/live", None),
            (Method::GET, "/api/agents/summary", None),
            (Method::GET, "/api/agents/projects", None),
            (Method::GET, "/api/agents/sessions", None),
            (Method::GET, "/api/agents/config", None),
            (Method::PUT, "/api/agents/config", Some(config_body())),
            (Method::GET, "/api/agents/bindings", None),
            (Method::GET, "/api/agents/inventory", None),
            (
                Method::POST,
                "/api/agents/bindings",
                Some(json!({"sessionId":"s","paneId":"%1"})),
            ),
            (
                Method::DELETE,
                "/api/agents/bindings",
                Some(json!({"sessionId":"s"})),
            ),
            (Method::POST, "/api/agents/rescan", Some(json!({}))),
            (Method::POST, "/api/agents/prices/refresh", Some(json!({}))),
        ] {
            let response = app
                .clone()
                .oneshot(request(method.clone(), path, false, body))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }
    #[tokio::test]
    async fn mutating_routes_reject_cross_origin_and_non_json_bodies() {
        let (_directory, state) = fixture();
        let app = app(state);
        for (method, path, body) in [
            (Method::PUT, "/api/agents/config", config_body()),
            (Method::POST, "/api/agents/rescan", json!({})),
            (
                Method::POST,
                "/api/agents/bindings",
                json!({"sessionId":"s","paneId":"%1"}),
            ),
            (
                Method::DELETE,
                "/api/agents/bindings",
                json!({"sessionId":"s"}),
            ),
            (Method::POST, "/api/agents/prices/refresh", json!({})),
        ] {
            let mut req = request(method, path, true, Some(body));
            req.headers_mut()
                .insert("origin", "https://untrusted.example".parse().unwrap());
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let mut req = request(Method::POST, "/api/agents/rescan", true, Some(json!({})));
        req.headers_mut()
            .insert("content-type", "text/plain".parse().unwrap());
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }
    #[tokio::test]
    async fn empty_local_sources_return_valid_snapshots_and_null_unknown_metrics() {
        let (_directory, state) = fixture();
        state.agents.initialize().await.unwrap();
        let app = app(state);
        let live = app
            .clone()
            .oneshot(request(Method::GET, "/api/agents/live", true, None))
            .await
            .unwrap();
        assert_eq!(live.status(), StatusCode::OK);
        let live = json_response(live).await;
        assert_eq!(live["workingCount"], 0);
        assert!(live["live"].as_array().unwrap().is_empty());
        let summary = app
            .clone()
            .oneshot(request(Method::GET, "/api/agents/summary", true, None))
            .await
            .unwrap();
        assert_eq!(summary.status(), StatusCode::OK);
        let summary = json_response(summary).await;
        assert_eq!(summary["totals"]["tasks"], 0);
        assert!(summary["totals"]["tokens"].is_null());
        assert!(summary["totals"]["estimatedCostUsd"].is_null());
        assert!(summary["totals"]["reportedCostUsd"].is_null());
        for path in ["/api/agents/projects", "/api/agents/sessions"] {
            let response = app
                .clone()
                .oneshot(request(Method::GET, path, true, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(json_response(response).await.as_array().unwrap().is_empty());
        }
        let response = app
            .oneshot(request(Method::GET, "/api/agents/inventory", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_response(response).await["available"], false);
    }
    #[tokio::test]
    async fn malformed_filters_return_400_on_the_real_history_and_live_routes() {
        let (_directory, state) = fixture();
        let app = app(state);
        for query in [
            "provider=unknown",
            "from=2026-02-30",
            "from=2026-10-26&to=2026-10-25",
            "limit=-1",
            "limit=0",
            "limit=501",
            "offset=-1",
            "offset=18446744073709551615",
            "bucket=hour",
        ] {
            for path in ["summary", "sessions", "projects", "live"] {
                let uri = format!("/api/agents/{path}?{query}");
                assert_eq!(
                    app.clone()
                        .oneshot(request(Method::GET, &uri, true, None))
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::BAD_REQUEST,
                    "{uri}"
                );
            }
        }
    }
    #[tokio::test]
    async fn real_source_tasks_obey_provider_project_and_pagination_filters() {
        let (directory, mut state) = fixture();
        let codex = directory.path().join("codex/sessions");
        let claude = directory.path().join("claude/projects");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let lines = [
            json!({"type":"session_meta","timestamp":timestamp,"payload":{"id":"code","cwd":"/code-project","source":"cli"}}),
            json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"task_started","turn_id":"turn"}}),
        ];
        std::fs::write(
            codex.join("rollout-code.jsonl"),
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        std::fs::write(claude.join("claude.jsonl"),format!("{}\n",json!({"type":"user","sessionId":"claude","timestamp":timestamp,"cwd":"/claude-project","message":{"content":"private input"}}))).unwrap();
        let mut config = state.agents.config().await;
        config.roots = vec![
            AgentRoot::new(AgentProvider::Codex, codex, directory.path().join("codex")),
            AgentRoot::new(
                AgentProvider::Claude,
                claude,
                directory.path().join("claude"),
            ),
        ];
        state.agents = super::super::AgentStatsStore::new(config);
        state.agents.initialize().await.unwrap();
        let app = app(state);
        let response = app
            .clone()
            .oneshot(request(
                Method::GET,
                "/api/agents/live?provider=codex",
                true,
                None,
            ))
            .await
            .unwrap();
        let live = json_response(response).await;
        assert_eq!(live["workingCount"], 1);
        assert_eq!(live["live"][0]["provider"], "codex");
        let response = app
            .clone()
            .oneshot(request(
                Method::GET,
                "/api/agents/live?project=%2Fclaude-project",
                true,
                None,
            ))
            .await
            .unwrap();
        let live = json_response(response).await;
        assert_eq!(live["workingCount"], 1);
        assert_eq!(live["live"][0]["provider"], "claude");
        assert!(live["live"][0]["tokens"].is_null());
        let response = app
            .oneshot(request(
                Method::GET,
                "/api/agents/sessions?limit=1&offset=1",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let tasks = json_response(response).await;
        assert_eq!(tasks.as_array().unwrap().len(), 1);
        assert!(!tasks.to_string().contains("private input"));
    }
    #[tokio::test]
    async fn config_editor_preserves_private_write_paths_and_persists_toml() {
        let (directory, state) = fixture();
        let original = state.agents.config().await;
        let app = app(state.clone());
        let response = app
            .clone()
            .oneshot(request(Method::GET, "/api/agents/config", true, None))
            .await
            .unwrap();
        let dto = json_response(response).await;
        for key in ["cachePath", "configPath", "pricePath"] {
            assert!(dto.get(key).is_none());
        }
        let mut body = config_body();
        body["historyDays"] = json!(30);
        body["roots"] = json!([{"provider":"codex","path":directory.path().join("not-yet-created/sessions"),"profile":directory.path().join("not-yet-created")}]);
        let response = app
            .clone()
            .oneshot(request(Method::PUT, "/api/agents/config", true, Some(body)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_response(response).await["historyDays"], 30);
        let current = state.agents.config().await;
        assert_eq!(current.cache_path, original.cache_path);
        assert_eq!(current.config_path, original.config_path);
        assert_eq!(current.price_path, original.price_path);
        let saved: AgentConfig =
            toml::from_str(&std::fs::read_to_string(original.config_path.unwrap()).unwrap())
                .unwrap();
        assert_eq!(saved.history_days, 30);
        assert_eq!(saved.roots.len(), 1);
        assert_eq!(saved.roots[0].provider, AgentProvider::Codex);
        let response = app
            .oneshot(request(Method::GET, "/api/agents/config", true, None))
            .await
            .unwrap();
        assert_eq!(json_response(response).await["historyDays"], 30);
    }
    #[tokio::test]
    async fn config_validation_rejects_internal_paths_relative_roots_and_insecure_urls() {
        let (_directory, state) = fixture();
        let app = app(state.clone());
        let mut payloads = Vec::new();
        for key in ["cachePath", "configPath", "pricePath"] {
            let mut body = config_body();
            body[key] = json!("/tmp/forbidden-write");
            payloads.push(body);
        }
        let mut body = config_body();
        body["roots"] =
            json!([{"provider":"codex","path":"relative/sessions","profile":"/profile"}]);
        payloads.push(body);
        let mut body = config_body();
        body["roots"] =
            json!([{"provider":"codex","path":"/sessions","profile":"relative-profile"}]);
        payloads.push(body);
        let mut body = config_body();
        body["roots"] = json!([{"provider":"unknown","path":"/sessions","profile":"/profile"}]);
        payloads.push(body);
        for url in [
            "http://example.test/prices",
            "https://user:secret@example.test/prices",
        ] {
            let mut body = config_body();
            body["priceUpdateUrl"] = json!(url);
            payloads.push(body);
        }
        for days in [0, 3661] {
            let mut body = config_body();
            body["historyDays"] = json!(days);
            payloads.push(body);
        }
        for body in payloads {
            assert_eq!(
                app.clone()
                    .oneshot(request(Method::PUT, "/api/agents/config", true, Some(body)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(state.agents.config().await.history_days, 91);
    }
    #[tokio::test]
    async fn manual_binding_errors_do_not_guess_a_process_and_delete_is_idempotent() {
        let (_directory, state) = fixture();
        let app = app(state);
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/agents/bindings",
                true,
                Some(json!({"sessionId":"missing","paneId":"%1"})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/agents/bindings",
                true,
                Some(json!({"sessionId":"missing","paneId":"%1","pid":123})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(request(Method::GET, "/api/agents/bindings", true, None))
            .await
            .unwrap();
        let value = json_response(response).await;
        assert_eq!(value["bindings"], json!([]));
        assert_eq!(value["inventory"]["available"], false);
        for _ in 0..2 {
            assert_eq!(
                app.clone()
                    .oneshot(request(
                        Method::DELETE,
                        "/api/agents/bindings",
                        true,
                        Some(json!({"sessionId":"missing"}))
                    ))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
    }
    #[tokio::test]
    async fn remote_dispatch_happens_before_local_validation() {
        let (_directory, state) = fixture();
        let app = app(state);
        let response = app
            .oneshot(request(
                Method::GET,
                "/api/agents/summary?server=other&provider=invalid",
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("remote mode disabled"));
    }
    #[tokio::test]
    async fn rescan_returns_accepted_and_price_refresh_without_source_does_not_use_network() {
        let (_directory, state) = fixture();
        let app = app(state);
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            app.clone().oneshot(request(
                Method::POST,
                "/api/agents/rescan",
                true,
                Some(json!({})),
            )),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let response = app
            .oneshot(request(
                Method::POST,
                "/api/agents/prices/refresh",
                true,
                Some(json!({})),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let prices = json_response(response).await;
        assert!(prices["models"].as_u64().unwrap() > 0);
        assert!(prices["error"].is_null());
    }
    #[tokio::test]
    async fn busy_ingestion_does_not_block_http_rescans_or_status() {
        let (_directory, state) = fixture();
        state.agents.initialize().await.unwrap();
        let guard = state.agents.test_ingestion_guard().await;
        let app = app(state.clone());
        for _ in 0..8 {
            let response = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                app.clone().oneshot(request(
                    Method::POST,
                    "/api/agents/rescan",
                    true,
                    Some(json!({})),
                )),
            )
            .await
            .expect("HTTP must not await the busy scanner")
            .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert_eq!(json_response(response).await["scanning"], true);
        }
        let response = app
            .oneshot(request(Method::GET, "/api/agents/status", true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_response(response).await["scanning"], true);
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while state.agents.status().await.scanning {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.agents.status().await.state, "ready");
    }
}
