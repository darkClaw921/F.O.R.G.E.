//! Native interactive overlays and one-capture-per-physical-pane attention.
//! Text is transient and never persisted; this module never sends terminal input.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::tmux::{AgentPaneInfo, SessionInfo};

const META: &str = "FORGE_ATTENTION:#{pane_id}|#{pane_pid}|#{pane_in_mode}|#{pane_dead}";

fn content(line: &str) -> &str {
    line.trim().trim_start_matches('┃').trim()
}

fn joined(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| content(line))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A final native footer is required, so a quoted overlay followed by normal
/// input, shell output or a completed answer cannot keep the indicator lit.
fn footer_start(lines: &[&str], predicate: impl Fn(&str) -> bool) -> Option<usize> {
    (lines.len().saturating_sub(4)..lines.len())
        .rev()
        .find(|&start| predicate(&joined(&lines[start..])))
}

fn selected_item(line: &str) -> Option<(bool, &str)> {
    let line = content(line);
    let selected = line.starts_with(['›', '❯']);
    let line = line.trim_start_matches(['›', '❯']).trim_start();
    let (number, text) = line.split_once('.')?;
    if number.is_empty()
        || number.len() > 2
        || !number.bytes().all(|n| n.is_ascii_digit())
        || number.parse::<u8>().ok()? == 0
        || text.trim().is_empty()
    {
        return None;
    }
    Some((selected, text.trim()))
}

fn header_before(lines: &[&str], end: usize, predicate: impl Fn(&str) -> bool) -> Option<usize> {
    (0..end)
        .rev()
        .find(|&start| predicate(&joined(&lines[start..end.min(start + 3)])))
}

