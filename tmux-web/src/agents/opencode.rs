//! Read-only OpenCode SQLite ingestion and provider-neutral task events.
//!
//! SQLite projects only metadata; prompts, replies and tool output never leave
//! the source database. A cursor retains small per-message contributions so a
//! streaming update, revert or rowid reuse replaces earlier data atomically.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::model::{
    AgentEvent, AgentEventKind, AgentProvider, AgentSessionId, AgentTokens, IDLE_MS, SETTLE_MS,
};

const RECHECK_BATCH: usize = 200;
const TAIL_LENGTH: usize = 64;

/// Serializable metadata and events belonging to one source database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenCodeCursor {
    pub events: Vec<AgentEvent>,
    identity: (u64, u64),
    version: u8,
    profile: String,
    since_ms: i64,
    last_rowid: i64,
    rows: BTreeMap<String, Message>,
    tail: Vec<(i64, String)>,
    /// Number of source rows whose JSON metadata was decoded in this read.
    pub decoded_rows: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Message {
    rowid: i64,
    id: String,
    session: String,
    parent_session: Option<String>,
    directory: String,
    project: String,
    role: String,
    parent_message: String,
    model: String,
    created: i64,
    updated: i64,
    completed: Option<i64>,
    finish: Option<String>,
    error: bool,
    tool_calls: bool,
    auto_compaction: bool,
    tokens: Option<AgentTokens>,
    cost: Option<f64>,
    outcome: Option<String>,
    status: Option<String>,
    length: i64,
}

impl Message {
    fn writing(&self) -> bool {
        (self.role == "assistant"
            && self.completed.is_none()
            && self.finish.is_none()
            && !self.error)
            || (self.role == "compaction" && self.status.as_deref() == Some("running"))
    }
}

#[derive(Debug, Clone)]
struct Layout {
    version: u8,
    messages: &'static str,
    sessions: &'static str,
    parent_column: bool,
    parts: bool,
}

