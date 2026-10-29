//! Exact task/process/pane matching, without current-directory heuristics.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};

use super::model::{AgentTask, RuntimeBinding};
use super::runtime::{RuntimeInventory, RuntimePane, RuntimeProcess};

#[derive(Debug, Clone, Default)]
pub(crate) struct BindingBook {
    pub manual: BTreeMap<String, RuntimeBinding>,
    /// An explicit detach lasts for this process identity, not forever.
    pub detached: BTreeMap<String, RuntimeBinding>,
}

fn descendant(mut pid: u32, ancestor: u32, inventory: &RuntimeInventory) -> bool {
    let mut seen = BTreeSet::new();
    while pid > 0 && seen.insert(pid) {
        if pid == ancestor {
            return true;
        }
        match inventory.parent_pids.get(&pid) {
            Some(parent) => pid = *parent,
            None => break,
        }
    }
    false
}

/// Collapse only runtime wrappers (node/bun) into their actual native child.
/// Independent native CLIs in the same pane remain distinct/ambiguous.
fn leaves<'a>(
    pane: &'a RuntimePane,
    provider: super::model::AgentProvider,
    inventory: &RuntimeInventory,
) -> Vec<&'a RuntimeProcess> {
    let candidates: Vec<_> = pane
        .candidates
        .iter()
        .filter(|process| process.provider == provider)
        .collect();
    candidates
        .iter()
        .copied()
        .filter(|process| {
            !process.wrapper
                || !candidates.iter().any(|other| {
                    other.pid != process.pid && descendant(other.pid, process.pid, inventory)
                })
        })
        .collect()
}

fn matching_sessions(
    process: &RuntimeProcess,
    tasks: &[AgentTask],
    inventory: &RuntimeInventory,
) -> BTreeSet<String> {
    let mut matches = BTreeSet::new();
    // A node wrapper may own the explicit resume argument while its child
    // owns the native process identity and rollout file.
    for source in inventory.processes.iter().filter(|source| {
        source.pid == process.pid
            || source.wrapper
                && source.provider == process.provider
                && descendant(process.pid, source.pid, inventory)
    }) {
        for evidence in &source.evidence {
            let candidates: BTreeSet<_> = tasks
                .iter()
                .filter(|task| {
                    task.provider == process.provider
                        && task.native_id == evidence.native_id
                        && evidence
                            .profile
                            .as_ref()
                            .is_none_or(|profile| profile == &task.profile)
                })
                .map(|task| task.session_id.clone())
                .collect();
            // Native IDs copied into several profiles are not enough to choose.
            if candidates.len() == 1 {
                matches.extend(candidates);
            }
        }
    }
    let active: BTreeSet<_> = matches
        .iter()
        .filter(|id| {
            tasks
                .iter()
                .any(|task| &task.session_id == *id && task.ended_at_ms.is_none())
        })
        .cloned()
        .collect();
    if !active.is_empty() {
        active
    } else {
        matches
    }
}

fn present(binding: &RuntimeBinding, inventory: &RuntimeInventory, task: &AgentTask) -> bool {
    binding.process_started_at.is_some()
        && inventory.panes.iter().any(|pane| {
            pane.pane_id == binding.pane_id
                && pane.candidates.iter().any(|process| {
                    process.pid == binding.pid
                        && (process.process_started_at == binding.process_started_at
                            || process.process_started_at.is_none())
                        && process.provider == task.provider
                })
        })
}

fn present_identity(binding: &RuntimeBinding, inventory: &RuntimeInventory) -> bool {
    if binding.process_started_at.is_none() {
        return false;
    }
    inventory.panes.iter().any(|pane| {
        if pane.pane_id != binding.pane_id {
            return false;
        }
        match pane
            .candidates
            .iter()
            .find(|process| process.pid == binding.pid)
        {
            Some(process) => {
                process.process_started_at.is_none()
                    || process.process_started_at == binding.process_started_at
            }
            // A partial candidate probe is unknown, not process exit. Such a
            // retained choice is emitted only after a matching provider task
            // and a verified candidate are available again.
            None => {
                inventory.parent_pids.contains_key(&binding.pid)
                    && descendant(binding.pid, pane.pid, inventory)
            }
        }
    })
}

