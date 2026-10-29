//! Native global Codex memory and persistent sub-agent Markdown memory.
//!
//! Files are read only on demand from `CODEX_HOME` (default `~/.codex`).
//! The API exposes root-relative keys under `memories/` and `agent-memory/`;
//! no project cwd is used because Codex memory is global. Existing Markdown
//! may be edited, but directories/files are never created. Native generated
//! memory can subsequently be rewritten by Codex itself.

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const SOURCES: [&str; 2] = ["memories", "agent-memory"];
const MAX_FILES: usize = 1024;
const MAX_DEPTH: usize = 8;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;

/// One editable file; `name` is relative to CODEX_HOME, including its source.
#[derive(Debug, Serialize)]
pub struct MemoryFile {
    pub name: String,
    pub content: String,
}

/// Response consumed by the shared brain modal. A missing native MEMORY.md
/// has `index_name=null`; sub-agent indexes remain ordinary named files.
#[derive(Debug, Serialize)]
pub struct CodexMemory {
    pub dir: String,
    pub exists: bool,
    pub scope: &'static str,
    pub index_name: Option<String>,
    pub index: String,
    pub files: Vec<MemoryFile>,
    pub truncated: bool,
}

impl CodexMemory {
    fn empty(dir: &Path) -> Self {
        Self {
            dir: dir.display().to_string(),
            exists: false,
            scope: "global",
            index_name: None,
            index: String::new(),
            files: Vec::new(),
            truncated: false,
        }
    }
}

/// Resolve the explicit CLI home before the default; empty env values are ignored.
fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".codex"))
        })
}

/// Accept only unambiguous source-relative Markdown keys, never absolute paths,
/// traversal, backslashes, empty segments, or non-Markdown files.
fn valid_name(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    parts.len() >= 2
        && parts.len() <= MAX_DEPTH + 2
        && SOURCES.contains(&parts[0])
        && !name.contains('\\')
        && !name.contains('\0')
        && parts
            .iter()
            .all(|p| !p.is_empty() && *p != "." && *p != "..")
        && name.to_ascii_lowercase().ends_with(".md")
}

/// Open every directory and the final file relative to already-open descriptors.
/// O_NOFOLLOW prevents both file and intermediate-directory symlink escapes,
/// including replacements between enumeration/validation and the final open.
#[cfg(unix)]
fn open_memory_file(
    root: &std::fs::File,
    name: &str,
    write: bool,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let parts: Vec<_> = name.split('/').collect();
    let mut parent = root.try_clone()?;
    for (i, part) in parts.iter().enumerate() {
        let c = std::ffi::CString::new(*part)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let last = i + 1 == parts.len();
        let flags = libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if last {
                if write {
                    libc::O_WRONLY
                } else {
                    libc::O_RDONLY
                }
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY
            };
        // SAFETY: c is NUL-terminated, the borrowed parent descriptor is live,
        // no O_CREAT is used, and the returned descriptor is owned exactly once.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), c.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        parent = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    if !parent.metadata()?.is_file() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    Ok(parent)
}

#[cfg(not(unix))]
fn open_memory_file(
    _root: &std::fs::File,
    _name: &str,
    _write: bool,
) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "memory editing requires descriptor-relative filesystem access",
    ))
}

