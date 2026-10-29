//! Private, versioned metadata-only source cursors and contributions.
use super::{
    jsonl::JsonlCursor, model::AgentEvent, opencode::OpenCodeCursor, sources::SourceDescriptor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const CACHE_VERSION: u32 = 3;
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub inode: u64,
    pub len: u64,
    pub modified_ms: i64,
    pub wal_len: u64,
    pub wal_modified_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceState {
    pub descriptor: SourceDescriptor,
    pub stamp: Option<Stamp>,
    pub jsonl: Option<JsonlCursor>,
    pub opencode: Option<OpenCodeCursor>,
    pub more: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub session_ids: BTreeSet<String>,
    /// Actual event owners, excluding parent-only linkage. Rebuilt once after
    /// loading old caches and whenever this source contribution changes.
    #[serde(skip)]
    pub event_session_ids: BTreeSet<String>,
}
impl SourceState {
    pub fn new(descriptor: SourceDescriptor) -> Self {
        Self {
            descriptor,
            stamp: None,
            jsonl: None,
            opencode: None,
            more: true,
            error: None,
            session_ids: BTreeSet::new(),
            event_session_ids: BTreeSet::new(),
        }
    }
    pub fn events(&self) -> &[super::model::AgentEvent] {
        self.jsonl
            .as_ref()
            .map(|c| c.events.as_slice())
            .or_else(|| self.opencode.as_ref().map(|c| c.events.as_slice()))
            .unwrap_or(&[])
    }
    pub fn refresh_session_ids(&mut self) {
        let mut owners = BTreeSet::new();
        let mut related = BTreeSet::new();
        for event in self.events() {
            // One encoded identity per distinct source/session, rather than
            // encoding the same identity for every historical event.
            owners.insert(&event.session);
            related.insert(&event.session);
            related.extend(event.parent.iter());
        }
        let owners = owners.into_iter().map(|id| id.key()).collect();
        let related = related.into_iter().map(|id| id.key()).collect();
        self.event_session_ids = owners;
        self.session_ids = related;
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheSnapshot {
    pub version: u32,
    pub updated_at_ms: i64,
    pub sources: BTreeMap<String, SourceState>,
    #[serde(default)]
    pub runtime_barriers: Vec<AgentEvent>,
}
pub fn load(path: &Path) -> Option<CacheSnapshot> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 256 * 1024 * 1024 {
        return None;
    }
    let mut snapshot: CacheSnapshot = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if snapshot.version != CACHE_VERSION {
        return None;
    }
    for source in snapshot.sources.values_mut() {
        source.refresh_session_ids();
    }
    (snapshot.version == CACHE_VERSION).then_some(snapshot)
}
pub fn save(
    path: &Path,
    sources: &BTreeMap<String, SourceState>,
    updated_at_ms: i64,
    runtime_barriers: &[AgentEvent],
) -> anyhow::Result<()> {
    #[derive(Serialize)]
    struct SnapshotRef<'a> {
        version: u32,
        updated_at_ms: i64,
        sources: &'a BTreeMap<String, SourceState>,
        runtime_barriers: &'a [AgentEvent],
    }
    let snapshot = SnapshotRef {
        version: CACHE_VERSION,
        updated_at_ms,
        sources,
        runtime_barriers,
    };
    forge_atomic::write_bytes_atomic(path, &serde_json::to_vec(&snapshot)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{
        model::{AgentEventKind, AgentProvider, AgentSessionId},
        sources::SourceDescriptor,
    };

    #[test]
    fn old_v3_cache_rebuilds_owner_ids_without_treating_parent_links_as_owners() {
        let dir = tempfile::tempdir().unwrap();
        let owner = AgentSessionId {
            provider: AgentProvider::Codex,
            profile: "/profile".into(),
            native_id: "child".into(),
        };
        let parent = AgentSessionId {
            native_id: "root".into(),
            ..owner.clone()
        };
        let mut source = SourceState::new(SourceDescriptor {
            path: "/journal".into(),
            provider: owner.provider,
            profile: owner.profile.clone(),
            modified_ms: 10,
        });
        source.jsonl = Some(JsonlCursor {
            events: vec![AgentEvent {
                id: "activity".into(),
                at_ms: 10,
                session: owner.clone(),
                sequence: 1,
                parent: Some(parent.clone()),
                project: "/project".into(),
                model: "model".into(),
                kind: AgentEventKind::Active,
            }],
            ..Default::default()
        });
        source.refresh_session_ids();
        assert_eq!(source.event_session_ids, BTreeSet::from([owner.key()]));
        assert_eq!(
            source.session_ids,
            BTreeSet::from([owner.key(), parent.key()])
        );
        let sources = BTreeMap::from([("/journal".into(), source)]);
        let legacy = CacheSnapshot {
            version: CACHE_VERSION,
            updated_at_ms: 10,
            sources: sources.clone(),
            runtime_barriers: vec![],
        };
        let expected = serde_json::to_vec(&legacy).unwrap();
        assert!(!String::from_utf8_lossy(&expected).contains("event_session_ids"));
        let path = dir.path().join("snapshot.json");
        save(&path, &sources, 10, &[]).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected,
            "borrowed snapshot must keep the old v3 byte representation"
        );
        let loaded = load(&path).unwrap();
        assert_eq!(
            loaded.sources["/journal"].event_session_ids,
            BTreeSet::from([owner.key()])
        );
        assert_eq!(
            loaded.sources["/journal"].session_ids,
            BTreeSet::from([owner.key(), parent.key()])
        );
    }
}