/// Read appended/changed metadata in one coherent read transaction.
///
/// Errors leave `previous` untouched. In particular SQLITE_BUSY is never a
/// task completion signal. Do not use SQLite's immutable mode here: committed
/// messages may live exclusively in the writer's WAL.
pub fn read(
    path: &Path,
    profile: &str,
    since_ms: i64,
    previous: Option<&OpenCodeCursor>,
) -> Result<OpenCodeCursor> {
    let metadata = std::fs::metadata(path).context("OpenCode database metadata")?;
    if !metadata.is_file() {
        bail!("OpenCode source is not a regular file");
    }
    let identity = file_identity(&metadata);
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("OpenCode read-only database")?;
    connection.busy_timeout(Duration::from_millis(500))?;
    let transaction = connection.unchecked_transaction()?;
    let layout = layout(&transaction)?;
    let compatible = previous.filter(|cursor| {
        cursor.identity == identity
            && cursor.version == layout.version
            && cursor.profile == profile
            && cursor.since_ms <= since_ms
    });
    let mut cursor = compatible.cloned().unwrap_or_default();
    cursor.decoded_rows = 0;
    cursor.identity = identity;
    cursor.profile = profile.to_owned();
    cursor.version = layout.version;
    cursor.since_ms = since_ms;

    // Validate the last saved rowids before trusting the append boundary.
    // A revert may delete every recent row and SQLite may reuse its rowid.
    if !cursor.tail.is_empty() {
        let mut newest_valid = None;
        let sql = format!("SELECT id FROM {} WHERE rowid = ?1", layout.messages);
        let mut statement = transaction.prepare(&sql)?;
        for (rowid, id) in cursor.tail.iter().rev() {
            let current: Option<String> =
                statement.query_row([rowid], |row| row.get(0)).optional()?;
            if current.as_deref() == Some(id.as_str()) {
                newest_valid = Some(*rowid);
                break;
            }
        }
        cursor.last_rowid = newest_valid.unwrap_or(0);
    }

    let projection = projection(&layout);
    let ids: Vec<String> = cursor.rows.keys().cloned().collect();
    let mut changed = BTreeSet::new();
    let mut present = BTreeSet::new();
    for batch in ids.chunks(RECHECK_BATCH) {
        let placeholders = vec!["?"; batch.len()].join(",");
        let parent = if layout.parent_column {
            "s.parent_id"
        } else {
            "NULL"
        };
        // Closed rows need no JSON reads. Mutable rows additionally compare
        // data length because some streamed chunks share a millisecond stamp.
        let sql = format!(
            "SELECT m.id, m.rowid, m.time_updated, s.directory, {parent}, m.session_id
            FROM {} m JOIN {} s ON s.id=m.session_id WHERE m.id IN ({placeholders})",
            layout.messages, layout.sessions
        );
        let mut statement = transaction.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(batch.iter()))?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            present.insert(id.clone());
            let old = &cursor.rows[&id];
            let updated = row.get::<_, Option<i64>>(2)?.unwrap_or(old.created);
            let project = row.get::<_, Option<String>>(3)?.unwrap_or_default();
            let parent: Option<String> = row.get(4)?;
            let session: String = row.get(5)?;
            let mut differs = old.rowid != row.get::<_, i64>(1)?
                || old.updated != updated
                || old.directory != project
                || old.parent_session != nonempty(parent)
                || old.session != session;
            if !differs && old.writing() {
                let length: i64 = transaction.query_row(
                    &format!("SELECT length(data) FROM {} WHERE id=?1", layout.messages),
                    [&id],
                    |row| row.get(0),
                )?;
                differs = old.length != length;
            }
            if differs {
                changed.insert(id);
            }
        }
    }
    cursor
        .rows
        .retain(|id, row| present.contains(id) && row.created >= since_ms);

    for batch in changed
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .chunks(RECHECK_BATCH)
    {
        for id in batch {
            cursor.rows.remove(id);
        }
        let placeholders = vec!["?"; batch.len()].join(",");
        let sql = format!("{projection} WHERE m.id IN ({placeholders}) ORDER BY m.rowid");
        read_rows(
            &transaction,
            &sql,
            params_from_iter(batch.iter()),
            &mut cursor,
            since_ms,
        )?;
    }

    let start = if cursor.last_rowid == 0 && layout.version == 1 {
        first_recent_row(&transaction, layout.messages, since_ms)?
    } else {
        cursor.last_rowid
    };
    let sql = format!("{projection} WHERE m.rowid > ?1 AND m.time_created >= ?2 ORDER BY m.rowid");
    read_rows(
        &transaction,
        &sql,
        params![start, since_ms],
        &mut cursor,
        since_ms,
    )?;
    // Remember the physical end even if migration appended only old history.
    cursor.last_rowid = transaction.query_row(
        &format!("SELECT coalesce(max(rowid),0) FROM {}", layout.messages),
        [],
        |row| row.get(0),
    )?;
    let mut statement = transaction.prepare(&format!(
        "SELECT rowid,id FROM {} ORDER BY rowid DESC LIMIT {TAIL_LENGTH}",
        layout.messages
    ))?;
    cursor.tail = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    cursor.tail.reverse();
    drop(statement);
    cursor.events = events(&cursor.rows, profile, layout.version);
    transaction.commit()?;
    Ok(cursor)
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}
#[cfg(not(unix))]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    (
        metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|t| t.as_secs())
            .unwrap_or(0),
        0,
    )
}

fn table_exists(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
}
fn layout(connection: &Connection) -> Result<Layout> {
    let version2 = table_exists(connection, "session_v2")?;
    let messages = if version2 {
        "session_message"
    } else {
        "message"
    };
    let sessions = if version2 { "session_v2" } else { "session" };
    if !table_exists(connection, messages)? || !table_exists(connection, sessions)? {
        bail!("unsupported OpenCode database schema");
    }
    let parent_column = connection.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{sessions}') WHERE name='parent_id')"
        ),
        [],
        |row| row.get(0),
    )?;
    Ok(Layout {
        version: if version2 { 2 } else { 1 },
        messages,
        sessions,
        parent_column,
        parts: table_exists(connection, "part")?,
    })
}

fn field(path: &str) -> String {
    format!("(CASE WHEN json_valid(m.data) THEN json_extract(m.data,'{path}') END)")
}