/// Codex approval overlays have an explicit request heading, a selected
/// numbered affirmative/refusal menu and the final confirm/cancel footer.
fn codex_approval(lines: &[&str]) -> bool {
    let Some(footer) = footer_start(lines, |text| {
        let t = text.to_lowercase();
        t.starts_with("press ")
            && t.contains(" to confirm")
            && (t.contains(" to cancel") || t.contains(" to go back"))
            && ["to cancel", "to go back", "to open thread"]
                .iter()
                .any(|suffix| t.ends_with(suffix))
    }) else {
        return false;
    };
    let Some(header) = header_before(lines, footer, |text| {
        [
            "Would you like to run the following command?",
            "Would you like to make the following edits?",
            "Would you like to grant these permissions?",
            "Would you like to send input to the existing terminal?",
            "Submit with unanswered questions?",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
            || text.starts_with("Would you like to send input to terminal ")
            || text.starts_with("Do you want to approve network access to ")
    })
    .or_else(|| {
        (0..footer).rev().find(|&i| {
            let line = content(lines[i]);
            line.ends_with(" needs your approval.") && !line.starts_with(['>', '+', '-', '`'])
        })
    }) else {
        return false;
    };
    let mut selected = false;
    let mut yes = false;
    let mut no = false;
    for line in &lines[header + 1..footer] {
        if let Some((cursor, text)) = selected_item(line) {
            selected |= cursor;
            let t = text.to_lowercase();
            yes |= t.starts_with("yes") || t.starts_with("proceed");
            no |= t.starts_with("no") || t.starts_with("cancel") || t.starts_with("go back");
        } else if content(line).starts_with(['›', '❯']) {
            return false;
        }
    }
    selected && yes && no
}

fn question_progress(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("Question ") else {
        return false;
    };
    let Some(fraction) = rest.split_whitespace().next() else {
        return false;
    };
    let Some((current, total)) = fraction.split_once('/') else {
        return false;
    };
    matches!((current.parse::<u16>(), total.parse::<u16>()), (Ok(c), Ok(t)) if c > 0 && c <= t)
}

/// Codex request_user_input choices and freeform/notes modes all retain their
/// progress header and answer-submission footer. Ordinary chat composers do not.
fn codex_question(lines: &[&str]) -> bool {
    let Some(footer) = footer_start(lines, |text| {
        let t = text.to_lowercase();
        (t.contains(" to submit answer") || t.contains(" to submit all"))
            && (t.contains(" to interrupt")
                || t.contains(" to clear notes")
                || t.contains(" to add notes")
                || t.contains(" to navigate questions"))
            && !t.contains("? for shortcuts")
            && [
                "to interrupt",
                "to submit answer",
                "to submit all",
                "to clear notes",
                "to navigate questions",
            ]
            .iter()
            .any(|suffix| t.ends_with(suffix))
    }) else {
        return false;
    };
    let Some(header) = (0..footer)
        .rev()
        .find(|&i| question_progress(content(lines[i])))
    else {
        return false;
    };
    lines[header + 1..footer]
        .iter()
        .any(|line| !content(line).is_empty())
}

/// OpenCode released/dev native permission and question boxes have a left
/// SplitBorder, fixed action hints, and replace the ordinary composer.
fn opencode_prompt(lines: &[&str]) -> bool {
    let Some(footer) = footer_start(lines, |text| {
        text.contains("enter ")
            && (text.contains("esc dismiss")
                || text.contains("⇆ select")
                || text.contains("esc cancel"))
    }) else {
        return false;
    };
    if !lines.iter().any(|line| line.trim_start().starts_with('┃')) {
        return false;
    }
    let tail = joined(&lines[footer..]);
    if !["enter confirm", "esc dismiss", "esc cancel", "fullscreen"]
        .iter()
        .any(|suffix| tail.ends_with(suffix))
    {
        return false;
    }
    let body = joined(&lines[..footer]);
    if tail.contains("enter confirm") && tail.contains("⇆ select") {
        return (lines[..footer]
            .iter()
            .any(|line| content(line) == "△ Permission required")
            && body.contains("Allow once")
            && body.contains("Reject"))
            || (lines[..footer]
                .iter()
                .any(|line| content(line) == "△ Always allow")
                && body.contains("until OpenCode is restarted")
                && body.contains("Confirm")
                && body.contains("Cancel"));
    }
    if tail.contains("enter confirm")
        && tail.contains("esc cancel")
        && lines[..footer]
            .iter()
            .any(|line| content(line) == "△ Reject permission")
        && body.contains("Tell OpenCode what to do differently")
    {
        return true;
    }
    if !tail.contains("esc dismiss") {
        return false;
    }
    let options = lines[..footer].iter().any(|line| {
        selected_item(line).is_some()
            || content(line).starts_with("[ ] ")
            || content(line).starts_with("[✓] ")
    });
    (options
        && tail.contains("↑↓ select")
        && ["enter submit", "enter toggle", "enter confirm"]
            .iter()
            .any(|s| tail.contains(s)))
        || (tail.contains("⇆ tab")
            && tail.contains("enter submit")
            && lines[..footer].iter().any(|line| content(line) == "Review"))
}

pub(super) fn detect(pane: &str) -> bool {
    let lines: Vec<_> = pane
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    codex_approval(&lines)
        || codex_question(&lines)
        || opencode_prompt(&lines)
        || super::detect_claude_prompt(pane)
}

/// Private sockets are used only by isolated terminal tests; production always
/// reads the existing tmux server and never sends keys, approvals or answers.
#[derive(Default)]
pub(super) struct Capture {
    socket: Option<PathBuf>,
}
impl Capture {
    async fn command(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.socket {
            cmd.arg("-S").arg(socket).args(["-f", "/dev/null"]);
        }
        cmd.args(args).kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(3), cmd.output())
            .await
            .context("attention pane capture timed out")??;
        if !out.status.success() {
            bail!("attention pane capture unavailable");
        }
        Ok(String::from_utf8(out.stdout)?)
    }
    pub(super) async fn pane(&self, pane: &AgentPaneInfo) -> Result<bool> {
        let id = &pane.pane_id;
        if !id
            .strip_prefix('%')
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        {
            bail!("invalid attention pane identity");
        }
        let out = self
            .command(&[
                "display-message",
                "-p",
                "-t",
                id,
                META,
                ";",
                "capture-pane",
                "-p",
                "-J",
                "-t",
                id,
                ";",
                "display-message",
                "-p",
                "-t",
                id,
                META,
            ])
            .await?;
        let (head, body) = out.split_once('\n').context("missing attention metadata")?;
        let (body, tail) = body
            .trim_end_matches('\n')
            .rsplit_once('\n')
            .context("missing attention capture")?;
        if head != tail {
            bail!("attention pane changed during capture");
        }
        let expected = format!("FORGE_ATTENTION:{}|{}|", id, pane.pid);
        let state = head
            .strip_prefix(&expected)
            .context("attention pane process changed")?;
        match state {
            "0|0" => Ok(detect(body)),
            "1|0" => bail!("attention copy-mode contents are historical"),
            "0|1" | "1|1" => Ok(false),
            _ => bail!("invalid attention pane metadata"),
        }
    }
}