/// Bounded metadata enumeration; only regular Markdown is eligible. Actual
/// content access separately reopens each path without following any symlinks.
fn collect_names(root: &Path, result: &mut CodexMemory) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut pending: Vec<_> = SOURCES.iter().map(|s| (root.join(s), 0usize)).collect();
    while let Some((dir, depth)) = pending.pop() {
        let meta = match std::fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !meta.is_dir() {
            continue;
        }
        result.exists = true;
        let mut entries = std::fs::read_dir(&dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((entry.path(), depth + 1));
                } else {
                    result.truncated = true;
                }
            } else if kind.is_file() {
                let path = entry.path();
                let Some(name) = path.strip_prefix(root).ok().and_then(|p| p.to_str()) else {
                    continue;
                };
                if valid_name(name) {
                    if names.len() == MAX_FILES {
                        result.truncated = true;
                        names.sort();
                        return Ok(names);
                    }
                    names.push(name.to_string());
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Read a consistent source scope using a pinned root descriptor. Limits keep
/// malformed/huge user stores from blocking the server or exhausting memory.
fn load_from(root: &Path) -> std::io::Result<CodexMemory> {
    let mut result = CodexMemory::empty(root);
    let descriptor = match std::fs::File::open(root) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(e) => return Err(e),
    };
    let names = collect_names(root, &mut result)?;
    let mut bytes = 0usize;
    for name in names {
        let file = match open_memory_file(&descriptor, &name, false) {
            Ok(f) => f,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                ) || e.raw_os_error() == Some(libc::ELOOP)
                    || e.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() > MAX_FILE_BYTES {
            result.truncated = true;
            continue;
        }
        let mut content = String::new();
        file.take(MAX_FILE_BYTES + 1).read_to_string(&mut content)?;
        if content.len() > MAX_FILE_BYTES as usize || bytes + content.len() > MAX_TOTAL_BYTES {
            result.truncated = true;
            continue;
        }
        bytes += content.len();
        if name == "memories/MEMORY.md" {
            result.index_name = Some(name);
            result.index = content;
        } else {
            result.files.push(MemoryFile { name, content });
        }
    }
    Ok(result)
}

/// Saving edits never creates memory or changes another file via a symlink.
/// Missing files return 404; invalid/unsafe paths return 400; I/O failures 500.
fn save_into(root: &Path, name: &str, content: &str) -> Result<(), (StatusCode, String)> {
    if !valid_name(name) {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid Codex memory file name".into(),
        ));
    }
    if content.len() > MAX_FILE_BYTES as usize {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "memory file exceeds 1 MiB".into(),
        ));
    }
    let action = || -> std::io::Result<()> {
        let descriptor = std::fs::File::open(root)?;
        let mut file = open_memory_file(&descriptor, name, true)?;
        file.set_len(0)?;
        file.write_all(content.as_bytes())
    };
    action().map_err(|e| {
        let status = if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else if e.kind() == std::io::ErrorKind::InvalidInput
            || e.raw_os_error() == Some(libc::ELOOP)
            || e.raw_os_error() == Some(libc::ENOTDIR)
        {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, format!("cannot save Codex memory: {e}"))
    })
}

/// GET /api/codex-memory is local-only and independent of the active project.
/// Remote requests return an empty result, matching the existing Claude API.
pub async fn get_memory(
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    if crate::extract_server_id(&q).is_some() {
        return Ok(Json(CodexMemory::empty(Path::new(""))).into_response());
    }
    let Some(root) = codex_home() else {
        return Ok(Json(CodexMemory::empty(Path::new(""))).into_response());
    };
    let data = tokio::task::spawn_blocking(move || load_from(&root))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(data).into_response())
}

/// PUT accepts only the root-relative filename received from GET and its body.
/// Unknown `path` fields are ignored because this memory is global.
#[derive(Debug, Deserialize)]
pub struct PutMemoryReq {
    file: String,
    content: String,
    server: Option<String>,
}

