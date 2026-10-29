//! Incremental JSONL reader. Cursors retain metadata, never source text.
use super::{
    model::AgentEvent,
    providers::{self, ParserState},
    sources::{modified_ms, SourceDescriptor},
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

const MAX_LINE: usize = 8 * 1024 * 1024;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JsonlCursor {
    pub offset: u64,
    pub len: u64,
    pub modified_ms: i64,
    pub inode: u64,
    pub checkpoint: u64,
    pub incomplete: bool,
    pub parser: ParserState,
    pub events: Vec<AgentEvent>,
}
#[derive(Debug)]
pub struct ReadResult {
    pub cursor: JsonlCursor,
    pub bytes_read: u64,
    pub more: bool,
    pub reset: bool,
}

/// Retain at most one pre-horizon context and the state crossing the boundary.
/// Old usage becomes a metadata-only activity anchor, never historical spend.
pub fn prune(cursor: &mut JsonlCursor, since_ms: i64) -> bool {
    use super::model::AgentEventKind;
    use std::collections::BTreeMap;
    let mut contexts: BTreeMap<String, AgentEvent> = BTreeMap::new();
    let mut boundary: BTreeMap<String, (AgentEvent, Option<AgentEvent>, Option<AgentEvent>)> =
        BTreeMap::new();
    let mut commands: BTreeMap<(String, String), AgentEvent> = BTreeMap::new();
    for e in cursor.events.iter().filter(|e| e.at_ms < since_ms) {
        let sid = e.session.key();
        match &e.kind {
            AgentEventKind::Context => {
                contexts.insert(sid, e.clone());
            }
            AgentEventKind::Started { .. } if e.parent.is_none() => {
                boundary.insert(sid, (e.clone(), None, None));
            }
            AgentEventKind::Active if e.parent.is_none() => {
                let b = boundary
                    .entry(sid)
                    .or_insert_with(|| (e.clone(), None, None));
                b.1 = Some(e.clone());
                b.2 = None;
            }
            AgentEventKind::Usage { .. } => {
                let root = e.parent.as_ref().map(|p| p.key()).unwrap_or(sid);
                if let Some((start, active, settled)) = boundary.get_mut(&root) {
                    let mut anchor = e.clone();
                    anchor.id = format!("retention:activity:{root}:{}", e.at_ms);
                    anchor.session = start.session.clone();
                    anchor.parent = None;
                    anchor.project = start.project.clone();
                    anchor.model = start.model.clone();
                    anchor.kind = AgentEventKind::Active;
                    *active = Some(anchor);
                    *settled = None;
                }
            }
            AgentEventKind::Settled if e.parent.is_none() => {
                if let Some(b) = boundary.get_mut(&sid) {
                    b.2 = Some(e.clone());
                }
            }
            AgentEventKind::Ended { .. } if e.parent.is_none() => {
                boundary.remove(&sid);
                commands.retain(|(s, _), _| s != &sid);
            }
            AgentEventKind::Command { id, running } => {
                let key = (sid, id.clone());
                if *running {
                    commands.insert(key, e.clone());
                } else {
                    commands.remove(&key);
                }
            }
            _ => {}
        }
    }
    let anchors: Vec<AgentEvent> = contexts
        .into_values()
        .chain(boundary.into_values().flat_map(|(start, active, settled)| {
            std::iter::once(start).chain(active).chain(settled)
        }))
        .chain(commands.into_values())
        .collect();
    let before: BTreeMap<_, _> = cursor
        .events
        .iter()
        .filter(|e| e.at_ms < since_ms)
        .map(|e| ((e.session.key(), e.id.clone()), e))
        .collect();
    let changed = before.len() != anchors.len()
        || anchors
            .iter()
            .any(|e| before.get(&(e.session.key(), e.id.clone())).copied() != Some(e));
    cursor.events.retain(|e| e.at_ms >= since_ms);
    cursor.events.extend(anchors);
    cursor
        .events
        .sort_by(|a, b| a.sequence.cmp(&b.sequence).then_with(|| a.id.cmp(&b.id)));
    // Most unchanged retained anchors do not require a new index publication.
    changed
}

fn inode(m: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        0
    }
}
fn checkpoint(file: &mut File, offset: u64) -> std::io::Result<u64> {
    let start = offset.saturating_sub(256);
    file.seek(SeekFrom::Start(start))?;
    let mut b = vec![0; usize::try_from(offset - start).unwrap_or(256)];
    file.read_exact(&mut b)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    Ok(h.finish())
}
/// Consume a complete line without ever allocating an unbounded tool payload.
fn bounded_line<R: BufRead>(r: &mut R) -> std::io::Result<(Vec<u8>, u64, bool)> {
    let mut kept = Vec::new();
    let mut consumed = 0;
    let mut oversized = false;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok((kept, consumed, false));
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(buf.len());
        let end = buf[n - 1] == b'\n';
        consumed += n as u64;
        if !oversized && kept.len() + n <= MAX_LINE {
            kept.extend_from_slice(&buf[..n]);
        } else {
            kept.clear();
            oversized = true;
        }
        r.consume(n);
        if end {
            return Ok((kept, consumed, true));
        }
    }
}