/// Aggregate independent physical results without content-hash deduplication.
/// A successful answer in one pane cannot clear another pane still waiting.
pub(super) fn aggregate(
    sessions: &[SessionInfo],
    panes: &[AgentPaneInfo],
    captures: &BTreeMap<String, bool>,
) -> Vec<super::SessionAttention> {
    sessions
        .iter()
        .map(|s| super::SessionAttention {
            name: s.name.clone(),
            id: s.id.clone(),
            attached: s.attached,
            session_group: s.session_group.clone(),
            detected: panes
                .iter()
                .filter(|p| p.tmux_session == s.name)
                .any(|p| captures.get(&p.pane_id).copied().unwrap_or(false)),
        })
        .collect()
}

/// Remember uncertainty per physical pane/process, not per session. Unknown
/// capture B cannot retain an already answered A just because A lit the tab.
#[derive(Default)]
pub(super) struct Observer {
    known: BTreeMap<String, (u32, bool)>,
}
impl Observer {
    fn record(&mut self, pane: &AgentPaneInfo, result: Option<bool>) -> bool {
        if let Some(flag) = result {
            self.known.insert(pane.pane_id.clone(), (pane.pid, flag));
            flag
        } else {
            self.known
                .get(&pane.pane_id)
                .filter(|(pid, _)| *pid == pane.pid)
                .is_some_and(|(_, flag)| *flag)
        }
    }
    pub(super) async fn collect(
        &mut self,
        sessions: &[SessionInfo],
        previous: &HashMap<String, bool>,
    ) -> Vec<super::SessionAttention> {
        let panes = match crate::tmux::list_agent_panes().await {
            Ok(panes) => panes,
            Err(error) => {
                tracing::debug!(%error, "attention pane inventory unavailable");
                return sessions
                    .iter()
                    .map(|s| super::SessionAttention {
                        name: s.name.clone(),
                        id: s.id.clone(),
                        attached: s.attached,
                        session_group: s.session_group.clone(),
                        detected: previous.get(&s.name).copied().unwrap_or(false),
                    })
                    .collect();
            }
        };
        let client = Capture::default();
        let mut captures = BTreeMap::new();
        for pane in &panes {
            if captures.contains_key(&pane.pane_id) {
                continue;
            }
            let result = match client.pane(pane).await {
                Ok(flag) => Some(flag),
                Err(error) => {
                    tracing::debug!(pane = %pane.pane_id, %error, "attention capture unknown");
                    None
                }
            };
            captures.insert(pane.pane_id.clone(), self.record(pane, result));
        }
        self.known.retain(|id, _| captures.contains_key(id));
        aggregate(sessions, &panes, &captures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const EXEC: &str = "Would you like to run the following command?\nReason: network verification\n$ curl example.test\n› 1. Yes, proceed (y)\n  2. Yes, and allow this session (a)\n  3. No, and tell Codex what to do differently (esc)\nPress enter to confirm or esc to cancel";
    const QUESTION: &str = "Question 1/2 (2 unanswered)\nWhich target should be used?\n› 1. Local\n  2. Remote\ntab to add notes | enter to submit answer | ←/→ to navigate questions | esc to interrupt";
    const OPENCODE: &str = "┃ △ Permission required\n┃ Execute requested command\n┃ Allow once  Allow always  Reject\n┃ ⇆ select  enter confirm";

    #[test]
    fn native_codex_command_patch_network_permissions_stdin_and_mcp_are_detected() {
        for heading in [
            "Would you like to run the following command?",
            "Would you like to make the following edits?",
            "Would you like to grant these permissions?",
            "Do you want to approve network access to \"example.test\"?",
            "Would you like to send input to terminal 123?",
            "Would you like to send input to the existing terminal?",
            "project-tools needs your approval.",
        ] {
            let pane = EXEC.replacen(EXEC.lines().next().unwrap(), heading, 1);
            assert!(detect(&pane), "heading {heading}");
        }
        let selected_second = EXEC.replace("› 1.", "  1.").replace("  2.", "› 2.");
        assert!(detect(&selected_second));
        let wrapped = EXEC
            .replace(
                "Would you like to run the following command?",
                "Would you like to run\nthe following command?",
            )
            .replace(
                "Press enter to confirm or esc to cancel",
                "Press enter to confirm or\nesc to cancel",
            );
        assert!(detect(&wrapped));
        assert!(detect(&format!("{EXEC} or o to open thread")));
        assert!(!detect(&EXEC.replace('›', " ")));
    }

    #[test]
    fn codex_structured_choices_freeform_notes_and_unanswered_confirmation_wait() {
        assert!(detect(QUESTION));
        assert!(detect("Question 1/1 (1 unanswered)\nDescribe your requirements.\n› Type your answer (optional)\nenter to submit answer | esc to interrupt"));
        assert!(detect("Question 1/1 (1 unanswered)\nDescribe your requirements.\n› Typed answer in progress\n⌃j to submit answer | f12 to interrupt"));
        assert!(detect("Question 1/1 (1 unanswered)\nChoose deployment.\n› 1. Local\n  2. Remote\n› Add notes\ntab or esc to clear notes | enter to submit answer"));
        assert!(detect("Submit with unanswered questions?\n2 unanswered questions\n› 1. Proceed\n  2. Go back\nPress enter to confirm or esc to go back"));
    }

    #[test]
    fn idle_composers_prose_history_source_quotes_and_answered_screens_are_not_attention() {
        for pane in [
            "Which option do you prefer?",
            "› Ask Codex to do anything\n? for shortcuts",
            "Question 1/1\nPreviously answered\n✔ Answer received",
            "# Questions\nWhich target? Local",
        ] {
            assert!(!detect(pane), "ordinary text {pane}");
        }
        for native in [EXEC, QUESTION, OPENCODE] {
            for suffix in [
                "\n› Ask Codex to do anything\n? for shortcuts",
                "\n$ ",
                "\nuser@host %",
                "\nCompleted successfully",
            ] {
                assert!(!detect(&format!("{native}{suffix}")), "stale {suffix:?}");
            }
            let quoted = native
                .lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!detect(&quoted));
            let diff = native
                .lines()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!detect(&diff));
        }
    }

