//! Ephemeral Codex TUI name → native thread identity, for shared app-server CLIs.
//! Names are queried and compared in memory; never cached, returned or logged.
use super::{
    model::AgentProvider,
    runtime::NativeEvidence,
    sources::{self, AgentRoot},
};
use crate::tmux::AgentPaneInfo;
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn terminal_name<'a>(title: &'a str, path: &str) -> Option<&'a str> {
    let project = Path::new(path).file_name()?.to_str()?;
    let mut name = title.strip_suffix(&format!(" | {project}"))?;
    if let Some(first) = name
        .chars()
        .next()
        .filter(|c| ('\u{2800}'..='\u{28ff}').contains(c))
    {
        name = name.get(first.len_utf8()..)?.strip_prefix(' ')?;
    }
    (!name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control)).then_some(name)
}

fn state_database(profile: &str) -> std::io::Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(profile) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut databases = Vec::new();
    for (count, entry) in entries.enumerate() {
        if count >= 4096 {
            return Err(std::io::Error::other("profile enumeration limit"));
        }
        let entry = entry?;
        let candidate = (|| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            entry
                .file_type()
                .ok()?
                .is_file()
                .then(|| (version, entry.path()))
        })();
        if let Some(candidate) = candidate {
            databases.push(candidate);
        }
    }
    databases.sort_by_key(|(version, _)| *version);
    Ok(databases.pop().map(|(_, path)| path))
}

fn matches(connection: &Connection, name: &str, path: &str) -> rusqlite::Result<Vec<String>> {
    // Deliberately do not SELECT title/first_user_message: those can be prompts.
    // Older schemas without an explicit native name produce no new evidence.
    let mut statement = connection.prepare("SELECT id,cwd FROM threads WHERE name=?1 LIMIT 65")?;
    let rows = statement.query_map([name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut found = Vec::new();
    let cwd = sources::canonical(Path::new(path));
    let mut count = 0;
    for row in rows {
        let (id, path) = row?;
        count += 1;
        if sources::canonical(Path::new(&path)) == cwd {
            found.push(id);
        }
    }
    if count >= 65 {
        return Ok(Vec::new());
    }
    Ok(found)
}

/// Require a unique explicit native name AND canonical cwd across every profile.
/// A project alone never supplies an identity; duplicate names remain ambiguous.
pub(crate) fn evidence(
    panes: &[AgentPaneInfo],
    roots: &[AgentRoot],
) -> BTreeMap<String, NativeEvidence> {
    let profiles: BTreeSet<_> = roots
        .iter()
        .filter(|root| root.provider == AgentProvider::Codex)
        .map(|root| root.profile.as_str())
        .collect();
    let mut connections = Vec::new();
    for profile in profiles {
        let path = match state_database(profile) {
            Ok(Some(path)) => path,
            Ok(None) => continue,
            Err(_) => return BTreeMap::new(),
        };
        let Ok(connection) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            return BTreeMap::new();
        };
        if connection.busy_timeout(Duration::from_millis(50)).is_err() {
            return BTreeMap::new();
        }
        connections.push((profile, connection));
    }
    let mut result = BTreeMap::new();
    for pane in panes {
        let Some(name) = terminal_name(&pane.title, &pane.path) else {
            continue;
        };
        let mut found = BTreeSet::new();
        let mut complete = true;
        for (profile, connection) in &connections {
            match matches(connection, name, &pane.path) {
                Ok(ids) => {
                    for native_id in ids {
                        found.insert(NativeEvidence {
                            native_id,
                            profile: Some((*profile).into()),
                        });
                    }
                }
                Err(_) => complete = false,
            }
        }
        if complete && found.len() == 1 {
            result.insert(pane.pane_id.clone(), found.into_iter().next().unwrap());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_native_name_and_project_suffix_are_recognized() {
        assert_eq!(
            terminal_name("⠋ Native name | project", "/tmp/project"),
            Some("Native name")
        );
        assert_eq!(
            terminal_name("Native name | project", "/tmp/project"),
            Some("Native name")
        );
        assert_eq!(terminal_name("⠋ Native name | other", "/tmp/project"), None);
        assert_eq!(terminal_name("MacBook.local", "/tmp/project"), None);
    }
    #[test]
    fn exact_name_is_scoped_by_cwd_and_duplicates_are_retained() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE threads(id TEXT,cwd TEXT,name TEXT,title TEXT); INSERT INTO threads VALUES('a','/one','Native name','prompt never selected'),('b','/two','Native name','prompt'),('c','/one','Different name','Native name');").unwrap();
        assert_eq!(matches(&db, "Native name", "/one").unwrap(), vec!["a"]);
        assert!(matches(&db, "Native name", "/none").unwrap().is_empty());
        db.execute(
            "INSERT INTO threads VALUES('d','/one','Native name','prompt')",
            [],
        )
        .unwrap();
        assert_eq!(matches(&db, "Native name", "/one").unwrap().len(), 2);
        db.execute_batch("ALTER TABLE threads RENAME COLUMN name TO old_name;")
            .unwrap();
        assert!(matches(&db, "Native name", "/one").is_err());
    }
    fn pane(path: &str) -> AgentPaneInfo {
        AgentPaneInfo {
            pane_id: "%1".into(),
            tmux_session: "one".into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            pid: 1,
            tty: "/dev/pts/1".into(),
            path: path.into(),
            title: "⠋ Native name | project".into(),
            active: true,
            session_group: None,
        }
    }
    #[test]
    fn wal_name_evidence_is_read_only_and_profile_ambiguity_is_rejected() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut roots = Vec::new();
        let mut writers = Vec::new();
        for directory in [&first, &second] {
            let db = Connection::open(directory.path().join("state_5.sqlite")).unwrap();
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE threads(id TEXT,cwd TEXT,name TEXT); INSERT INTO threads VALUES('native-id','/tmp/project','Native name');").unwrap();
            roots.push(AgentRoot::new(
                AgentProvider::Codex,
                directory.path().join("sessions"),
                directory.path().into(),
            ));
            writers.push(db);
        }
        let panes = [pane("/tmp/project")];
        let found = evidence(&panes, &roots[..1]);
        assert_eq!(found["%1"].native_id, "native-id");
        assert_eq!(
            found["%1"].profile.as_deref(),
            Some(roots[0].profile.as_str())
        );
        assert!(evidence(&panes, &roots).is_empty());
        let inaccessible = tempfile::tempdir().unwrap();
        std::fs::write(
            inaccessible.path().join("state_5.sqlite"),
            b"invalid sqlite",
        )
        .unwrap();
        let mut uncertain = vec![roots[0].clone()];
        uncertain.push(AgentRoot::new(
            AgentProvider::Codex,
            inaccessible.path().join("sessions"),
            inaccessible.path().into(),
        ));
        assert!(evidence(&panes, &uncertain).is_empty());
        writers[0]
            .execute(
                "INSERT INTO threads VALUES('another-id','/tmp/project','Native name')",
                [],
            )
            .unwrap();
        assert!(evidence(&panes, &roots[..1]).is_empty());
        assert!(evidence(&[pane("/tmp/other")], &roots[..1]).is_empty());
        assert!(!first.path().join("state_6.sqlite").exists());
    }
}
