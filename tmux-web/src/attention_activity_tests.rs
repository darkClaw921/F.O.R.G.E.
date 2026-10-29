//! Journal-derived activity and Echo idle regression scenarios.
use super::*;
use crate::agents::{
    AgentProvider, AgentSessionId, AgentSnapshot, AgentTask, AgentTaskState, RuntimeBinding,
};

fn session(name: &str, attached: u32, group: Option<&str>) -> GenSnapshot {
    GenSnapshot {
        name: name.into(),
        id: format!("${name}"),
        attached,
        session_group: group.map(str::to_owned),
        changed: false,
    }
}
fn task(native: &str, started: i64, names: &[&str]) -> AgentTask {
    let json = serde_json::json!({"type":"event_msg","timestamp":chrono::DateTime::from_timestamp_millis(started).unwrap().to_rfc3339(),"payload":{"type":"task_started","turn_id":native}});
    let events = crate::agents::providers::parse(
        AgentProvider::Codex,
        "/profile",
        native,
        json.to_string().as_bytes(),
        started,
        1,
        &mut Default::default(),
    );
    let mut task = crate::agents::index::fold(events).tasks.remove(0);
    task.session_id = AgentSessionId {
        provider: AgentProvider::Codex,
        profile: "/profile".into(),
        native_id: native.into(),
    }
    .key();
    task.bindings = names
        .iter()
        .map(|name| RuntimeBinding {
            session_id: task.session_id.clone(),
            tmux_session: (*name).into(),
            pane_id: "%1".into(),
            pid: 1,
            process_started_at: Some(1),
        })
        .collect();
    task
}
fn snapshot(tasks: Vec<AgentTask>, now: i64) -> AgentSnapshot {
    AgentSnapshot {
        working_count: tasks.len(),
        live: tasks,
        now_ms: now,
        ..Default::default()
    }
}

#[test]
fn quiet_output_keeps_original_task_clock_and_terminal_logs_do_not_count() {
    let sessions = vec![session("tab", 0, None)];
    let task = task("one", 1000, &["tab"]);
    let mut observer = AgentActivityObserver::new(0);
    let (a, _, done) = observer.update(
        &sessions,
        &snapshot(vec![task.clone()], 1200),
        &[task.clone()],
    );
    assert!(a["tab"].is_generating);
    assert!(done.is_empty());
    let (a, _, _) = observer.update(
        &sessions,
        &snapshot(vec![task.clone()], 120_000),
        &[task.clone()],
    );
    assert_eq!(a["tab"].age_secs(120_000), Some(119));
    let (a, _, done) = observer.update(&sessions, &AgentSnapshot::default(), &[]);
    assert!(!a["tab"].is_generating);
    assert_eq!(a["tab"].working_agent_count, 0);
    assert!(done.is_empty());
}

#[tokio::test]
async fn waiting_unknown_and_resume_never_create_echo_idle() {
    let state = AttentionState::new();
    let sessions = vec![session("tab", 0, None)];
    let mut task = task("one", 1000, &["tab"]);
    let mut observer = AgentActivityObserver::new(0);
    let (a, b, f) = observer.update(
        &sessions,
        &snapshot(vec![task.clone()], 2000),
        &[task.clone()],
    );
    state.apply_agent_activity(a, b, f).await;
    task.state = AgentTaskState::Waiting;
    let quiet = AgentSnapshot {
        waiting: vec![task.clone()],
        ..Default::default()
    };
    let (a, b, f) = observer.update(&sessions, &quiet, &[task.clone()]);
    state.apply_agent_activity(a, b, f).await;
    assert!(!state.agent_activity_snapshot().await["tab"].is_generating);
    assert!(state.idle_snapshot().await.is_empty());
    task.state = AgentTaskState::Unknown;
    let unknown = AgentSnapshot {
        unknown: vec![task.clone()],
        ..Default::default()
    };
    let (a, b, f) = observer.update(&sessions, &unknown, &[task.clone()]);
    state.apply_agent_activity(a, b, f).await;
    assert!(state.idle_snapshot().await.is_empty());
    task.state = AgentTaskState::Working;
    let (a, b, f) = observer.update(&sessions, &snapshot(vec![task.clone()], 3000), &[task]);
    assert_eq!(a["tab"].started_at_ms, Some(1000));
    state.apply_agent_activity(a, b, f).await;
    assert!(state.idle_snapshot().await.is_empty());
}