    #[test]
    fn opencode_released_and_dev_permissions_questions_and_review_are_detected() {
        assert!(detect(OPENCODE));
        assert!(detect(&OPENCODE.replace("Allow always", "Always allow")));
        assert!(detect("┃ △ Always allow\n┃ This will allow the following patterns until OpenCode is restarted.\n┃ Confirm  Cancel\n┃ ⇆ select  enter confirm"));
        assert!(detect("┃ △ Reject permission\n┃ Tell OpenCode what to do differently\n┃ Typed reason\n┃ enter confirm  esc cancel"));
        assert!(detect(
            "┃ Deployment target\n┃ 1. Local\n┃ 2. Remote\n┃ ↑↓ select  enter submit  esc dismiss"
        ));
        assert!(detect("┃ Deployment target\n┃ [ ] Local\n┃ [✓] Remote\n┃ ⇆ tab  ↑↓ select  enter toggle  esc dismiss"));
        assert!(detect("┃ Deployment target\n┃ 1. Local\n┃ 2. Type your own answer\n┃ Typed custom answer\n┃ ↑↓ select  enter submit  esc dismiss"));
        assert!(detect(
            "┃ Review\n┃ Target: Local\n┃ ⇆ tab  enter submit  esc dismiss"
        ));
        assert!(!detect("┃ Type your own answer"));
        assert!(!detect(&OPENCODE.replace('┃', " ")));
    }

