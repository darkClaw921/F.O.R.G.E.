//! Runs the agent dashboard's behavior checks through the normal Cargo gate.
use std::path::{Path, PathBuf};
use std::process::Command;

fn tests(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(root.join("tests/frontend"))
        .expect("frontend test directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.starts_with("agent_stats_") && name.ends_with(".test.mjs"))
        })
        .collect();
    files.sort();
    files
}

#[test]
fn agent_dashboard_behavior_tests_pass() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("Node unavailable: agent frontend checks were not run");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = tests(root);
    assert!(
        files.len() >= 6,
        "agent frontend checks unexpectedly missing"
    );
    let mut failures = Vec::new();
    for file in files {
        let result = Command::new("node")
            .arg(&file)
            .current_dir(root)
            .output()
            .expect("start Node");
        if !result.status.success() {
            failures.push(format!(
                "{}\n{}\n{}",
                file.display(),
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn live_view_and_browser_checks_are_included() {
    let files = tests(Path::new(env!("CARGO_MANIFEST_DIR")));
    for expected in [
        "agent_stats_core.test.mjs",
        "agent_stats_live.test.mjs",
        "agent_stats_view.test.mjs",
        "agent_stats_history.test.mjs",
        "agent_stats_settings.test.mjs",
        "agent_stats_browser.test.mjs",
    ] {
        assert!(
            files
                .iter()
                .any(|file| file.file_name().is_some_and(|name| name == expected)),
            "missing {expected}"
        );
    }
}
