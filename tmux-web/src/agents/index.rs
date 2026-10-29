//! Deterministic folding of normalized source contributions into tasks/usage.
use super::model::*;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct CanonicalKeys {
    sessions: BTreeMap<AgentSessionId, String>,
    profiles: BTreeMap<AgentProvider, BTreeMap<String, String>>,
}
impl CanonicalKeys {
    fn session(&mut self, id: &AgentSessionId) -> String {
        if let Some(key) = self.sessions.get(id) {
            return key.clone();
        }
        let key = id.key();
        self.sessions.insert(id.clone(), key.clone());
        key
    }
    fn profile(&mut self, id: &AgentSessionId) -> String {
        let profiles = self.profiles.entry(id.provider).or_default();
        if let Some(key) = profiles.get(&id.profile) {
            return key.clone();
        }
        let key = serde_json::to_string(&(id.provider, &id.profile)).expect("string tuple");
        profiles.insert(id.profile.clone(), key.clone());
        key
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentIndex {
    pub tasks: Vec<AgentTask>,
    pub usage: Vec<UsageRecord>,
}

fn rank(kind: &AgentEventKind) -> u8 {
    match kind {
        AgentEventKind::Context => 0,
        AgentEventKind::Started { .. } => 1,
        AgentEventKind::Active => 2,
        AgentEventKind::Command { .. } => 3,
        AgentEventKind::Usage { .. } => 4,
        AgentEventKind::Settled => 5,
        AgentEventKind::Ended { .. } => 6,
        AgentEventKind::Interrupted { .. } => 7,
    }
}
fn root_for(id: &str, parents: &BTreeMap<String, String>) -> String {
    if !parents.contains_key(id) {
        return id.to_owned();
    }
    let mut id = id.to_owned();
    let mut seen = BTreeSet::new();
    while seen.insert(id.clone()) {
        match parents.get(&id) {
            Some(p) => id = p.clone(),
            None => break,
        }
    }
    id
}
fn resumed_task_id(barrier: &AgentEvent) -> String {
    let reason = if matches!(
        &barrier.kind,
        AgentEventKind::Interrupted {
            confirmed_exit: true,
            ..
        }
    ) {
        "process-resume"
    } else {
        "offline-resume"
    };
    format!("{reason}:{}", barrier.id)
}
fn new_task(event: &AgentEvent, task_id: &str) -> AgentTask {
    let sid = event.session.key();
    AgentTask {
        id: format!("{sid}:{task_id}:{}", event.at_ms),
        session_id: sid,
        provider: event.session.provider,
        profile: event.session.profile.clone(),
        native_id: event.session.native_id.clone(),
        parent_session_id: None,
        project: event.project.clone(),
        model: event.model.clone(),
        started_at_ms: event.at_ms,
        last_activity_at_ms: event.at_ms,
        ended_at_ms: None,
        duration_ms: None,
        elapsed_ms: 0,
        state: AgentTaskState::Working,
        completed: None,
        tokens: None,
        estimated_cost_usd: None,
        reported_cost_usd: None,
        requests: 0,
        unpriced_requests: 0,
        bindings: Vec::new(),
        running_commands: BTreeSet::new(),
        settled_at_ms: None,
    }
}
fn touch(task: &mut AgentTask, event: &AgentEvent) {
    task.last_activity_at_ms = task.last_activity_at_ms.max(event.at_ms);
    task.settled_at_ms = None;
    if !event.project.is_empty() {
        task.project = event.project.clone();
    }
    if !event.model.is_empty() {
        task.model = event.model.clone();
    }
}
fn usage_key(event: &AgentEvent, response_id: &str, keys: &mut CanonicalKeys) -> (String, String) {
    let response = if event.session.provider == AgentProvider::Codex {
        response_id
            .strip_prefix("native:")
            .and_then(|s| s.split_once(':'))
            .map(|(_, r)| format!("native:{r}"))
            .unwrap_or_else(|| response_id.to_owned())
    } else {
        response_id.to_owned()
    };
    (keys.profile(&event.session), response)
}

/// Canonical event IDs deduplicate archived/resumed sources. Stream chunks
/// replace the same response with maxima instead of becoming extra requests.
pub fn fold(events: impl IntoIterator<Item = AgentEvent>) -> AgentIndex {
    let events: Vec<_> = events.into_iter().collect();
    fold_refs(events.iter())
}

/// Borrow unchanged source contributions. Only duplicate streamed usage needs
/// an owned copy for merging; normal history is never deep-cloned for a fold.
pub fn fold_refs<'a>(events: impl IntoIterator<Item = &'a AgentEvent>) -> AgentIndex {
    let mut unique: BTreeMap<(String, String), Cow<'a, AgentEvent>> = BTreeMap::new();
    let mut keys = CanonicalKeys::default();
    let mut first_usage: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut native: BTreeSet<(String, String)> = BTreeSet::new();
    let mut reply_moments: Vec<(String, i64, u64)> = Vec::new();
    for event in events {
        if matches!(
            &event.kind,
            AgentEventKind::Usage {
                tokens: Some(_),
                ..
            }
        ) && !event.model.is_empty()
        {
            reply_moments.push((keys.session(&event.session), event.at_ms, event.sequence));
        }
        let key = match &event.kind {
            AgentEventKind::Usage { response_id, .. } => {
                let key = usage_key(event, response_id, &mut keys);
                first_usage
                    .entry(key.clone())
                    .and_modify(|date| *date = (*date).min(event.at_ms))
                    .or_insert(event.at_ms);
                if event.session.provider == AgentProvider::Codex {
                    if let Some(namespace) = response_id
                        .strip_prefix("native:")
                        .and_then(|r| r.split_once(':'))
                        .map(|p| p.0)
                    {
                        native.insert((event.session.profile.clone(), namespace.to_owned()));
                    }
                }
                key
            }
            _ => (keys.session(&event.session), event.id.clone()),
        };
        if let Some(previous) = unique.get_mut(&key) {
            // Non-usage duplicates keep the first source without allocating.
            if !matches!(
                (&previous.kind, &event.kind),
                (AgentEventKind::Usage { .. }, AgentEventKind::Usage { .. })
            ) {
                continue;
            }
            let previous = previous.to_mut();
            if let (
                AgentEventKind::Usage {
                    tokens: a,
                    requests: ar,
                    reported_cost_usd: ac,
                    cache_write_1h: ah,
                    ..
                },
                AgentEventKind::Usage {
                    tokens: b,
                    requests: br,
                    reported_cost_usd: bc,
                    cache_write_1h: bh,
                    ..
                },
            ) = (&mut previous.kind, &event.kind)
            {
                if let Some(b) = b {
                    if let Some(a) = a {
                        a.merge(b);
                    } else {
                        *a = Some(b.clone());
                    }
                }
                *ar = (*ar).max(*br);
                *ac = match (*ac, *bc) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                *ah = (*ah).or(*bh);
                if !event.model.is_empty() {
                    previous.model = event.model.clone();
                }
                if !event.project.is_empty() {
                    previous.project = event.project.clone();
                }
                // Usage attribution keeps the first timestamp separately;
                // its newest chunk is still evidence that the task works.
                previous.at_ms = previous.at_ms.max(event.at_ms);
                previous.sequence = previous.sequence.max(event.sequence);
            }
        } else {
            unique.insert(key, Cow::Borrowed(event));
        }
    }
    let mut ordered: Vec<Cow<'a, AgentEvent>> = unique
        .into_values()
        .filter(|e| {
            if let AgentEventKind::Usage { response_id, .. } = &e.kind {
                if e.session.provider == AgentProvider::Codex {
                    if let Some(namespace) = response_id
                        .strip_prefix("cumulative:")
                        .and_then(|s| s.rsplit_once(':'))
                        .map(|p| p.0)
                    {
                        return !native
                            .contains(&(e.session.profile.clone(), namespace.to_owned()));
                    }
                }
            }
            true
        })
        .collect();
    ordered.sort_by(|a, b| {
        a.at_ms
            .cmp(&b.at_ms)
            .then_with(|| a.sequence.cmp(&b.sequence))
            .then_with(|| rank(&a.kind).cmp(&rank(&b.kind)))
            .then_with(|| a.id.cmp(&b.id))
    });
    let parents: BTreeMap<String, String> = ordered
        .iter()
        .filter_map(|e| {
            e.parent
                .as_ref()
                .map(|p| (keys.session(&e.session), keys.session(p)))
        })
        .collect();
    let mut index = AgentIndex::default();
    let mut current: BTreeMap<String, usize> = BTreeMap::new();
    let mut interrupted: BTreeMap<String, AgentEvent> = BTreeMap::new();
    let mut replies: BTreeMap<String, Vec<(i64, u64)>> = BTreeMap::new();
    for (session, at, sequence) in reply_moments {
        replies
            .entry(root_for(&session, &parents))
            .or_default()
            .push((at, sequence));
    }
    for event in ordered {
        let event = event.as_ref();
        let sid = keys.session(&event.session);
        let root = root_for(&sid, &parents);
        let child = parents.contains_key(&sid);
        match &event.kind {
            AgentEventKind::Started { task_id } if !child => {
                interrupted.remove(&sid);
                if let Some(&pos) = current.get(&sid) {
                    let old = &mut index.tasks[pos];
                    if old.id == format!("{sid}:{task_id}:{}", event.at_ms) {
                        touch(old, &event);
                        continue;
                    }
                    old.state = AgentTaskState::Unknown;
                    old.ended_at_ms = Some(old.last_activity_at_ms);
                    old.elapsed_ms = old
                        .last_activity_at_ms
                        .saturating_sub(old.started_at_ms)
                        .max(0) as u64;
                }
                current.insert(sid, index.tasks.len());
                index.tasks.push(new_task(&event, task_id));
            }
            AgentEventKind::Active if !child => {
                if let Some(&pos) = current.get(&sid) {
                    if event.at_ms - index.tasks[pos].last_activity_at_ms
                        >= event.session.provider.resume_ms()
                    {
                        let old = &mut index.tasks[pos];
                        old.state = AgentTaskState::Unknown;
                        old.ended_at_ms = Some(old.last_activity_at_ms);
                        current.remove(&sid);
                    }
                }
                let barrier = interrupted.remove(&sid);
                let pos = *current.entry(sid.clone()).or_insert_with(|| {
                    let pos = index.tasks.len();
                    index.tasks.push(new_task(
                        &event,
                        &barrier
                            .map(|b| resumed_task_id(&b))
                            .unwrap_or_else(|| format!("resume:{}", event.at_ms)),
                    ));
                    pos
                });
                touch(&mut index.tasks[pos], &event);
            }
            AgentEventKind::Ended {
                completed,
                duration_ms,
            } if !child => {
                if let Some(pos) = current.remove(&sid) {
                    let task = &mut index.tasks[pos];
                    touch(task, &event);
                    task.ended_at_ms = Some(event.at_ms.max(task.started_at_ms));
                    task.duration_ms = Some(duration_ms.unwrap_or_else(|| {
                        event.at_ms.saturating_sub(task.started_at_ms).max(0) as u64
                    }));
                    task.elapsed_ms = task.duration_ms.unwrap_or(0);
                    task.completed = Some(*completed);
                    task.state = AgentTaskState::Ended;
                    task.running_commands.clear();
                }
            }
            AgentEventKind::Context => {
                if let Some(&pos) = current.get(&sid) {
                    let task = &mut index.tasks[pos];
                    if !event.project.is_empty() {
                        task.project = event.project.clone();
                    }
                    if !event.model.is_empty() {
                        task.model = event.model.clone();
                    }
                }
            }
            AgentEventKind::Settled if !child => {
                if let Some(&pos) = current.get(&sid) {
                    touch(&mut index.tasks[pos], &event);
                    index.tasks[pos].settled_at_ms = Some(event.at_ms);
                }
            }
            AgentEventKind::Command { id, running } if !child => {
                if let Some(&pos) = current.get(&sid) {
                    let task = &mut index.tasks[pos];
                    if *running {
                        task.running_commands.insert(id.clone());
                    } else {
                        task.running_commands.remove(id);
                    }
                }
            }
            AgentEventKind::Usage {
                response_id,
                tokens,
                requests,
                reported_cost_usd,
                fast,
                cache_write_1h,
                ..
            } => {
                index.usage.push(UsageRecord {
                    id: format!("{sid}:{response_id}"),
                    session_id: sid.clone(),
                    root_session_id: root.clone(),
                    provider: event.session.provider,
                    profile: event.session.profile.clone(),
                    project: event.project.clone(),
                    model: event.model.clone(),
                    at_ms: first_usage
                        .get(&usage_key(event, response_id, &mut keys))
                        .copied()
                        .unwrap_or(event.at_ms),
                    tokens: tokens.clone(),
                    requests: *requests,
                    estimated_cost_usd: None,
                    reported_cost_usd: *reported_cost_usd,
                    fast: *fast,
                    cache_write_1h: *cache_write_1h,
                });
                if event.session.provider != AgentProvider::Copilot {
                    // Only a runtime interruption authorizes a usage-only
                    // resume. Ordinary orphaned historical usage owns no task.
                    if tokens.is_some() && !current.contains_key(&root) {
                        if let Some(barrier) = interrupted.remove(&root) {
                            let mut resumed = barrier.clone();
                            resumed.at_ms = event.at_ms;
                            resumed.sequence = event.sequence;
                            if !event.project.is_empty() {
                                resumed.project = event.project.clone();
                            }
                            if !child && !event.model.is_empty() {
                                resumed.model = event.model.clone();
                            }
                            current.insert(root.clone(), index.tasks.len());
                            index
                                .tasks
                                .push(new_task(&resumed, &resumed_task_id(&barrier)));
                        }
                    }
                    if let Some(&pos) = current.get(&root) {
                        let task = &mut index.tasks[pos];
                        task.last_activity_at_ms = task.last_activity_at_ms.max(event.at_ms);
                        if let Some(tokens) = tokens {
                            task.tokens
                                .get_or_insert_with(AgentTokens::default)
                                .add(tokens);
                        }
                        task.requests = task.requests.saturating_add(*requests);
                        if let Some(cost) = reported_cost_usd {
                            *task.reported_cost_usd.get_or_insert(0.) += cost;
                        }
                    }
                }
            }
            AgentEventKind::Interrupted {
                task_id,
                offline_since_ms,
                confirmed_exit,
            } if !child => {
                if let Some(&pos) = current.get(&sid) {
                    let task = &mut index.tasks[pos];
                    let replied = replies.get(&root).is_some_and(|moments| {
                        moments.iter().any(|(at, sequence)| {
                            *at >= *offline_since_ms
                                && (*at < event.at_ms
                                    || *at == event.at_ms && *sequence < event.sequence)
                        })
                    });
                    if task.id == *task_id
                        && (*confirmed_exit || task.running_commands.is_empty() && !replied)
                    {
                        task.state = AgentTaskState::Unknown;
                        task.ended_at_ms = Some(
                            task.last_activity_at_ms
                                .min(event.at_ms)
                                .max(task.started_at_ms),
                        );
                        task.duration_ms = None;
                        task.elapsed_ms = task
                            .ended_at_ms
                            .unwrap()
                            .saturating_sub(task.started_at_ms)
                            .max(0) as u64;
                        task.completed = None;
                        task.running_commands.clear();
                        task.settled_at_ms = None;
                        current.remove(&sid);
                        interrupted.insert(sid, event.clone());
                    }
                }
            }
            _ => {}
        }
    }
    index
}

/// Rendering quiet/settled state does not synthesize a successful end event.
pub fn task_at(task: &AgentTask, now_ms: i64) -> AgentTask {
    let mut task = task.clone();
    // Runtime probes may decorate a recent journal task as unavailable.
    // Re-rendering that projection must not turn it back into Working.
    if task.state == AgentTaskState::Unknown && task.ended_at_ms.is_none() {
        task.elapsed_ms = task
            .last_activity_at_ms
            .saturating_sub(task.started_at_ms)
            .max(0) as u64;
        return task;
    }
    if task.ended_at_ms.is_none() {
        task.elapsed_ms = now_ms.saturating_sub(task.started_at_ms).max(0) as u64;
        let silent = now_ms.saturating_sub(task.last_activity_at_ms);
        if task
            .settled_at_ms
            .is_some_and(|at| now_ms - at >= SETTLE_MS)
        {
            task.state = AgentTaskState::Unknown;
        } else if silent >= task.provider.resume_ms() {
            task.state = AgentTaskState::Unknown;
        } else if silent >= IDLE_MS {
            task.state = AgentTaskState::Waiting;
        } else {
            task.state = AgentTaskState::Working;
        }
        if task.state == AgentTaskState::Unknown {
            task.elapsed_ms = task
                .last_activity_at_ms
                .saturating_sub(task.started_at_ms)
                .max(0) as u64;
        }
    }
    task
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(provider: AgentProvider, at: i64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent {
            id: format!("{at}:{kind:?}"),
            at_ms: at,
            sequence: 0,
            session: AgentSessionId {
                provider,
                profile: "/p".into(),
                native_id: "s".into(),
            },
            parent: None,
            project: "/project".into(),
            model: String::new(),
            kind,
        }
    }
    #[test]
    fn borrowed_stream_merge_never_mutates_cached_source_contributions() {
        let start = event(
            AgentProvider::Codex,
            1,
            AgentEventKind::Started {
                task_id: "turn".into(),
            },
        );
        let usage = event(
            AgentProvider::Codex,
            2,
            AgentEventKind::Usage {
                response_id: "native:s:response".into(),
                tokens: Some(AgentTokens {
                    input: 10,
                    output: 2,
                    ..Default::default()
                }),
                requests: 1,
                reported_cost_usd: None,
                fast: false,
                cumulative: false,
                cache_write_1h: None,
            },
        );
        let mut streamed = usage.clone();
        streamed.at_ms = 3;
        if let AgentEventKind::Usage {
            tokens: Some(tokens),
            ..
        } = &mut streamed.kind
        {
            tokens.output = 5;
        }
        let sources = vec![start, usage, streamed];
        let before = serde_json::to_vec(&sources).unwrap();
        let folded = fold_refs(sources.iter());
        assert_eq!(serde_json::to_vec(&sources).unwrap(), before);
        assert_eq!(folded.usage.len(), 1);
        assert_eq!(folded.usage[0].tokens.as_ref().unwrap().output, 5);
        assert_eq!(folded.usage[0].at_ms, 2);
        assert_eq!(folded.tasks[0].last_activity_at_ms, 3);
    }
    #[test]
    fn quiet_and_resume_keep_original_start() {
        let s = event(
            AgentProvider::Claude,
            1,
            AgentEventKind::Started {
                task_id: "a".into(),
            },
        );
        let i = fold([s.clone()]);
        assert_eq!(task_at(&i.tasks[0], IDLE_MS).state, AgentTaskState::Working);
        assert_eq!(
            task_at(&i.tasks[0], IDLE_MS + 1).state,
            AgentTaskState::Waiting
        );
        let i = fold([
            s,
            event(AgentProvider::Claude, IDLE_MS + 100, AgentEventKind::Active),
        ]);
        assert_eq!(i.tasks.len(), 1);
        assert_eq!(i.tasks[0].started_at_ms, 1);
        let i = fold([
            event(
                AgentProvider::Claude,
                1,
                AgentEventKind::Started {
                    task_id: "a".into(),
                },
            ),
            event(AgentProvider::Claude, 3_600_001, AgentEventKind::Active),
        ]);
        assert_eq!(i.tasks.len(), 2);
    }
    #[test]
    fn explicit_duration_and_abort_are_preserved() {
        let i = fold([
            event(
                AgentProvider::Codex,
                10,
                AgentEventKind::Started {
                    task_id: "a".into(),
                },
            ),
            event(
                AgentProvider::Codex,
                1000,
                AgentEventKind::Ended {
                    completed: false,
                    duration_ms: Some(500),
                },
            ),
        ]);
        assert_eq!(i.tasks[0].completed, Some(false));
        assert_eq!(i.tasks[0].duration_ms, Some(500));
    }
    #[test]
    fn resume_expiry_and_settle_boundaries_are_deterministic() {
        for p in AgentProvider::ALL {
            let i = fold([event(
                p,
                100,
                AgentEventKind::Started {
                    task_id: "a".into(),
                },
            )]);
            assert_eq!(
                task_at(&i.tasks[0], 100 + IDLE_MS - 1).state,
                AgentTaskState::Working
            );
            assert_eq!(
                task_at(&i.tasks[0], 100 + IDLE_MS).state,
                AgentTaskState::Waiting
            );
            assert_eq!(
                task_at(&i.tasks[0], 100 + p.resume_ms() - 1).state,
                AgentTaskState::Waiting
            );
            assert_eq!(
                task_at(&i.tasks[0], 100 + p.resume_ms()).state,
                AgentTaskState::Unknown
            );
        }
        let i = fold([
            event(
                AgentProvider::Opencode,
                100,
                AgentEventKind::Started {
                    task_id: "a".into(),
                },
            ),
            event(AgentProvider::Opencode, 200, AgentEventKind::Settled),
        ]);
        assert_eq!(
            task_at(&i.tasks[0], 200 + SETTLE_MS - 1).state,
            AgentTaskState::Working
        );
        assert_eq!(
            task_at(&i.tasks[0], 200 + SETTLE_MS).state,
            AgentTaskState::Unknown
        );
        assert_eq!(task_at(&i.tasks[0], 200 + SETTLE_MS).completed, None);
    }
    #[test]
    fn streamed_usage_and_duplicate_sources_do_not_double_count() {
        let mut a = event(
            AgentProvider::Codex,
            20,
            AgentEventKind::Usage {
                response_id: "native:r".into(),
                tokens: Some(AgentTokens {
                    input: 100,
                    output: 5,
                    ..Default::default()
                }),
                requests: 1,
                reported_cost_usd: None,
                fast: false,
                cumulative: false,
                cache_write_1h: None,
            },
        );
        a.id = "r".into();
        let mut b = a.clone();
        b.id = "different-log-uuid".into();
        if let AgentEventKind::Usage {
            tokens: Some(t), ..
        } = &mut b.kind
        {
            t.output = 10;
        }
        let i = fold([a, b]);
        assert_eq!(i.usage.len(), 1);
        assert_eq!(i.usage[0].tokens.as_ref().unwrap().total(), 110);
    }
    #[test]
    fn source_order_keeps_previous_completion_before_next_same_stamp_start() {
        let a = event(
            AgentProvider::Codex,
            1,
            AgentEventKind::Started {
                task_id: "old".into(),
            },
        );
        let mut end = event(
            AgentProvider::Codex,
            100,
            AgentEventKind::Ended {
                completed: true,
                duration_ms: None,
            },
        );
        end.sequence = 100;
        let mut next = event(
            AgentProvider::Codex,
            100,
            AgentEventKind::Started {
                task_id: "next".into(),
            },
        );
        next.sequence = 200;
        let i = fold([a, end, next]);
        assert_eq!(i.tasks.len(), 2);
        assert_eq!(i.tasks[0].completed, Some(true));
        assert_eq!(i.tasks[1].ended_at_ms, None);
    }
    #[test]
    fn distinct_native_tasks_with_same_start_stamp_are_not_merged() {
        let mut a = event(
            AgentProvider::Codex,
            1,
            AgentEventKind::Started {
                task_id: "a".into(),
            },
        );
        a.sequence = 1;
        let mut b = event(
            AgentProvider::Codex,
            1,
            AgentEventKind::Started {
                task_id: "b".into(),
            },
        );
        b.sequence = 2;
        let i = fold([a, b]);
        assert_eq!(i.tasks.len(), 2);
        assert_ne!(i.tasks[0].id, i.tasks[1].id);
    }
    #[test]
    fn streamed_usage_has_first_billing_date_and_latest_activity() {
        let start = event(
            AgentProvider::Codex,
            0,
            AgentEventKind::Started {
                task_id: "a".into(),
            },
        );
        let a = event(
            AgentProvider::Codex,
            1,
            AgentEventKind::Usage {
                response_id: "native:s:r".into(),
                tokens: Some(AgentTokens {
                    input: 10,
                    output: 1,
                    ..Default::default()
                }),
                requests: 1,
                reported_cost_usd: None,
                fast: false,
                cumulative: false,
                cache_write_1h: None,
            },
        );
        let mut b = a.clone();
        b.id = "new-stream-block".into();
        b.at_ms = IDLE_MS + 100;
        let i = fold([start, a, b]);
        assert_eq!(i.usage.len(), 1);
        assert_eq!(i.usage[0].at_ms, 1);
        assert_eq!(
            task_at(&i.tasks[0], IDLE_MS + 101).state,
            AgentTaskState::Working
        );
    }

    #[test]
    fn post_cut_activity_in_the_same_millisecond_starts_a_new_epoch() {
        let start = event(
            AgentProvider::Claude,
            1,
            AgentEventKind::Started {
                task_id: "original".into(),
            },
        );
        let task = fold([start.clone()]).tasks.remove(0);
        let mut barrier = event(
            AgentProvider::Claude,
            30,
            AgentEventKind::Interrupted {
                task_id: task.id,
                offline_since_ms: 10,
                confirmed_exit: false,
            },
        );
        barrier.sequence = 100;
        let mut reply = event(
            AgentProvider::Claude,
            30,
            AgentEventKind::Usage {
                response_id: "new-reply".into(),
                tokens: Some(AgentTokens {
                    input: 10,
                    output: 2,
                    ..Default::default()
                }),
                requests: 1,
                reported_cost_usd: None,
                fast: false,
                cumulative: false,
                cache_write_1h: None,
            },
        );
        reply.sequence = 200;
        reply.model = "real-model".into();
        let index = fold([start, barrier, reply]);
        assert_eq!(index.tasks.len(), 2);
        assert_eq!(index.tasks[0].completed, None);
        assert_eq!(index.tasks[0].ended_at_ms, Some(1));
        assert_eq!(index.tasks[1].started_at_ms, 30);
        assert_eq!(index.usage.len(), 1);
    }
}
