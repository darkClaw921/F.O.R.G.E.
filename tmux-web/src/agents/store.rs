//! Background ingestion and coherent snapshots shared by indicators/statistics.
use super::{
    cache::{self, SourceState, Stamp},
    index::{self, AgentIndex},
    jsonl,
    model::*,
    opencode,
    pricing::PriceCatalog,
    sources::{self, AgentConfig},
};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

const READ_BUDGET: u64 = 16 * 1024 * 1024;
const NEW_FILES_PER_PASS: usize = 32;
const RECENT_MS: i64 = 30 * 60 * 1000;

#[derive(Default)]
struct IoState {
    sources: BTreeMap<String, SourceState>,
    updated_at_ms: i64,
    last_save_ms: i64,
    loaded: bool,
    dirty: bool,
    rebuild: bool,
    open_sessions: BTreeSet<String>,
    runtime_barriers: Vec<AgentEvent>,
}
#[derive(Clone, Default)]
struct Published {
    index: Arc<AgentIndex>,
    status: AgentStatus,
    unavailable: BTreeSet<String>,
}
#[derive(Default)]
struct Runtime {
    bindings: Vec<RuntimeBinding>,
    processes: BTreeMap<String, (Option<bool>, i64)>,
    /// The task a false observation closed; a new epoch can share its millisecond.
    process_epochs: BTreeMap<String, String>,
    offline_since: Option<(Instant, i64)>,
}
#[derive(Default)]
struct Pricing {
    catalog: PriceCatalog,
    loaded: bool,
    attempted: Option<Instant>,
    failed: bool,
    error: Option<String>,
}
struct Inner {
    config: RwLock<AgentConfig>,
    io: Arc<StdMutex<IoState>>,
    published: RwLock<Published>,
    runtime: RwLock<Runtime>,
    gate: Mutex<()>,
    pricing: RwLock<Pricing>,
    price_gate: Mutex<()>,
    rescan_requested: AtomicBool,
    discovery_requested: AtomicBool,
    configure_gate: Mutex<()>,
}
/// Cheap-clonable store; filesystem and SQLite work runs exclusively in spawn_blocking.
#[derive(Clone)]
pub struct AgentStatsStore {
    inner: Arc<Inner>,
}