fn projection(layout: &Layout) -> String {
    let role = if layout.version == 2 {
        "m.type".to_owned()
    } else {
        field("$.role")
    };
    let parent = if layout.parent_column {
        "s.parent_id"
    } else {
        "NULL"
    };
    let tool_calls = if layout.version == 2 {
        "EXISTS(SELECT 1 FROM json_each(CASE WHEN json_valid(m.data) THEN json_extract(m.data,'$.content') ELSE '[]' END) c
            WHERE json_extract(c.value,'$.type')='tool' AND coalesce(json_extract(c.value,'$.executed'),0)=0)".to_owned()
    } else if layout.parts {
        "EXISTS(SELECT 1 FROM part p WHERE p.message_id=m.id AND json_valid(p.data)
            AND json_extract(p.data,'$.type')='tool' AND coalesce(json_extract(p.data,'$.metadata.providerExecuted'),0)=0
            AND NOT (coalesce(json_extract(p.data,'$.state.status'),'')='error'
                AND coalesce(json_extract(p.data,'$.state.metadata.interrupted'),0)=1))".to_owned()
    } else {
        "0".to_owned()
    };
    let compaction = if layout.version == 1 && layout.parts {
        format!(
            "({}=1 AND EXISTS(SELECT 1 FROM part p WHERE p.message_id={} AND json_valid(p.data)
            AND json_extract(p.data,'$.type')='compaction' AND json_extract(p.data,'$.auto')=1))",
            field("$.summary"),
            field("$.parentID")
        )
    } else {
        "0".to_owned()
    };
    let model = format!("coalesce({}, {}, {}, {}, {}, CASE WHEN json_valid(m.data) THEN CASE WHEN json_type(m.data,'$.model')='text' THEN {} END END,'')",
        field("$.model_id"),field("$.modelID"),field("$.model.modelID"),field("$.model.id"),field("$.model_id"),field("$.model"));
    let fields = [
        "$.parentID",
        "$.time.created",
        "$.time.completed",
        "$.finish",
        "$.tokens.input",
        "$.tokens.output",
        "$.tokens.reasoning",
        "$.tokens.cache.read",
        "$.tokens.cache.write",
        "$.cost",
        "$.outcome",
        "$.status",
    ]
    .map(field);
    let error = "(CASE WHEN json_valid(m.data) THEN json_type(m.data,'$.error') IS NOT NULL AND json_type(m.data,'$.error') != 'null' ELSE 0 END)";
    format!("SELECT m.rowid,m.id,m.session_id,s.directory,{parent},{role},{model},m.time_created,m.time_updated,length(m.data),
        coalesce({},s.directory,''),{},{error},{tool_calls},{compaction}
        FROM {} m JOIN {} s ON s.id=m.session_id",
        field("$.path.cwd"),fields.join(","),layout.messages,layout.sessions)
}

fn read_rows<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
    cursor: &mut OpenCodeCursor,
    since_ms: i64,
) -> Result<()> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(parameters)?;
    while let Some(row) = rows.next()? {
        cursor.decoded_rows += 1;
        let role: Option<String> = row.get(5)?;
        let Some(role) = role else { continue };
        let mut message = Message {
            rowid: row.get(0)?,
            id: row.get(1)?,
            session: row.get(2)?,
            directory: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            project: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
            parent_session: nonempty(row.get(4)?),
            role,
            model: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
            created: row.get::<_, Option<i64>>(12)?.or(row.get(7)?).unwrap_or(0),
            updated: row.get::<_, Option<i64>>(8)?.or(row.get(7)?).unwrap_or(0),
            length: row.get::<_, Option<i64>>(9)?.unwrap_or(0),
            parent_message: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
            completed: row.get(13)?,
            finish: row.get(14)?,
            error: row.get::<_, Option<bool>>(23)?.unwrap_or(false),
            tool_calls: row.get::<_, Option<bool>>(24)?.unwrap_or(false),
            auto_compaction: row.get::<_, Option<bool>>(25)?.unwrap_or(false),
            cost: row
                .get::<_, Option<f64>>(20)?
                .filter(|cost| cost.is_finite() && *cost >= 0.0),
            outcome: row.get(21)?,
            status: row.get(22)?,
            tokens: None,
        };
        let values = (
            row.get::<_, Option<i64>>(15)?,
            row.get::<_, Option<i64>>(16)?,
            row.get::<_, Option<i64>>(17)?,
            row.get::<_, Option<i64>>(18)?,
            row.get::<_, Option<i64>>(19)?,
        );
        if values.0.is_some()
            || values.1.is_some()
            || values.2.is_some()
            || values.3.is_some()
            || values.4.is_some()
        {
            let positive = |value: Option<i64>| value.unwrap_or(0).max(0) as u64;
            let tokens = AgentTokens {
                input: positive(values.0),
                output: positive(values.1).saturating_add(positive(values.2)),
                reasoning: values.2.map(|n| n.max(0) as u64),
                cache_read: positive(values.3),
                cache_write: positive(values.4),
            };
            if tokens.total() > 0 {
                message.tokens = Some(tokens);
            }
        }
        if message.created >= since_ms {
            cursor.rows.insert(message.id.clone(), message);
        }
    }
    Ok(())
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