#[tokio::test]
async fn only_observed_explicit_completion_or_abort_sets_idle_once() {
    for completed in [true, false] {
        let state = AttentionState::new();
        let sessions = vec![session("tab", 0, None)];
        let mut task = task("one", 1000, &["tab"]);
        let mut observer = AgentActivityObserver::new(500);
        let (a, b, f) = observer.update(
            &sessions,
            &snapshot(vec![task.clone()], 2000),
            &[task.clone()],
        );
        state.apply_agent_activity(a, b, f).await;
        task.state = AgentTaskState::Ended;
        task.completed = Some(completed);
        task.ended_at_ms = Some(3000);
        let (a, b, f) = observer.update(&sessions, &AgentSnapshot::default(), &[task.clone()]);
        assert!(f.contains("tab"));
        state.apply_agent_activity(a, b, f).await;
        assert!(state.idle_snapshot().await.contains_key("tab"));
        let (_, _, f) = observer.update(&sessions, &AgentSnapshot::default(), &[task]);
        assert!(f.is_empty());
    }
}

#[tokio::test]
async fn bootstrap_replay_and_attached_completion_do_not_trigger_echo() {
    let state = AttentionState::new();
    let sessions = vec![session("tab", 1, None)];
    let mut historical = task("old", 1, &["tab"]);
    historical.completed = Some(true);
    historical.ended_at_ms = Some(10);
    historical.state = AgentTaskState::Ended;
    let mut observer = AgentActivityObserver::new(100);
    let (_, _, f) = observer.update(&sessions, &AgentSnapshot::default(), &[historical]);
    assert!(f.is_empty());
    let mut task = task("new", 1000, &["tab"]);
    let (a, b, f) = observer.update(
        &sessions,
        &snapshot(vec![task.clone()], 2000),
        &[task.clone()],
    );
    state.apply_agent_activity(a, b, f).await;
    state.update_attached(&HashSet::from(["tab".into()])).await;
    task.completed = Some(true);
    task.ended_at_ms = Some(3000);
    task.state = AgentTaskState::Ended;
    let (a, b, f) = observer.update(&sessions, &AgentSnapshot::default(), &[task]);
    state.apply_agent_activity(a, b, f).await;
    assert!(state.idle_snapshot().await.is_empty());
}

#[test]
fn linked_pane_aliases_choose_attached_primary_without_double_counting() {
    let sessions = vec![
        session("a", 0, Some("linked")),
        session("b", 1, Some("linked")),
    ];
    let task = task("one", 1000, &["a", "b"]);
    let live = snapshot(vec![task.clone()], 2000);
    let mut observer = AgentActivityObserver::new(0);
    let (a, busy, _) = observer.update(&sessions, &live, &[task]);
    assert_eq!(live.working_count, 1);
    assert!(!a["a"].is_generating);
    assert!(a["b"].is_generating);
    assert_eq!(a["b"].working_agent_count, 1);
    assert_eq!(busy.len(), 2);
}

#[test]
fn several_roots_and_unbound_same_project_are_counted_only_on_their_own_panes() {
    let sessions = vec![session("a", 0, None), session("b", 0, None)];
    let a = task("one", 1000, &["a"]);
    let b = task("two", 2000, &["a"]);
    let unbound = task("outside", 1000, &[]);
    let mut observer = AgentActivityObserver::new(0);
    let (out, _, _) = observer.update(
        &sessions,
        &snapshot(vec![a.clone(), b.clone(), unbound.clone()], 3000),
        &[a, b, unbound],
    );
    assert_eq!(out["a"].working_agent_count, 2);
    assert_eq!(out["a"].started_at_ms, Some(1000));
    assert!(!out["b"].is_generating);
}

#[tokio::test]
async fn independent_orange_attention_and_live_activity_can_coexist() {
    let state = AttentionState::new();
    state.set("tab", true).await;
    state
        .apply_agent_activity(
            HashMap::from([(
                "tab".into(),
                SessionAgentActivity {
                    is_generating: true,
                    started_at_ms: Some(1000),
                    working_agent_count: 1,
                    providers: vec![AgentProvider::Codex],
                },
            )]),
            HashSet::from(["tab".into()]),
            HashSet::new(),
        )
        .await;
    assert!(state.snapshot().await["tab"]);
    assert!(state.agent_activity_snapshot().await["tab"].is_generating);
    assert!(state.idle_snapshot().await.is_empty());
}