    fn session(name: &str, id: &str, group: Option<&str>, attached: u32) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            id: id.into(),
            attached,
            windows: 2,
            created: 0,
            path: "/project".into(),
            session_group: group.map(String::from),
        }
    }
    fn pane(name: &str, id: &str, pid: u32, active: bool) -> AgentPaneInfo {
        AgentPaneInfo {
            pane_id: id.into(),
            tmux_session: name.into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            pid,
            tty: "/dev/test".into(),
            path: "/project".into(),
            title: String::new(),
            active,
            session_group: None,
        }
    }
    fn flags(items: Vec<super::super::SessionAttention>) -> HashMap<String, bool> {
        super::super::deduplicate_attention(&items)
            .into_iter()
            .collect()
    }

    #[test]
    fn identical_independent_prompts_light_both_and_any_inactive_pane_keeps_session_waiting() {
        let sessions = [session("one", "$1", None, 0), session("two", "$2", None, 0)];
        let panes = [
            pane("one", "%1", 10, true),
            pane("one", "%2", 20, false),
            pane("two", "%3", 30, true),
        ];
        let mut captures = BTreeMap::from([
            ("%1".into(), false),
            ("%2".into(), detect(EXEC)),
            ("%3".into(), detect(EXEC)),
        ]);
        assert_eq!(
            flags(aggregate(&sessions, &panes, &captures)),
            HashMap::from([("one".into(), true), ("two".into(), true)])
        );
        captures.insert("%1".into(), true);
        captures.insert("%2".into(), false);
        assert!(flags(aggregate(&sessions, &panes, &captures))["one"]);
        captures.insert("%1".into(), false);
        assert!(!flags(aggregate(&sessions, &panes, &captures))["one"]);
    }

    #[test]
    fn only_linked_aliases_deduplicate_and_unknown_reads_retain_the_affected_pane_only() {
        let sessions = [
            session("one", "$1", Some("g"), 0),
            session("alias", "$2", Some("g"), 1),
        ];
        let a = pane("one", "%1", 10, true);
        let mut alias = a.clone();
        alias.tmux_session = "alias".into();
        let b = pane("one", "%2", 20, false);
        let panes = [a.clone(), alias, b.clone()];
        let mut observer = Observer::default();
        assert!(observer.record(&a, Some(true)));
        assert!(!observer.record(&b, Some(false)));
        assert!(observer.record(&a, None));
        assert!(!observer.record(&a, Some(false)));
        assert!(
            !observer.record(&b, None),
            "unknown B must not retain previously answered A"
        );
        assert!(
            !observer.record(&pane("one", "%1", 99, true), None),
            "changed process cannot inherit stale attention"
        );
        let captures = BTreeMap::from([("%1".into(), true), ("%2".into(), false)]);
        let result = flags(aggregate(&sessions, &panes, &captures));
        assert!(!result["one"]);
        assert!(result["alias"]);
    }

    struct Server(Capture);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(socket) = &self.0.socket {
                let _ = std::process::Command::new("tmux")
                    .arg("-S")
                    .arg(socket)
                    .args(["-f", "/dev/null", "kill-server"])
                    .output();
            }
        }
    }
    fn quote(path: &std::path::Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    #[tokio::test]
    async fn isolated_inactive_physical_pane_lights_and_clears_without_any_input_or_window_switch()
    {
        let dir = tempfile::tempdir().unwrap();
        let client = Capture {
            socket: Some(dir.path().join("tmux.sock")),
        };
        let _server = Server(Capture {
            socket: client.socket.clone(),
        });
        let display = dir.path().join("display");
        let script = dir.path().join("display.py");
        std::fs::write(&display, EXEC).unwrap();
        std::fs::write(&script, "import sys,time\np=sys.argv[1]\nold=None\nwhile True:\n s=open(p).read()\n if s!=old:\n  sys.stdout.write('\\x1b[2J\\x1b[H'+s.replace('\\n','\\r\\n'));sys.stdout.flush();old=s\n time.sleep(.02)\n").unwrap();
        client
            .command(&[
                "new-session",
                "-d",
                "-s",
                "attention_test",
                "-n",
                "active",
                "-x",
                "120",
                "-y",
                "30",
                "sleep 60",
            ])
            .await
            .unwrap();
        let launch = format!("exec python3 {} {}", quote(&script), quote(&display));
        let metadata = client
            .command(&[
                "new-window",
                "-d",
                "-t",
                "attention_test:",
                "-n",
                "waiting",
                "-P",
                "-F",
                "#{pane_id}|#{pane_pid}",
                &launch,
            ])
            .await
            .unwrap();
        let (id, pid) = metadata.trim().split_once('|').unwrap();
        let target = pane("attention_test", id, pid.parse().unwrap(), false);
        let mut detected = false;
        for _ in 0..100 {
            if client.pane(&target).await.unwrap_or(false) {
                detected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            detected,
            "actual inactive terminal capture must detect native overlay"
        );
        let mut observer = Observer::default();
        let sessions = [session("attention_test", "$1", None, 0)];
        let mut captures = BTreeMap::new();
        captures.insert(id.into(), observer.record(&target, Some(true)));
        assert!(flags(aggregate(&sessions, &[target.clone()], &captures))["attention_test"]);
        client.command(&["copy-mode", "-t", id]).await.unwrap();
        assert!(client.pane(&target).await.is_err());
        captures.insert(id.into(), observer.record(&target, None));
        assert!(flags(aggregate(&sessions, &[target.clone()], &captures))["attention_test"]);
        client
            .command(&["copy-mode", "-q", "-t", id])
            .await
            .unwrap();
        std::fs::write(
            &display,
            "Request approved and completed.\n› Ask Codex to do anything\n? for shortcuts",
        )
        .unwrap();
        for _ in 0..100 {
            if !client.pane(&target).await.unwrap_or(true) {
                detected = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!detected);
        captures.insert(id.into(), observer.record(&target, Some(false)));
        assert!(!flags(aggregate(&sessions, &[target.clone()], &captures))["attention_test"]);
        client.command(&["copy-mode", "-t", id]).await.unwrap();
        assert!(client.pane(&target).await.is_err());
        assert!(
            !observer.record(&target, None),
            "copying old overlays must not arm previously false attention"
        );
        let active = client
            .command(&[
                "display-message",
                "-p",
                "-t",
                "attention_test:",
                "#{window_name}",
            ])
            .await
            .unwrap();
        assert_eq!(active.trim(), "active");
    }
}
