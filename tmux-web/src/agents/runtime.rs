//! OS process/terminal inventory for exact coding-agent session bindings.
//!
//! The public snapshot contains identities and extracted session IDs only.
//! Command lines and environment blocks are transient and never logged/stored.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::model::{AgentProvider, RuntimeBinding};
use super::sources::{self, AgentConfig, AgentRoot};

const ARG_LIMIT: usize = 256 * 1024;
const FD_CACHE_SECS: u64 = 30;
const MAX_CANDIDATES: usize = 128;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInventory {
    pub panes: Vec<RuntimePane>,
    pub processes: Vec<RuntimeProcess>,
    pub updated_at_ms: i64,
    pub available: bool,
    pub errors: Vec<String>,
    #[serde(skip)]
    pub(crate) registry: Vec<RegistryRecord>,
    #[serde(skip)]
    pub(crate) parent_pids: BTreeMap<u32, u32>,
    #[serde(skip)]
    pub(crate) registry_complete_profiles: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePane {
    pub pane_id: String,
    pub tmux_session: String,
    pub session_id: String,
    pub window_id: String,
    pub pid: u32,
    pub process_started_at: Option<u64>,
    pub tty: String,
    pub path: String,
    pub active: bool,
    pub candidates: Vec<RuntimeProcess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub process_started_at: Option<u64>,
    pub provider: AgentProvider,
    pub native_ids: Vec<String>,
    #[serde(skip)]
    pub(crate) evidence: Vec<NativeEvidence>,
    #[serde(skip)]
    pub(crate) wrapper: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NativeEvidence {
    pub native_id: String,
    pub profile: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RegistryRecord {
    pub native_id: String,
    pub profile: String,
    pub pid: u32,
    pub modified_ms: i64,
}

#[derive(Debug, Clone)]
struct ProcessRow {
    pid: u32,
    parent_pid: u32,
    tty: String,
    command: String,
}

#[derive(Clone)]
struct ProbeCache {
    at: Instant,
    process: RuntimeProcess,
    fd_at: Instant,
    fd_evidence: Vec<NativeEvidence>,
}

#[derive(Default)]
pub(crate) struct RuntimeState {
    pub inventory: RuntimeInventory,
    cache: BTreeMap<(u32, u64), ProbeCache>,
    pub bindings: Vec<RuntimeBinding>,
    pub store: Option<super::store::AgentStatsStore>,
    pub book: super::bindings::BindingBook,
    pub observed_registry: BTreeMap<String, (u32, Option<u64>)>,
}

/// Shared OS inventory and binding lifecycle; never owns a CLI process.
#[derive(Clone)]
pub struct AgentRuntime {
    pub(crate) state: Arc<Mutex<RuntimeState>>,
    pub(crate) gate: Arc<Mutex<()>>,
    created_at_ms: i64,
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}
impl AgentRuntime {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeState::default())),
            gate: Arc::new(Mutex::new(())),
            created_at_ms: super::store::now_ms(),
        }
    }
    pub async fn inventory(&self) -> RuntimeInventory {
        self.state.lock().await.inventory.clone()
    }

    pub async fn bindings(&self) -> Vec<RuntimeBinding> {
        self.state.lock().await.bindings.clone()
    }

    /// Refresh exact bindings and OS observations independently of journal IO.
    pub async fn refresh(&self, store: &super::store::AgentStatsStore) -> Result<()> {
        let _gate = self.gate.lock().await;
        self.state.lock().await.store = Some(store.clone());
        let config = store.config().await;
        if !config.enabled {
            let mut state = self.state.lock().await;
            state.bindings.clear();
            state.book = Default::default();
            state.observed_registry.clear();
            state.inventory = RuntimeInventory {
                available: true,
                updated_at_ms: super::store::now_ms(),
                ..Default::default()
            };
            drop(state);
            store.update_runtime(vec![], vec![], None).await;
            return Ok(());
        }
        let previous = self.bindings().await;
        let inventory = match self.probe(&config).await {
            Ok(inventory) => inventory,
            Err(error) => {
                let unknown = previous
                    .iter()
                    .map(|binding| super::model::ProcessObservation {
                        session_id: binding.session_id.clone(),
                        running: None,
                    })
                    .collect();
                store.update_runtime(previous, unknown, None).await;
                return Err(error);
            }
        };
        let tasks = store
            .runtime_tasks(super::store::now_ms() - 24 * 3_600_000)
            .await;
        let (bindings, seen_registry) = {
            let mut state = self.state.lock().await;
            let bindings = super::bindings::resolve(&tasks, &inventory, &previous, &mut state.book);
            for record in &inventory.registry {
                for task in tasks.iter().filter(|task| {
                    task.provider == AgentProvider::Claude
                        && task.native_id == record.native_id
                        && task.profile == record.profile
                }) {
                    let start = inventory
                        .processes
                        .iter()
                        .find(|process| process.pid == record.pid)
                        .and_then(|process| process.process_started_at);
                    if start.is_some_and(|start| {
                        record.modified_ms > 0 && start > record.modified_ms as u64 + 1000
                    }) {
                        continue;
                    }
                    if start.is_some() {
                        state
                            .observed_registry
                            .insert(task.session_id.clone(), (record.pid, start));
                    }
                }
            }
            state
                .observed_registry
                .retain(|id, _| tasks.iter().any(|task| &task.session_id == id));
            state.bindings = bindings.clone();
            (bindings, state.observed_registry.clone())
        };
        let checks = process_checks(
            &tasks,
            &inventory,
            &previous,
            &bindings,
            &seen_registry,
            self.created_at_ms,
        );
        let observations = tokio::task::spawn_blocking(move || {
            checks
                .into_iter()
                .map(|(id, checks)| {
                    let mut unknown = false;
                    let mut running = false;
                    for (pid, start, stale) in checks {
                        let observed = if stale {
                            Some(false)
                        } else {
                            identity_alive(pid, start)
                        };
                        match observed {
                            Some(true) => running = true,
                            Some(false) => {}
                            None => unknown = true,
                        }
                    }
                    super::model::ProcessObservation {
                        session_id: id,
                        running: if running {
                            Some(true)
                        } else if unknown {
                            None
                        } else {
                            Some(false)
                        },
                    }
                })
                .collect::<Vec<_>>()
        })
        .await?;
        let online = online_state().await;
        store.update_runtime(bindings, observations, online).await;
        Ok(())
    }

    pub fn spawn_worker(
        &self,
        store: super::store::AgentStatsStore,
    ) -> tokio::task::JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(2));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                if let Err(error) = runtime.refresh(&store).await {
                    tracing::debug!(%error,"agent runtime inventory temporarily unavailable");
                }
            }
        })
    }

    pub async fn bind(&self, session_id: &str, pane_id: &str) -> Result<RuntimeBinding> {
        let store = self
            .state
            .lock()
            .await
            .store
            .clone()
            .context("agent runtime has not initialized yet")?;
        self.refresh(&store).await?;
        let _gate = self.gate.lock().await;
        let tasks = store
            .runtime_tasks(super::store::now_ms() - 24 * 3_600_000)
            .await;
        let binding = {
            let state = self.state.lock().await;
            super::bindings::manual_candidate(
                session_id,
                pane_id,
                &tasks,
                &state.inventory,
                &state.bindings,
            )?
        };
        let candidate = binding.clone();
        let alive = tokio::task::spawn_blocking(move || {
            identity_alive(candidate.pid, candidate.process_started_at)
        })
        .await?;
        if alive != Some(true) {
            bail!("agent process changed or its identity is unavailable");
        }
        let bindings = {
            let mut state = self.state.lock().await;
            state.book.manual.retain(|_, previous| {
                previous.pid != binding.pid
                    || previous.process_started_at != binding.process_started_at
            });
            state.book.detached.retain(|_, previous| {
                previous.pid != binding.pid
                    || previous.process_started_at != binding.process_started_at
            });
            state
                .book
                .manual
                .insert(session_id.to_owned(), binding.clone());
            let inventory = state.inventory.clone();
            let previous = state.bindings.clone();
            let bindings = super::bindings::resolve(&tasks, &inventory, &previous, &mut state.book);
            state.bindings = bindings.clone();
            bindings
        };
        store.set_runtime(bindings).await;
        Ok(binding)
    }

    pub async fn unbind(&self, session_id: &str) -> Result<()> {
        let _gate = self.gate.lock().await;
        let (store, bindings) = {
            let mut state = self.state.lock().await;
            let current = state
                .bindings
                .iter()
                .find(|binding| binding.session_id == session_id)
                .cloned();
            state.book.manual.remove(session_id);
            if let Some(binding) = current {
                state.book.detached.insert(session_id.to_owned(), binding);
            }
            state
                .bindings
                .retain(|binding| binding.session_id != session_id);
            (state.store.clone(), state.bindings.clone())
        };
        if let Some(store) = store {
            store.set_runtime(bindings).await;
        }
        Ok(())
    }

    /// Replace the last inventory only after both tmux and OS probes succeeded.
    pub(crate) async fn probe(&self, config: &AgentConfig) -> Result<RuntimeInventory> {
        let panes = match crate::tmux::list_agent_panes().await {
            Ok(panes) => panes,
            Err(error) => {
                self.failed("tmux inventory unavailable").await;
                return Err(error);
            }
        };
        let rows = match process_rows().await {
            Ok(rows) => rows,
            Err(error) => {
                self.failed("process inventory unavailable").await;
                return Err(error);
            }
        };
        let roots = config.all_roots();
        let registry_roots = roots.clone();
        let (registry, mut errors, registry_complete_profiles) =
            tokio::task::spawn_blocking(move || registry_records(&registry_roots)).await?;
        let pane_roots: BTreeSet<u32> = panes.iter().map(|pane| pane.pid).collect();
        let registry_pids: BTreeSet<u32> = registry.iter().map(|record| record.pid).collect();
        let mut candidates: Vec<ProcessRow> = rows
            .values()
            .filter(|row| {
                let descendant = descendant_of(row.pid, &pane_roots, &rows);
                registry_pids.contains(&row.pid) || candidate_command(&row.command, descendant)
            })
            .cloned()
            .collect();
        candidates.sort_by_key(|row| (!descendant_of(row.pid, &pane_roots, &rows), row.pid));
        if candidates.len() > MAX_CANDIDATES {
            candidates.truncate(MAX_CANDIDATES);
            errors.push("agent process probe limit reached".to_owned());
        }
        let old_cache = self.state.lock().await.cache.clone();
        let probing_registry = registry.clone();
        let probing_roots = roots.clone();
        let (mut processes, mut cache) = tokio::task::spawn_blocking(move || {
            let mut processes = Vec::new();
            let mut cache = BTreeMap::new();
            for row in candidates {
                let start = process_started_at(row.pid);
                if let Some(key) = start.map(|start| (row.pid, start)) {
                    if let Some(previous) = old_cache
                        .get(&key)
                        .filter(|entry| entry.at.elapsed() < Duration::from_secs(10))
                    {
                        let mut process = previous.process.clone();
                        process.parent_pid = row.parent_pid;
                        // Registry evidence is refreshed independently of cached argv/fds.
                        add_registry(&mut process, &probing_registry);
                        process.evidence.extend(previous.fd_evidence.clone());
                        processes.push(process);
                        cache.insert(key, previous.clone());
                        continue;
                    }
                }
                let Some(mut process) =
                    probe_process(&row, start, &probing_roots, &probing_registry)
                else {
                    continue;
                };
                if start.is_some() && process_started_at(row.pid) != start {
                    continue;
                }
                add_registry(&mut process, &probing_registry);
                if let Some(start) = start {
                    let old = old_cache.get(&(row.pid, start));
                    // Keep last exact fd evidence until the bounded fd probe refreshes it.
                    cache.insert(
                        (row.pid, start),
                        ProbeCache {
                            at: Instant::now(),
                            process: process.clone(),
                            fd_at: old.map(|old| old.fd_at).unwrap_or_else(|| {
                                Instant::now() - Duration::from_secs(FD_CACHE_SECS + 1)
                            }),
                            fd_evidence: old.map(|old| old.fd_evidence.clone()).unwrap_or_default(),
                        },
                    );
                    if let Some(old) = old {
                        process.evidence.extend(old.fd_evidence.clone());
                    }
                }
                processes.push(process);
            }
            (processes, cache)
        })
        .await?;
        let mut pending = tokio::task::JoinSet::new();
        let mut fd_targets = Vec::new();
        for process in &processes {
            if process.provider == AgentProvider::Opencode
                || registry.iter().any(|record| record.pid == process.pid)
            {
                continue;
            }
            let Some(start) = process.process_started_at else {
                continue;
            };
            if cache
                .get(&(process.pid, start))
                .is_some_and(|entry| entry.fd_at.elapsed() >= Duration::from_secs(FD_CACHE_SECS))
            {
                fd_targets.push((process.pid, start, process.provider));
            }
        }
        let mut targets = fd_targets.into_iter();
        loop {
            while pending.len() < 4 {
                let Some((pid, start, provider)) = targets.next() else {
                    break;
                };
                let roots = roots.clone();
                pending.spawn(async move {
                    (pid, start, open_file_evidence(pid, provider, &roots).await)
                });
            }
            let Some(result) = pending.join_next().await else {
                break;
            };
            if let Ok((pid, start, Ok(evidence))) = result {
                if let Some(entry) = cache.get_mut(&(pid, start)) {
                    entry.fd_at = Instant::now();
                    entry.fd_evidence = evidence;
                    if let Some(process) = processes.iter_mut().find(|process| {
                        process.pid == pid && process.process_started_at == Some(start)
                    }) {
                        process.evidence = entry.process.evidence.clone();
                        process.evidence.extend(entry.fd_evidence.clone());
                    }
                }
            }
        }
        for process in &mut processes {
            process.evidence.sort();
            process.evidence.dedup();
            process.native_ids = process
                .evidence
                .iter()
                .map(|evidence| evidence.native_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        // New Codex TUIs delegate journals to one shared daemon. Their native
        // terminal name still identifies the selected thread in the state DB.
        let title_panes = panes.clone();
        let title_roots = roots.clone();
        let titles = tokio::task::spawn_blocking(move || {
            super::codex_terminal::evidence(&title_panes, &title_roots)
        })
        .await?;
        for pane in &panes {
            let ids: Vec<_> = processes
                .iter()
                .filter(|process| {
                    process.provider == AgentProvider::Codex
                        && !process.wrapper
                        && descendant_of(process.pid, &BTreeSet::from([pane.pid]), &rows)
                        && rows
                            .get(&process.pid)
                            .is_some_and(|row| owns_terminal(row, &pane.tty))
                })
                .map(|process| process.pid)
                .collect();
            if ids.len() != 1 {
                continue;
            }
            if let Some(evidence) = titles.get(&pane.pane_id) {
                if let Some(process) = processes.iter_mut().find(|p| p.pid == ids[0]) {
                    process.evidence.push(evidence.clone());
                    process.evidence.sort();
                    process.evidence.dedup();
                    process.native_ids = process
                        .evidence
                        .iter()
                        .map(|e| e.native_id.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                }
            }
        }
        let root_pids: Vec<u32> = panes.iter().map(|pane| pane.pid).collect();
        let starts = tokio::task::spawn_blocking(move || {
            root_pids
                .into_iter()
                .map(|pid| (pid, process_started_at(pid)))
                .collect::<BTreeMap<_, _>>()
        })
        .await?;
        let inventory = RuntimeInventory {
            panes: panes
                .into_iter()
                .map(|pane| RuntimePane {
                    pane_id: pane.pane_id,
                    tmux_session: pane.tmux_session,
                    session_id: pane.session_id,
                    window_id: pane.window_id,
                    pid: pane.pid,
                    process_started_at: starts.get(&pane.pid).copied().flatten(),
                    tty: pane.tty.clone(),
                    path: pane.path,
                    active: pane.active,
                    candidates: processes
                        .iter()
                        .filter(|process| {
                            descendant_of(process.pid, &BTreeSet::from([pane.pid]), &rows)
                                && rows
                                    .get(&process.pid)
                                    .is_some_and(|row| owns_terminal(row, &pane.tty))
                        })
                        .cloned()
                        .collect(),
                })
                .collect(),
            processes,
            updated_at_ms: super::store::now_ms(),
            available: true,
            errors,
            registry,
            parent_pids: rows
                .iter()
                .map(|(pid, row)| (*pid, row.parent_pid))
                .collect(),
            registry_complete_profiles,
        };
        let mut state = self.state.lock().await;
        state.cache = cache;
        state.inventory = inventory.clone();
        Ok(inventory)
    }
    async fn failed(&self, message: &str) {
        let mut state = self.state.lock().await;
        state.inventory.available = false;
        state.inventory.errors = vec![message.to_owned()];
    }
}

fn process_checks(
    tasks: &[super::model::AgentTask],
    inventory: &RuntimeInventory,
    previous: &[RuntimeBinding],
    bindings: &[RuntimeBinding],
    seen_registry: &BTreeMap<String, (u32, Option<u64>)>,
    created_at_ms: i64,
) -> BTreeMap<String, BTreeSet<(u32, Option<u64>, bool)>> {
    if !inventory.available {
        return BTreeMap::new();
    }
    let mut checks: BTreeMap<String, BTreeSet<(u32, Option<u64>, bool)>> = BTreeMap::new();
    for binding in previous.iter().chain(bindings) {
        checks
            .entry(binding.session_id.clone())
            .or_default()
            .insert((binding.pid, binding.process_started_at, false));
    }
    for (id, (pid, start)) in seen_registry {
        checks
            .entry(id.clone())
            .or_default()
            .insert((*pid, *start, false));
    }
    for record in &inventory.registry {
        let start = inventory
            .processes
            .iter()
            .find(|process| process.pid == record.pid)
            .and_then(|process| process.process_started_at);
        let stale = start.is_some_and(|start| {
            record.modified_ms > 0 && start > record.modified_ms as u64 + 1000
        });
        for task in tasks.iter().filter(|task| {
            task.provider == AgentProvider::Claude
                && task.profile == record.profile
                && task.native_id == record.native_id
        }) {
            checks
                .entry(task.session_id.clone())
                .or_default()
                .insert((record.pid, start, stale));
        }
    }
    // Only an entirely readable, listed registry can prove that a previously
    // running native session disappeared. Missing folders/partial writes are
    // unknown, and a still-alive PID may now serve a different session.
    for (id, (pid, start)) in seen_registry {
        if let Some(task) = tasks.iter().find(|task| &task.session_id == id) {
            if inventory.registry_complete_profiles.contains(&task.profile)
                && !inventory.registry.iter().any(|record| {
                    record.profile == task.profile && record.native_id == task.native_id
                })
            {
                checks.insert(id.clone(), BTreeSet::from([(*pid, *start, true)]));
            }
        }
    }
    // History first discovered after startup must not replay as live work.
    // A listed, completely readable Claude registry proves which pre-start
    // sessions still have processes; a new task remains unknown until observed.
    for task in tasks.iter().filter(|task| {
        task.provider == AgentProvider::Claude
            && task.ended_at_ms.is_none()
            && task.last_activity_at_ms < created_at_ms
    }) {
        if inventory.registry_complete_profiles.contains(&task.profile)
            && !inventory
                .registry
                .iter()
                .any(|record| record.profile == task.profile && record.native_id == task.native_id)
        {
            checks.insert(task.session_id.clone(), BTreeSet::from([(0, None, true)]));
        }
    }
    checks
}

async fn process_rows() -> Result<BTreeMap<u32, ProcessRow>> {
    let mut command = tokio::process::Command::new("ps");
    command
        .args(["-axo", "pid=,ppid=,tty=,comm="])
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .context("process inventory timed out")??;
    if !output.status.success() || output.stdout.len() > 8 * 1024 * 1024 {
        bail!("process inventory unavailable");
    }
    parse_process_rows(&String::from_utf8_lossy(&output.stdout))
}
fn parse_process_rows(output: &str) -> Result<BTreeMap<u32, ProcessRow>> {
    let mut rows = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.trim().splitn(2, char::is_whitespace);
        let pid: u32 = fields.next().context("missing process ID")?.parse()?;
        let rest = fields
            .next()
            .context("missing parent process")?
            .trim_start();
        let mut fields = rest.splitn(2, char::is_whitespace);
        let parent_pid: u32 = fields
            .next()
            .context("missing parent process ID")?
            .parse()?;
        let rest = fields
            .next()
            .context("missing process terminal")?
            .trim_start();
        let mut terminal_fields = rest.splitn(2, char::is_whitespace);
        let tty = terminal_fields
            .next()
            .context("missing process terminal")?
            .to_owned();
        let command = terminal_fields.next().unwrap_or("").trim().to_owned();
        rows.insert(
            pid,
            ProcessRow {
                pid,
                parent_pid,
                tty,
                command,
            },
        );
    }
    if rows.is_empty() {
        bail!("empty process inventory");
    }
    Ok(rows)
}
fn owns_terminal(row: &ProcessRow, tty: &str) -> bool {
    row.tty != "?" && row.tty != "??" && row.tty == tty.strip_prefix("/dev/").unwrap_or(tty)
}
fn descendant_of(mut pid: u32, roots: &BTreeSet<u32>, rows: &BTreeMap<u32, ProcessRow>) -> bool {
    let mut seen = BTreeSet::new();
    while pid > 0 && seen.insert(pid) {
        if roots.contains(&pid) {
            return true;
        }
        match rows.get(&pid) {
            Some(row) => pid = row.parent_pid,
            None => break,
        }
    }
    false
}
fn basename(value: &str) -> &str {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .trim_start_matches('-')
}
fn named_provider(executable: &str) -> Option<AgentProvider> {
    let name = basename(executable).to_ascii_lowercase();
    match name.as_str() {
        "codex" => Some(AgentProvider::Codex),
        "claude" | "claude-code" => Some(AgentProvider::Claude),
        "opencode" => Some(AgentProvider::Opencode),
        "copilot" | "github-copilot" => Some(AgentProvider::Copilot),
        _ if name.contains('.')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            && executable.split('/').any(|part| part == "claude") =>
        {
            Some(AgentProvider::Claude)
        }
        _ => None,
    }
}
fn candidate_command(command: &str, descendant: bool) -> bool {
    named_provider(command).is_some()
        || descendant && matches!(basename(command), "node" | "nodejs" | "bun" | "deno")
        || descendant
            && basename(command).contains('.')
            && basename(command)
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
}
fn probe_process(
    row: &ProcessRow,
    start: Option<u64>,
    roots: &[AgentRoot],
    registry: &[RegistryRecord],
) -> Option<RuntimeProcess> {
    if !own_process(row.pid) {
        return None;
    }
    let block = process_arguments(row.pid);
    let (arguments, environment) = block
        .map(|bytes| parse_argument_block(&bytes))
        .unwrap_or_default();
    let registry_provider = registry
        .iter()
        .any(|record| record.pid == row.pid)
        .then_some(AgentProvider::Claude);
    let provider = named_provider(&row.command)
        .or(registry_provider)
        .or_else(|| {
            arguments
                .first()
                .and_then(|executable| named_provider(executable))
        })
        .or_else(|| script_provider(&arguments))?;
    // A managed/shared server may descend from the first TUI that launched it,
    // but owns threads for many terminals and desktop clients. It is not a
    // pane's interactive agent and must never claim those threads for that pane.
    if provider == AgentProvider::Codex && codex_service(&arguments) {
        return None;
    }
    let profile = profile_from_environment(provider, &environment, roots);
    let evidence = resume_ids(provider, &arguments)
        .into_iter()
        .map(|native_id| NativeEvidence {
            native_id,
            profile: profile.clone(),
        })
        .collect();
    Some(RuntimeProcess {
        pid: row.pid,
        parent_pid: row.parent_pid,
        process_started_at: start,
        provider,
        native_ids: Vec::new(),
        evidence,
        wrapper: matches!(basename(&row.command), "node" | "nodejs" | "bun" | "deno"),
    })
}
fn codex_service(arguments: &[String]) -> bool {
    // KERN_PROCARGS2 includes the executable path before argv[0] on macOS.
    let mut index = 1;
    if arguments
        .get(index)
        .is_some_and(|argument| named_provider(argument) == Some(AgentProvider::Codex))
    {
        index += 1;
    }
    arguments
        .get(index)
        .is_some_and(|argument| matches!(argument.as_str(), "app-server" | "app-server-daemon"))
}
fn script_provider(arguments: &[String]) -> Option<AgentProvider> {
    arguments
        .iter()
        .skip(1)
        .take(4)
        .find_map(|argument| match basename(argument) {
            "codex.js" => Some(AgentProvider::Codex),
            "claude.js" | "cli.js" if argument.contains("@anthropic-ai/claude-code/") => {
                Some(AgentProvider::Claude)
            }
            "opencode.js" => Some(AgentProvider::Opencode),
            "copilot.js" => Some(AgentProvider::Copilot),
            _ if argument.contains("/@github/copilot/") => Some(AgentProvider::Copilot),
            _ => None,
        })
}
fn resume_ids(provider: AgentProvider, arguments: &[String]) -> BTreeSet<String> {
    let flags: &[&str] = match provider {
        AgentProvider::Codex => &["--thread-id"],
        AgentProvider::Claude => &["--resume", "--session-id", "-r"],
        AgentProvider::Opencode => &["--session", "-s"],
        AgentProvider::Copilot => &["--resume"],
    };
    let mut ids = BTreeSet::new();
    for (index, argument) in arguments.iter().enumerate().skip(1) {
        if argument == "--" {
            break;
        }
        if (flags.contains(&argument.as_str())
            || provider == AgentProvider::Codex && argument == "resume")
            && index + 1 < arguments.len()
        {
            let id = &arguments[index + 1];
            if native_id(id) {
                ids.insert(id.clone());
            }
        }
        for flag in flags {
            if let Some(id) = argument.strip_prefix(&format!("{flag}=")) {
                if native_id(id) {
                    ids.insert(id.to_owned());
                }
            }
        }
    }
    ids
}
fn native_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
fn profile_from_environment(
    provider: AgentProvider,
    environment: &BTreeMap<String, String>,
    roots: &[AgentRoot],
) -> Option<String> {
    let raw = match provider {
        AgentProvider::Codex => environment.get("CODEX_HOME").map(PathBuf::from),
        AgentProvider::Claude => environment.get("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        AgentProvider::Opencode => environment
            .get("XDG_DATA_HOME")
            .map(|path| PathBuf::from(path).join("opencode")),
        AgentProvider::Copilot => None,
    };
    let raw = raw.or_else(|| {
        environment.get("HOME").map(|home| {
            PathBuf::from(home).join(match provider {
                AgentProvider::Codex => ".codex",
                AgentProvider::Claude => ".claude",
                AgentProvider::Copilot => ".copilot",
                AgentProvider::Opencode => ".local/share/opencode",
            })
        })
    })?;
    let canonical = sources::canonical(&raw).to_string_lossy().into_owned();
    roots
        .iter()
        .any(|root| root.provider == provider && root.profile == canonical)
        .then_some(canonical)
}

/// Decode native null-delimited argv/env, retaining only four profile variables.
fn parse_argument_block(bytes: &[u8]) -> (Vec<String>, BTreeMap<String, String>) {
    let mut arguments = Vec::new();
    let mut environment = BTreeMap::new();
    for value in bytes
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let Ok(value) = std::str::from_utf8(value) else {
            continue;
        };
        if let Some((key, value)) = value.split_once('=') {
            if matches!(
                key,
                "HOME" | "CODEX_HOME" | "CLAUDE_CONFIG_DIR" | "XDG_DATA_HOME"
            ) {
                environment.insert(key.to_owned(), value.to_owned());
            }
            // Environment variables are never command-line session evidence.
            if key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
            {
                continue;
            }
        }
        arguments.push(value.to_owned());
    }
    (arguments, environment)
}

fn registry_records(roots: &[AgentRoot]) -> (Vec<RegistryRecord>, Vec<String>, BTreeSet<String>) {
    let profiles: BTreeSet<_> = roots
        .iter()
        .filter(|root| root.provider == AgentProvider::Claude)
        .map(|root| root.profile.clone())
        .collect();
    let mut records = Vec::new();
    let mut errors = Vec::new();
    let mut complete_profiles = BTreeSet::new();
    for profile in profiles {
        let directory = Path::new(&profile).join("sessions");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                errors.push("Claude process registry unavailable".to_owned());
                continue;
            }
        };
        let mut complete = true;
        for (index, entry) in entries.enumerate() {
            if index >= 4096 {
                complete = false;
                errors.push("Claude process registry limit reached".to_owned());
                break;
            }
            let Ok(entry) = entry else {
                complete = false;
                errors.push("Claude process registry incomplete".to_owned());
                continue;
            };
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let Some(filename_pid) = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let value = read_small_json(&path, 64 * 1024);
            let Some(value) = value else {
                complete = false;
                errors.push("Claude process registry incomplete".to_owned());
                continue;
            };
            if value
                .get("pidDomain")
                .and_then(|value| value.as_str())
                .is_some_and(|domain| domain != host_pid_domain())
            {
                continue;
            }
            let Some(pid) = value
                .get("pid")
                .and_then(|value| value.as_u64())
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|pid| *pid > 0 && *pid == filename_pid)
            else {
                complete = false;
                continue;
            };
            let Some(id) = value
                .get("sessionId")
                .and_then(|value| value.as_str())
                .filter(|id| native_id(id))
            else {
                complete = false;
                continue;
            };
            let modified_ms = std::fs::metadata(&path)
                .ok()
                .map(|metadata| sources::modified_ms(&metadata))
                .unwrap_or(0);
            records.push(RegistryRecord {
                native_id: id.to_owned(),
                profile: profile.clone(),
                pid,
                modified_ms,
            });
        }
        if complete {
            complete_profiles.insert(profile);
        }
    }
    (records, errors, complete_profiles)
}
fn host_pid_domain() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}
fn read_small_json(path: &Path, limit: usize) -> Option<serde_json::Value> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > limit {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
fn add_registry(process: &mut RuntimeProcess, registry: &[RegistryRecord]) {
    if process.provider != AgentProvider::Claude {
        return;
    }
    for record in registry.iter().filter(|record| record.pid == process.pid) {
        if process
            .process_started_at
            .is_some_and(|start| record.modified_ms > 0 && start > record.modified_ms as u64 + 1000)
        {
            continue;
        }
        let evidence = NativeEvidence {
            native_id: record.native_id.clone(),
            profile: Some(record.profile.clone()),
        };
        if !process.evidence.contains(&evidence) {
            process.evidence.push(evidence);
        }
    }
}

async fn open_file_evidence(
    pid: u32,
    provider: AgentProvider,
    roots: &[AgentRoot],
) -> Result<Vec<NativeEvidence>> {
    #[cfg(target_os = "macos")]
    let paths = {
        let mut command = tokio::process::Command::new("lsof");
        command
            .args(["-nP", "-a", "-p", &pid.to_string(), "-Fn"])
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(1), command.output())
            .await
            .context("agent file probe timed out")??;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!("agent file probe unavailable");
        }
        if output.stdout.len() > 4 * 1024 * 1024 {
            bail!("agent file probe limit exceeded");
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix('n'))
            .filter(|path| path.starts_with('/'))
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    };
    #[cfg(target_os = "linux")]
    let paths = {
        let directory = PathBuf::from(format!("/proc/{pid}/fd"));
        tokio::task::spawn_blocking(move || {
            let mut paths = Vec::new();
            for entry in std::fs::read_dir(directory)?.take(4096).flatten() {
                if let Ok(path) = std::fs::read_link(entry.path()) {
                    paths.push(path);
                }
            }
            Ok::<_, std::io::Error>(paths)
        })
        .await??
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let paths = Vec::<PathBuf>::new();
    Ok(file_evidence(provider, &paths, roots))
}
fn file_evidence(
    provider: AgentProvider,
    paths: &[PathBuf],
    roots: &[AgentRoot],
) -> Vec<NativeEvidence> {
    let mut found = BTreeSet::new();
    for path in paths {
        for root in roots
            .iter()
            .filter(|root| root.provider == provider && path.starts_with(&root.path))
        {
            if matches!(provider, AgentProvider::Codex | AgentProvider::Claude)
                && path
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let id = match provider {
                AgentProvider::Codex => path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .filter(|stem| stem.starts_with("rollout-"))
                    .and_then(|stem| stem.get(stem.len().checked_sub(36)?..))
                    .filter(|id| uuid(id))
                    .map(str::to_owned),
                AgentProvider::Claude => path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .filter(|id| uuid(id))
                    .map(str::to_owned),
                AgentProvider::Copilot
                    if path.file_name().is_some_and(|name| name == "events.jsonl")
                        && path
                            .strip_prefix(&root.path)
                            .is_ok_and(|relative| relative.components().count() == 2) =>
                {
                    path.parent()
                        .and_then(|parent| parent.file_name())
                        .and_then(|name| name.to_str())
                        .filter(|id| native_id(id))
                        .map(str::to_owned)
                }
                _ => None,
            };
            if let Some(native_id) = id {
                found.insert(NativeEvidence {
                    native_id,
                    profile: Some(root.profile.clone()),
                });
            }
        }
    }
    found.into_iter().collect()
}
fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(target_os = "macos")]
pub(crate) fn process_started_at(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let count = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (count == size).then(|| {
        info.pbi_start_tvsec
            .saturating_mul(1000)
            .saturating_add(info.pbi_start_tvusec / 1000)
    })
}
#[cfg(target_os = "linux")]
pub(crate) fn process_started_at(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse::<u64>()
        .ok()?;
    let frequency = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if frequency <= 0 {
        return None;
    }
    let boot = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("btime ")
                .and_then(|time| time.parse::<u64>().ok())
        })?;
    Some(
        boot.saturating_mul(1000)
            .saturating_add(ticks.saturating_mul(1000) / frequency as u64),
    )
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process_started_at(_pid: u32) -> Option<u64> {
    None
}