/// Actual journal files/SQLite -> store -> runtime projection -> tab/Echo + HTTP.
/// The fake bindings replace only OS discovery; no CLI or tmux process is run.
#[tokio::test]
async fn four_provider_replay_drives_tabs_http_and_restart_without_replaying_idle() {
    use crate::agents::{AgentConfig, AgentRoot, AgentStatsStore};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        Router,
    };
    use serde_json::{json, Value};
    use std::io::Write;
    use tower::ServiceExt;
    async fn get(store: AgentStatsStore, path: &str) -> Value {
        let mut state = crate::tests::test_app_state(None);
        state.agents = store;
        let response = crate::agents::routes::register_routes(Router::new())
            .with_state(state)
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_success());
        serde_json::from_slice(
            &to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    let append = |path: &std::path::Path, rows: Vec<Value>| {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for row in rows {
            writeln!(f, "{row}").unwrap();
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = crate::agents::store::now_ms();
    let start = now - 20_000;
    let stamp = |ms| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339()
    };
    let codex_root = root.join("codex");
    let claude_root = root.join("claude");
    let copilot_root = root.join("copilot");
    let oc_root = root.join("opencode");
    for path in [
        codex_root.join("sessions"),
        claude_root.join("projects/p"),
        copilot_root.join("session-state/cp"),
        oc_root.clone(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    let codex = codex_root.join("sessions/codex.jsonl");
    let claude = claude_root.join("projects/p/claude.jsonl");
    let copilot = copilot_root.join("session-state/cp/events.jsonl");
    append(
        &codex,
        vec![
            json!({"type":"session_meta","timestamp":stamp(start),"payload":{"id":"codex","source":"cli","cwd":"/synthetic-project"}}),
            json!({"type":"turn_context","timestamp":stamp(start),"payload":{"model":"gpt-6.1-sol"}}),
            json!({"type":"event_msg","timestamp":stamp(start),"payload":{"type":"task_started","turn_id":"codex-task"}}),
        ],
    );
    append(
        &claude,
        vec![
            json!({"type":"user","uuid":"c-user","sessionId":"claude","timestamp":stamp(start),"cwd":"/synthetic-project","message":{"content":"synthetic"}}),
        ],
    );
    append(
        &copilot,
        vec![
            json!({"type":"session.start","id":"cp-start","timestamp":stamp(start),"data":{"sessionId":"cp","selectedModel":"gpt-6.1-sol","context":{"cwd":"/synthetic-project"}}}),
            json!({"type":"user.message","id":"cp-user","timestamp":stamp(start),"data":{}}),
            json!({"type":"assistant.turn_start","id":"cp-turn1","timestamp":stamp(start),"data":{"turnId":"iteration-1"}}),
        ],
    );
    let db = rusqlite::Connection::open(oc_root.join("opencode.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT,parent_id TEXT); CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT); CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,data TEXT); INSERT INTO session VALUES('oc','/synthetic-project',NULL);").unwrap();
    db.execute(
        "INSERT INTO message VALUES('u','oc',?1,?1,?2)",
        rusqlite::params![start, json!({"role":"user"}).to_string()],
    )
    .unwrap();
    let config = AgentConfig {
        discover_defaults: false,
        cache_path: Some(root.join("agent-cache.json")),
        roots: vec![
            AgentRoot::new(
                AgentProvider::Codex,
                codex_root.join("sessions"),
                codex_root,
            ),
            AgentRoot::new(
                AgentProvider::Claude,
                claude_root.join("projects"),
                claude_root,
            ),
            AgentRoot::new(
                AgentProvider::Copilot,
                copilot_root.join("session-state"),
                copilot_root,
            ),
            AgentRoot::new(AgentProvider::Opencode, oc_root.clone(), oc_root),
        ],
        ..Default::default()
    };
    let store = AgentStatsStore::new(config.clone());
    store.initialize().await.unwrap();
    let live = store.snapshot().await;
    assert_eq!(live.working_count, 4);
    let bindings: Vec<RuntimeBinding> = live
        .live
        .iter()
        .enumerate()
        .flat_map(|(i, task)| {
            ["tab", "mirror"]
                .into_iter()
                .map(move |name| RuntimeBinding {
                    session_id: task.session_id.clone(),
                    tmux_session: name.into(),
                    pane_id: format!("%{}", i + 1),
                    pid: (i + 1) as u32,
                    process_started_at: Some(1),
                })
        })
        .collect();
    store.set_runtime(bindings.clone()).await;
    let sessions = vec![
        session("tab", 0, Some("linked")),
        session("mirror", 0, Some("linked")),
    ];
    let state = AttentionState::new();
    let mut observer = AgentActivityObserver::new(start - 1000);
    let live = store.snapshot().await;
    let recent = store.recent_tasks(start - 1000).await;
    let (activity, busy, finished) = observer.update(&sessions, &live, &recent);
    assert_eq!(activity["tab"].working_agent_count, 4);
    assert_eq!(activity["tab"].providers.len(), 4);
    assert!(!activity["mirror"].is_generating);
    state.apply_agent_activity(activity, busy, finished).await;
    assert!(state.idle_snapshot().await.is_empty());
    assert_eq!(
        get(store.clone(), "/api/agents/live").await["workingCount"],
        4
    );
    let step = start + 5000;
    append(
        &codex,
        vec![
            json!({"type":"token_usage_record","timestamp":stamp(step),"payload":{"response_id":"codex-response","usage":{"input_tokens":100,"output_tokens":20}}}),
        ],
    );
    append(
        &claude,
        vec![
            json!({"type":"assistant","uuid":"c-tool","sessionId":"claude","timestamp":stamp(step),"message":{"id":"c-tool-response","model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":20,"output_tokens":5},"content":[{"type":"tool_use","name":"Bash","id":"bash-1"}]}}),
            json!({"type":"user","uuid":"c-result","sessionId":"claude","timestamp":stamp(step+1000),"message":{"content":[{"type":"tool_result","tool_use_id":"bash-1","content":"synthetic"}]}}),
        ],
    );
    append(
        &copilot,
        vec![
            json!({"type":"assistant.message","id":"cp-tool","timestamp":stamp(step),"data":{"apiCallId":"cp-call1","model":"gpt-6.1-sol","phase":"commentary","toolRequests":[{"id":"tool"}]}}),
            json!({"type":"assistant.turn_end","id":"cp-tool-end","timestamp":stamp(step+1000),"data":{"turnId":"iteration-1"}}),
        ],
    );
    db.execute("INSERT INTO message VALUES('tool','oc',?1,?1,?2)",rusqlite::params![step,json!({"role":"assistant","parentID":"u","modelID":"gpt-6.1-sol","finish":"tool-calls","time":{"completed":step},"tokens":{"input":5,"output":2}}).to_string()]).unwrap();
    store.refresh(false).await.unwrap();
    let live = store.snapshot().await;
    assert_eq!(
        live.working_count, 4,
        "tool iterations must not end root tasks"
    );
    let (activity, busy, finished) =
        observer.update(&sessions, &live, &store.recent_tasks(start - 1000).await);
    assert!(finished.is_empty());
    assert_eq!(activity["tab"].started_at_ms, Some(start));
    state.apply_agent_activity(activity, busy, finished).await;
    assert!(state.idle_snapshot().await.is_empty());
    let end = start + 15_000;
    append(
        &codex,
        vec![
            json!({"type":"event_msg","timestamp":stamp(end),"payload":{"type":"turn_aborted","duration_ms":15000}}),
        ],
    );
    append(
        &claude,
        vec![
            json!({"type":"assistant","uuid":"c-final","sessionId":"claude","timestamp":stamp(end),"message":{"id":"c-final-response","model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":3}}}),
        ],
    );
    append(
        &copilot,
        vec![
            json!({"type":"assistant.turn_start","id":"cp-turn2","timestamp":stamp(end-1000),"data":{"turnId":"iteration-2"}}),
            json!({"type":"assistant.message","id":"cp-final","timestamp":stamp(end),"data":{"apiCallId":"cp-call2","model":"gpt-6.1-sol","phase":"final"}}),
            json!({"type":"assistant.turn_end","id":"cp-end","timestamp":stamp(end),"data":{"turnId":"iteration-2"}}),
            json!({"type":"session.shutdown","id":"cp-shutdown","timestamp":stamp(end),"data":{"modelMetrics":{"gpt-6.1-sol":{"requests":{"count":2},"usage":{"inputTokens":30,"outputTokens":10}}}}}),
        ],
    );
    db.execute("INSERT INTO message VALUES('final','oc',?1,?1,?2)",rusqlite::params![end,json!({"role":"assistant","parentID":"u","modelID":"gpt-6.1-sol","finish":"stop","time":{"completed":end},"tokens":{"input":9,"output":3}}).to_string()]).unwrap();
    store.refresh(false).await.unwrap();
    assert_eq!(store.snapshot().await.working_count, 0);
    let (activity, busy, finished) = observer.update(
        &sessions,
        &store.snapshot().await,
        &store.recent_tasks(start - 1000).await,
    );
    assert_eq!(finished, HashSet::from(["tab".into()]));
    state.apply_agent_activity(activity, busy, finished).await;
    assert!(state.idle_snapshot().await.contains_key("tab"));
    let today = chrono::Local::now().date_naive();
    let url = format!(
        "/api/agents/summary?from={}&to={}",
        today - chrono::Days::new(1),
        today + chrono::Days::new(1)
    );
    let summary = get(store.clone(), &url).await;
    assert_eq!(summary["totals"]["aborted"], 1);
    assert_eq!(summary["byProvider"].as_object().unwrap().len(), 4);
    assert!(summary["totals"]["tokens"]["output"].as_u64().unwrap() > 0);
    assert_eq!(
        get(store.clone(), "/api/agents/live").await["workingCount"],
        0
    );
    store.flush().await.unwrap();
    let restarted = AgentStatsStore::new(config);
    restarted.initialize().await.unwrap();
    assert_eq!(
        get(restarted.clone(), &url).await["totals"],
        summary["totals"]
    );
    let fresh_state = AttentionState::new();
    let mut fresh = AgentActivityObserver::new(crate::agents::store::now_ms());
    let (activity, busy, finished) = fresh.update(
        &sessions,
        &restarted.snapshot().await,
        &restarted.recent_tasks(start - 1000).await,
    );
    assert!(finished.is_empty());
    fresh_state
        .apply_agent_activity(activity, busy, finished)
        .await;
    assert!(fresh_state.idle_snapshot().await.is_empty());

    // A confirmed killed Claude process must not finish the task for Echo or
    // preserve its old clock when the same transcript is resumed shortly after.
    let second_start = crate::agents::store::now_ms() - 1000;
    append(
        &claude,
        vec![
            json!({"type":"user","uuid":"c-after-complete","sessionId":"claude","timestamp":stamp(second_start),"message":{"content":"synthetic next task"}}),
        ],
    );
    restarted.refresh(false).await.unwrap();
    restarted.set_runtime(bindings.clone()).await;
    let killed = restarted
        .snapshot()
        .await
        .live
        .into_iter()
        .find(|t| t.provider == AgentProvider::Claude)
        .unwrap();
    let mut kill_observer = AgentActivityObserver::new(second_start - 1);
    let kill_state = AttentionState::new();
    let (a, b, f) = kill_observer.update(
        &sessions,
        &restarted.snapshot().await,
        &restarted.recent_tasks(start - 1000).await,
    );
    kill_state.apply_agent_activity(a, b, f).await;
    restarted
        .update_runtime(
            bindings.clone(),
            vec![crate::agents::ProcessObservation {
                session_id: killed.session_id.clone(),
                running: Some(false),
            }],
            Some(true),
        )
        .await;
    assert_eq!(restarted.snapshot().await.working_count, 0);
    let (a, b, f) = kill_observer.update(
        &sessions,
        &restarted.snapshot().await,
        &restarted.recent_tasks(start - 1000).await,
    );
    assert!(f.is_empty());
    kill_state.apply_agent_activity(a, b, f).await;
    assert!(kill_state.idle_snapshot().await.is_empty());
    let resumed_at = crate::agents::store::now_ms() + 10;
    append(
        &claude,
        vec![
            json!({"type":"user","uuid":"c-resumed-after-kill","sessionId":"claude","timestamp":stamp(resumed_at),"message":{"content":"synthetic resumed task"}}),
        ],
    );
    restarted.refresh(false).await.unwrap();
    let resumed = restarted
        .snapshot()
        .await
        .live
        .into_iter()
        .find(|t| t.provider == AgentProvider::Claude)
        .unwrap();
    assert_ne!(resumed.id, killed.id);
    assert_eq!(resumed.started_at_ms, resumed_at);
    let (a, b, f) = kill_observer.update(
        &sessions,
        &restarted.snapshot().await,
        &restarted.recent_tasks(start - 1000).await,
    );
    assert!(a["tab"].is_generating);
    assert_eq!(a["tab"].started_at_ms, Some(resumed_at));
    assert!(f.is_empty());
    kill_state.apply_agent_activity(a, b, f).await;
    append(
        &claude,
        vec![
            json!({"type":"assistant","uuid":"c-resumed-end","sessionId":"claude","timestamp":stamp(resumed_at+1000),"message":{"id":"c-resumed-final-response","model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}}),
        ],
    );
    restarted.refresh(false).await.unwrap();
    let (a, b, f) = kill_observer.update(
        &sessions,
        &restarted.snapshot().await,
        &restarted.recent_tasks(start - 1000).await,
    );
    assert_eq!(f, HashSet::from(["tab".into()]));
    kill_state.apply_agent_activity(a, b, f).await;
    assert!(kill_state.idle_snapshot().await.contains_key("tab"));
}