fn expand(binding: &RuntimeBinding, inventory: &RuntimeInventory) -> Vec<RuntimeBinding> {
    inventory
        .panes
        .iter()
        .filter(|pane| pane.pane_id == binding.pane_id)
        .map(|pane| {
            let mut bound = binding.clone();
            bound.tmux_session = pane.tmux_session.clone();
            bound
        })
        .collect()
}

/// Resolve a task only when its exact evidence identifies one physical pane.
pub(crate) fn resolve(
    tasks: &[AgentTask],
    inventory: &RuntimeInventory,
    previous: &[RuntimeBinding],
    book: &mut BindingBook,
) -> Vec<RuntimeBinding> {
    if !inventory.available {
        return previous.to_vec();
    }
    let task_map: BTreeMap<_, _> = tasks
        .iter()
        .map(|task| (task.session_id.clone(), task))
        .collect();
    book.manual
        .retain(|_, binding| present_identity(binding, inventory));
    book.detached
        .retain(|_, binding| present_identity(binding, inventory));
    let mut choices: BTreeMap<String, BTreeMap<(String, u32, u64), RuntimeBinding>> =
        BTreeMap::new();
    for pane in &inventory.panes {
        for provider in super::model::AgentProvider::ALL {
            for process in leaves(pane, provider, inventory) {
                let Some(start) = process.process_started_at else {
                    continue;
                };
                let matches = matching_sessions(process, tasks, inventory);
                if matches.len() != 1 {
                    continue;
                }
                let id = matches.into_iter().next().unwrap();
                choices.entry(id.clone()).or_default().insert(
                    (pane.pane_id.clone(), process.pid, start),
                    RuntimeBinding {
                        session_id: id,
                        tmux_session: pane.tmux_session.clone(),
                        pane_id: pane.pane_id.clone(),
                        pid: process.pid,
                        process_started_at: Some(start),
                    },
                );
            }
        }
    }
    let mut selected: BTreeMap<String, RuntimeBinding> = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for (id, choices) in choices {
        if choices.len() == 1 {
            selected.insert(id, choices.into_values().next().unwrap());
        } else {
            ambiguous.insert(id);
        }
    }
    for previous in previous {
        if ambiguous.contains(&previous.session_id) {
            continue;
        }
        if selected.contains_key(&previous.session_id) {
            continue;
        }
        let Some(task) = task_map.get(&previous.session_id) else {
            continue;
        };
        if !present(previous, inventory, task) {
            continue;
        }
        let process = inventory.processes.iter().find(|process| {
            process.pid == previous.pid
                && (process.process_started_at == previous.process_started_at
                    || process.process_started_at.is_none())
        });
        let Some(process) = process else { continue };
        let current = matching_sessions(process, tasks, inventory);
        // Temporary fd/registry silence does not sever a previously exact link.
        if current.is_empty() || current == BTreeSet::from([previous.session_id.clone()]) {
            selected.insert(previous.session_id.clone(), previous.clone());
        }
    }
    for (id, binding) in &book.manual {
        if task_map
            .get(id)
            .is_some_and(|task| present(binding, inventory, task))
        {
            selected.insert(id.clone(), binding.clone());
        }
    }
    for id in book.detached.keys() {
        selected.remove(id);
    }
    let manual_identities: BTreeMap<_, _> = book
        .manual
        .iter()
        .filter(|(id, _)| selected.contains_key(*id))
        .map(|(id, binding)| ((binding.pid, binding.process_started_at), id))
        .collect();
    selected.retain(|id, binding| {
        manual_identities
            .get(&(binding.pid, binding.process_started_at))
            .is_none_or(|manual| *manual == id)
    });
    let mut result: Vec<_> = selected
        .values()
        .flat_map(|binding| expand(binding, inventory))
        .collect();
    result.sort_by(|a, b| {
        (&a.session_id, &a.tmux_session, &a.pane_id).cmp(&(
            &b.session_id,
            &b.tmux_session,
            &b.pane_id,
        ))
    });
    result.dedup_by(|a, b| {
        a.session_id == b.session_id && a.tmux_session == b.tmux_session && a.pane_id == b.pane_id
    });
    result
}