/// Find a v1 append floor without parsing older JSON. V1 saves rows in time order.
fn first_recent_row(connection: &Connection, table: &str, since_ms: i64) -> Result<i64> {
    if since_ms <= 0 {
        return Ok(0);
    }
    let mut low = 0_i64;
    let mut high: i64 = connection.query_row(
        &format!("SELECT coalesce(max(rowid),0) FROM {table}"),
        [],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(&format!(
        "SELECT rowid,time_created FROM {table} WHERE rowid>=?1 ORDER BY rowid LIMIT 1"
    ))?;
    while low < high {
        let middle = low + (high - low + 1) / 2;
        let found: Option<(i64, Option<i64>)> = statement
            .query_row([middle], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        match found {
            Some((rowid, created)) if created.unwrap_or(0) < since_ms => low = rowid,
            _ => high = middle - 1,
        }
    }
    Ok(low)
}

#[derive(Default)]
struct Turn {
    open: bool,
    started: i64,
    last: i64,
    settled: Option<i64>,
    writing: String,
    active_prompt: String,
    prompt_at: i64,
    model: String,
    project: String,
    completed_prompts: BTreeSet<String>,
}

fn event(
    message: &Message,
    profile: &str,
    suffix: &str,
    at_ms: i64,
    kind: AgentEventKind,
    turn: &Turn,
) -> AgentEvent {
    AgentEvent {
        id: format!("opencode:{}:{}:{suffix}", message.session, message.id),
        at_ms,
        sequence: message.rowid.max(0) as u64,
        session: AgentSessionId {
            provider: AgentProvider::Opencode,
            profile: profile.to_owned(),
            native_id: message.session.clone(),
        },
        parent: message.parent_session.as_ref().map(|id| AgentSessionId {
            provider: AgentProvider::Opencode,
            profile: profile.to_owned(),
            native_id: id.clone(),
        }),
        project: turn.project.clone(),
        model: turn.model.clone(),
        kind,
    }
}

fn events(messages: &BTreeMap<String, Message>, profile: &str, version: u8) -> Vec<AgentEvent> {
    let mut ordered: Vec<&Message> = messages.values().collect();
    ordered.sort_by_key(|message| (message.created, message.rowid));
    let mut turns: BTreeMap<String, Turn> = BTreeMap::new();
    let mut result = Vec::new();
    for message in ordered {
        let turn = turns.entry(message.session.clone()).or_default();
        if !message.model.is_empty() {
            turn.model = message.model.clone();
        }
        if !message.project.is_empty() {
            turn.project = message.project.clone();
        }
        let at = message.created;
        let prior_activity = turn.last;
        turn.last = turn.last.max(at);
        result.push(event(
            message,
            profile,
            "context",
            at,
            AgentEventKind::Context,
            turn,
        ));
        match message.role.as_str() {
            "user" => {
                let quiet = turn
                    .settled
                    .map(|settled| at.saturating_sub(settled) >= SETTLE_MS)
                    .unwrap_or(
                        turn.writing.is_empty()
                            && prior_activity > 0
                            && at.saturating_sub(prior_activity) >= IDLE_MS,
                    );
                turn.active_prompt = message.id.clone();
                turn.prompt_at = at;
                turn.settled = None;
                if !turn.open || quiet {
                    if turn.open {
                        result.push(event(
                            message,
                            profile,
                            "previous-ended",
                            prior_activity,
                            AgentEventKind::Ended {
                                completed: false,
                                duration_ms: None,
                            },
                            turn,
                        ));
                    }
                    turn.open = true;
                    turn.started = at;
                    turn.writing.clear();
                    result.push(event(
                        message,
                        profile,
                        "started",
                        at,
                        AgentEventKind::Started {
                            task_id: message.id.clone(),
                        },
                        turn,
                    ));
                } else {
                    result.push(event(
                        message,
                        profile,
                        "active",
                        at,
                        AgentEventKind::Active,
                        turn,
                    ));
                }
            }
            "assistant" => {
                // V2 may close an abandoned step retroactively as the next
                // one starts; that bookkeeping row has no lifecycle of its own.
                if version == 2
                    && message.completed.is_some()
                    && message.finish.is_none()
                    && !message.error
                    && message.tokens.is_none()
                {
                    continue;
                }
                let already = !message.parent_message.is_empty()
                    && turn.completed_prompts.contains(&message.parent_message);
                let matches = message.parent_message.is_empty()
                    || turn.active_prompt.is_empty()
                    || message.parent_message == turn.active_prompt;
                if turn.open
                    && !turn.writing.is_empty()
                    && turn.writing != message.id
                    && matches
                    && !message.parent_message.is_empty()
                    && turn.prompt_at > turn.started
                {
                    result.push(event(
                        message,
                        profile,
                        "interrupted-previous",
                        turn.prompt_at,
                        AgentEventKind::Ended {
                            completed: false,
                            duration_ms: None,
                        },
                        turn,
                    ));
                    turn.started = turn.prompt_at;
                    result.push(event(
                        message,
                        profile,
                        "new-prompt",
                        turn.prompt_at,
                        AgentEventKind::Started {
                            task_id: turn.active_prompt.clone(),
                        },
                        turn,
                    ));
                }
                if !already && matches && !turn.open {
                    turn.open = true;
                    turn.started = at;
                    result.push(event(
                        message,
                        profile,
                        "started",
                        at,
                        AgentEventKind::Started {
                            task_id: format!("assistant:{}", message.id),
                        },
                        turn,
                    ));
                }
                if message.writing() {
                    turn.writing = message.id.clone();
                } else if turn.writing == message.id {
                    turn.writing.clear();
                }
                if let Some(settled) = turn.settled {
                    if at >= settled {
                        turn.settled = None;
                    }
                }
                usage(message, profile, turn, &mut result);
                let ends = message.error
                    || message
                        .finish
                        .as_ref()
                        .map(|finish| {
                            finish != "tool-calls"
                                && finish != "unknown"
                                && !message.tool_calls
                                && !message.auto_compaction
                        })
                        .unwrap_or(message.completed.is_some());
                if ends {
                    if turn.open && !already && matches {
                        let end = message.completed.unwrap_or(message.updated.max(at));
                        result.push(event(
                            message,
                            profile,
                            "ended",
                            end,
                            AgentEventKind::Ended {
                                completed: !message.error,
                                duration_ms: Some(end.saturating_sub(turn.started).max(0) as u64),
                            },
                            turn,
                        ));
                        turn.open = false;
                        turn.settled = None;
                        turn.active_prompt.clear();
                        turn.writing.clear();
                    }
                    if !message.parent_message.is_empty() {
                        turn.completed_prompts
                            .insert(message.parent_message.clone());
                    }
                } else if !already && matches {
                    let active = message.completed.unwrap_or(message.updated.max(at));
                    turn.last = turn.last.max(active);
                    if message.completed.is_some() && turn.open && version == 1 {
                        turn.settled = Some(active);
                        result.push(event(
                            message,
                            profile,
                            "settled",
                            active,
                            AgentEventKind::Settled,
                            turn,
                        ));
                    } else {
                        result.push(event(
                            message,
                            profile,
                            "active",
                            active,
                            AgentEventKind::Active,
                            turn,
                        ));
                    }
                }
            }
            "compaction" => {
                usage(message, profile, turn, &mut result);
                if turn.open {
                    let active = message.updated.max(at);
                    turn.last = turn.last.max(active);
                    turn.settled = None;
                    result.push(event(
                        message,
                        profile,
                        "active",
                        active,
                        AgentEventKind::Active,
                        turn,
                    ));
                }
            }
            "idle" => {
                if turn.open {
                    result.push(event(
                        message,
                        profile,
                        "ended",
                        at,
                        AgentEventKind::Ended {
                            completed: message.outcome.as_deref() == Some("succeeded"),
                            duration_ms: Some(at.saturating_sub(turn.started).max(0) as u64),
                        },
                        turn,
                    ));
                }
                turn.open = false;
                turn.writing.clear();
                turn.settled = None;
                turn.active_prompt.clear();
            }
            "tool" => {
                if turn.open {
                    result.push(event(
                        message,
                        profile,
                        "active",
                        message.updated.max(at),
                        AgentEventKind::Active,
                        turn,
                    ));
                }
            }
            _ => {}
        }
    }
    result
}

fn usage(message: &Message, profile: &str, turn: &Turn, result: &mut Vec<AgentEvent>) {
    if message.tokens.is_some() || message.cost.unwrap_or(0.0) > 0.0 {
        result.push(event(
            message,
            profile,
            "usage",
            message.created,
            AgentEventKind::Usage {
                response_id: message.id.clone(),
                tokens: message.tokens.clone(),
                requests: 1,
                reported_cost_usd: message.cost.filter(|cost| *cost > 0.0),
                fast: false,
                cumulative: false,
                cache_write_1h: None,
            },
            turn,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn database(version: u8) -> (tempfile::TempDir, std::path::PathBuf, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("opencode.db");
        let connection = Connection::open(&path).unwrap();
        let schema = if version == 2 {
            "CREATE TABLE session_v2(id TEXT PRIMARY KEY,directory TEXT,parent_id TEXT);
             CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
             CREATE INDEX session_message_time_created_idx ON session_message(time_created);"
        } else {
            "CREATE TABLE session(id TEXT PRIMARY KEY,directory TEXT,parent_id TEXT);
             CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
             CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,data TEXT);"
        };
        connection.execute_batch(schema).unwrap();
        connection
            .execute(
                if version == 2 {
                    "INSERT INTO session_v2 VALUES('s','/project',NULL)"
                } else {
                    "INSERT INTO session VALUES('s','/project',NULL)"
                },
                [],
            )
            .unwrap();
        (directory, path, connection)
    }
    fn insert(
        connection: &Connection,
        version: u8,
        id: &str,
        session: &str,
        created: i64,
        updated: i64,
        data: Value,
    ) {
        if version == 2 {
            connection.execute("INSERT INTO session_message(id,session_id,type,time_created,time_updated,data) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id,session,data["role"].as_str().unwrap(),created,updated,data.to_string()]).unwrap();
        } else {
            connection.execute("INSERT INTO message(id,session_id,time_created,time_updated,data) VALUES(?1,?2,?3,?4,?5)",params![id,session,created,updated,data.to_string()]).unwrap();
        }
    }
    fn lifecycle(cursor: &OpenCodeCursor) -> Vec<&AgentEventKind> {
        cursor
            .events
            .iter()
            .filter_map(|event| match event.kind {
                AgentEventKind::Context | AgentEventKind::Usage { .. } => None,
                _ => Some(&event.kind),
            })
            .collect()
    }

    #[test]
    fn v1_tool_iterations_are_one_task_and_reasoning_is_counted_once() {
        let (_directory, path, connection) = database(1);
        // V1 also owns a table named session_message in recent releases.
        connection
            .execute_batch("CREATE TABLE session_message(id TEXT PRIMARY KEY);")
            .unwrap();
        insert(
            &connection,
            1,
            "u",
            "s",
            1000,
            1000,
            json!({"role":"user","model":{"modelID":"gpt-test"}}),
        );
        insert(
            &connection,
            1,
            "a1",
            "s",
            2000,
            3000,
            json!({"role":"assistant","parentID":"u","modelID":"gpt-test","time":{"created":2000,"completed":3000},"finish":"tool-calls",
            "tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":3,"write":1}}}),
        );
        insert(
            &connection,
            1,
            "a2",
            "s",
            4000,
            5000,
            json!({"role":"assistant","parentID":"u","time":{"created":4000,"completed":5000},"finish":"stop"}),
        );
        let cursor = read(&path, "/profile", 0, None).unwrap();
        assert_eq!(cursor.version, 1);
        assert_eq!(
            cursor
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::Started { .. }))
                .count(),
            1
        );
        assert_eq!(
            cursor
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::Settled))
                .count(),
            1
        );
        assert!(cursor.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::Ended {
                completed: true,
                duration_ms: Some(4000)
            }
        )));
        let tokens = cursor
            .events
            .iter()
            .find_map(|event| match &event.kind {
                AgentEventKind::Usage { tokens, .. } => tokens.as_ref(),
                _ => None,
            })
            .unwrap();
        assert_eq!(tokens.total(), 20);
        assert_eq!(tokens.output, 6);
        assert_eq!(tokens.reasoning, Some(2));
        let unchanged = read(&path, "/profile", 0, Some(&cursor)).unwrap();
        assert_eq!(unchanged.decoded_rows, 0);
        assert_eq!(cursor.events, unchanged.events);
    }

    #[test]
    fn v2_uses_idle_rows_and_does_not_settle_between_steps() {
        let (_directory, path, connection) = database(2);
        insert(
            &connection,
            2,
            "u",
            "s",
            1000,
            1000,
            json!({"role":"user","model":{"id":"model"}}),
        );
        insert(
            &connection,
            2,
            "a",
            "s",
            2000,
            3000,
            json!({"role":"assistant","parentID":"u","finish":"tool-calls","time":{"created":2000,"completed":3000},"tokens":{"input":5,"output":3}}),
        );
        insert(
            &connection,
            2,
            "c",
            "s",
            4000,
            4500,
            json!({"role":"compaction","status":"completed","tokens":{"input":7,"output":2},"model":{"id":"compact"}}),
        );
        insert(
            &connection,
            2,
            "i",
            "s",
            5000,
            5000,
            json!({"role":"idle","outcome":"succeeded"}),
        );
        let cursor = read(&path, "/profile", 0, None).unwrap();
        assert!(!cursor
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::Settled)));
        assert!(cursor.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::Ended {
                completed: true,
                duration_ms: Some(4000)
            }
        )));
        assert_eq!(
            cursor
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::Usage { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn streaming_update_replaces_contribution_and_reads_committed_wal() {
        let (_directory, path, connection) = database(2);
        connection
            .execute_batch("PRAGMA journal_mode=WAL;")
            .unwrap();
        insert(&connection, 2, "u", "s", 1000, 1000, json!({"role":"user"}));
        insert(
            &connection,
            2,
            "a",
            "s",
            2000,
            2000,
            json!({"role":"assistant","parentID":"u","tokens":{"input":5,"output":1}}),
        );
        let before = read(&path, "/profile", 0, None).unwrap();
        // Same millisecond update; a different payload length still matters.
        connection
            .execute(
                "UPDATE session_message SET data=?1 WHERE id='a'",
                [
                    json!({"role":"assistant","parentID":"u","tokens":{"input":5,"output":100}})
                        .to_string(),
                ],
            )
            .unwrap();
        let after = read(&path, "/profile", 0, Some(&before)).unwrap();
        assert_eq!(after.decoded_rows, 1);
        let usages: Vec<_> = after
            .events
            .iter()
            .filter_map(|event| {
                if let AgentEventKind::Usage { tokens, .. } = &event.kind {
                    Some(tokens.as_ref().unwrap())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].output, 100);
        connection.execute("UPDATE session_message SET time_updated=3000,data=?1 WHERE id='a'",[json!({"role":"assistant","parentID":"u","time":{"created":2000,"completed":3000},"finish":"stop","tokens":{"input":5,"output":100}}).to_string()]).unwrap();
        let final_read = read(&path, "/profile", 0, Some(&after)).unwrap();
        assert!(final_read.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::Ended {
                completed: true,
                ..
            }
        )));
        assert_eq!(
            final_read
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::Usage { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn revert_rowid_reuse_and_deleted_sessions_remove_old_events() {
        let (_directory, path, connection) = database(1);
        insert(&connection, 1, "u", "s", 1000, 1000, json!({"role":"user"}));
        insert(
            &connection,
            1,
            "old",
            "s",
            2000,
            3000,
            json!({"role":"assistant","parentID":"u","finish":"stop","time":{"completed":3000},"tokens":{"input":10,"output":2}}),
        );
        let before = read(&path, "/profile", 0, None).unwrap();
        connection
            .execute("DELETE FROM message WHERE id='old'", [])
            .unwrap();
        insert(
            &connection,
            1,
            "new",
            "s",
            2500,
            2500,
            json!({"role":"assistant","parentID":"u","tokens":{"input":2,"output":1}}),
        );
        let after = read(&path, "/profile", 0, Some(&before)).unwrap();
        assert!(!after.events.iter().any(|event| event.id.contains(":old:")));
        assert!(after.events.iter().any(|event| event.id.contains(":new:")));
        assert!(!after
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::Ended { .. })));
        connection.execute("DELETE FROM session", []).unwrap();
        let deleted = read(&path, "/profile", 0, Some(&after)).unwrap();
        assert!(deleted.events.is_empty());
    }

    #[test]
    fn schema_replacement_resets_cursor_and_v2_history_need_not_be_rowid_ordered() {
        let (_directory, path, connection) = database(1);
        insert(&connection, 1, "u", "s", 1000, 1000, json!({"role":"user"}));
        let before = read(&path, "/profile", 0, None).unwrap();
        connection.execute_batch("CREATE TABLE session_v2(id TEXT PRIMARY KEY,directory TEXT,parent_id TEXT);
            CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);
            INSERT INTO session_v2 VALUES('s','/project',NULL);").unwrap();
        insert(
            &connection,
            2,
            "recent",
            "s",
            5000,
            5000,
            json!({"role":"user"}),
        );
        insert(
            &connection,
            2,
            "old",
            "s",
            1000,
            1000,
            json!({"role":"user"}),
        );
        let after = read(&path, "/profile", 4000, Some(&before)).unwrap();
        assert_eq!(after.version, 2);
        assert_eq!(after.rows.len(), 1);
        assert!(after
            .events
            .iter()
            .any(|event| event.id.contains(":recent:")));
        assert!(!after.events.iter().any(|event| event.id.contains(":old:")));
    }

    #[test]
    fn subagent_parent_and_privacy_are_preserved() {
        let (_directory, path, connection) = database(1);
        connection
            .execute("INSERT INTO session VALUES('child','/project','s')", [])
            .unwrap();
        insert(
            &connection,
            1,
            "child-u",
            "child",
            1000,
            1000,
            json!({"role":"user","content":"private prompt must disappear"}),
        );
        insert(
            &connection,
            1,
            "child-a",
            "child",
            2000,
            3000,
            json!({"role":"assistant","parentID":"child-u","finish":"stop","time":{"completed":3000},"cost":0.15,"tokens":{"input":10,"output":5},"content":"private answer must disappear"}),
        );
        let cursor = read(&path, "/profile", 0, None).unwrap();
        assert!(cursor
            .events
            .iter()
            .all(|event| event.parent.as_ref().unwrap().native_id == "s"));
        let serialized = serde_json::to_string(&cursor).unwrap();
        assert!(!serialized.contains("private prompt"));
        assert!(!serialized.contains("private answer"));
        let restored: OpenCodeCursor = serde_json::from_str(&serialized).unwrap();
        assert_eq!(cursor.events, restored.events);
    }

    #[test]
    fn corrupt_row_and_non_regular_source_do_not_fabricate_tasks() {
        let (directory, path, connection) = database(1);
        connection
            .execute(
                "INSERT INTO message VALUES('bad','s',1000,1000,'not json')",
                [],
            )
            .unwrap();
        let cursor = read(&path, "/profile", 0, None).unwrap();
        assert!(lifecycle(&cursor).is_empty());
        assert!(read(directory.path(), "/profile", 0, None).is_err());
    }

    #[test]
    fn busy_source_returns_error_without_mutating_previous_cursor() {
        let (_directory, path, connection) = database(1);
        insert(&connection, 1, "u", "s", 1000, 1000, json!({"role":"user"}));
        let before = read(&path, "/profile", 0, None).unwrap();
        let original = serde_json::to_vec(&before).unwrap();
        connection.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        assert!(read(&path, "/profile", 0, Some(&before)).is_err());
        connection.execute_batch("ROLLBACK;").unwrap();
        assert_eq!(serde_json::to_vec(&before).unwrap(), original);
    }
}
