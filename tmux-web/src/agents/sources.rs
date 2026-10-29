//! Profile discovery, configuration and bounded regular-file enumeration.
use super::model::{AgentProvider, HISTORY_DAYS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRoot {
    pub provider: AgentProvider,
    /// Actual journal directory (or an OpenCode database file).
    pub path: PathBuf,
    /// Account/config root shared by normal and archived logs.
    pub profile: String,
}
impl AgentRoot {
    pub fn new(provider: AgentProvider, path: PathBuf, profile: PathBuf) -> Self {
        Self {
            provider,
            path: canonical(&path),
            profile: canonical(&profile).to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConfig {
    pub enabled: bool,
    pub discover_defaults: bool,
    pub roots: Vec<AgentRoot>,
    pub history_days: i64,
    pub cache_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub price_path: Option<PathBuf>,
    pub price_update_url: Option<String>,
    pub price_updates: bool,
}
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            discover_defaults: true,
            roots: Vec::new(),
            history_days: HISTORY_DAYS,
            cache_path: None,
            config_path: None,
            price_path: None,
            price_update_url: None,
            price_updates: true,
        }
    }
}
impl AgentConfig {
    pub fn with_default_paths() -> Self {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Self::default();
        };
        let config_dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("forge");
        let config_path = config_dir.join("agents.toml");
        let mut config = Self {
            cache_path: Some(home.join(".forge/agent-stats-cache.json")),
            price_path: Some(home.join(".forge/agent-prices.json")),
            config_path: Some(config_path.clone()),
            ..Self::default()
        };
        // A config augments discovered profiles instead of disabling defaults.
        if let Ok(bytes) = std::fs::read_to_string(&config_path) {
            match toml::from_str::<Self>(&bytes) {
                Ok(saved) => {
                    config.enabled = saved.enabled;
                    config.discover_defaults = saved.discover_defaults;
                    config.roots = saved.roots;
                    config.history_days = saved.history_days.clamp(1, 3660);
                    config.price_update_url = saved.price_update_url;
                    config.price_updates = saved.price_updates;
                }
                Err(e) => tracing::warn!(error=%e,"ignoring invalid agents configuration"),
            }
        }
        config
    }
    pub fn all_roots(&self) -> Vec<AgentRoot> {
        let mut roots = if self.discover_defaults {
            default_roots()
        } else {
            Vec::new()
        };
        roots.extend(
            self.roots
                .iter()
                .cloned()
                .map(|r| AgentRoot::new(r.provider, r.path, PathBuf::from(r.profile))),
        );
        let mut seen = BTreeSet::new();
        roots.retain(|r| seen.insert((r.provider, r.path.clone())));
        roots
    }
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(path) = &self.config_path {
            forge_atomic::write_bytes_atomic(path, toml::to_string_pretty(self)?.as_bytes())?;
        }
        Ok(())
    }
}

pub fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Standard profiles plus explicitly selected CLI homes. Symlink duplicates collapse.
pub fn default_roots() -> Vec<AgentRoot> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    let mut add = |provider, path: PathBuf, profile: PathBuf| {
        roots.push(AgentRoot::new(provider, path, profile))
    };
    let mut claude = vec![home.join(".claude"), home.join(".config/claude")];
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty()) {
        claude.push(PathBuf::from(dir));
    }
    for root in claude {
        add(AgentProvider::Claude, root.join("projects"), root);
    }
    let mut codex = vec![home.join(".codex")];
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
        codex.push(PathBuf::from(dir));
    }
    for root in codex {
        add(AgentProvider::Codex, root.join("sessions"), root.clone());
        add(AgentProvider::Codex, root.join("archived_sessions"), root);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let oc = data.join("opencode");
    add(AgentProvider::Opencode, oc.clone(), oc);
    let cp = home.join(".copilot");
    add(AgentProvider::Copilot, cp.join("session-state"), cp);
    let mut seen = BTreeSet::new();
    roots.retain(|r| seen.insert((r.provider, r.path.clone())));
    roots
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub path: PathBuf,
    pub provider: AgentProvider,
    pub profile: String,
    pub modified_ms: i64,
}
pub fn modified_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub fn discover(roots: &[AgentRoot], since_ms: i64) -> (Vec<SourceDescriptor>, Vec<String>) {
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut errors = Vec::new();
    for root in roots {
        let mut dirs = vec![(root.path.clone(), 0usize)];
        while let Some((path, depth)) = dirs.pop() {
            if depth > 16 {
                continue;
            }
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            if meta.is_dir() {
                match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            dirs.push((entry.path(), depth + 1));
                        }
                    }
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                };
                continue;
            }
            // FIFOs, sockets, devices and directory symlinks are never read.
            if !meta.is_file() {
                continue;
            }
            let accepted = match root.provider {
                AgentProvider::Opencode => path.file_name().is_some_and(|n| n == "opencode.db"),
                AgentProvider::Copilot => {
                    path.file_name().is_some_and(|n| n == "events.jsonl")
                        && path.parent().and_then(Path::parent) == Some(root.path.as_path())
                }
                _ => path.extension().is_some_and(|s| s == "jsonl"),
            };
            if !accepted {
                continue;
            }
            let modified = modified_ms(&meta);
            if root.provider != AgentProvider::Opencode && modified < since_ms {
                continue;
            }
            let path = canonical(&path);
            if seen.insert((root.provider, path.clone())) {
                files.push(SourceDescriptor {
                    path,
                    provider: root.provider,
                    profile: root.profile.clone(),
                    modified_ms: modified,
                });
            }
        }
    }
    files.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    (files, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_is_provider_specific_and_regular_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("session")).unwrap();
        std::fs::write(root.join("session/events.jsonl"), "{}\n").unwrap();
        std::fs::write(root.join("session/other.jsonl"), "{}\n").unwrap();
        let r = AgentRoot::new(
            AgentProvider::Copilot,
            root.to_path_buf(),
            root.to_path_buf(),
        );
        let (f, e) = discover(&[r], 0);
        assert!(e.is_empty());
        assert_eq!(f.len(), 1);
    }
}
