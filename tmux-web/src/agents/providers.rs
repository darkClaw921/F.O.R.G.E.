//! Metadata-only JSONL adapters for Codex, Claude Code and GitHub Copilot.
//!
//! Parsers emit journal facts; the store owns quiet timeouts and live process
//! validation. State is serializable so an append can resume without replaying
//! private conversation content. Native request IDs survive copied/archived logs.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{AgentEvent, AgentEventKind, AgentProvider, AgentSessionId, AgentTokens};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParserState {
    pub session: String,
    pub project: String,
    pub model: String,
    pub parent: Option<String>,
    pub turn_open: bool,
    pub fast: bool,
    pub running_commands: BTreeSet<String>,
    codex_header_read: bool,
    codex_totals_session: String,
    codex_totals: BTreeMap<String, AgentTokens>,
    codex_native_usage: BTreeSet<String>,
    copilot_turn_id: Option<String>,
    copilot_final: bool,
    copilot_request_models: BTreeMap<String, String>,
    copilot_requests: BTreeMap<String, u64>,
    copilot_reported_requests: BTreeMap<String, u64>,
    copilot_totals: BTreeMap<String, AgentTokens>,
}

struct Input<'a> {
    provider: AgentProvider,
    profile: &'a str,
    fallback: &'a str,
    sequence: u64,
    at_ms: i64,
    key: String,
}

impl Input<'_> {
    fn event(&self, state: &ParserState, suffix: &str, kind: AgentEventKind) -> AgentEvent {
        let native = if state.session.is_empty() {
            self.fallback
        } else {
            &state.session
        };
        AgentEvent {
            id: format!("{}:{}", self.key, suffix),
            at_ms: self.at_ms,
            sequence: self.sequence,
            session: self.identity(native),
            parent: state.parent.as_deref().map(|id| self.identity(id)),
            project: state.project.clone(),
            model: state.model.clone(),
            kind,
        }
    }

    fn identity(&self, native: &str) -> AgentSessionId {
        AgentSessionId {
            provider: self.provider,
            profile: self.profile.into(),
            native_id: native.into(),
        }
    }

    fn start(&self, state: &ParserState) -> AgentEvent {
        self.event(
            state,
            "start",
            AgentEventKind::Started {
                task_id: self.key.clone(),
            },
        )
    }
}

/// Parse one complete line. Invalid or unsupported records do not change state.
/// `modified_ms` is the file's timestamp, never the current wall clock, so old
/// untimestamped history cannot become a newly working task during a rescan.
pub fn parse(
    provider: AgentProvider,
    profile: &str,
    fallback_session: &str,
    line: &[u8],
    modified_ms: i64,
    sequence: u64,
    state: &mut ParserState,
) -> Vec<AgentEvent> {
    let Ok(json) = serde_json::from_slice::<Value>(line) else {
        return Vec::new();
    };
    let Some(kind) = string(&json, "type") else {
        return Vec::new();
    };
    let recorded_at = timestamp(json.get("timestamp"));
    let at_ms = recorded_at.unwrap_or(modified_ms);
    let key = string(&json, "uuid")
        .or_else(|| string(&json, "id"))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if recorded_at.is_some() {
                format!("{}:{kind}:{at_ms}", provider.slug())
            } else {
                format!("{}:{kind}:{at_ms}:{sequence}", provider.slug())
            }
        });
    let input = Input {
        provider,
        profile,
        fallback: fallback_session,
        sequence,
        at_ms,
        key,
    };
    match provider {
        AgentProvider::Codex => codex(&json, kind, &input, state),
        AgentProvider::Claude => claude(&json, kind, &input, state),
        AgentProvider::Copilot => copilot(&json, kind, &input, state),
        AgentProvider::Opencode => Vec::new(),
    }
}

fn string<'a>(json: &'a Value, key: &str) -> Option<&'a str> {
    json.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn count(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}

fn flag(json: &Value, key: &str) -> bool {
    json.get(key).and_then(Value::as_bool) == Some(true)
}

fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(s) = value.as_str() {
        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(date.timestamp_millis());
        }
        if let Ok(number) = s.parse::<f64>() {
            return numeric_timestamp(number);
        }
    }
    value.as_f64().and_then(numeric_timestamp)
}

fn numeric_timestamp(number: f64) -> Option<i64> {
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    // CLI epoch times are seconds or milliseconds; their magnitudes differ.
    let millis = if number < 100_000_000_000.0 {
        number * 1000.0
    } else {
        number
    };
    (millis <= i64::MAX as f64).then_some(millis as i64)
}

fn nonnegative_cost(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

fn canonical_project(value: &str) -> String {
    std::fs::canonicalize(value)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| value.into())
}

fn set_context(json: &Value, state: &mut ParserState, session_key: &str) {
    if let Some(id) = string(json, session_key) {
        state.session = id.into();
    }
    if let Some(cwd) = string(json, "cwd") {
        state.project = canonical_project(cwd);
    }
}

fn usage(
    input: &Input<'_>,
    state: &ParserState,
    response_id: String,
    tokens: Option<AgentTokens>,
    requests: u64,
    reported_cost_usd: Option<f64>,
    cumulative: bool,
) -> AgentEvent {
    // Streaming chunks may have different log UUIDs. Record identity is the
    // response ID, while event identity also preserves later metadata updates.
    input.event(
        state,
        &format!("usage:{response_id}"),
        AgentEventKind::Usage {
            response_id,
            tokens,
            requests,
            reported_cost_usd,
            fast: state.fast,
            cumulative,
            cache_write_1h: None,
        },
    )
}