impl AgentStatsStore {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: RwLock::new(config),
                io: Arc::new(StdMutex::new(IoState::default())),
                published: RwLock::new(Published::default()),
                runtime: RwLock::new(Runtime::default()),
                gate: Mutex::new(()),
                pricing: RwLock::new(Pricing::default()),
                price_gate: Mutex::new(()),
                rescan_requested: AtomicBool::new(false),
                discovery_requested: AtomicBool::new(false),
                configure_gate: Mutex::new(()),
            }),
        }
    }
    pub fn with_default_paths() -> Self {
        Self::new(AgentConfig::with_default_paths())
    }
    /// Load a valid cache and read recent logs first. Call from a background task.
    pub async fn initialize(&self) -> Result<()> {
        let _gate = self.inner.price_gate.lock().await;
        if !self.inner.pricing.read().await.loaded {
            let path = self.config().await.price_path;
            let cached =
                tokio::task::spawn_blocking(move || path.as_deref().and_then(PriceCatalog::load))
                    .await?;
            let mut pricing = self.inner.pricing.write().await;
            if let Some(catalog) = cached {
                pricing.catalog = catalog;
            }
            pricing.loaded = true;
        }
        drop(_gate);
        self.refresh(true).await
    }
    pub async fn config(&self) -> AgentConfig {
        self.inner.config.read().await.clone()
    }
    #[cfg(test)]
    pub(crate) async fn test_ingestion_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.gate.lock().await
    }
    pub async fn configure(&self, mut config: AgentConfig) -> Result<()> {
        // The atomic writer uses a PID-named temporary path. Serialize the
        // complete save/swap/rebuild transaction for this configuration file.
        let _configure = self.inner.configure_gate.lock().await;
        config.history_days = config.history_days.clamp(1, 3660);
        for root in &mut config.roots {
            root.path = sources::canonical(&root.path);
            root.profile = sources::canonical(std::path::Path::new(&root.profile))
                .to_string_lossy()
                .into_owned();
        }
        let saved = config.clone();
        tokio::task::spawn_blocking(move || saved.save()).await??;
        *self.inner.config.write().await = config;
        self.rescan().await
    }
    /// Spawn once at startup. Server startup never waits for history scans.
    pub fn spawn_worker(&self) -> tokio::task::JoinHandle<()> {
        let store = self.clone();
        tokio::spawn(async move {
            if let Err(error) = store.initialize().await {
                tracing::warn!(%error,"agent index initialization failed");
            }
            let price_store = store.clone();
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    timer.tick().await;
                    let _ = price_store.refresh_prices(false).await;
                }
            });
            let mut last_discovery = Instant::now();
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let discover = last_discovery.elapsed() >= Duration::from_secs(30);
                if discover {
                    last_discovery = Instant::now();
                }
                if let Err(error) = store.refresh(discover).await {
                    tracing::warn!(%error,"agent index refresh failed");
                }
            }
        })
    }
    pub async fn refresh(&self, discover: bool) -> Result<()> {
        // The journal and attention workers can tick at the same cadence.
        // Keep discovery pending when another refresh owns the gate, so a
        // resumed historical log cannot remain invisible indefinitely.
        if discover {
            self.inner
                .discovery_requested
                .store(true, Ordering::Release);
        }
        let Ok(_gate) = self.inner.gate.try_lock() else {
            return Ok(());
        };
        let discover = self.inner.discovery_requested.swap(false, Ordering::AcqRel);
        let result = self.refresh_inner(discover).await;
        if discover && result.is_err() {
            self.inner
                .discovery_requested
                .store(true, Ordering::Release);
        }
        result
    }
    async fn refresh_inner(&self, discover: bool) -> Result<()> {
        let config = self.config().await;
        let io = self.inner.io.clone();
        let now_ms = now_ms();
        let catalog = self.inner.pricing.read().await.catalog.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut result = ingest(io, &config, discover, now_ms)?;
            if let Some(index) = &mut result.index {
                apply_prices(index, &catalog);
            }
            Ok::<_, anyhow::Error>(result)
        })
        .await
        .context("agent ingestion worker")??;
        let mut published = self.inner.published.write().await;
        if let Some(index) = result.index {
            published.index = Arc::new(index);
        }
        published.status = result.status;
        published.unavailable = result.unavailable;
        Ok(())
    }
    pub async fn rescan(&self) -> Result<()> {
        // Serialize reset with the same ingestion gate so a previous pass
        // cannot overwrite a newly selected profile configuration.
        let _gate = self.inner.gate.lock().await;
        {
            let io = self.inner.io.clone();
            tokio::task::spawn_blocking(move || {
                let mut io = io
                    .lock()
                    .map_err(|_| anyhow::anyhow!("agent index lock poisoned"))?;
                io.sources.clear();
                io.loaded = true;
                io.dirty = true;
                io.rebuild = true;
                io.open_sessions.clear();
                Ok::<_, anyhow::Error>(())
            })
            .await??;
        }
        self.refresh_inner(true).await
    }
    /// Coalesce HTTP requests into one nonblocking destructive rebuild.
    pub async fn request_rescan(&self) -> AgentStatus {
        if self
            .inner
            .rescan_requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let store = self.clone();
            tokio::spawn(async move {
                if let Err(error) = store.rescan().await {
                    let mut p = store.inner.published.write().await;
                    p.status.errors.push(format!("rescan: {error:#}"));
                }
                store.inner.rescan_requested.store(false, Ordering::Release);
            });
        }
        self.status().await
    }
    /// Persist even a partially indexed, valid cursor snapshot on shutdown.
    pub async fn flush(&self) -> Result<()> {
        let _gate = self.inner.gate.lock().await;
        let config = self.config().await;
        let io = self.inner.io.clone();
        tokio::task::spawn_blocking(move || {
            let mut io = io
                .lock()
                .map_err(|_| anyhow::anyhow!("agent index lock poisoned"))?;
            if let Some(path) = config.cache_path {
                cache::save(&path, &io.sources, io.updated_at_ms, &io.runtime_barriers)?;
                io.dirty = false;
                io.last_save_ms = now_ms();
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
    pub async fn status(&self) -> AgentStatus {
        let mut status = self.inner.published.read().await.status.clone();
        if self.inner.rescan_requested.load(Ordering::Acquire) {
            status.scanning = true;
            status.state = "scanning".into();
        }
        status
    }
    /// Small journal-only metadata view for process matching and explicit ends.
    /// Does not clone usage history or turn a failed probe into completion.
    pub async fn recent_tasks(&self, since_ms: i64) -> Vec<AgentTask> {
        self.inner
            .published
            .read()
            .await
            .index
            .tasks
            .iter()
            .filter(|t| {
                (t.ended_at_ms.is_none() || t.completed.is_some())
                    && (t.last_activity_at_ms >= since_ms
                        || t.ended_at_ms.is_none() && t.started_at_ms >= since_ms)
            })
            .cloned()
            .collect()
    }
    /// Process matching also needs unfinished interrupted epochs. Runtime
    /// bindings live until their process exits, independently of Echo events.
    pub async fn runtime_tasks(&self, since_ms: i64) -> Vec<AgentTask> {
        self.inner
            .published
            .read()
            .await
            .index
            .tasks
            .iter()
            .filter(|task| task.completed.is_none() || task.last_activity_at_ms >= since_ms)
            .cloned()
            .collect()
    }
    pub async fn price_status(&self) -> PriceStatus {
        let pricing = self.inner.pricing.read().await;
        let mut status = pricing.catalog.status();
        status.error = pricing.error.clone();
        status
    }
    /// A failed or invalid download never replaces the last good catalogue.
    /// Automatic attempts are daily; failures wait six hours before retrying.
    pub async fn refresh_prices(&self, force: bool) -> Result<PriceStatus> {
        let Ok(_gate) = self.inner.price_gate.try_lock() else {
            return Ok(self.price_status().await);
        };
        let config = self.config().await;
        let Some(url) = config.price_update_url.clone() else {
            return Ok(self.price_status().await);
        };
        let catalog = {
            let mut pricing = self.inner.pricing.write().await;
            if !force
                && (!config.price_updates
                    || pricing.attempted.is_some_and(|at| {
                        at.elapsed()
                            < Duration::from_secs(if pricing.failed { 6 * 3600 } else { 24 * 3600 })
                    }))
            {
                return Ok({
                    let mut s = pricing.catalog.status();
                    s.error = pricing.error.clone();
                    s
                });
            }
            pricing.attempted = Some(Instant::now());
            pricing.catalog.clone()
        };
        match catalog.update(&url).await {
            Ok(new) => {
                let saved = new.clone();
                let path = config.price_path;
                let save_result = tokio::task::spawn_blocking(move || {
                    if let Some(path) = path {
                        saved.save(&path)?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await?;
                if let Err(error) = save_result {
                    let mut pricing = self.inner.pricing.write().await;
                    pricing.failed = true;
                    pricing.error = Some(error.to_string());
                    return Err(error);
                }
                let _ingestion = self.inner.gate.lock().await;
                {
                    let mut pricing = self.inner.pricing.write().await;
                    pricing.catalog = new.clone();
                    pricing.failed = false;
                    pricing.error = None;
                }
                // Repricing is independent of source cursors and cannot double counts.
                let current = self.inner.published.read().await.index.clone();
                let repriced = tokio::task::spawn_blocking(move || {
                    let mut index = (*current).clone();
                    apply_prices(&mut index, &new);
                    index
                })
                .await?;
                self.inner.published.write().await.index = Arc::new(repriced);
                Ok(self.price_status().await)
            }
            Err(error) => {
                let mut pricing = self.inner.pricing.write().await;
                pricing.failed = true;
                pricing.error = Some(error.to_string());
                Err(error)
            }
        }
    }
    /// Runtime inventory is metadata only. `None` never means process exit.
    pub async fn update_runtime(
        &self,
        bindings: Vec<RuntimeBinding>,
        processes: Vec<ProcessObservation>,
        online: Option<bool>,
    ) {
        self.update_runtime_at(bindings, processes, online, now_ms())
            .await
    }
    async fn update_runtime_at(
        &self,
        bindings: Vec<RuntimeBinding>,
        processes: Vec<ProcessObservation>,
        online: Option<bool>,
        now: i64,
    ) {
        // A cut and its replay share the ingestion gate: an older scan cannot
        // publish over the newly closed epoch, and all views see one index.
        let _ingestion = self.inner.gate.lock().await;
        let published = self.inner.published.read().await.clone();
        let mut runtime = self.inner.runtime.write().await;
        runtime.bindings = bindings;
        // Confirmed terminal observations outlive a disappearing PID and an
        // unavailable probe. A later journal activity makes the tombstone old.
        let mut observed: BTreeMap<_, _> = runtime
            .processes
            .iter()
            .filter(|(_, value)| value.0 == Some(false))
            .map(|(id, v)| (id.clone(), *v))
            .collect();
        for p in processes {
            let task = published
                .index
                .tasks
                .iter()
                .rev()
                .find(|task| task.session_id == p.session_id && task.ended_at_ms.is_none());
            if let Some((_, ended_at)) = observed.get(&p.session_id) {
                let resumed = task.is_some_and(|task| {
                    task.last_activity_at_ms > *ended_at
                        || runtime
                            .process_epochs
                            .get(&p.session_id)
                            .is_some_and(|id| id != &task.id)
                });
                if !resumed {
                    continue;
                }
                observed.remove(&p.session_id);
                if p.running == Some(false) {
                    if let Some(task) = task {
                        runtime
                            .process_epochs
                            .insert(p.session_id.clone(), task.id.clone());
                    }
                }
                observed.insert(p.session_id, (p.running, now));
                continue;
            }
            let at = runtime
                .processes
                .get(&p.session_id)
                .filter(|(old, _)| *old == p.running)
                .map(|(_, at)| *at)
                .unwrap_or(now);
            if p.running == Some(false) {
                if let Some(task) = task {
                    runtime
                        .process_epochs
                        .insert(p.session_id.clone(), task.id.clone());
                }
            }
            observed.insert(p.session_id, (p.running, at));
        }
        runtime.processes = observed;
        let outage = runtime
            .offline_since
            .filter(|(since, _)| online != Some(true) && since.elapsed() >= Duration::from_secs(20))
            .map(|(_, at)| at);
        match online {
            Some(true) => runtime.offline_since = None,
            Some(false) if runtime.offline_since.is_none() => {
                runtime.offline_since = Some((Instant::now(), now))
            }
            _ => {}
        }
        let exits: BTreeMap<_, _> = runtime
            .processes
            .iter()
            .filter_map(|(id, (running, at))| {
                (*running == Some(false)).then_some((id.clone(), *at))
            })
            .collect();
        let exit_epochs = runtime.process_epochs.clone();
        drop(runtime);
        {
            let candidates: Vec<_> = published
                .index
                .tasks
                .iter()
                .filter_map(|task| {
                    if task.ended_at_ms.is_some() {
                        return None;
                    }
                    if let Some(at) = exits.get(&task.session_id).filter(|at| {
                        task.last_activity_at_ms <= **at
                            && exit_epochs
                                .get(&task.session_id)
                                .is_none_or(|id| id == &task.id)
                    }) {
                        return Some((task.clone(), true, *at, 0));
                    }
                    outage
                        .filter(|_| {
                            task.provider == AgentProvider::Claude
                                && task.running_commands.is_empty()
                        })
                        .map(|offline_since| (task.clone(), false, now, offline_since))
                })
                .collect();
            if !candidates.is_empty() {
                let io = self.inner.io.clone();
                let config = self.config().await;
                let roots: BTreeMap<_, _> = published
                    .index
                    .usage
                    .iter()
                    .map(|u| (u.session_id.clone(), u.root_session_id.clone()))
                    .collect();
                let result = tokio::task::spawn_blocking(move || {
                    let mut io = io
                        .lock()
                        .map_err(|_| anyhow::anyhow!("agent index lock poisoned"))?;
                    let mut changed = false;
                    for (task, confirmed_exit, cut_at, offline_since) in candidates {
                        let replying = !confirmed_exit
                            && io.sources.values().flat_map(|s| s.events()).any(|event| {
                                event.at_ms >= offline_since
                                    && event.at_ms <= cut_at
                                    && !event.model.is_empty()
                                    && matches!(
                                        &event.kind,
                                        AgentEventKind::Usage {
                                            tokens: Some(_),
                                            ..
                                        }
                                    )
                                    && {
                                        let id = event.session.key();
                                        roots.get(&id).unwrap_or(&id) == &task.session_id
                                    }
                            });
                        if replying {
                            continue;
                        }
                        let id = if confirmed_exit {
                            format!("runtime:process-exit:{}:{cut_at}", task.id)
                        } else {
                            format!("runtime:offline:{}:{offline_since}", task.id)
                        };
                        if io.runtime_barriers.iter().any(|event| event.id == id) {
                            continue;
                        }
                        let sequence = io
                            .sources
                            .values()
                            .flat_map(|s| s.events())
                            .filter(|event| {
                                event.at_ms == cut_at && {
                                    let id = event.session.key();
                                    roots.get(&id).unwrap_or(&id) == &task.session_id
                                }
                            })
                            .map(|event| event.sequence)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1);
                        io.runtime_barriers.push(AgentEvent {
                            id,
                            at_ms: cut_at,
                            sequence,
                            session: AgentSessionId {
                                provider: task.provider,
                                profile: task.profile,
                                native_id: task.native_id,
                            },
                            parent: None,
                            project: task.project,
                            model: task.model,
                            kind: AgentEventKind::Interrupted {
                                task_id: task.id,
                                offline_since_ms: offline_since,
                                confirmed_exit,
                            },
                        });
                        changed = true;
                    }
                    if changed {
                        io.rebuild = true;
                        io.dirty = true;
                        io.updated_at_ms = now;
                        // Persist the metadata cut immediately, even while a
                        // background history import still has pending files.
                        if let Some(path) = &config.cache_path {
                            if let Err(error) = cache::save(
                                path,
                                &io.sources,
                                io.updated_at_ms,
                                &io.runtime_barriers,
                            ) {
                                tracing::warn!(%error,"cannot persist agent runtime interruption");
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(changed)
                })
                .await;
                match result {
                    Ok(Ok(true)) => {
                        if let Err(error) = self.refresh_inner(false).await {
                            tracing::warn!(%error,"cannot replay agent runtime interruption");
                        }
                    }
                    Ok(Ok(false)) => {}
                    error => tracing::warn!(?error, "cannot record agent runtime interruption"),
                }
            }
        }
    }
    pub async fn set_runtime(&self, bindings: Vec<RuntimeBinding>) {
        let mut runtime = self.inner.runtime.write().await;
        runtime.bindings = bindings;
    }
    pub async fn snapshot(&self) -> AgentSnapshot {
        let now = now_ms();
        let published = self.inner.published.read().await;
        let runtime = self.inner.runtime.read().await;
        let mut snapshot = AgentSnapshot {
            now_ms: now,
            updated_at_ms: published.status.updated_at_ms,
            ..Default::default()
        };
        for task in &published.index.tasks {
            let task = runtime_task(
                task,
                now,
                &runtime,
                &published.unavailable,
                &published.index.usage,
            );
            if task.ended_at_ms.is_some()
                || now - task.last_activity_at_ms >= task.provider.resume_ms()
            {
                continue;
            }
            match task.state {
                AgentTaskState::Working => {
                    snapshot.working_count += 1;
                    *snapshot.by_provider.entry(task.provider).or_default() += 1;
                    snapshot.live.push(task);
                }
                AgentTaskState::Waiting => snapshot.waiting.push(task),
                AgentTaskState::Unknown => snapshot.unknown.push(task),
                AgentTaskState::Ended => {}
            }
        }
        snapshot.live.sort_by_key(|t| t.started_at_ms);
        snapshot
    }
    /// Returned index is an immutable coherent copy for route-level calculations.
    pub async fn index_snapshot(&self) -> AgentIndex {
        let now = now_ms();
        let published = self.inner.published.read().await;
        let runtime = self.inner.runtime.read().await;
        let mut index = (*published.index).clone();
        index.tasks = index
            .tasks
            .iter()
            .map(|t| runtime_task(t, now, &runtime, &published.unavailable, &index.usage))
            .collect();
        index
    }
    pub async fn summary(&self, query: AgentQuery) -> Result<AgentSummary> {
        let index = self.index_snapshot().await;
        let status = self.status().await;
        super::summary::summary(
            &index,
            &query,
            now_ms(),
            status.updated_at_ms,
            self.price_status().await,
        )
    }
    pub async fn projects(&self, query: AgentQuery) -> Result<Vec<AgentShare>> {
        super::summary::projects(&self.index_snapshot().await, &query, now_ms())
    }
    pub async fn sessions(&self, query: AgentQuery) -> Result<Vec<AgentTask>> {
        super::summary::sessions(&self.index_snapshot().await, &query, now_ms())
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn apply_prices(index: &mut AgentIndex, catalog: &PriceCatalog) {
    for record in &mut index.usage {
        record.estimated_cost_usd = catalog.price(record);
    }
    // Live journal updates reprice the coherent index. Restrict each task to
    // its own root instead of rescanning every historical session's usage.
    let mut root_usage: BTreeMap<&str, Vec<&UsageRecord>> = BTreeMap::new();
    for record in &index.usage {
        if record.provider != AgentProvider::Copilot {
            root_usage
                .entry(record.root_session_id.as_str())
                .or_default()
                .push(record);
        }
    }
    for task in &mut index.tasks {
        task.estimated_cost_usd = None;
        task.unpriced_requests = 0;
        for record in root_usage
            .get(task.session_id.as_str())
            .into_iter()
            .flatten()
            .filter(|r| {
                r.at_ms >= task.started_at_ms && task.ended_at_ms.is_none_or(|end| r.at_ms <= end)
            })
        {
            if let Some(cost) = record.estimated_cost_usd {
                *task.estimated_cost_usd.get_or_insert(0.) += cost;
            } else if record.reported_cost_usd.is_none() {
                task.unpriced_requests = task.unpriced_requests.saturating_add(record.requests);
            }
        }
    }
}
fn runtime_task(
    task: &AgentTask,
    now: i64,
    runtime: &Runtime,
    unavailable: &BTreeSet<String>,
    _usage: &[UsageRecord],
) -> AgentTask {
    let mut task = index::task_at(task, now);
    task.bindings = runtime
        .bindings
        .iter()
        .filter(|b| b.session_id == task.session_id)
        .cloned()
        .collect();
    if task.ended_at_ms.is_some() {
        return task;
    }
    if let Some((Some(false), at)) = runtime.processes.get(&task.session_id) {
        if task.last_activity_at_ms <= *at
            && runtime
                .process_epochs
                .get(&task.session_id)
                .is_none_or(|id| id == &task.id)
        {
            task.state = AgentTaskState::Ended;
            task.ended_at_ms = Some((*at).max(task.started_at_ms));
            task.completed = Some(false);
            task.duration_ms = Some(at.saturating_sub(task.started_at_ms).max(0) as u64);
            task.elapsed_ms = task.duration_ms.unwrap_or(0);
        }
    } else if unavailable.contains(&task.session_id)
        || matches!(runtime.processes.get(&task.session_id), Some((None, _)))
    {
        task.state = AgentTaskState::Unknown;
    }
    if task.ended_at_ms.is_some() {
        return task;
    }
    if task.state == AgentTaskState::Unknown {
        task.elapsed_ms = task
            .last_activity_at_ms
            .saturating_sub(task.started_at_ms)
            .max(0) as u64;
    }
    task
}
struct IngestResult {
    index: Option<AgentIndex>,
    status: AgentStatus,
    unavailable: BTreeSet<String>,
}
fn stamp(path: &std::path::Path, sqlite: bool) -> std::io::Result<Stamp> {
    let m = std::fs::metadata(path)?;
    if !m.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        m.ino()
    };
    #[cfg(not(unix))]
    let inode = 0;
    let wal = sqlite
        .then(|| std::fs::metadata(format!("{}-wal", path.display())).ok())
        .flatten();
    Ok(Stamp {
        inode,
        len: m.len(),
        modified_ms: sources::modified_ms(&m),
        wal_len: wal.as_ref().map(|m| m.len()).unwrap_or(0),
        wal_modified_ms: wal.as_ref().map(sources::modified_ms).unwrap_or(0),
    })
}
fn ingest(
    io: Arc<StdMutex<IoState>>,
    config: &AgentConfig,
    discover: bool,
    now: i64,
) -> Result<IngestResult> {
    let mut io = io
        .lock()
        .map_err(|_| anyhow::anyhow!("agent index lock poisoned"))?;
    let since = now - config.history_days.clamp(1, 3660) * 86_400_000;
    let mut changed = io.rebuild;
    io.rebuild = false;
    let mut errors = Vec::new();
    if !io.loaded {
        if let Some(cached) = config.cache_path.as_ref().and_then(|p| cache::load(p)) {
            io.sources = cached.sources;
            io.runtime_barriers = cached.runtime_barriers;
            io.updated_at_ms = cached.updated_at_ms;
            changed = true;
        }
        io.loaded = true;
    }
    if !config.enabled {
        io.rebuild = true;
        return Ok(IngestResult {
            index: Some(AgentIndex::default()),
            status: AgentStatus {
                state: "disabled".into(),
                history_days: config.history_days,
                ..Default::default()
            },
            unavailable: BTreeSet::new(),
        });
    }
    if discover {
        let count = io.runtime_barriers.len();
        io.runtime_barriers.retain(|event| event.at_ms >= since);
        if count != io.runtime_barriers.len() {
            changed = true;
        }
        let roots = config.all_roots();
        let (files, failures) = sources::discover(&roots, since);
        errors.extend(failures);
        let mut accepted = BTreeSet::new();
        for descriptor in files {
            let key = descriptor.path.to_string_lossy().into_owned();
            accepted.insert(key.clone());
            io.sources
                .entry(key)
                .and_modify(|source| source.descriptor.modified_ms = descriptor.modified_ms)
                .or_insert_with(|| SourceState::new(descriptor));
        }
        let removed:Vec<String>=io.sources.iter().filter(|(path,source)|!accepted.contains(*path)&&(!roots.iter().any(|r|r.provider==source.descriptor.provider&&source.descriptor.path.starts_with(&r.path))||source.descriptor.modified_ms<since||matches!(std::fs::metadata(&source.descriptor.path),Err(e) if e.kind()==std::io::ErrorKind::NotFound))).map(|(p,_)|p.clone()).collect();
        for path in removed {
            io.sources.remove(&path);
            changed = true;
        }
    }
    let mut order: Vec<String> = io.sources.keys().cloned().collect();
    order.sort_by_key(|p| std::cmp::Reverse(io.sources[p].descriptor.modified_ms));
    let open_sessions = io.open_sessions.clone();
    let mut new_count = 0;
    let mut bytes_read = 0;
    let mut read_files = 0;
    let mut dirty = io.dirty;
    for path in order {
        let source = io.sources.get_mut(&path).expect("known source");
        let unparsed = source.jsonl.is_none() && source.opencode.is_none();
        if unparsed && new_count >= NEW_FILES_PER_PASS {
            continue;
        }
        let relevant = discover
            || unparsed
            || source.more
            || source.descriptor.modified_ms >= now - RECENT_MS
            || !source.session_ids.is_disjoint(&open_sessions);
        if !relevant {
            continue;
        }
        let sqlite = source.descriptor.provider == AgentProvider::Opencode;
        let signature = match stamp(&source.descriptor.path, sqlite) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                source.error = Some("source disappeared".into());
                changed = true;
                continue;
            }
            Err(e) => {
                source.error = Some(e.to_string());
                continue;
            }
        };
        if !source.more && source.stamp.as_ref() == Some(&signature) && source.error.is_none() {
            continue;
        }
        if bytes_read >= READ_BUDGET && !sqlite {
            continue;
        }
        if unparsed {
            new_count += 1;
        }
        read_files += 1;
        let result = if sqlite {
            opencode::read(
                &source.descriptor.path,
                &source.descriptor.profile,
                since,
                source.opencode.as_ref(),
            )
            .map(|cursor| {
                let changed = source.events() != cursor.events;
                source.opencode = Some(cursor);
                source.more = false;
                changed
            })
        } else {
            jsonl::read(
                &source.descriptor,
                source.jsonl.as_ref(),
                since,
                READ_BUDGET.saturating_sub(bytes_read),
            )
            .map(|result| {
                let changed = source.events() != result.cursor.events;
                bytes_read = bytes_read.saturating_add(result.bytes_read);
                source.more = result.more;
                source.jsonl = Some(result.cursor);
                changed
            })
        };
        match result {
            Ok(events_changed) => {
                source.descriptor.modified_ms = signature.modified_ms;
                source.stamp = Some(signature);
                source.error = None;
                source.refresh_session_ids();
                if events_changed {
                    changed = true;
                }
                dirty = true;
            }
            Err(e) => source.error = Some(format!("{e:#}")),
        }
    }
    let mut available = BTreeSet::new();
    let mut unavailable = BTreeSet::new();
    if discover {
        for source in io.sources.values_mut() {
            if let Some(cursor) = &mut source.jsonl {
                if jsonl::prune(cursor, since) {
                    changed = true;
                    dirty = true;
                    source.refresh_session_ids();
                }
            }
        }
    }
    for source in io.sources.values() {
        if let Some(error) = &source.error {
            if errors.len() < 20 {
                errors.push(format!("{}: {error}", source.descriptor.path.display()));
            }
        }
        if source.error.is_some() {
            unavailable.extend(source.event_session_ids.iter().cloned());
        } else {
            available.extend(source.event_session_ids.iter().cloned());
        }
    }
    unavailable.retain(|s| !available.contains(s));
    let pending = io
        .sources
        .values()
        .any(|s| s.error.is_none() && (s.more || s.jsonl.is_none() && s.opencode.is_none()));
    if changed {
        io.updated_at_ms = now;
    }
    io.dirty = dirty || changed;
    if io.dirty && !pending && (io.last_save_ms == 0 || now - io.last_save_ms >= 300_000) {
        if let Some(path) = &config.cache_path {
            match cache::save(path, &io.sources, io.updated_at_ms, &io.runtime_barriers) {
                Ok(()) => {
                    io.last_save_ms = now;
                    io.dirty = false;
                }
                Err(e) => errors.push(format!("cache: {e:#}")),
            }
        }
    }
    let index = changed.then(|| {
        index::fold_refs(
            io.sources
                .values()
                .flat_map(|s| s.events().iter())
                .filter(|e| e.at_ms >= since || !matches!(e.kind, AgentEventKind::Usage { .. }))
                .chain(io.runtime_barriers.iter()),
        )
    });
    if let Some(index) = &index {
        io.open_sessions = index
            .tasks
            .iter()
            .filter(|t| {
                t.ended_at_ms.is_none() && now - t.last_activity_at_ms < t.provider.resume_ms()
            })
            .map(|t| t.session_id.clone())
            .collect();
    }
    Ok(IngestResult {
        index,
        status: AgentStatus {
            state: if pending { "scanning" } else { "ready" }.into(),
            scanning: pending,
            files: io.sources.len(),
            scanned_files: io
                .sources
                .values()
                .filter(|s| s.jsonl.is_some() || s.opencode.is_some())
                .count(),
            updated_at_ms: io.updated_at_ms,
            errors,
            history_days: config.history_days,
            bytes_read,
            read_files,
            profiles: io
                .sources
                .values()
                .map(|source| source.descriptor.profile.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        },
        unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    #[ignore = "manual private-copy cache peak-memory comparison; FORGE_AGENT_PERF_SAVE_BASELINE=1 selects the previous clone path"]
    fn benchmark_cache_snapshot_peak() {
        let path = std::env::var_os("FORGE_AGENT_PERF_CACHE")
            .map(std::path::PathBuf::from)
            .expect("private cache-copy path");
        let snapshot = cache::load(&path).expect("valid metadata cache");
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("saved.json");
        let start = Instant::now();
        let baseline = std::env::var_os("FORGE_AGENT_PERF_SAVE_BASELINE").is_some();
        if baseline {
            let copied = cache::CacheSnapshot {
                version: cache::CACHE_VERSION,
                updated_at_ms: snapshot.updated_at_ms,
                sources: snapshot.sources.clone(),
                runtime_barriers: snapshot.runtime_barriers.clone(),
            };
            forge_atomic::write_bytes_atomic(&target, &serde_json::to_vec(&copied).unwrap())
                .unwrap();
        } else {
            cache::save(
                &target,
                &snapshot.sources,
                snapshot.updated_at_ms,
                &snapshot.runtime_barriers,
            )
            .unwrap();
        }
        eprintln!(
            "CACHE_SAVE_PERF baseline={baseline} bytes={} elapsed_ms={}",
            std::fs::metadata(target).unwrap().len(),
            start.elapsed().as_millis()
        );
    }
    #[test]
    fn cached_owner_availability_preserves_parent_only_and_healthy_duplicate_semantics() {
        let now = now_ms();
        let owner = AgentSessionId {
            provider: AgentProvider::Codex,
            profile: "/profile".into(),
            native_id: "child".into(),
        };
        let parent = AgentSessionId {
            native_id: "root".into(),
            ..owner.clone()
        };
        let mut source = SourceState::new(sources::SourceDescriptor {
            path: "/not-read".into(),
            provider: owner.provider,
            profile: owner.profile.clone(),
            modified_ms: now - RECENT_MS - 1,
        });
        source.jsonl = Some(jsonl::JsonlCursor {
            events: vec![AgentEvent {
                id: "activity".into(),
                at_ms: now,
                session: owner.clone(),
                sequence: 1,
                parent: Some(parent.clone()),
                project: "/project".into(),
                model: "model".into(),
                kind: AgentEventKind::Active,
            }],
            ..Default::default()
        });
        source.more = false;
        source.error = Some("unavailable".into());
        source.refresh_session_ids();
        let io = Arc::new(StdMutex::new(IoState {
            sources: BTreeMap::from([("bad".into(), source.clone())]),
            loaded: true,
            ..Default::default()
        }));
        let config = AgentConfig {
            discover_defaults: false,
            ..Default::default()
        };
        let result = ingest(io.clone(), &config, false, now).unwrap();
        assert_eq!(result.unavailable, BTreeSet::from([owner.key()]));
        assert!(!result.unavailable.contains(&parent.key()));
        source.error = None;
        io.lock()
            .unwrap()
            .sources
            .insert("healthy-copy".into(), source);
        assert!(ingest(io, &config, false, now)
            .unwrap()
            .unavailable
            .is_empty());
    }
    #[test]
    #[ignore = "manual read-only benchmark with FORGE_AGENT_PERF_CACHE pointing to a private cache copy"]
    fn benchmark_history_refresh_snapshot() {
        let path = std::env::var_os("FORGE_AGENT_PERF_CACHE")
            .map(std::path::PathBuf::from)
            .expect("set FORGE_AGENT_PERF_CACHE to a private metadata-cache copy");
        let snapshot = cache::load(&path).expect("valid metadata cache");
        let now = snapshot.updated_at_ms;
        let events: usize = snapshot.sources.values().map(|s| s.events().len()).sum();
        if let Some(cursor) = snapshot
            .sources
            .values()
            .filter_map(|source| source.jsonl.as_ref())
            .max_by_key(|cursor| cursor.events.len())
        {
            let start = Instant::now();
            for _ in 0..3 {
                std::hint::black_box(cursor.clone());
            }
            eprintln!(
                "CURSOR_CLONE_PERF events={} clone3_ms={}",
                cursor.events.len(),
                start.elapsed().as_millis()
            );
        }
        let mut state = IoState {
            sources: snapshot.sources,
            runtime_barriers: snapshot.runtime_barriers,
            updated_at_ms: now,
            loaded: true,
            ..Default::default()
        };
        // Benchmark only the immutable contributions: never open the original
        // journals and never write the source cache or user configuration.
        for source in state.sources.values_mut() {
            source.descriptor.modified_ms = now - RECENT_MS - 1;
            source.more = false;
        }
        let io = Arc::new(StdMutex::new(state));
        let config = AgentConfig {
            discover_defaults: false,
            ..Default::default()
        };
        let start = Instant::now();
        for _ in 0..3 {
            let result = ingest(io.clone(), &config, false, now).unwrap();
            assert_eq!(result.status.read_files, 0);
            assert_eq!(result.status.bytes_read, 0);
            assert!(result.index.is_none());
        }
        let unchanged = start.elapsed();
        io.lock().unwrap().rebuild = true;
        let start = Instant::now();
        let result = ingest(io.clone(), &config, false, now).unwrap();
        let fold = start.elapsed();
        let mut index = result.index.unwrap();
        let start = Instant::now();
        apply_prices(&mut index, &PriceCatalog::default());
        let prices = start.elapsed();
        let start = Instant::now();
        let temp = tempfile::tempdir().unwrap();
        let state = io.lock().unwrap();
        cache::save(
            &temp.path().join("snapshot.json"),
            &state.sources,
            now,
            &state.runtime_barriers,
        )
        .unwrap();
        let save = start.elapsed();
        eprintln!("HISTORY_PERF events={events} tasks={} usage={} unchanged3_ms={} fold_ms={} prices_ms={} save_ms={}", index.tasks.len(), index.usage.len(), unchanged.as_millis(), fold.as_millis(), prices.as_millis(), save.as_millis());
    }
    fn config(dir: &std::path::Path) -> AgentConfig {
        AgentConfig {
            discover_defaults: false,
            roots: vec![super::super::sources::AgentRoot::new(
                AgentProvider::Codex,
                dir.join("sessions"),
                dir.to_path_buf(),
            )],
            cache_path: Some(dir.join("cache.json")),
            ..Default::default()
        }
    }
    fn write_log(dir: &std::path::Path, ended: bool) -> std::path::PathBuf {
        let logs = dir.join("sessions");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("a.jsonl");
        let now = now_ms();
        let at = chrono::DateTime::from_timestamp_millis(now - 10_000)
            .unwrap()
            .to_rfc3339();
        let mut lines = vec![
            serde_json::json!({"type":"session_meta","timestamp":at,"payload":{"id":"session-a","cwd":"/project","source":"cli"}}),
            serde_json::json!({"type":"event_msg","timestamp":at,"payload":{"type":"task_started","turn_id":"turn-a"}}),
            serde_json::json!({"type":"token_usage_record","timestamp":at,"payload":{"response_id":"response-a","usage":{"input_tokens":100,"output_tokens":20}}}),
            serde_json::json!({"type":"response_item","timestamp":at,"payload":{"secret":"PRIVATE_PROMPT_AND_TOOL_OUTPUT"}}),
        ];
        if ended {
            lines.push(serde_json::json!({"type":"event_msg","timestamp":chrono::DateTime::from_timestamp_millis(now-5000).unwrap().to_rfc3339(),"payload":{"type":"task_complete","duration_ms":5000}}));
        }
        std::fs::write(
            &path,
            lines
                .into_iter()
                .map(|v| format!("{v}\n"))
                .collect::<String>(),
        )
        .unwrap();
        path
    }
    #[tokio::test]
    async fn contended_discovery_is_retried_by_next_tick_and_finds_resumed_old_logs() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_log(dir.path(), true);
        let old = now_ms() - 2 * 86_400_000;
        let history = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let mut row: serde_json::Value = serde_json::from_str(line).unwrap();
                row["timestamp"] = serde_json::json!(chrono::DateTime::from_timestamp_millis(
                    old + i as i64 * 1000
                )
                .unwrap()
                .to_rfc3339());
                format!("{row}\n")
            })
            .collect::<String>();
        std::fs::write(&path, history).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(old as u64)),
            )
            .unwrap();
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 0);

        // Resume an already indexed historical session while also creating a
        // new journal. Neither is eligible for an ordinary metadata-only tick.
        let at = chrono::Utc::now().to_rfc3339();
        let resumed = serde_json::json!({"type":"event_msg","timestamp":at,
            "payload":{"type":"task_started","turn_id":"resumed-turn"}});
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap(),
            "{resumed}"
        )
        .unwrap();
        let fresh = dir.path().join("fresh");
        let fresh_log = write_log(&fresh, false);
        let fresh_content = std::fs::read_to_string(fresh_log)
            .unwrap()
            .replace("session-a", "session-b")
            .replace("response-a", "response-b");
        std::fs::write(path.with_file_name("b.jsonl"), fresh_content).unwrap();
        store.refresh(false).await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 0);

        let guard = store.inner.gate.lock().await;
        store.refresh(true).await.unwrap();
        store.refresh(false).await.unwrap();
        drop(guard);
        store.refresh(false).await.unwrap();
        let live = store.snapshot().await;
        assert_eq!(live.working_count, 2);
        assert!(live
            .live
            .iter()
            .any(|task| task.id.contains("resumed-turn")));
        assert_eq!(store.status().await.files, 2);
        assert!(!store.inner.discovery_requested.load(Ordering::Acquire));
        assert!(
            store.inner.io.lock().unwrap().sources
                [&sources::canonical(&path).to_string_lossy().to_string()]
                .descriptor
                .modified_ms
                > old
        );
    }
    #[tokio::test]
    async fn unchanged_live_ticks_read_no_journal_bytes_and_cache_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_log(dir.path(), true);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        assert_eq!(store.index_snapshot().await.usage.len(), 1);
        assert_eq!(store.index_snapshot().await.tasks.len(), 1);
        let cache = std::fs::read_to_string(dir.path().join("cache.json")).unwrap();
        assert!(!cache.contains("PRIVATE_PROMPT"));
        store.refresh(false).await.unwrap();
        let s = store.status().await;
        assert_eq!(s.bytes_read, 0);
        assert_eq!(s.read_files, 0);
        let restarted = AgentStatsStore::new(config(dir.path()));
        restarted.initialize().await.unwrap();
        let a = store.index_snapshot().await;
        let b = restarted.index_snapshot().await;
        assert_eq!(
            serde_json::to_value(a.usage).unwrap(),
            serde_json::to_value(b.usage).unwrap()
        );
        assert_eq!(a.tasks[0].duration_ms, b.tasks[0].duration_ms);
        assert!(path.is_file());
    }
    #[tokio::test]
    async fn status_profiles_include_historical_accounts_and_deduplicate_archives() {
        use axum::{
            body::{to_bytes, Body},
            http::Request,
            Router,
        };
        use tower::ServiceExt;
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("active-profile");
        let historical = dir.path().join("historical-profile");
        write_log(&active, false);
        let history_log = write_log(&historical, true);
        let old = now_ms() - 3 * 86_400_000;
        let history = std::fs::read_to_string(&history_log)
            .unwrap()
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let mut row: serde_json::Value = serde_json::from_str(line).unwrap();
                row["timestamp"] = serde_json::json!(chrono::DateTime::from_timestamp_millis(
                    old + i as i64 * 1000
                )
                .unwrap()
                .to_rfc3339());
                format!("{row}\n")
            })
            .collect::<String>();
        std::fs::write(&history_log, &history).unwrap();
        let archives = historical.join("archived_sessions");
        std::fs::create_dir_all(&archives).unwrap();
        std::fs::write(archives.join("archived.jsonl"), history).unwrap();
        let config = AgentConfig {
            discover_defaults: false,
            roots: vec![
                sources::AgentRoot::new(
                    AgentProvider::Codex,
                    active.join("sessions"),
                    active.clone(),
                ),
                sources::AgentRoot::new(
                    AgentProvider::Codex,
                    historical.join("sessions"),
                    historical.clone(),
                ),
                sources::AgentRoot::new(AgentProvider::Codex, archives, historical.clone()),
            ],
            ..Default::default()
        };
        let store = AgentStatsStore::new(config);
        store.initialize().await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 1);
        assert_eq!(
            store.index_snapshot().await.tasks.len(),
            2,
            "archive copy must not create another session"
        );
        // The profile selector must use indexed accounts, not just the extra
        // roots currently exposed in settings or the one live account.
        store.inner.config.write().await.roots.clear();
        assert!(store.config().await.roots.is_empty());
        let expected: Vec<String> = [active, historical]
            .into_iter()
            .map(|path| sources::canonical(&path).to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(store.status().await.profiles, expected);
        let mut state = crate::tests::test_app_state(None);
        state.agents = store;
        let response = super::super::routes::register_routes(Router::new())
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/api/agents/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let json: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(json["profiles"], serde_json::json!(expected));
        assert_eq!(json["files"], 3);
    }
    #[tokio::test]
    async fn archive_duplicate_and_deletion_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_log(dir.path(), true);
        let copy = path.with_file_name("archive.jsonl");
        std::fs::copy(&path, &copy).unwrap();
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        assert_eq!(store.index_snapshot().await.usage.len(), 1);
        assert_eq!(store.index_snapshot().await.tasks.len(), 1);
        std::fs::remove_file(path).unwrap();
        store.refresh(true).await.unwrap();
        assert_eq!(store.index_snapshot().await.usage.len(), 1);
        std::fs::remove_file(copy).unwrap();
        store.refresh(true).await.unwrap();
        assert!(store.index_snapshot().await.usage.is_empty());
    }
    #[tokio::test]
    async fn reset_empty_roots_and_disable_enable_replace_published_index() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), false);
        let c = config(dir.path());
        let store = AgentStatsStore::new(c.clone());
        store.initialize().await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 1);
        let mut disabled = c.clone();
        disabled.enabled = false;
        store.configure(disabled).await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 0);
        store.configure(c).await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 1);
        store
            .configure(AgentConfig {
                discover_defaults: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(store.index_snapshot().await.tasks.is_empty());
    }
    #[tokio::test]
    async fn confirmed_exit_survives_empty_and_unavailable_probes() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), false);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        let id = store.snapshot().await.live[0].session_id.clone();
        store
            .update_runtime(
                vec![],
                vec![ProcessObservation {
                    session_id: id.clone(),
                    running: Some(false),
                }],
                None,
            )
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
        store.update_runtime(vec![], vec![], None).await;
        assert_eq!(store.snapshot().await.working_count, 0);
        store
            .update_runtime(
                vec![],
                vec![ProcessObservation {
                    session_id: id,
                    running: None,
                }],
                None,
            )
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
    }
    #[tokio::test]
    async fn append_and_partial_line_preserve_task_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_log(dir.path(), false);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        let started = store.snapshot().await.live[0].started_at_ms;
        let at = chrono::Utc::now().to_rfc3339();
        let finish = format!(
            "{}",
            serde_json::json!({"type":"event_msg","timestamp":at,"payload":{"type":"task_complete","duration_ms":10000}})
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(finish.as_bytes())
            .unwrap();
        store.refresh(false).await.unwrap();
        assert_eq!(store.snapshot().await.live[0].started_at_ms, started);
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        store.refresh(false).await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 0);
        assert_eq!(store.index_snapshot().await.tasks[0].completed, Some(true));
    }
    #[tokio::test]
    async fn corrupt_cache_rebuilds_without_losing_live_state() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), false);
        std::fs::write(dir.path().join("cache.json"), b"{broken").unwrap();
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 1);
    }
    #[tokio::test]
    async fn unavailable_runtime_stays_unknown_in_history_and_time() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), false);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        let id = store.snapshot().await.live[0].session_id.clone();
        store
            .update_runtime(
                vec![],
                vec![ProcessObservation {
                    session_id: id,
                    running: None,
                }],
                None,
            )
            .await;
        assert_eq!(store.snapshot().await.unknown.len(), 1);
        let today = chrono::Local::now().date_naive();
        let q = AgentQuery {
            from: Some((today - chrono::Days::new(1)).to_string()),
            to: Some((today + chrono::Days::new(1)).to_string()),
            ..Default::default()
        };
        let tasks = store.sessions(q.clone()).await.unwrap();
        assert_eq!(tasks[0].state, AgentTaskState::Unknown);
        let summary = store.summary(q).await.unwrap();
        assert_eq!(summary.totals.agent_time_ms, 0);
        assert_eq!(summary.totals.incomplete_tasks, 1);
    }
    #[tokio::test]
    async fn a_resumed_session_can_be_confirmed_exited_again() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), false);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        let id = store.snapshot().await.live[0].session_id.clone();
        let now = now_ms();
        store
            .inner
            .runtime
            .write()
            .await
            .processes
            .insert(id.clone(), (Some(false), now - 3000));
        {
            let mut published = store.inner.published.write().await;
            let mut index = (*published.index).clone();
            index.tasks[0].last_activity_at_ms = now - 1000;
            published.index = Arc::new(index);
        }
        assert_eq!(store.snapshot().await.working_count, 1);
        store
            .update_runtime(
                vec![],
                vec![ProcessObservation {
                    session_id: id,
                    running: Some(false),
                }],
                None,
            )
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
    }
    #[tokio::test]
    async fn four_provider_full_incremental_restart_and_rescan_are_equivalent() {
        use super::super::sources::AgentRoot;
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let now = now_ms();
        let at = chrono::DateTime::from_timestamp_millis(now - 10_000)
            .unwrap()
            .to_rfc3339();
        let end = chrono::DateTime::from_timestamp_millis(now - 5_000)
            .unwrap()
            .to_rfc3339();
        let codex = write_log(root, false);
        let claude_dir = root.join("claude/projects/p");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let claude = claude_dir.join("c.jsonl");
        std::fs::write(&claude,format!("{}\n",json!({"type":"user","uuid":"cu","sessionId":"claude-c","cwd":"/project","timestamp":at,"message":{"content":"synthetic prompt"}}))).unwrap();
        let copilot_dir = root.join("copilot/session-state/cp");
        std::fs::create_dir_all(&copilot_dir).unwrap();
        let copilot = copilot_dir.join("events.jsonl");
        std::fs::write(&copilot,format!("{}\n{}\n",json!({"type":"session.start","id":"cp-start","timestamp":at,"data":{"sessionId":"cp","selectedModel":"gpt-6.1-sol","context":{"cwd":"/project"}}}),json!({"type":"user.message","id":"cp-u","timestamp":at,"data":{}}))).unwrap();
        let oc_dir = root.join("oc");
        std::fs::create_dir(&oc_dir).unwrap();
        let database = rusqlite::Connection::open(oc_dir.join("opencode.db")).unwrap();
        database.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT,parent_id TEXT); CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT); CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,data TEXT); INSERT INTO session VALUES('oc','/project',NULL);").unwrap();
        database
            .execute(
                "INSERT INTO message VALUES('u','oc',?1,?1,?2)",
                rusqlite::params![now - 10000, json!({"role":"user"}).to_string()],
            )
            .unwrap();
        let mut c = config(root);
        c.roots.extend([
            AgentRoot::new(
                AgentProvider::Claude,
                root.join("claude/projects"),
                root.join("claude"),
            ),
            AgentRoot::new(
                AgentProvider::Copilot,
                root.join("copilot/session-state"),
                root.join("copilot"),
            ),
            AgentRoot::new(AgentProvider::Opencode, oc_dir.clone(), oc_dir),
        ]);
        let incremental = AgentStatsStore::new(c.clone());
        incremental.initialize().await.unwrap();
        assert_eq!(incremental.snapshot().await.working_count, 4);
        let append = |path: &std::path::Path, values: Vec<serde_json::Value>| {
            let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            for value in values {
                writeln!(f, "{value}").unwrap();
            }
        };
        append(
            &codex,
            vec![
                json!({"type":"event_msg","timestamp":end,"payload":{"type":"task_complete","duration_ms":5000}}),
            ],
        );
        append(
            &claude,
            vec![
                json!({"type":"assistant","uuid":"ca","sessionId":"claude-c","timestamp":end,"message":{"id":"claude-response","model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":5}}}),
            ],
        );
        append(
            &copilot,
            vec![
                json!({"type":"assistant.message","id":"cp-a","timestamp":end,"data":{"apiCallId":"request","model":"gpt-6.1-sol","phase":"final"}}),
                json!({"type":"assistant.turn_end","id":"cp-e","timestamp":end,"data":{}}),
                json!({"type":"session.shutdown","id":"cp-stop","timestamp":end,"data":{"modelMetrics":{"gpt-6.1-sol":{"requests":{"count":1},"usage":{"inputTokens":30,"outputTokens":10}}}}}),
            ],
        );
        database.execute("INSERT INTO message VALUES('a','oc',?1,?1,?2)",rusqlite::params![now-5000,json!({"role":"assistant","parentID":"u","modelID":"gpt-6.1-sol","finish":"stop","time":{"completed":now-5000},"tokens":{"input":5,"output":2}}).to_string()]).unwrap();
        incremental.refresh(false).await.unwrap();
        assert_eq!(incremental.snapshot().await.working_count, 0);
        let expected = incremental.index_snapshot().await;
        assert_eq!(expected.tasks.len(), 4);
        let restarted = AgentStatsStore::new(c.clone());
        restarted.initialize().await.unwrap();
        let mut cold_config = c;
        cold_config.cache_path = None;
        let cold = AgentStatsStore::new(cold_config);
        cold.initialize().await.unwrap();
        for actual in [
            restarted.index_snapshot().await,
            cold.index_snapshot().await,
        ] {
            assert_eq!(
                serde_json::to_value(&expected.usage).unwrap(),
                serde_json::to_value(&actual.usage).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&expected.tasks).unwrap(),
                serde_json::to_value(&actual.tasks).unwrap()
            );
        }
        incremental.rescan().await.unwrap();
        let rescanned = incremental.index_snapshot().await;
        assert_eq!(
            serde_json::to_value(expected.usage).unwrap(),
            serde_json::to_value(rescanned.usage).unwrap()
        );
    }
    #[tokio::test]
    async fn background_rescan_coalesces_while_ingestion_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), false);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        let gate = store.inner.gate.lock().await;
        for _ in 0..8 {
            assert!(store.request_rescan().await.scanning);
        }
        assert!(store.inner.rescan_requested.load(Ordering::Acquire));
        assert_eq!(store.snapshot().await.working_count, 1);
        drop(gate);
        tokio::time::timeout(Duration::from_secs(2), async {
            while store.inner.rescan_requested.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.snapshot().await.working_count, 1);
        assert!(!store.status().await.scanning);
    }
    #[tokio::test]
    async fn shutdown_flush_saves_the_latest_incremental_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_log(dir.path(), false);
        let store = AgentStatsStore::new(config(dir.path()));
        store.initialize().await.unwrap();
        let before = cache::load(&dir.path().join("cache.json")).unwrap();
        let key = sources::canonical(&path).to_string_lossy().into_owned();
        let old = before.sources[&key].jsonl.as_ref().unwrap().offset;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();
        store.refresh(false).await.unwrap();
        store.flush().await.unwrap();
        let after = cache::load(&dir.path().join("cache.json")).unwrap();
        assert!(after.sources[&key].jsonl.as_ref().unwrap().offset > old);
    }
    fn claude_config(dir: &std::path::Path) -> AgentConfig {
        AgentConfig {
            discover_defaults: false,
            roots: vec![super::super::sources::AgentRoot::new(
                AgentProvider::Claude,
                dir.join("claude/projects"),
                dir.join("claude"),
            )],
            cache_path: Some(dir.join("cache.json")),
            ..Default::default()
        }
    }
    fn append_claude(path: &std::path::Path, at: i64, mut record: serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        record["timestamp"] = serde_json::json!(chrono::DateTime::from_timestamp_millis(at)
            .unwrap()
            .to_rfc3339());
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{record}").unwrap();
    }
    fn initial_claude(dir: &std::path::Path, base: i64, bash: bool) -> std::path::PathBuf {
        use serde_json::json;
        let path = dir.join("claude/projects/p/claude-c.jsonl");
        append_claude(
            &path,
            base,
            json!({"type":"user","uuid":"u-original","sessionId":"claude-c","cwd":"/project","message":{"content":"synthetic prompt"}}),
        );
        let content = if bash {
            json!([{"type":"tool_use","name":"Bash","id":"bash-pending","input":{"command":"synthetic command"}}])
        } else {
            json!([])
        };
        append_claude(
            &path,
            base + 5000,
            json!({"type":"assistant","uuid":"a-original","sessionId":"claude-c","message":{"id":"response-original","model":"claude-sonnet-4-6","stop_reason":"tool_use","content":content,"usage":{"input_tokens":10,"output_tokens":2}}}),
        );
        path
    }
    async fn outage(store: &AgentStatsStore, from: i64, confirmed: i64) {
        store.inner.runtime.write().await.offline_since =
            Some((Instant::now() - Duration::from_secs(21), from));
        store
            .update_runtime_at(vec![], vec![], None, confirmed)
            .await;
    }
    fn all_dates() -> AgentQuery {
        let today = chrono::Local::now().date_naive();
        AgentQuery {
            from: Some((today - chrono::Days::new(1)).to_string()),
            to: Some((today + chrono::Days::new(1)).to_string()),
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn offline_epoch_survives_reconnect_and_restart_and_resumes_from_new_activity() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms() - 120_000;
        let path = initial_claude(dir.path(), base, false);
        let config = claude_config(dir.path());
        let store = AgentStatsStore::new(config.clone());
        store.initialize().await.unwrap();
        let original = store.snapshot().await.live[0].clone();
        outage(&store, base + 10_000, base + 31_000).await;
        assert_eq!(store.snapshot().await.working_count, 0);
        let index = store.index_snapshot().await;
        assert_eq!(index.tasks.len(), 1);
        assert_eq!(index.tasks[0].completed, None);
        assert_eq!(index.tasks[0].ended_at_ms, Some(base + 5000));
        assert!(
            store.recent_tasks(base - 1).await.is_empty(),
            "synthetic offline end must never reach Echo as a journal completion"
        );
        assert_eq!(
            store.runtime_tasks(base + 1_000_000).await.len(),
            1,
            "process matching retains interrupted metadata even outside the recent-Echo window"
        );
        let summary = store.summary(all_dates()).await.unwrap();
        assert_eq!(summary.totals.agent_time_ms, 5000);
        assert_eq!(summary.totals.completed, 0);
        assert_eq!(summary.totals.aborted, 0);
        assert_eq!(summary.totals.incomplete_tasks, 1);
        let cached = cache::load(&dir.path().join("cache.json")).unwrap();
        assert_eq!(cached.runtime_barriers.len(), 1);
        store
            .update_runtime_at(vec![], vec![], Some(true), base + 32_000)
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
        let restarted = AgentStatsStore::new(config);
        restarted.initialize().await.unwrap();
        assert_eq!(restarted.snapshot().await.working_count, 0);
        append_claude(
            &path,
            base + 33_000,
            json!({"type":"user","uuid":"u-tool-result-after-network","sessionId":"claude-c","message":{"content":[{"type":"tool_result","tool_use_id":"network-tool","content":"synthetic result"}]}}),
        );
        restarted.refresh(false).await.unwrap();
        let current = restarted.snapshot().await.live[0].clone();
        assert_eq!(current.started_at_ms, base + 33_000);
        assert_ne!(current.id, original.id);
        assert!(current.id.contains("offline-resume"));
        let tasks = restarted.sessions(all_dates()).await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].started_at_ms, current.started_at_ms);
        assert_eq!(tasks[1].state, AgentTaskState::Unknown);
        assert_eq!(restarted.recent_tasks(base - 1).await.len(), 1);
    }
    #[tokio::test]
    async fn repeated_offline_epochs_preserve_history_usage_and_explicit_completion() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms() - 120_000;
        let path = initial_claude(dir.path(), base, false);
        let config = claude_config(dir.path());
        let store = AgentStatsStore::new(config.clone());
        store.initialize().await.unwrap();
        outage(&store, base + 10_000, base + 31_000).await;
        store
            .update_runtime_at(vec![], vec![], Some(true), base + 32_000)
            .await;
        append_claude(
            &path,
            base + 33_000,
            json!({"type":"assistant","uuid":"a-second","sessionId":"claude-c","message":{"id":"response-second","model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":4,"output_tokens":1}}}),
        );
        append_claude(
            &path,
            base + 34_000,
            json!({"type":"user","uuid":"u-second-tool","sessionId":"claude-c","message":{"content":[{"type":"tool_result","tool_use_id":"second-tool","content":"synthetic result"}]}}),
        );
        store.refresh(false).await.unwrap();
        assert_eq!(store.snapshot().await.live[0].started_at_ms, base + 33_000);
        outage(&store, base + 35_000, base + 56_000).await;
        assert_eq!(store.snapshot().await.working_count, 0);
        store
            .update_runtime_at(vec![], vec![], Some(true), base + 57_000)
            .await;
        append_claude(
            &path,
            base + 60_000,
            json!({"type":"assistant","uuid":"a-third-final","sessionId":"claude-c","message":{"id":"response-third","model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":3}}}),
        );
        store.refresh(false).await.unwrap();
        assert_eq!(store.snapshot().await.working_count, 0);
        let index = store.index_snapshot().await;
        assert_eq!(index.tasks.len(), 3);
        assert_eq!(index.usage.len(), 3);
        assert_eq!(
            index
                .usage
                .iter()
                .map(|u| u.tokens.as_ref().unwrap().total())
                .sum::<u64>(),
            40
        );
        assert_eq!(
            index
                .tasks
                .iter()
                .map(|t| t.started_at_ms)
                .collect::<Vec<_>>(),
            vec![base, base + 33_000, base + 60_000]
        );
        assert_eq!(
            index
                .tasks
                .iter()
                .filter(|t| t.completed == Some(true))
                .count(),
            1
        );
        let summary = store.summary(all_dates()).await.unwrap();
        assert_eq!(summary.totals.agent_time_ms, 6000);
        assert_eq!(summary.totals.wall_time_ms, 6000);
        assert_eq!(summary.totals.incomplete_tasks, 2);
        assert_eq!(summary.totals.completed, 1);
        assert_eq!(summary.totals.tokens.as_ref().unwrap().total(), 40);
        let explicit = store.recent_tasks(base - 1).await;
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].completed, Some(true));
        assert_eq!(explicit[0].started_at_ms, base + 60_000);
        store.flush().await.unwrap();
        let restarted = AgentStatsStore::new(config);
        restarted.initialize().await.unwrap();
        assert_eq!(
            serde_json::to_value(index.tasks).unwrap(),
            serde_json::to_value(restarted.index_snapshot().await.tasks).unwrap()
        );
        assert_eq!(
            cache::load(&dir.path().join("cache.json"))
                .unwrap()
                .runtime_barriers
                .len(),
            2
        );
    }
    #[tokio::test]
    async fn offline_grace_preserves_pending_bash_and_model_replies() {
        use serde_json::json;
        let base = now_ms() - 120_000;
        for bash in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = initial_claude(dir.path(), base, bash);
            if !bash {
                append_claude(
                    &path,
                    base + 20_000,
                    json!({"type":"assistant","uuid":"a-local-reply","sessionId":"claude-c","message":{"id":"response-local","model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":5,"output_tokens":1}}}),
                );
            }
            let store = AgentStatsStore::new(claude_config(dir.path()));
            store.initialize().await.unwrap();
            outage(&store, base + 10_000, base + 31_000).await;
            assert_eq!(store.snapshot().await.working_count, 1);
            assert_eq!(store.snapshot().await.live[0].started_at_ms, base);
            assert!(store.inner.io.lock().unwrap().runtime_barriers.is_empty());
        }
    }
    #[tokio::test]
    async fn reconnect_before_the_first_grace_poll_does_not_end_a_task() {
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms() - 120_000;
        initial_claude(dir.path(), base, false);
        let store = AgentStatsStore::new(claude_config(dir.path()));
        store.initialize().await.unwrap();
        store.inner.runtime.write().await.offline_since =
            Some((Instant::now() - Duration::from_secs(21), base + 10_000));
        store
            .update_runtime_at(vec![], vec![], Some(true), base + 31_000)
            .await;
        assert_eq!(store.snapshot().await.working_count, 1);
        assert_eq!(store.snapshot().await.live[0].started_at_ms, base);
        assert!(store.inner.io.lock().unwrap().runtime_barriers.is_empty());
        assert!(store.inner.runtime.read().await.offline_since.is_none());
    }
    #[tokio::test]
    async fn a_late_discovered_model_reply_can_invalidate_a_cut_without_duplicating_usage() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms() - 120_000;
        let path = initial_claude(dir.path(), base, false);
        let store = AgentStatsStore::new(claude_config(dir.path()));
        store.initialize().await.unwrap();
        outage(&store, base + 10_000, base + 31_000).await;
        assert_eq!(store.snapshot().await.working_count, 0);
        append_claude(
            &path,
            base + 20_000,
            json!({"type":"assistant","uuid":"a-arrived-late","sessionId":"claude-c","message":{"id":"response-late","model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":5,"output_tokens":1}}}),
        );
        store.refresh(false).await.unwrap();
        let snapshot = store.snapshot().await;
        assert_eq!(snapshot.working_count, 1);
        assert_eq!(snapshot.live[0].started_at_ms, base);
        assert_eq!(store.index_snapshot().await.tasks.len(), 1);
        assert_eq!(store.index_snapshot().await.usage.len(), 2);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_configure_saves_and_publishes_one_consistent_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        let config = AgentConfig {
            discover_defaults: false,
            config_path: Some(path.clone()),
            ..Default::default()
        };
        let store = AgentStatsStore::new(config.clone());
        let mut writers = Vec::new();
        for n in 0..24 {
            let store = store.clone();
            let mut next = config.clone();
            next.history_days = 30 + n;
            next.enabled = n % 2 == 0;
            writers.push(tokio::spawn(async move { store.configure(next).await }));
        }
        for writer in writers {
            writer.await.unwrap().unwrap();
        }
        let disk: AgentConfig = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let live = store.config().await;
        assert_eq!(disk.history_days, live.history_days);
        assert_eq!(disk.enabled, live.enabled);
        assert_eq!(disk.config_path, live.config_path);
        assert_eq!(
            store.status().await.state,
            if live.enabled { "ready" } else { "disabled" }
        );
    }

    #[tokio::test]
    async fn killed_claude_restarts_same_file_as_a_new_epoch_and_keeps_journal_completion() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms() - 120_000;
        // A real process exit overrides both pending local Bash and a model
        // reply; these are exceptions only to an offline inference.
        let path = initial_claude(dir.path(), base, true);
        let config = claude_config(dir.path());
        let store = AgentStatsStore::new(config.clone());
        store.initialize().await.unwrap();
        let original = store.snapshot().await.live[0].clone();
        let id = original.session_id.clone();
        store
            .update_runtime_at(
                vec![],
                vec![ProcessObservation {
                    session_id: id.clone(),
                    running: Some(false),
                }],
                None,
                base + 10_000,
            )
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
        let index = store.index_snapshot().await;
        assert_eq!(index.tasks.len(), 1);
        assert_eq!(index.tasks[0].completed, None);
        assert_eq!(index.tasks[0].state, AgentTaskState::Unknown);
        assert_eq!(index.tasks[0].ended_at_ms, Some(base + 5000));
        assert!(index.tasks[0].running_commands.is_empty());
        assert!(
            store.recent_tasks(base - 1).await.is_empty(),
            "killed process is not a journal completion for Echo"
        );
        store
            .update_runtime_at(vec![], vec![], Some(true), base + 11_000)
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
        let cached = cache::load(&dir.path().join("cache.json")).unwrap();
        assert!(matches!(
            cached.runtime_barriers[0].kind,
            AgentEventKind::Interrupted {
                confirmed_exit: true,
                ..
            }
        ));
        let restarted = AgentStatsStore::new(config.clone());
        restarted.initialize().await.unwrap();
        assert_eq!(restarted.snapshot().await.working_count, 0);
        // ParserState.turn_open still belongs to the old source stream. Its
        // real user record emits Active, and replay must start a new epoch.
        append_claude(
            &path,
            base + 20_000,
            json!({"type":"user","uuid":"u-resumed-after-kill","sessionId":"claude-c","message":{"content":"resume synthetic work"}}),
        );
        restarted.refresh(false).await.unwrap();
        let current = restarted.snapshot().await.live[0].clone();
        assert_eq!(current.started_at_ms, base + 20_000);
        assert_ne!(current.id, original.id);
        assert!(current.id.contains("process-resume"));
        let tasks = restarted.sessions(all_dates()).await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].started_at_ms, base + 20_000);
        assert_eq!(tasks[1].completed, None);
        append_claude(
            &path,
            base + 25_000,
            json!({"type":"assistant","uuid":"a-final-after-kill","sessionId":"claude-c","message":{"id":"response-after-kill","model":"claude-sonnet-4-6","stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":3}}}),
        );
        restarted.refresh(false).await.unwrap();
        assert_eq!(restarted.snapshot().await.working_count, 0);
        let explicit = restarted.recent_tasks(base - 1).await;
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].completed, Some(true));
        assert_eq!(explicit[0].started_at_ms, base + 20_000);
        let summary = restarted.summary(all_dates()).await.unwrap();
        assert_eq!(summary.totals.completed, 1);
        assert_eq!(summary.totals.aborted, 0);
        assert_eq!(summary.totals.incomplete_tasks, 1);
        assert_eq!(summary.totals.agent_time_ms, 10_000);
        assert_eq!(summary.totals.wall_time_ms, 10_000);
        assert_eq!(summary.totals.tokens.as_ref().unwrap().total(), 35);
        restarted.flush().await.unwrap();
        let again = AgentStatsStore::new(config);
        again.initialize().await.unwrap();
        assert_eq!(again.index_snapshot().await.tasks.len(), 2);
        assert_eq!(again.recent_tasks(base - 1).await[0].completed, Some(true));
    }

    #[tokio::test]
    async fn same_millisecond_resume_ignores_old_exit_and_a_second_process_exit_cuts_the_new_epoch()
    {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms() - 120_000;
        let path = initial_claude(dir.path(), base, false);
        let store = AgentStatsStore::new(claude_config(dir.path()));
        store.initialize().await.unwrap();
        let original = store.snapshot().await.live[0].clone();
        let id = original.session_id.clone();
        let killed = base + 10_000;
        store
            .update_runtime_at(
                vec![],
                vec![ProcessObservation {
                    session_id: id.clone(),
                    running: Some(false),
                }],
                None,
                killed,
            )
            .await;
        append_claude(
            &path,
            killed,
            json!({"type":"user","uuid":"u-same-millisecond-resume","sessionId":"claude-c","message":{"content":"resumed synthetic work"}}),
        );
        store.refresh(false).await.unwrap();
        let resumed = store.snapshot().await.live[0].clone();
        assert_eq!(resumed.started_at_ms, killed);
        assert_ne!(resumed.id, original.id);
        store
            .update_runtime_at(vec![], vec![], None, killed + 1000)
            .await;
        assert_eq!(
            store.snapshot().await.working_count,
            1,
            "an older false observation must not cut a new same-ms epoch"
        );
        let binding = RuntimeBinding {
            session_id: id.clone(),
            tmux_session: "session".into(),
            pane_id: "%1".into(),
            pid: 101,
            process_started_at: Some(killed as u64),
        };
        store
            .update_runtime_at(
                vec![binding.clone()],
                vec![ProcessObservation {
                    session_id: id.clone(),
                    running: Some(true),
                }],
                None,
                killed + 1100,
            )
            .await;
        assert_eq!(store.snapshot().await.live[0].bindings[0].pid, 101);
        store
            .update_runtime_at(
                vec![],
                vec![ProcessObservation {
                    session_id: id.clone(),
                    running: Some(false),
                }],
                None,
                killed + 2000,
            )
            .await;
        assert_eq!(store.snapshot().await.working_count, 0);
        assert_eq!(store.index_snapshot().await.tasks.len(), 2);
        append_claude(
            &path,
            killed + 3000,
            json!({"type":"user","uuid":"u-after-second-process","sessionId":"claude-c","message":{"content":"third synthetic work"}}),
        );
        store.refresh(false).await.unwrap();
        assert_eq!(store.snapshot().await.live[0].started_at_ms, killed + 3000);
        assert_eq!(store.index_snapshot().await.tasks.len(), 3);
        assert_eq!(store.inner.io.lock().unwrap().runtime_barriers.len(), 2);
        assert_eq!(store.recent_tasks(base - 1).await.len(), 1);
    }
}