pub async fn put_memory(
    Query(q): Query<HashMap<String, String>>,
    Json(req): Json<PutMemoryReq>,
) -> Result<Response, (StatusCode, String)> {
    if crate::extract_server_id(&q).is_some()
        || req.server.as_deref().is_some_and(|s| !s.trim().is_empty())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Codex memory is local to this FORGE server".into(),
        ));
    }
    let root = codex_home().ok_or((StatusCode::NOT_FOUND, "Codex home is unavailable".into()))?;
    tokio::task::spawn_blocking(move || save_into(&root, &req.file, &req.content))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let t = TempDir::new().unwrap();
        std::fs::create_dir_all(t.path().join("memories/rollout_summaries")).unwrap();
        std::fs::create_dir_all(t.path().join("agent-memory/run-phase")).unwrap();
        std::fs::write(t.path().join("memories/MEMORY.md"), "# Global").unwrap();
        std::fs::write(t.path().join("memories/memory_summary.md"), "Summary").unwrap();
        std::fs::write(t.path().join("memories/rollout_summaries/one.md"), "One").unwrap();
        std::fs::write(t.path().join("agent-memory/run-phase/MEMORY.md"), "Agent").unwrap();
        t
    }

    #[test]
    fn global_and_subagent_memory_round_trip() {
        let t = fixture();
        let data = load_from(t.path()).unwrap();
        assert!(data.exists);
        assert_eq!(data.scope, "global");
        assert_eq!(data.index_name.as_deref(), Some("memories/MEMORY.md"));
        assert_eq!(data.index, "# Global");
        let dto = serde_json::to_value(&data).unwrap();
        assert_eq!(dto["dir"], t.path().display().to_string());
        assert_eq!(dto["index_name"], "memories/MEMORY.md");
        assert_eq!(dto["files"][0]["name"], "agent-memory/run-phase/MEMORY.md");
        assert_eq!(dto["truncated"], false);
        let names: Vec<_> = data.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "agent-memory/run-phase/MEMORY.md",
                "memories/memory_summary.md",
                "memories/rollout_summaries/one.md"
            ]
        );
        save_into(t.path(), "agent-memory/run-phase/MEMORY.md", "Обновлено 🧠").unwrap();
        assert_eq!(
            load_from(t.path()).unwrap().files[0].content,
            "Обновлено 🧠"
        );
        save_into(t.path(), "memories/MEMORY.md", "Changed").unwrap();
        assert_eq!(load_from(t.path()).unwrap().index, "Changed");
    }

    #[test]
    fn absent_store_and_absent_index_are_distinct() {
        let t = TempDir::new().unwrap();
        assert!(!load_from(t.path()).unwrap().exists);
        std::fs::create_dir(t.path().join("memories")).unwrap();
        let data = load_from(t.path()).unwrap();
        assert!(data.exists);
        assert!(data.index_name.is_none());
        assert!(data.files.is_empty());
        assert!(!load_from(&t.path().join("missing-home")).unwrap().exists);
    }

    #[test]
    fn invalid_paths_and_creating_files_are_rejected() {
        let t = fixture();
        for name in [
            "MEMORY.md",
            "../evil.md",
            "/memories/MEMORY.md",
            "memories/../evil.md",
            "memories//a.md",
            "memories/./a.md",
            "other/a.md",
            "agent-memory\\foo.md",
            "memories/file.txt",
        ] {
            assert_eq!(
                save_into(t.path(), name, "bad").unwrap_err().0,
                StatusCode::BAD_REQUEST,
                "{name}"
            );
        }
        assert_eq!(
            save_into(t.path(), "memories/missing.md", "bad")
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert!(!t.path().join("memories/missing.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_directory_and_source_symlinks_cannot_escape() {
        use std::os::unix::fs::symlink;
        let t = fixture();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.md"), "Do not touch").unwrap();
        symlink(
            outside.path().join("secret.md"),
            t.path().join("memories/link.md"),
        )
        .unwrap();
        symlink(outside.path(), t.path().join("memories/linked-dir")).unwrap();
        let data = load_from(t.path()).unwrap();
        assert!(!data.files.iter().any(|f| f.name.contains("link")));
        for name in ["memories/link.md", "memories/linked-dir/secret.md"] {
            assert_eq!(
                save_into(t.path(), name, "bad").unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
        std::fs::remove_dir_all(t.path().join("agent-memory")).unwrap();
        symlink(outside.path(), t.path().join("agent-memory")).unwrap();
        assert_eq!(
            save_into(t.path(), "agent-memory/secret.md", "bad")
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.md")).unwrap(),
            "Do not touch"
        );
    }

    #[test]
    fn bounded_large_files_are_reported_and_not_editable() {
        let t = fixture();
        std::fs::write(
            t.path().join("memories/large.md"),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let data = load_from(t.path()).unwrap();
        assert!(data.truncated);
        assert!(!data.files.iter().any(|f| f.name.ends_with("large.md")));
        assert_eq!(
            save_into(
                t.path(),
                "memories/MEMORY.md",
                &"x".repeat(MAX_FILE_BYTES as usize + 1)
            )
            .unwrap_err()
            .0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(load_from(t.path()).unwrap().index, "# Global");
    }

    #[tokio::test]
    async fn routes_reject_remote_and_invalid_bodies_without_reading_user_memory() {
        use axum::{
            body::{to_bytes, Body},
            http::Request,
            routing::get,
            Router,
        };
        use tower::ServiceExt;
        let app = Router::new().route("/api/codex-memory", get(get_memory).put(put_memory));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/codex-memory?server=remote")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(value["exists"], false);
        assert_eq!(value["scope"], "global");
        assert!(value["index_name"].is_null());
        for (uri, body, expected) in [
            (
                "/api/codex-memory?server=remote",
                r#"{"file":"memories/MEMORY.md","content":"x"}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                "/api/codex-memory",
                r#"{"server":"remote","file":"memories/MEMORY.md","content":"x"}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                "/api/codex-memory",
                r#"{"file":"../bad.md","content":"x"}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                "/api/codex-memory",
                r#"{"content":"x"}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }
}