/// A failed read returns an error; callers retain the last good contribution.
pub fn read(
    source: &SourceDescriptor,
    previous: Option<&JsonlCursor>,
    since_ms: i64,
    budget: u64,
) -> anyhow::Result<ReadResult> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&source.path)?
    };
    #[cfg(not(unix))]
    let mut file = File::open(&source.path)?;
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "source is not a regular file");
    let len = meta.len();
    let mtime = modified_ms(&meta);
    let ino = inode(&meta);
    let mut reset = previous.is_none();
    let mut verified_bytes = 0;
    if let Some(p) = previous {
        reset = p.inode != ino
            || len < p.offset
            || mtime < p.modified_ms
            || (len == p.len && mtime != p.modified_ms);
        if !reset && p.offset > 0 {
            verified_bytes += p.offset.min(256);
            reset = checkpoint(&mut file, p.offset)? != p.checkpoint;
        }
    }
    let mut cursor = if reset {
        JsonlCursor::default()
    } else {
        previous.cloned().unwrap_or_default()
    };
    if !reset
        && cursor.len == len
        && cursor.modified_ms == mtime
        && (cursor.offset == len || cursor.incomplete)
    {
        return Ok(ReadResult {
            cursor,
            bytes_read: verified_bytes,
            more: false,
            reset: false,
        });
    }
    cursor.len = len;
    cursor.modified_ms = mtime;
    cursor.inode = ino;
    cursor.incomplete = false;
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut r = BufReader::with_capacity(64 * 1024, file);
    let mut bytes_read = 0;
    let fallback = source
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    loop {
        let (line, consumed, complete) = bounded_line(&mut r)?;
        bytes_read += consumed;
        if !complete {
            cursor.incomplete = consumed > 0;
            break;
        }
        let line_offset = cursor.offset;
        cursor.offset = cursor.offset.saturating_add(consumed);
        if !line.is_empty() {
            let events = providers::parse(
                source.provider,
                &source.profile,
                fallback,
                &line,
                mtime,
                line_offset,
                &mut cursor.parser,
            );
            cursor.events.extend(events.into_iter().filter(|e| {
                e.at_ms >= since_ms || !matches!(e.kind, super::model::AgentEventKind::Usage { .. })
            }));
        }
        if bytes_read >= budget {
            break;
        }
    }
    cursor.checkpoint = checkpoint(r.get_mut(), cursor.offset)?;
    bytes_read += verified_bytes + cursor.offset.min(256);
    let more = cursor.offset < len && !cursor.incomplete;
    Ok(ReadResult {
        cursor,
        bytes_read,
        more,
        reset,
    })
}

#[cfg(test)]
mod tests {
    use super::super::model::AgentProvider;
    use super::*;
    fn descriptor(path: &std::path::Path) -> SourceDescriptor {
        SourceDescriptor {
            path: path.into(),
            provider: AgentProvider::Codex,
            profile: "/test".into(),
            modified_ms: 0,
        }
    }
    #[test]
    fn partial_append_and_unchanged_do_not_reparse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let s = descriptor(&path);
        std::fs::write(&path,b"{\"type\":\"event_msg\",\"timestamp\":\"2026-10-25T10:00:00Z\",\"payload\":{\"type\":\"task_started\"}}").unwrap();
        let a = read(&s, None, 0, 1024).unwrap();
        assert_eq!(a.cursor.offset, 0);
        assert!(a.cursor.events.is_empty());
        let b = read(&s, Some(&a.cursor), 0, 1024).unwrap();
        assert_eq!(b.bytes_read, 0);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let c = read(&s, Some(&b.cursor), 0, 1024).unwrap();
        assert_eq!(c.cursor.events.len(), 1);
        assert_eq!(c.cursor.offset, c.cursor.len);
        // The standalone reader verifies a bounded checkpoint. The coordinator
        // avoids opening unchanged sources altogether using metadata.
        assert!(read(&s, Some(&c.cursor), 0, 1024).unwrap().bytes_read <= 256);
    }
    #[test]
    fn replacement_discards_contribution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let s = descriptor(&path);
        std::fs::write(&path, b"{}\n").unwrap();
        let a = read(&s, None, 0, 1024).unwrap();
        std::fs::write(&path, b"\n").unwrap();
        let b = read(&s, Some(&a.cursor), 0, 1024).unwrap();
        assert!(b.reset);
        assert_eq!(b.cursor.offset, 1);
    }
}
