//! Stable provider-neutral types for local coding-agent journals.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const HISTORY_DAYS: i64 = 91;
pub const IDLE_MS: i64 = 10 * 60 * 1000;
pub const SETTLE_MS: i64 = 30 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    Codex,
    Claude,
    Opencode,
    Copilot,
}

impl AgentProvider {
    pub const ALL: [Self; 4] = [Self::Codex, Self::Claude, Self::Opencode, Self::Copilot];
    pub fn slug(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
            Self::Copilot => "copilot",
        }
    }
    pub fn resume_ms(self) -> i64 {
        if self == Self::Claude {
            3_600_000
        } else {
            6 * 3_600_000
        }
    }
}

/// Profile root is canonical and shared by normal/archived copies of a log.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionId {
    pub provider: AgentProvider,
    pub profile: String,
    pub native_id: String,
}
impl AgentSessionId {
    pub fn key(&self) -> String {
        serde_json::to_string(&(self.provider, &self.profile, &self.native_id))
            .expect("string tuple")
    }
}

/// Input excludes cache traffic. Output includes reasoning; do not add it twice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: Option<u64>,
}
impl AgentTokens {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
    pub fn merge(&mut self, other: &Self) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
        self.reasoning = match (self.reasoning, other.reasoning) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    pub fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.reasoning = match (self.reasoning, other.reasoning) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (a, b) => a.or(b),
        };
    }
    pub fn difference(&self, previous: &Self) -> Self {
        Self {
            input: self.input.saturating_sub(previous.input),
            output: self.output.saturating_sub(previous.output),
            cache_read: self.cache_read.saturating_sub(previous.cache_read),
            cache_write: self.cache_write.saturating_sub(previous.cache_write),
            reasoning: self
                .reasoning
                .map(|n| n.saturating_sub(previous.reasoning.unwrap_or(0))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEventKind {
    Context,
    Started {
        task_id: String,
    },
    Active,
    Settled,
    Ended {
        completed: bool,
        duration_ms: Option<u64>,
    },
    /// A local runtime observation, never a successful journal completion.
    /// Replaying it closes only the named epoch; late journal evidence can
    /// invalidate the cut when a model reply or local command kept it alive.
    Interrupted {
        task_id: String,
        offline_since_ms: i64,
        /// A confirmed process exit has no offline reply/command exceptions.
        #[serde(default)]
        confirmed_exit: bool,
    },
    Usage {
        response_id: String,
        tokens: Option<AgentTokens>,
        requests: u64,
        reported_cost_usd: Option<f64>,
        fast: bool,
        cumulative: bool,
        #[serde(default)]
        cache_write_1h: Option<u64>,
    },
    Command {
        id: String,
        running: bool,
    },
}

/// Only metadata is retained. Prompt/reply/tool output never enter this type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvent {
    pub id: String,
    pub at_ms: i64,
    pub session: AgentSessionId,
    /// Source byte offset / database row order resolves equal timestamps.
    #[serde(default)]
    pub sequence: u64,
    pub parent: Option<AgentSessionId>,
    pub project: String,
    pub model: String,
    pub kind: AgentEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentTaskState {
    Working,
    Waiting,
    Ended,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBinding {
    pub session_id: String,
    pub tmux_session: String,
    pub pane_id: String,
    /// Process start identity in Unix milliseconds, not system clock ticks.
    pub pid: u32,
    pub process_started_at: Option<u64>,
}

/// `None` is an unavailable probe, not evidence that an agent exited.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessObservation {
    pub session_id: String,
    pub running: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTask {
    pub id: String,
    pub session_id: String,
    pub provider: AgentProvider,
    pub profile: String,
    pub native_id: String,
    pub parent_session_id: Option<String>,
    pub project: String,
    pub model: String,
    pub started_at_ms: i64,
    pub last_activity_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub state: AgentTaskState,
    pub completed: Option<bool>,
    pub tokens: Option<AgentTokens>,
    pub estimated_cost_usd: Option<f64>,
    pub reported_cost_usd: Option<f64>,
    pub requests: u64,
    pub unpriced_requests: u64,
    pub bindings: Vec<RuntimeBinding>,
    #[serde(default)]
    pub running_commands: BTreeSet<String>,
    #[serde(default)]
    pub settled_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: String,
    pub session_id: String,
    pub root_session_id: String,
    pub provider: AgentProvider,
    pub profile: String,
    pub project: String,
    pub model: String,
    pub at_ms: i64,
    pub tokens: Option<AgentTokens>,
    pub requests: u64,
    pub estimated_cost_usd: Option<f64>,
    pub reported_cost_usd: Option<f64>,
    pub fast: bool,
    #[serde(default)]
    pub cache_write_1h: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub provider: Option<AgentProvider>,
    pub profile: Option<String>,
    pub project: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub bucket: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTotals {
    pub tasks: u64,
    pub completed: u64,
    pub aborted: u64,
    pub requests: u64,
    pub tokens: Option<AgentTokens>,
    pub estimated_cost_usd: Option<f64>,
    pub reported_cost_usd: Option<f64>,
    pub unpriced_requests: u64,
    pub unknown_token_requests: u64,
    pub agent_time_ms: u64,
    pub wall_time_ms: u64,
    #[serde(default)]
    pub incomplete_tasks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBucket {
    pub start: String,
    pub totals: AgentTotals,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentShare {
    pub id: String,
    pub name: String,
    pub provider: Option<AgentProvider>,
    pub totals: AgentTotals,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub updated_at_ms: i64,
    pub totals: AgentTotals,
    pub by_provider: BTreeMap<AgentProvider, AgentTotals>,
    pub models: Vec<AgentShare>,
    pub projects: Vec<AgentShare>,
    pub series: Vec<AgentBucket>,
    pub pricing: PriceStatus,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub state: String,
    pub scanning: bool,
    pub files: usize,
    pub scanned_files: usize,
    pub updated_at_ms: i64,
    pub errors: Vec<String>,
    pub history_days: i64,
    pub bytes_read: u64,
    pub read_files: usize,
    /// Canonical profile roots of indexed sources, including historical runs.
    #[serde(default)]
    pub profiles: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub now_ms: i64,
    pub updated_at_ms: i64,
    pub working_count: usize,
    pub by_provider: BTreeMap<AgentProvider, usize>,
    pub live: Vec<AgentTask>,
    pub waiting: Vec<AgentTask>,
    pub unknown: Vec<AgentTask>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceStatus {
    pub date: Option<String>,
    pub source: Option<String>,
    pub models: usize,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs_and_identity_are_stable() {
        for p in AgentProvider::ALL {
            assert_eq!(serde_json::to_value(p).unwrap(), p.slug());
        }
        let a = AgentSessionId {
            provider: AgentProvider::Codex,
            profile: "/a:b".into(),
            native_id: "c".into(),
        };
        let b = AgentSessionId {
            provider: AgentProvider::Codex,
            profile: "/a".into(),
            native_id: "b:c".into(),
        };
        assert_ne!(a.key(), b.key());
    }
    #[test]
    fn reasoning_is_not_added_to_output() {
        let t = AgentTokens {
            input: 10,
            output: 20,
            reasoning: Some(15),
            ..Default::default()
        };
        assert_eq!(t.total(), 30);
        assert!(serde_json::to_value(AgentTotals::default()).unwrap()["tokens"].is_null());
    }
}