fn fast_tier(tier: &str) -> bool {
    matches!(tier, "fast" | "priority")
}

fn codex_tokens(value: &Value) -> AgentTokens {
    let input = count(value.get("input_tokens"));
    let cache_read = count(value.get("cached_input_tokens")).min(input);
    let cache_write =
        count(value.get("cache_write_input_tokens")).min(input.saturating_sub(cache_read));
    AgentTokens {
        input: input.saturating_sub(cache_read).saturating_sub(cache_write),
        output: count(value.get("output_tokens")),
        cache_read,
        cache_write,
        reasoning: value.get("reasoning_output_tokens").and_then(Value::as_u64),
    }
}

fn nested_parent(value: &Value, depth: u8) -> Option<&str> {
    if depth == 0 {
        return None;
    }
    for key in ["session_id", "root_thread_id", "parent_thread_id"] {
        if let Some(id) = string(value, key) {
            return Some(id);
        }
    }
    value
        .as_object()?
        .values()
        .find_map(|child| nested_parent(child, depth - 1))
}

fn codex(json: &Value, kind: &str, input: &Input<'_>, state: &mut ParserState) -> Vec<AgentEvent> {
    let Some(payload) = json.get("payload").filter(|p| p.is_object()) else {
        return Vec::new();
    };
    match kind {
        "session_meta" => {
            if let Some(id) = string(payload, "id") {
                state.codex_totals_session = id.into();
            }
            if state.codex_header_read {
                return Vec::new();
            }
            state.codex_header_read = true;
            set_context(payload, state, "id");
            let source = payload
                .get("source")
                .or_else(|| payload.get("thread_source"));
            let child = source.is_some_and(|source| {
                source.get("subagent").is_some()
                    || source.get("internal").is_some()
                    || matches!(source.as_str(), Some("subagent" | "internal"))
            });
            if child {
                state.parent = nested_parent(payload, 1)
                    .or_else(|| source.and_then(|s| nested_parent(s, 5)))
                    .filter(|id| *id != state.session)
                    .map(str::to_owned)
                    .or_else(|| Some("__internal".into()));
            }
            vec![input.event(state, "context", AgentEventKind::Context)]
        }
        "turn_context" => {
            if let Some(model) = string(payload, "model") {
                state.model = model.into();
            }
            if let Some(cwd) = string(payload, "cwd") {
                state.project = canonical_project(cwd);
            }
            if let Some(tier) = string(payload, "service_tier") {
                state.fast = fast_tier(tier);
            }
            vec![input.event(state, "context", AgentEventKind::Context)]
        }
        "token_usage_record" => {
            let Some(tokens) = payload.get("usage").filter(|v| v.is_object()) else {
                return Vec::new();
            };
            if state.session.is_empty() {
                if let Some(id) = string(payload, "session_id") {
                    state.session = id.into();
                }
            }
            // Native ownership follows this response or the log's own thread.
            // A copied header changes only the cumulative-total namespace.
            let native_session = string(payload, "session_id")
                .unwrap_or_else(|| {
                    if state.session.is_empty() {
                        input.fallback
                    } else {
                        &state.session
                    }
                })
                .to_owned();
            state.codex_native_usage.insert(native_session.clone());
            let response = string(payload, "response_id")
                .map(str::to_owned)
                .unwrap_or_else(|| format!("fallback:{native_session}:{}", input.at_ms));
            vec![usage(
                input,
                state,
                format!("native:{native_session}:{response}"),
                Some(codex_tokens(tokens)),
                1,
                nonnegative_cost(payload.get("cost_usd")),
                false,
            )]
        }
        "event_msg" => match string(payload, "type").unwrap_or("") {
            "task_started" => {
                state.turn_open = true;
                let mut event = input.start(state);
                event.at_ms = timestamp(payload.get("started_at")).unwrap_or(input.at_ms);
                if let AgentEventKind::Started { task_id } = &mut event.kind {
                    *task_id = string(payload, "turn_id")
                        .or_else(|| string(payload, "task_id"))
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("codex:{}:{}", event.at_ms, input.sequence));
                    event.id = format!("{}:start:{task_id}", input.key);
                }
                vec![event]
            }
            "task_complete" | "turn_aborted" => {
                let was_open = std::mem::replace(&mut state.turn_open, false);
                if !was_open {
                    return Vec::new();
                }
                let mut event = input.event(
                    state,
                    "end",
                    AgentEventKind::Ended {
                        completed: string(payload, "type") == Some("task_complete"),
                        duration_ms: payload.get("duration_ms").and_then(Value::as_u64),
                    },
                );
                event.at_ms = timestamp(payload.get("completed_at")).unwrap_or(input.at_ms);
                vec![event]
            }
            "thread_settings_applied" => {
                if let Some(tier) = payload
                    .get("thread_settings")
                    .and_then(|v| string(v, "service_tier"))
                {
                    state.fast = fast_tier(tier);
                }
                Vec::new()
            }
            "token_count" => {
                let Some(info) = payload.get("info") else {
                    return Vec::new();
                };
                let Some(total_value) = info.get("total_token_usage").filter(|v| v.is_object())
                else {
                    return Vec::new();
                };
                let namespace = if state.codex_totals_session.is_empty() {
                    if state.session.is_empty() {
                        input.fallback.to_owned()
                    } else {
                        state.session.clone()
                    }
                } else {
                    state.codex_totals_session.clone()
                };
                let total = codex_tokens(total_value);
                let previous = state
                    .codex_totals
                    .insert(namespace.clone(), total.clone())
                    .unwrap_or_default();
                if !state.codex_native_usage.is_empty() || total.total() <= previous.total() {
                    return Vec::new();
                }
                let delta = info
                    .get("last_token_usage")
                    .filter(|v| v.is_object())
                    .map(codex_tokens)
                    .unwrap_or_else(|| total.difference(&previous));
                let response = format!("cumulative:{namespace}:{}", total.total());
                vec![usage(input, state, response, Some(delta), 1, None, true)]
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn own_text_ends(content: Option<&Value>) -> bool {
    fn ends(text: &str) -> bool {
        text.starts_with("[Request interrupted by user") || text.starts_with("<local-command-std")
    }
    match content {
        Some(Value::String(text)) => ends(text),
        Some(Value::Array(blocks)) => blocks.iter().any(|block| {
            string(block, "type") == Some("text") && string(block, "text").is_some_and(ends)
        }),
        _ => false,
    }
}

fn command_ends(input: &Input<'_>, state: &mut ParserState) -> Vec<AgentEvent> {
    let commands = std::mem::take(&mut state.running_commands);
    commands
        .into_iter()
        .map(|id| {
            input.event(
                state,
                &format!("command-end:{id}"),
                AgentEventKind::Command { id, running: false },
            )
        })
        .collect()
}

fn claude(json: &Value, kind: &str, input: &Input<'_>, state: &mut ParserState) -> Vec<AgentEvent> {
    if !matches!(kind, "user" | "assistant") {
        return Vec::new();
    }
    let sidechain = flag(json, "isSidechain");
    if !sidechain {
        set_context(json, state, "sessionId");
    }
    let content = json
        .get("message")
        .and_then(|message| message.get("content"));
    if kind == "user" {
        if sidechain {
            return Vec::new();
        }
        if own_text_ends(content) || flag(json, "toolEndsTurn") {
            let open = std::mem::replace(&mut state.turn_open, false);
            let mut events = command_ends(input, state);
            if open {
                events.push(input.event(
                    state,
                    "end",
                    AgentEventKind::Ended {
                        completed: flag(json, "toolEndsTurn") && !own_text_ends(content),
                        duration_ms: None,
                    },
                ));
            }
            return events;
        }
        if flag(json, "isMeta") {
            return Vec::new();
        }
        let results: Vec<&Value> = content
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|block| string(block, "type") == Some("tool_result"))
            .collect();
        if state.turn_open {
            let mut events = Vec::new();
            if results.is_empty() {
                events.extend(command_ends(input, state));
            } else {
                for result in results {
                    if let Some(id) = string(result, "tool_use_id") {
                        if state.running_commands.remove(id) {
                            events.push(input.event(
                                state,
                                &format!("command-end:{id}"),
                                AgentEventKind::Command {
                                    id: id.into(),
                                    running: false,
                                },
                            ));
                        }
                    }
                }
            }
            events.push(input.event(state, "active", AgentEventKind::Active));
            return events;
        }
        // A tool result by itself is never a new human request.
        if !results.is_empty() {
            return Vec::new();
        }
        state.turn_open = true;
        let mut events = command_ends(input, state);
        events.push(input.start(state));
        return events;
    }

    let Some(message) = json.get("message").filter(|v| v.is_object()) else {
        return Vec::new();
    };
    let model = string(message, "model").unwrap_or("");
    if !sidechain && !model.is_empty() {
        state.model = model.into();
    }
    let mut events = Vec::new();
    let final_reply = matches!(
        string(message, "stop_reason"),
        Some("end_turn" | "stop_sequence" | "max_tokens" | "refusal")
    );
    if !sidechain && !final_reply {
        events.push(if state.turn_open {
            input.event(state, "active", AgentEventKind::Active)
        } else {
            input.start(state)
        });
        state.turn_open = true;
    }
    if !model.is_empty() && !model.starts_with('<') {
        if let Some(counters) = message.get("usage").filter(|v| v.is_object()) {
            let response = string(message, "id").unwrap_or("");
            let request = string(json, "requestId").unwrap_or("");
            let response_id = if response.is_empty() && request.is_empty() {
                format!(
                    "claude:{}:{}",
                    if state.session.is_empty() {
                        input.fallback
                    } else {
                        &state.session
                    },
                    input.at_ms
                )
            } else {
                format!("claude:{response}:{request}")
            };
            let tokens = AgentTokens {
                input: count(counters.get("input_tokens")),
                output: count(counters.get("output_tokens")),
                cache_read: count(counters.get("cache_read_input_tokens")),
                cache_write: count(counters.get("cache_creation_input_tokens")),
                reasoning: counters
                    .get("output_tokens_details")
                    .and_then(|v| v.get("thinking_tokens"))
                    .and_then(Value::as_u64),
            };
            let mut event = usage(
                input,
                state,
                response_id,
                Some(tokens),
                1,
                nonnegative_cost(counters.get("cost_usd")),
                false,
            );
            event.model = model.into();
            if let AgentEventKind::Usage {
                fast,
                cache_write_1h,
                ..
            } = &mut event.kind
            {
                *fast = string(counters, "speed") == Some("fast");
                *cache_write_1h = counters
                    .get("cache_creation")
                    .filter(|v| v.is_object())
                    .map(|v| count(v.get("ephemeral_1h_input_tokens")));
            }
            if sidechain {
                let parent = string(json, "sessionId").unwrap_or_else(|| {
                    if state.session.is_empty() {
                        input.fallback
                    } else {
                        &state.session
                    }
                });
                let child = string(json, "agentId").unwrap_or(input.fallback);
                event.session = input.identity(&format!("sidechain:{child}"));
                event.parent = Some(input.identity(parent));
                if let Some(cwd) = string(json, "cwd") {
                    event.project = canonical_project(cwd);
                }
            }
            events.push(event);
        }
    }
    if sidechain {
        return events;
    }
    if final_reply {
        events.extend(command_ends(input, state));
        if std::mem::replace(&mut state.turn_open, false) {
            events.push(input.event(
                state,
                "end",
                AgentEventKind::Ended {
                    completed: !flag(json, "isApiErrorMessage") && !model.starts_with('<'),
                    duration_ms: None,
                },
            ));
        }
    } else {
        for block in content.and_then(Value::as_array).into_iter().flatten() {
            if string(block, "type") == Some("tool_use") && string(block, "name") == Some("Bash") {
                if let Some(id) = string(block, "id") {
                    if state.running_commands.insert(id.into()) {
                        events.push(input.event(
                            state,
                            &format!("command-start:{id}"),
                            AgentEventKind::Command {
                                id: id.into(),
                                running: true,
                            },
                        ));
                    }
                }
            }
        }
    }
    events
}

fn copilot_project(json: &Value) -> Option<&str> {
    ["gitRoot", "cwd", "repository"]
        .iter()
        .find_map(|key| string(json, key))
}

fn copilot(
    json: &Value,
    kind: &str,
    input: &Input<'_>,
    state: &mut ParserState,
) -> Vec<AgentEvent> {
    let Some(data) = json.get("data").filter(|v| v.is_object()) else {
        return Vec::new();
    };
    let agent = string(json, "agentId");
    if agent.is_some() && kind != "assistant.message" {
        return Vec::new();
    }
    match kind {
        "session.start" => {
            if let Some(id) = string(data, "sessionId") {
                state.session = id.into();
            }
            if let Some(model) = string(data, "selectedModel") {
                state.model = model.into();
            }
            if let Some(path) = data.get("context").and_then(copilot_project) {
                state.project = canonical_project(path);
            }
            vec![input.event(state, "context", AgentEventKind::Context)]
        }
        "session.context_changed" => {
            if let Some(path) = copilot_project(data) {
                state.project = canonical_project(path);
            }
            vec![input.event(state, "context", AgentEventKind::Context)]
        }
        "session.model_change" => {
            if let Some(model) = string(data, "newModel") {
                state.model = model.into();
            }
            vec![input.event(state, "context", AgentEventKind::Context)]
        }
        "user.message" | "assistant.turn_start" => {
            state.copilot_final = false;
            state.copilot_turn_id = if kind == "assistant.turn_start" {
                string(data, "turnId").map(str::to_owned)
            } else {
                None
            };
            if let Some(model) = string(data, "model") {
                state.model = model.into();
            }
            let event = if state.turn_open {
                input.event(state, "active", AgentEventKind::Active)
            } else {
                input.start(state)
            };
            state.turn_open = true;
            vec![event]
        }
        "assistant.message" => {
            let model = string(data, "model")
                .unwrap_or_else(|| if agent.is_none() { &state.model } else { "" })
                .to_owned();
            let scope = agent
                .map(|s| format!("agent:{s}"))
                .unwrap_or_else(|| "root".into());
            let call = string(data, "apiCallId")
                .map(|id| format!("api:{id}"))
                .unwrap_or_else(|| {
                    format!(
                        "message:{}",
                        string(data, "messageId").unwrap_or(&input.key)
                    )
                });
            let request = format!("{scope}:{call}");
            let mut events = Vec::new();
            let old = state.copilot_request_models.get(&request).cloned();
            if old.is_none() || old.as_deref() == Some("") && !model.is_empty() {
                if let Some(old) = old {
                    let count = state.copilot_requests.entry(old).or_default();
                    *count = count.saturating_sub(1);
                }
                state
                    .copilot_request_models
                    .insert(request.clone(), model.clone());
                *state.copilot_requests.entry(model.clone()).or_default() += 1;
                let native = if state.session.is_empty() {
                    input.fallback
                } else {
                    &state.session
                };
                let mut event = usage(
                    input,
                    state,
                    format!("copilot:{native}:{request}:activity"),
                    None,
                    1,
                    None,
                    false,
                );
                event.model = model.clone();
                events.push(event);
            }
            if agent.is_none() {
                state.model = model;
                state.copilot_final = !data
                    .get("toolRequests")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                    && !matches!(string(data, "phase"), Some("thinking" | "commentary"));
            }
            // A child reply is activity of the root's currently open task,
            // while the child's lifecycle cannot close or reopen that task.
            if state.turn_open {
                events.push(input.event(state, "active", AgentEventKind::Active));
            }
            events
        }
        "assistant.turn_end" => {
            if !state.turn_open
                || state
                    .copilot_turn_id
                    .as_deref()
                    .is_some_and(|id| Some(id) != string(data, "turnId"))
            {
                return Vec::new();
            }
            if !state.copilot_final {
                return vec![input.event(state, "active", AgentEventKind::Active)];
            }
            state.turn_open = false;
            state.copilot_final = false;
            vec![input.event(
                state,
                "end",
                AgentEventKind::Ended {
                    completed: true,
                    duration_ms: None,
                },
            )]
        }
        "abort" => {
            state.copilot_final = false;
            if !std::mem::replace(&mut state.turn_open, false) {
                return Vec::new();
            }
            vec![input.event(
                state,
                "end",
                AgentEventKind::Ended {
                    completed: false,
                    duration_ms: None,
                },
            )]
        }
        "session.shutdown" => {
            let mut events = copilot_shutdown(data, input, state);
            state.copilot_final = false;
            if std::mem::replace(&mut state.turn_open, false) {
                events.push(input.event(
                    state,
                    "end",
                    AgentEventKind::Ended {
                        completed: false,
                        duration_ms: None,
                    },
                ));
            }
            events
        }
        _ => Vec::new(),
    }
}

fn copilot_tokens(metric: &Value) -> Option<AgentTokens> {
    let details = metric.get("tokenDetails");
    let counters = metric.get("usage");
    if !details.is_some_and(Value::is_object) && !counters.is_some_and(Value::is_object) {
        return None;
    }
    let read = |detail: &str, fallback: &str| {
        details
            .and_then(|v| v.get(detail))
            .and_then(|v| v.get("tokenCount"))
            .and_then(Value::as_u64)
            .or_else(|| {
                counters
                    .and_then(|v| v.get(fallback))
                    .and_then(Value::as_u64)
            })
            .unwrap_or(0)
    };
    Some(AgentTokens {
        input: read("input", "inputTokens"),
        output: read("output", "outputTokens"),
        cache_read: read("cache_read", "cacheReadTokens"),
        cache_write: read("cache_write", "cacheWriteTokens"),
        reasoning: counters
            .and_then(|v| v.get("reasoningTokens"))
            .and_then(Value::as_u64),
    })
}

fn copilot_shutdown(data: &Value, input: &Input<'_>, state: &mut ParserState) -> Vec<AgentEvent> {
    let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut growth = BTreeMap::new();
    for (model, metric) in metrics {
        if !metric.is_object() {
            continue;
        }
        let tokens = copilot_tokens(metric);
        if let Some(total) = tokens {
            let previous = state.copilot_totals.insert(model.clone(), total.clone());
            let delta = match previous {
                Some(previous) if total.total() >= previous.total() => total.difference(&previous),
                _ => total.clone(),
            };
            let reported = count(metric.get("requests").and_then(|v| v.get("count")))
                .max(u64::from(total.total() > 0));
            let previous = state
                .copilot_reported_requests
                .entry(model.clone())
                .or_default();
            *previous = (*previous).max(reported);
            growth.insert(model.clone(), Some(delta));
        } else {
            let reported = count(metric.get("requests").and_then(|v| v.get("count")));
            let previous = state
                .copilot_reported_requests
                .entry(model.clone())
                .or_default();
            *previous = (*previous).max(reported);
            growth.insert(model.clone(), None);
        }
    }
    let unresolved = state.copilot_requests.get("").copied().unwrap_or(0);
    let deficits: BTreeMap<String, u64> = state
        .copilot_reported_requests
        .iter()
        .map(|(model, n)| {
            (
                model.clone(),
                n.saturating_sub(state.copilot_requests.get(model).copied().unwrap_or(0)),
            )
        })
        .collect();
    let mut remaining = deficits
        .values()
        .fold(0u64, |a, b| a.saturating_add(*b))
        .saturating_sub(unresolved);
    let mut requests = BTreeMap::new();
    for (model, deficit) in deficits {
        let certain = deficit.saturating_sub(unresolved);
        remaining = remaining.saturating_sub(certain);
        requests.insert(model, certain);
    }
    if remaining > 0 {
        requests.insert(String::new(), remaining);
    }
    let models: BTreeSet<_> = growth.keys().chain(requests.keys()).cloned().collect();
    let mut events = Vec::new();
    for model in models {
        let tokens = growth.remove(&model).flatten();
        let count = requests.get(&model).copied().unwrap_or(0);
        if count == 0 && tokens.as_ref().is_none_or(|tokens| tokens.total() == 0) {
            continue;
        }
        *state.copilot_requests.entry(model.clone()).or_default() += count;
        let native = if state.session.is_empty() {
            input.fallback
        } else {
            &state.session
        };
        let mut event = usage(
            input,
            state,
            format!("copilot:{native}:checkpoint:{}:{model}", input.key),
            tokens,
            count,
            None,
            false,
        );
        event.model = model;
        events.push(event);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(
        provider: AgentProvider,
        state: &mut ParserState,
        json: Value,
        seq: u64,
    ) -> Vec<AgentEvent> {
        parse(
            provider,
            "/profile",
            "fallback",
            &serde_json::to_vec(&json).unwrap(),
            1_700_000_000_000,
            seq,
            state,
        )
    }
    fn kinds(events: &[AgentEvent]) -> Vec<&AgentEventKind> {
        events.iter().map(|e| &e.kind).collect()
    }
    fn usage_tokens(events: &[AgentEvent]) -> Vec<(&str, &AgentTokens, u64)> {
        events
            .iter()
            .filter_map(|e| match &e.kind {
                AgentEventKind::Usage {
                    response_id,
                    tokens: Some(t),
                    requests,
                    ..
                } => Some((response_id.as_str(), t, *requests)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn codex_exact_boundaries_and_provider_duration() {
        let mut s = ParserState::default();
        line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"session_meta","payload":{"id":"main","cwd":"/project","source":"cli"}}),
            0,
        );
        let start = line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"event_msg","timestamp":"2026-10-25T12:00:00Z","payload":{"type":"task_started","started_at":1792929600,"turn_id":"t1"}}),
            10,
        );
        assert!(matches!(&start[0].kind,AgentEventKind::Started{task_id} if task_id=="t1"));
        assert_eq!(start[0].at_ms, 1_792_929_600_000);
        assert!(line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"response_item","payload":{"type":"message"}}),
            20
        )
        .is_empty());
        assert!(s.turn_open);
        let end = line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"event_msg","payload":{"type":"task_complete","completed_at":1792929610,"duration_ms":9000}}),
            30,
        );
        assert!(matches!(
            end[0].kind,
            AgentEventKind::Ended {
                completed: true,
                duration_ms: Some(9000)
            }
        ));
        assert!(!s.turn_open);
    }

    #[test]
    fn codex_copied_header_preserves_identity_and_totals_namespace() {
        let mut s = ParserState::default();
        line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"session_meta","payload":{"id":"resumed","cwd":"/own","source":"cli"}}),
            0,
        );
        line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"session_meta","payload":{"id":"original","cwd":"/copied","source":{"subagent":{}}}}),
            1,
        );
        let events = line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":30,"output_tokens":20,"reasoning_output_tokens":10}}}}),
            2,
        );
        assert_eq!(s.session, "resumed");
        assert_eq!(s.project, "/own");
        assert!(s.parent.is_none());
        let u = usage_tokens(&events);
        assert_eq!(u[0].0, "cumulative:original:120");
        assert_eq!(u[0].1.input, 70);
        assert_eq!(u[0].1.total(), 120);
        assert!(line(AgentProvider::Codex,&mut s,json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":30,"output_tokens":20,"reasoning_output_tokens":10}}}}),3).is_empty());
    }

    #[test]
    fn codex_native_usage_supersedes_fallback_and_child_names_root() {
        let mut s = ParserState::default();
        line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"session_meta","payload":{"id":"child","session_id":"root","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}}),
            0,
        );
        let e = line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"event_msg","payload":{"type":"task_started"}}),
            1,
        );
        assert_eq!(e[0].parent.as_ref().unwrap().native_id, "root");
        let e = line(
            AgentProvider::Codex,
            &mut s,
            json!({"type":"token_usage_record","payload":{"response_id":"resp","usage":{"input_tokens":100,"output_tokens":20}}}),
            2,
        );
        assert_eq!(usage_tokens(&e)[0].0, "native:child:resp");
        assert!(line(AgentProvider::Codex,&mut s,json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"output_tokens":30}}}}),3).is_empty());
        assert!(matches!(
            line(
                AgentProvider::Codex,
                &mut s,
                json!({"type":"event_msg","payload":{"type":"turn_aborted"}}),
                4
            )[0]
            .kind,
            AgentEventKind::Ended {
                completed: false,
                ..
            }
        ));
    }

    #[test]
    fn claude_tool_iteration_and_quoted_interruption_do_not_finish() {
        let mut s = ParserState::default();
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"uuid":"u1","type":"user","sessionId":"root","message":{"content":"ask"}}),
            0,
        );
        assert!(matches!(e[0].kind, AgentEventKind::Started { .. }));
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"uuid":"a1","type":"assistant","message":{"model":"claude-sonnet-4","id":"response","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Bash","id":"cmd1","input":{"command":"secret"}}],"usage":{"input_tokens":10,"output_tokens":2}}}),
            1,
        );
        assert!(kinds(&e)
            .iter()
            .any(|k| matches!(k, AgentEventKind::Command { running: true, .. })));
        assert!(s.turn_open);
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"other","content":"[Request interrupted by user]"}]}}),
            2,
        );
        assert!(s.running_commands.contains("cmd1"));
        assert!(kinds(&e)
            .iter()
            .all(|k| !matches!(k, AgentEventKind::Ended { .. })));
        line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"cmd1","content":"result"}]}}),
            3,
        );
        assert!(s.running_commands.is_empty());
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"uuid":"a2","type":"assistant","message":{"id":"response2","model":"claude-sonnet-4","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":3}}}),
            4,
        );
        assert!(matches!(
            e.last().unwrap().kind,
            AgentEventKind::Ended {
                completed: true,
                ..
            }
        ));
        assert!(!s.turn_open);
    }

    #[test]
    fn claude_sidechain_and_meta_cannot_open_or_close_main() {
        let mut s = ParserState::default();
        assert!(line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","isMeta":true,"message":{"content":"hidden"}}),
            0
        )
        .is_empty());
        assert!(line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","isSidechain":true,"message":{"content":"child"}}),
            1
        )
        .is_empty());
        line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","sessionId":"main","message":{"content":"ask"}}),
            2,
        );
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"assistant","sessionId":"main","agentId":"a","isSidechain":true,"message":{"id":"childresp","model":"claude-haiku","stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":6}}}),
            3,
        );
        assert!(s.turn_open);
        assert_eq!(e[0].session.native_id, "sidechain:a");
        assert_eq!(e[0].parent.as_ref().unwrap().native_id, "main");
        assert_eq!(e.len(), 1);
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"assistant","isApiErrorMessage":true,"message":{"model":"<synthetic>","stop_reason":"end_turn"}}),
            4,
        );
        assert!(matches!(
            e.last().unwrap().kind,
            AgentEventKind::Ended {
                completed: false,
                ..
            }
        ));
    }

    #[test]
    fn claude_streaming_ids_are_stable_and_state_keeps_no_content() {
        let mut s = ParserState::default();
        let a = line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"assistant","requestId":"req","message":{"id":"resp","model":"claude-sonnet","content":[{"type":"text","text":"private-answer"}],"usage":{"input_tokens":5,"output_tokens":2}}}),
            0,
        );
        let b = line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"assistant","requestId":"req","message":{"id":"resp","model":"claude-sonnet","usage":{"input_tokens":5,"output_tokens":8}}}),
            20,
        );
        assert_eq!(usage_tokens(&a)[0].0, usage_tokens(&b)[0].0);
        assert!(!serde_json::to_string(&s)
            .unwrap()
            .contains("private-answer"));
        let e = line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","message":{"content":"[Request interrupted by user for tool use]"}}),
            30,
        );
        assert!(matches!(
            e.last().unwrap().kind,
            AgentEventKind::Ended {
                completed: false,
                ..
            }
        ));
        assert!(line(
            AgentProvider::Claude,
            &mut s,
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"x"}]}}),
            40
        )
        .is_empty());
    }

    #[test]
    fn claude_tool_ends_turn_and_local_output() {
        for (close, completed) in [
            (
                json!({"type":"user","toolEndsTurn":true,"message":{"content":[{"type":"tool_result"}]}}),
                true,
            ),
            (
                json!({"type":"user","message":{"content":"<local-command-stdout>done"}}),
                false,
            ),
        ] {
            let mut s = ParserState::default();
            line(
                AgentProvider::Claude,
                &mut s,
                json!({"type":"user","message":{"content":"ask"}}),
                0,
            );
            let e = line(AgentProvider::Claude, &mut s, close, 1);
            assert!(
                matches!(e.last().unwrap().kind,AgentEventKind::Ended{completed:c,..} if c==completed)
            );
        }
    }

    #[test]
    fn copilot_tool_iterations_are_one_task_and_match_turn_id() {
        let mut s = ParserState::default();
        line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"session.start","data":{"sessionId":"cp","selectedModel":"gpt-5","context":{"cwd":"/project"}}}),
            0,
        );
        let e = line(
            AgentProvider::Copilot,
            &mut s,
            json!({"id":"user","type":"user.message","data":{}}),
            1,
        );
        assert!(matches!(e[0].kind, AgentEventKind::Started { .. }));
        line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.turn_start","data":{"turnId":"t1"}}),
            2,
        );
        let e = line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.message","data":{"apiCallId":"api","phase":"commentary","toolRequests":[{"name":"shell"}]}}),
            3,
        );
        assert!(matches!(
            e[0].kind,
            AgentEventKind::Usage {
                tokens: None,
                requests: 1,
                ..
            }
        ));
        assert!(matches!(
            line(
                AgentProvider::Copilot,
                &mut s,
                json!({"type":"assistant.turn_end","data":{"turnId":"t1"}}),
                4
            )[0]
            .kind,
            AgentEventKind::Active
        ));
        line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.turn_start","data":{"turnId":"t2"}}),
            5,
        );
        line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.message","data":{"apiCallId":"api2","phase":"final"}}),
            6,
        );
        assert!(line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.turn_end","data":{"turnId":"t1"}}),
            7
        )
        .is_empty());
        assert!(s.turn_open);
        assert!(matches!(
            line(
                AgentProvider::Copilot,
                &mut s,
                json!({"type":"assistant.turn_end","data":{"turnId":"t2"}}),
                8
            )[0]
            .kind,
            AgentEventKind::Ended {
                completed: true,
                ..
            }
        ));
    }

    #[test]
    fn copilot_api_ids_are_scoped_and_late_model_updates_same_record() {
        let mut s = ParserState::default();
        let a = line(
            AgentProvider::Copilot,
            &mut s,
            json!({"id":"a","type":"assistant.message","data":{"apiCallId":"same"}}),
            0,
        );
        let b = line(
            AgentProvider::Copilot,
            &mut s,
            json!({"id":"b","type":"assistant.message","data":{"apiCallId":"same","model":"gpt-5"}}),
            1,
        );
        let c = line(
            AgentProvider::Copilot,
            &mut s,
            json!({"id":"c","type":"assistant.message","agentId":"child","data":{"apiCallId":"same","model":"claude-sonnet"}}),
            2,
        );
        fn response(e: &AgentEvent) -> &str {
            if let AgentEventKind::Usage { response_id, .. } = &e.kind {
                response_id
            } else {
                panic!()
            }
        }
        assert_eq!(response(&a[0]), response(&b[0]));
        assert_ne!(response(&a[0]), response(&c[0]));
        assert_eq!(s.copilot_requests["gpt-5"], 1);
        assert_eq!(s.copilot_requests[""], 0);
        assert!(line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.message","data":{"apiCallId":"same","model":"gpt-5"}}),
            3
        )
        .is_empty());
        assert!(!s.turn_open);
    }

    #[test]
    fn copilot_shutdown_is_cumulative_and_reset_safe() {
        let mut s = ParserState::default();
        let metric = |n| json!({"type":"session.shutdown","id":format!("shutdown{n}"),"data":{"modelMetrics":{"gpt-5":{"usage":{"inputTokens":n,"outputTokens":10,"reasoningTokens":5},"requests":{"count":2}}}}});
        let a = line(AgentProvider::Copilot, &mut s, metric(100), 0);
        assert_eq!(usage_tokens(&a)[0].1.total(), 110);
        assert_eq!(usage_tokens(&a)[0].2, 2);
        assert!(line(AgentProvider::Copilot, &mut s, metric(100), 1).is_empty());
        let b = line(AgentProvider::Copilot, &mut s, metric(150), 2);
        assert_eq!(usage_tokens(&b)[0].1.input, 50);
        assert_eq!(usage_tokens(&b)[0].1.output, 0);
        assert_eq!(usage_tokens(&b)[0].2, 0);
        let c = line(AgentProvider::Copilot, &mut s, metric(20), 3);
        assert_eq!(usage_tokens(&c)[0].1.total(), 30);
    }

    #[test]
    fn copilot_shutdown_reconciles_unresolved_requests_once() {
        let mut s = ParserState::default();
        line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"assistant.message","data":{"apiCallId":"unknown"}}),
            0,
        );
        let e = line(
            AgentProvider::Copilot,
            &mut s,
            json!({"type":"session.shutdown","data":{"modelMetrics":{"m1":{"requests":{"count":1},"usage":{"inputTokens":10}},"m2":{"requests":{"count":1},"usage":{"inputTokens":10}}}}}),
            1,
        );
        let extra: u64 = e
            .iter()
            .map(|e| {
                if let AgentEventKind::Usage { requests, .. } = e.kind {
                    requests
                } else {
                    0
                }
            })
            .sum();
        assert_eq!(extra, 1);
        assert_eq!(s.copilot_requests.values().sum::<u64>(), 2);
    }

    #[test]
    fn invalid_lines_and_old_missing_timestamps_do_not_use_now() {
        let mut s = ParserState::default();
        assert!(parse(AgentProvider::Claude, "p", "s", b"{broken", 10, 0, &mut s).is_empty());
        let e = parse(
            AgentProvider::Claude,
            "p",
            "s",
            br#"{"type":"user","message":{"content":"ask"}}"#,
            10,
            1,
            &mut s,
        );
        assert_eq!(e[0].at_ms, 10);
        assert_eq!(
            timestamp(Some(&json!("2026-10-25T12:00:00Z"))),
            Some(1_792_929_600_000)
        );
    }

    #[test]
    fn child_native_response_does_not_replace_parent_cumulative_history() {
        let mut parent = ParserState::default();
        let mut child = ParserState::default();
        let mut events = line(
            AgentProvider::Codex,
            &mut parent,
            json!({"type":"session_meta","payload":{"id":"P","source":"cli"}}),
            0,
        );
        events.extend(line(AgentProvider::Codex,&mut parent,json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}),10));
        events.extend(line(AgentProvider::Codex,&mut child,json!({"type":"session_meta","payload":{"id":"C","session_id":"P","source":{"subagent":"review"}}}),0));
        line(
            AgentProvider::Codex,
            &mut child,
            json!({"type":"session_meta","payload":{"id":"P","source":"cli"}}),
            5,
        );
        let own = line(
            AgentProvider::Codex,
            &mut child,
            json!({"type":"token_usage_record","payload":{"response_id":"child-response","usage":{"input_tokens":6,"output_tokens":1}}}),
            20,
        );
        assert_eq!(usage_tokens(&own)[0].0, "native:C:child-response");
        events.extend(own);
        let index = super::super::index::fold(events);
        assert_eq!(index.usage.len(), 2);
        assert_eq!(
            index
                .usage
                .iter()
                .map(|u| u.tokens.as_ref().unwrap().total())
                .sum::<u64>(),
            117
        );
        // A native record in this file switches off only this file's future
        // fallback, even if its last copied header names another thread.
        assert!(line(AgentProvider::Codex,&mut child,json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":130,"output_tokens":15}}}}),30).is_empty());
        let copied = line(
            AgentProvider::Codex,
            &mut child,
            json!({"type":"token_usage_record","payload":{"session_id":"P","response_id":"parent-response","usage":{"input_tokens":5,"output_tokens":2}}}),
            40,
        );
        assert_eq!(usage_tokens(&copied)[0].0, "native:P:parent-response");
    }
}