pub(crate) fn identity_alive(pid: u32, start: Option<u64>) -> Option<bool> {
    if let Some(actual) = process_started_at(pid) {
        return start.map(|expected| actual == expected);
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Some(false);
        }
    }
    None
}
#[cfg(target_os = "macos")]
fn own_process(pid: u32) -> bool {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        ) == size
            && info.pbi_uid == libc::geteuid()
    }
}
#[cfg(target_os = "linux")]
fn own_process(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}"))
        .is_ok_and(|metadata| metadata.uid() == unsafe { libc::geteuid() })
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn own_process(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn process_arguments(pid: u32) -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut bytes = vec![0_u8; ARG_LIMIT];
    let mut length = bytes.len();
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            bytes.as_mut_ptr() as *mut libc::c_void,
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || length < 4 || length > ARG_LIMIT {
        return None;
    }
    bytes.truncate(length);
    // argc precedes the executable path, then padding, argv and environment.
    Some(bytes[4..].to_vec())
}
#[cfg(target_os = "linux")]
fn process_arguments(pid: u32) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take(ARG_LIMIT as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut environment = Vec::new();
    if let Ok(file) = std::fs::File::open(format!("/proc/{pid}/environ")) {
        let _ = file.take(ARG_LIMIT as u64).read_to_end(&mut environment);
    }
    bytes.push(0);
    bytes.extend(environment);
    Some(bytes)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_arguments(_pid: u32) -> Option<Vec<u8>> {
    None
}

/// Local route/link state only; no external connections or DNS requests.
pub(crate) async fn online_state() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let (ipv4, ipv6) = tokio::join!(mac_default_route(false), mac_default_route(true));
        combine_routes(ipv4, ipv6)
    }
    #[cfg(target_os = "linux")]
    {
        let routes = tokio::fs::read_to_string("/proc/net/route").await.ok()?;
        let ipv4: Vec<_> = routes
            .lines()
            .skip(1)
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.len() > 3
                    && fields[1] == "00000000"
                    && u32::from_str_radix(fields[3], 16).is_ok_and(|flags| flags & 1 != 0))
                .then(|| fields[0].to_owned())
            })
            .collect();
        if ipv4.iter().any(|name| interface_online(name) == Some(true)) {
            return Some(true);
        }
        let ipv6 = tokio::fs::read_to_string("/proc/net/ipv6_route")
            .await
            .ok()?;
        let ipv6: Vec<_> = ipv6
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.len() >= 10
                    && fields[0] == "00000000000000000000000000000000"
                    && fields[1] == "00"
                    && fields.last() != Some(&"lo"))
                .then(|| fields.last().unwrap().to_string())
            })
            .collect();
        let states: Vec<_> = ipv4
            .iter()
            .chain(&ipv6)
            .map(|name| interface_online(name))
            .collect();
        if states.contains(&Some(true)) {
            Some(true)
        } else if states.contains(&None) {
            None
        } else {
            Some(false)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
async fn mac_default_route(ipv6: bool) -> Option<bool> {
    let mut command = tokio::process::Command::new("/sbin/route");
    command
        .args(["-n", "get"])
        .env("LC_ALL", "C")
        .kill_on_drop(true);
    if ipv6 {
        command.arg("-inet6");
    }
    command.arg("default");
    let output = tokio::time::timeout(Duration::from_secs(1), command.output())
        .await
        .ok()?
        .ok()?;
    classify_mac_route(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        interface_online,
    )
}

#[cfg(any(target_os = "macos", test))]
fn classify_mac_route(
    success: bool,
    stdout: &str,
    stderr: &str,
    link: impl FnOnce(&str) -> Option<bool>,
) -> Option<bool> {
    if success {
        if let Some(interface) = stdout
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("interface:").map(str::trim))
            .filter(|name| !name.is_empty())
        {
            return link(interface);
        }
    }
    let error = stderr.to_ascii_lowercase();
    if [
        "not in table",
        "not found",
        "not been found",
        "network is unreachable",
        "no such process",
    ]
    .iter()
    .any(|message| error.contains(message))
    {
        Some(false)
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn combine_routes(ipv4: Option<bool>, ipv6: Option<bool>) -> Option<bool> {
    match (ipv4, ipv6) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

#[cfg(unix)]
fn interface_online(name: &str) -> Option<bool> {
    let mut first: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut first) } != 0 {
        return None;
    }
    let mut current = first;
    let mut result = None;
    while !current.is_null() {
        let interface = unsafe { &*current };
        if !interface.ifa_name.is_null()
            && unsafe { std::ffi::CStr::from_ptr(interface.ifa_name) }.to_bytes() == name.as_bytes()
        {
            let flags = interface.ifa_flags;
            result = Some(
                flags & libc::IFF_UP as u32 != 0
                    && flags & libc::IFF_RUNNING as u32 != 0
                    && flags & libc::IFF_LOOPBACK as u32 == 0,
            );
            if result == Some(true) {
                break;
            }
        }
        current = interface.ifa_next;
    }
    unsafe { libc::freeifaddrs(first) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_codex_services_are_not_interactive_pane_agents() {
        for arguments in [
            vec!["codex", "app-server", "--listen", "unix://"],
            vec!["codex", "app-server", "daemon", "pid-update-loop"],
            vec!["/bin/codex", "/bin/codex", "app-server"],
        ] {
            assert!(codex_service(
                &arguments.into_iter().map(str::to_owned).collect::<Vec<_>>()
            ));
        }
        assert!(!codex_service(&["codex".into()]));
        assert!(!codex_service(&[
            "codex".into(),
            "resume".into(),
            "native-id".into()
        ]));
    }
    #[test]
    fn ipv6_only_default_route_is_online_and_probe_failures_are_unknown() {
        assert_eq!(combine_routes(Some(false), Some(true)), Some(true));
        assert_eq!(combine_routes(Some(true), None), Some(true));
        assert_eq!(combine_routes(Some(false), Some(false)), Some(false));
        assert_eq!(combine_routes(Some(false), None), None);
        assert_eq!(combine_routes(None, None), None);
        assert_eq!(
            classify_mac_route(true, "route to: default\n interface: en0\n", "", |name| {
                assert_eq!(name, "en0");
                Some(true)
            }),
            Some(true)
        );
        assert_eq!(
            classify_mac_route(true, "route to: default\n interface: en0\n", "", |_| Some(
                false
            )),
            Some(false)
        );
        assert_eq!(
            classify_mac_route(true, "missing interface", "", |_| Some(true)),
            None
        );
        assert_eq!(
            classify_mac_route(false, "", "route: route has not been found", |_| Some(true)),
            Some(false)
        );
        assert_eq!(
            classify_mac_route(
                false,
                "",
                "route: writing to routing socket: not in table",
                |_| Some(true)
            ),
            Some(false)
        );
        assert_eq!(
            classify_mac_route(
                true,
                "",
                "route: writing to routing socket: not in table",
                |_| Some(true)
            ),
            Some(false)
        );
        assert_eq!(
            classify_mac_route(false, "", "route: Permission denied", |_| Some(true)),
            None
        );
    }
    #[test]
    fn process_tree_traversal_finds_grandchildren_and_rejects_same_cwd_guessing() {
        let rows = parse_process_rows(
            "1 0 ?? /sbin/init\n10 1 ttys001 -zsh\n11 10 ttys001 /usr/bin/node\n12 11 ttys001 /bin/codex\n20 1 ?? /bin/codex\n",
        )
        .unwrap();
        let roots = BTreeSet::from([10]);
        assert!(descendant_of(12, &roots, &rows));
        assert!(!descendant_of(20, &roots, &rows));
        assert!(owns_terminal(&rows[&12], "/dev/ttys001"));
        assert!(!owns_terminal(&rows[&20], "/dev/ttys001"));
        assert!(!owns_terminal(&rows[&12], "/dev/ttys002"));
        assert!(candidate_command("/usr/bin/node", true));
        assert!(!candidate_command("/usr/bin/node", false));
        assert_eq!(
            named_provider("/local/share/claude/versions/2.1.172"),
            Some(AgentProvider::Claude)
        );
    }
    #[test]
    fn arguments_extract_only_explicit_session_ids_and_profile_environment() {
        let (arguments,environment)=parse_argument_block(b"codex\0resume\0abc-123\0--api-key\0secret\0CODEX_HOME=/profiles/a\0OTHER_KEY=private\0");
        assert_eq!(
            resume_ids(AgentProvider::Codex, &arguments),
            BTreeSet::from(["abc-123".to_owned()])
        );
        assert_eq!(
            environment.get("CODEX_HOME").map(String::as_str),
            Some("/profiles/a")
        );
        assert!(!environment.contains_key("OTHER_KEY"));
        assert!(resume_ids(
            AgentProvider::Codex,
            &["codex".into(), "fork".into(), "parent-id".into()]
        )
        .is_empty());
        assert!(resume_ids(
            AgentProvider::Codex,
            &[
                "codex".into(),
                "prompt mentions resume".into(),
                "abc".into()
            ]
        )
        .is_empty());
    }
    #[test]
    fn open_files_need_provider_roots_and_native_ids() {
        let root = AgentRoot::new(
            AgentProvider::Codex,
            PathBuf::from("/profile/sessions"),
            PathBuf::from("/profile"),
        );
        let id = "019a1122-3344-5566-7788-99aabbccddee";
        let found = file_evidence(
            AgentProvider::Codex,
            &[
                PathBuf::from(format!(
                    "/profile/sessions/2026/rollout-2026-10-25-{id}.jsonl"
                )),
                PathBuf::from("/elsewhere/credentials.json"),
            ],
            &[root],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].native_id, id);
        assert_eq!(found[0].profile.as_deref(), Some("/profile"));
    }
    #[test]
    fn process_identity_is_os_start_time_and_prevents_pid_reuse() {
        let pid = std::process::id();
        let start = process_started_at(pid).expect("own Unix process identity");
        assert_eq!(identity_alive(pid, Some(start)), Some(true));
        assert_eq!(
            identity_alive(pid, Some(start.saturating_add(1))),
            Some(false)
        );
        assert_eq!(identity_alive(pid, None), None);
    }
    #[test]
    fn native_argument_probe_is_transient_and_snapshot_excludes_private_evidence() {
        let bytes = process_arguments(std::process::id()).expect("own process arguments");
        let (arguments, _) = parse_argument_block(&bytes);
        assert!(!arguments.is_empty());
        let process = RuntimeProcess {
            pid: 1,
            parent_pid: 0,
            process_started_at: Some(1),
            provider: AgentProvider::Codex,
            native_ids: vec!["id".into()],
            evidence: vec![NativeEvidence {
                native_id: "id".into(),
                profile: Some("private-probe-data".into()),
            }],
            wrapper: false,
        };
        let snapshot = serde_json::to_string(&process).unwrap();
        assert!(!snapshot.contains("private-probe-data"));
        assert!(!snapshot.contains("arguments"));
    }
    #[test]
    fn unmatched_global_tasks_are_not_process_exit_and_registry_absence_requires_complete_scan() {
        let identity = super::super::model::AgentSessionId {
            provider: AgentProvider::Claude,
            profile: "/profile".into(),
            native_id: "native".into(),
        };
        let task = super::super::index::fold([super::super::model::AgentEvent {
            id: "start".into(),
            at_ms: 1,
            sequence: 1,
            session: identity,
            parent: None,
            project: String::new(),
            model: String::new(),
            kind: super::super::model::AgentEventKind::Started {
                task_id: "turn".into(),
            },
        }])
        .tasks
        .remove(0);
        let mut inventory = RuntimeInventory {
            available: true,
            ..Default::default()
        };
        assert!(
            process_checks(&[task.clone()], &inventory, &[], &[], &BTreeMap::new(), 0).is_empty()
        );
        let seen = BTreeMap::from([(
            task.session_id.clone(),
            (std::process::id(), process_started_at(std::process::id())),
        )]);
        let incomplete = process_checks(&[task.clone()], &inventory, &[], &[], &seen, 0);
        assert!(!incomplete[&task.session_id].iter().any(|check| check.2));
        inventory
            .registry_complete_profiles
            .insert("/profile".into());
        let missing = process_checks(&[task.clone()], &inventory, &[], &[], &seen, 0);
        assert!(missing[&task.session_id].iter().all(|check| check.2));
    }
    #[test]
    fn cold_claude_history_is_closed_only_by_a_complete_listed_registry() {
        let identity = super::super::model::AgentSessionId {
            provider: AgentProvider::Claude,
            profile: "/profile".into(),
            native_id: "native".into(),
        };
        let task = super::super::index::fold([super::super::model::AgentEvent {
            id: "start".into(),
            at_ms: 10,
            sequence: 1,
            session: identity,
            parent: None,
            project: String::new(),
            model: String::new(),
            kind: super::super::model::AgentEventKind::Started {
                task_id: "turn".into(),
            },
        }])
        .tasks
        .remove(0);
        let mut inventory = RuntimeInventory {
            available: true,
            ..Default::default()
        };
        // A missing folder or partial read is not present in complete_profiles.
        assert!(
            process_checks(&[task.clone()], &inventory, &[], &[], &BTreeMap::new(), 100).is_empty()
        );
        inventory
            .registry_complete_profiles
            .insert("/profile".into());
        let closed = process_checks(&[task.clone()], &inventory, &[], &[], &BTreeMap::new(), 100);
        assert!(closed[&task.session_id].iter().all(|check| check.2));
        let mut new_task = task.clone();
        new_task.started_at_ms = 110;
        new_task.last_activity_at_ms = 110;
        assert!(
            process_checks(&[new_task], &inventory, &[], &[], &BTreeMap::new(), 100).is_empty()
        );
        inventory.available = false;
        assert!(
            process_checks(&[task.clone()], &inventory, &[], &[], &BTreeMap::new(), 100).is_empty()
        );
        inventory.available = true;
        inventory.registry.push(RegistryRecord {
            native_id: task.native_id.clone(),
            profile: task.profile.clone(),
            pid: std::process::id(),
            modified_ms: super::super::store::now_ms(),
        });
        let running = process_checks(&[task], &inventory, &[], &[], &BTreeMap::new(), 100);
        assert!(!running.values().flatten().any(|check| check.2));
    }
    #[test]
    fn registry_domains_and_partial_files_do_not_become_fake_processes() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path();
        std::fs::create_dir(profile.join("sessions")).unwrap();
        let pid = std::process::id();
        let path = profile.join(format!("sessions/{pid}.json"));
        std::fs::write(
            &path,
            serde_json::json!({"pid":pid,"sessionId":"native-session","pidDomain":"container"})
                .to_string(),
        )
        .unwrap();
        let roots = [AgentRoot::new(
            AgentProvider::Claude,
            profile.join("projects"),
            profile.to_path_buf(),
        )];
        assert!(registry_records(&roots).0.is_empty());
        std::fs::write(&path, "partial {").unwrap();
        let (records, errors, _) = registry_records(&roots);
        assert!(records.is_empty());
        assert!(!errors.is_empty());
        std::fs::write(&path,serde_json::json!({"pid":pid,"sessionId":"native-session","pidDomain":host_pid_domain()}).to_string()).unwrap();
        let (records, errors, _) = registry_records(&roots);
        assert_eq!(records.len(), 1);
        assert!(errors.is_empty());
    }
    #[tokio::test]
    async fn failed_probe_keeps_last_panes_and_bindings_but_marks_inventory_unknown() {
        let runtime = AgentRuntime::new();
        {
            let mut state = runtime.state.lock().await;
            state.inventory.available = true;
            state.inventory.updated_at_ms = 42;
            state.bindings.push(RuntimeBinding {
                session_id: "s".into(),
                tmux_session: "one".into(),
                pane_id: "%1".into(),
                pid: 1,
                process_started_at: Some(1),
            });
        }
        runtime.failed("unavailable").await;
        let inventory = runtime.inventory().await;
        assert!(!inventory.available);
        assert_eq!(inventory.updated_at_ms, 42);
        assert_eq!(runtime.state.lock().await.bindings.len(), 1);
    }
}