pub(crate) fn manual_candidate(
    session_id: &str,
    pane_id: &str,
    tasks: &[AgentTask],
    inventory: &RuntimeInventory,
    existing: &[RuntimeBinding],
) -> Result<RuntimeBinding> {
    if !inventory.available {
        bail!("agent process inventory is unavailable");
    }
    let task = tasks
        .iter()
        .filter(|task| task.session_id == session_id)
        .max_by_key(|task| task.last_activity_at_ms)
        .context("agent session not found")?;
    if task.ended_at_ms.is_some() {
        bail!("agent task has already ended");
    }
    let pane = inventory
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .context("tmux pane not found")?;
    let candidates = leaves(pane, task.provider, inventory);
    if candidates.len() != 1 {
        bail!("pane does not contain exactly one matching agent process");
    }
    let process = candidates[0];
    let start = process
        .process_started_at
        .context("agent process identity is unavailable")?;
    let exact = matching_sessions(process, tasks, inventory);
    if !exact.is_empty() && !exact.contains(session_id) {
        bail!("process belongs to another agent session");
    }
    if existing.iter().any(|binding| {
        binding.session_id != session_id
            && binding.pid == process.pid
            && binding.process_started_at == Some(start)
            && tasks
                .iter()
                .any(|other| other.session_id == binding.session_id && other.ended_at_ms.is_none())
    }) {
        bail!("process is already bound to another active session");
    }
    Ok(RuntimeBinding {
        session_id: session_id.to_owned(),
        tmux_session: pane.tmux_session.clone(),
        pane_id: pane_id.to_owned(),
        pid: process.pid,
        process_started_at: Some(start),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::{AgentProvider, AgentTaskState};
    use crate::agents::runtime::NativeEvidence;
    fn task(id: &str) -> AgentTask {
        AgentTask {
            id: format!("task-{id}"),
            session_id: id.into(),
            provider: AgentProvider::Codex,
            profile: "/profile".into(),
            native_id: id.into(),
            parent_session_id: None,
            project: "/same".into(),
            model: "model".into(),
            started_at_ms: 10,
            last_activity_at_ms: 20,
            ended_at_ms: None,
            duration_ms: None,
            elapsed_ms: 10,
            state: AgentTaskState::Working,
            completed: None,
            tokens: None,
            estimated_cost_usd: None,
            reported_cost_usd: None,
            requests: 0,
            unpriced_requests: 0,
            bindings: vec![],
            running_commands: Default::default(),
            settled_at_ms: None,
        }
    }
    fn process(
        pid: u32,
        parent: u32,
        start: u64,
        id: Option<&str>,
        wrapper: bool,
    ) -> RuntimeProcess {
        RuntimeProcess {
            pid,
            parent_pid: parent,
            process_started_at: Some(start),
            provider: AgentProvider::Codex,
            native_ids: id.map(|id| vec![id.into()]).unwrap_or_default(),
            evidence: id
                .map(|id| {
                    vec![NativeEvidence {
                        native_id: id.into(),
                        profile: Some("/profile".into()),
                    }]
                })
                .unwrap_or_default(),
            wrapper,
        }
    }
    fn pane(id: &str, name: &str, pid: u32, candidates: Vec<RuntimeProcess>) -> RuntimePane {
        RuntimePane {
            pane_id: id.into(),
            tmux_session: name.into(),
            session_id: format!("${pid}"),
            window_id: format!("@{pid}"),
            pid,
            process_started_at: Some(1),
            tty: format!("/dev/{pid}"),
            path: "/same".into(),
            active: false,
            candidates,
        }
    }
    fn inventory(panes: Vec<RuntimePane>) -> RuntimeInventory {
        let processes: BTreeMap<_, _> = panes
            .iter()
            .flat_map(|pane| pane.candidates.iter())
            .map(|process| (process.pid, process.clone()))
            .collect();
        RuntimeInventory {
            parent_pids: processes
                .values()
                .map(|process| (process.pid, process.parent_pid))
                .collect(),
            processes: processes.into_values().collect(),
            panes,
            available: true,
            ..Default::default()
        }
    }
    #[test]
    fn same_cwd_never_binds_an_unidentified_process() {
        let inventory = inventory(vec![
            pane("%1", "one", 10, vec![process(11, 10, 11, None, false)]),
            pane("%2", "two", 20, vec![process(21, 20, 21, Some("b"), false)]),
        ]);
        let bindings = resolve(
            &[task("a"), task("b")],
            &inventory,
            &[],
            &mut BindingBook::default(),
        );
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].session_id, "b");
        assert_eq!(bindings[0].tmux_session, "two");
    }
    #[test]
    fn wrapper_and_native_leaf_count_as_one_manual_candidate() {
        let inventory = inventory(vec![pane(
            "%1",
            "one",
            10,
            vec![
                process(11, 10, 11, None, true),
                process(12, 11, 12, None, false),
            ],
        )]);
        let binding = manual_candidate("a", "%1", &[task("a")], &inventory, &[]).unwrap();
        assert_eq!(binding.pid, 12);
        let mut book = BindingBook::default();
        book.manual.insert("a".into(), binding);
        assert_eq!(resolve(&[task("a")], &inventory, &[], &mut book).len(), 1);
    }
    #[test]
    fn independent_native_clis_are_ambiguous_even_inside_one_pane() {
        let inventory = inventory(vec![pane(
            "%1",
            "one",
            10,
            vec![
                process(11, 10, 11, None, false),
                process(12, 10, 12, None, false),
            ],
        )]);
        assert!(manual_candidate("a", "%1", &[task("a")], &inventory, &[]).is_err());
    }
    #[test]
    fn linked_panes_expand_and_manual_mapping_expires_on_pid_reuse() {
        let process = process(11, 10, 11, None, false);
        let mut inventory = inventory(vec![
            pane("%1", "one", 10, vec![process.clone()]),
            pane("%1", "linked", 10, vec![process]),
        ]);
        let binding = manual_candidate("a", "%1", &[task("a")], &inventory, &[]).unwrap();
        let mut book = BindingBook::default();
        book.manual.insert("a".into(), binding);
        let linked = resolve(&[task("a")], &inventory, &[], &mut book);
        assert_eq!(linked.len(), 2);
        for pane in &mut inventory.panes {
            pane.candidates[0].process_started_at = Some(99);
        }
        inventory.processes[0].process_started_at = Some(99);
        assert!(resolve(&[task("a")], &inventory, &linked, &mut book).is_empty());
        assert!(book.manual.is_empty());
    }
    #[test]
    fn explicit_resume_on_wrapper_binds_native_child_in_inactive_pane() {
        let inventory = inventory(vec![pane(
            "%1",
            "one",
            10,
            vec![
                process(11, 10, 11, Some("a"), true),
                process(12, 11, 12, None, false),
            ],
        )]);
        let bindings = resolve(&[task("a")], &inventory, &[], &mut BindingBook::default());
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].pid, 12);
    }
    #[test]
    fn duplicate_native_session_across_profiles_is_not_guessed() {
        let mut other = task("a");
        other.session_id = "other-profile".into();
        other.profile = "/other".into();
        let mut process = process(11, 10, 11, Some("a"), false);
        process.evidence[0].profile = None;
        let inventory = inventory(vec![pane("%1", "one", 10, vec![process])]);
        assert!(resolve(
            &[task("a"), other],
            &inventory,
            &[],
            &mut BindingBook::default()
        )
        .is_empty());
    }
    #[test]
    fn failed_inventory_retains_previous_and_detach_lasts_only_for_process() {
        let mut inventory = inventory(vec![pane(
            "%1",
            "one",
            10,
            vec![process(11, 10, 11, Some("a"), false)],
        )]);
        let mut book = BindingBook::default();
        let bindings = resolve(&[task("a")], &inventory, &[], &mut book);
        inventory.available = false;
        assert_eq!(
            resolve(&[task("a")], &inventory, &bindings, &mut book),
            bindings
        );
        inventory.available = true;
        book.detached.insert("a".into(), bindings[0].clone());
        assert!(resolve(&[task("a")], &inventory, &bindings, &mut book).is_empty());
        inventory.panes[0].candidates[0].process_started_at = Some(99);
        inventory.processes[0].process_started_at = Some(99);
        assert_eq!(resolve(&[task("a")], &inventory, &[], &mut book).len(), 1);
        assert!(book.detached.is_empty());
    }
    #[test]
    fn manual_binding_survives_offline_interruption_and_new_activity_on_same_process() {
        let inventory = inventory(vec![pane(
            "%1",
            "one",
            10,
            vec![process(11, 10, 11, None, false)],
        )]);
        let original = task("a");
        let binding = manual_candidate("a", "%1", &[original.clone()], &inventory, &[]).unwrap();
        let mut book = BindingBook::default();
        book.manual.insert("a".into(), binding.clone());
        let initial = resolve(&[original.clone()], &inventory, &[], &mut book);
        assert_eq!(initial, vec![binding.clone()]);
        let mut interrupted = original.clone();
        interrupted.ended_at_ms = Some(25);
        interrupted.completed = None;
        interrupted.state = AgentTaskState::Unknown;
        let during = resolve(&[interrupted.clone()], &inventory, &initial, &mut book);
        assert_eq!(during, initial);
        assert_eq!(book.manual.len(), 1);
        let mut resumed = original;
        resumed.id = "new-offline-epoch".into();
        resumed.started_at_ms = 30;
        resumed.last_activity_at_ms = 40;
        assert_eq!(
            resolve(&[interrupted, resumed], &inventory, &during, &mut book),
            vec![binding]
        );
        assert_eq!(book.manual.len(), 1);
    }
    #[test]
    fn task_history_cutoff_does_not_expire_process_lifetime_manual_or_detach_choices() {
        let inventory = inventory(vec![pane(
            "%1",
            "one",
            10,
            vec![process(11, 10, 11, None, false)],
        )]);
        let original = task("a");
        let binding = manual_candidate("a", "%1", &[original.clone()], &inventory, &[]).unwrap();
        let mut book = BindingBook::default();
        book.manual.insert("a".into(), binding.clone());
        // Yesterday's completed task fell outside the metadata query, while
        // its interactive CLI remains alive waiting for a new prompt.
        assert!(resolve(&[], &inventory, &[binding.clone()], &mut book).is_empty());
        assert_eq!(book.manual.len(), 1);
        let mut resumed = original;
        resumed.id = "tomorrow-prompt".into();
        resumed.started_at_ms = 2 * 86_400_000;
        resumed.last_activity_at_ms = resumed.started_at_ms;
        assert_eq!(
            resolve(&[resumed.clone()], &inventory, &[], &mut book),
            vec![binding.clone()]
        );
        book.manual.clear();
        book.detached.insert("a".into(), binding);
        assert!(resolve(&[], &inventory, &[], &mut book).is_empty());
        assert_eq!(book.detached.len(), 1);
        assert!(resolve(&[resumed], &inventory, &[], &mut book).is_empty());
        assert_eq!(book.detached.len(), 1);
    }
}
