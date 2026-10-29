//! Resume a Codex terminal after its native model-capacity error.
//! Only verified Codex processes and an idle, empty styled composer qualify.
//! Captured text is transient; state and logs retain only hashes/identities.
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::agents::model::AgentProvider;
use crate::agents::runtime::{identity_alive, AgentRuntime, RuntimeInventory};

const ERROR: &str = "Selected model is at capacity. Please try a different model.";
const MESSAGE: &str = "продолжи";
const TICK: Duration = Duration::from_secs(2);
const INITIAL_DELAY_MS: u64 = 15_000;
const STABLE_MS: u64 = 2_000;
const META_FORMAT: &str = "FORGE_CAPACITY_META:#{pane_id}\u{1f}#{pane_pid}\u{1f}#{cursor_x}\u{1f}#{cursor_y}\u{1f}#{cursor_flag}\u{1f}#{pane_width}\u{1f}#{pane_height}\u{1f}#{pane_in_mode}\u{1f}#{alternate_on}\u{1f}#{pane_dead}";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    pane: String,
    root_pid: u32,
    root_start: u64,
    pid: u32,
    start: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    pane: String,
    pid: u32,
    x: usize,
    y: usize,
    cursor: bool,
    width: usize,
    height: usize,
    in_mode: bool,
    alternate: bool,
    dead: bool,
}
impl Meta {
    fn parse(line: &str) -> Option<Self> {
        let f: Vec<_> = line
            .strip_prefix("FORGE_CAPACITY_META:")?
            .split('\u{1f}')
            .collect();
        if f.len() != 10 || !valid_pane(f[0]) {
            return None;
        }
        let flag = |value: &str| match value {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        };
        Some(Self {
            pane: f[0].into(),
            pid: f[1].parse().ok()?,
            x: f[2].parse().ok()?,
            y: f[3].parse().ok()?,
            cursor: flag(f[4])?,
            width: f[5].parse().ok()?,
            height: f[6].parse().ok()?,
            in_mode: flag(f[7])?,
            alternate: flag(f[8])?,
            dead: flag(f[9])?,
        })
    }
}

#[derive(Debug, Clone)]
struct Screen {
    meta: Meta,
    raw: String,
}

#[derive(Debug, Clone, Copy, Default)]
struct Style {
    dim: bool,
    bg: Option<u32>,
    red: bool,
}
#[derive(Debug, Clone)]
struct Cell {
    ch: char,
    style: Style,
}
#[derive(Debug, Clone, Default)]
struct Line {
    text: String,
    cells: Vec<Cell>,
}

fn sgr(style: &mut Style, params: &str) -> Option<()> {
    let p: Vec<u32> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split(';')
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .ok()?
    };
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            0 => *style = Style::default(),
            2 => style.dim = true,
            22 => style.dim = false,
            30..=37 | 90..=97 => style.red = matches!(p[i], 31 | 91),
            39 => style.red = false,
            40..=47 | 100..=107 => style.bg = Some(p[i]),
            49 => style.bg = None,
            code @ (38 | 48) => {
                let (color, red, consumed) = match p.get(i + 1)? {
                    5 => {
                        let n = *p.get(i + 2)?;
                        (0x1000000 | n, matches!(n, 1 | 9), 2)
                    }
                    2 => {
                        let (r, g, b) = (*p.get(i + 2)?, *p.get(i + 3)?, *p.get(i + 4)?);
                        if r > 255 || g > 255 || b > 255 {
                            return None;
                        }
                        (
                            0x2000000 | r << 16 | g << 8 | b,
                            r > g + 25 && r > b + 25,
                            4,
                        )
                    }
                    _ => return None,
                };
                if code == 48 {
                    style.bg = Some(color);
                } else {
                    style.red = red;
                }
                // Color operands (notably the truecolor selector 2) are not SGR attributes.
                i += consumed;
            }
            _ => {}
        }
        i += 1;
    }
    Some(())
}

fn styled_lines(raw: &str) -> Option<Vec<Line>> {
    let mut lines = vec![Line::default()];
    let mut style = Style::default();
    let mut at = 0;
    while at < raw.len() {
        let rest = &raw[at..];
        if let Some(params) = rest.strip_prefix("\u{1b}[") {
            let end = params.find('m')?;
            if end > 80 {
                return None;
            }
            sgr(&mut style, &params[..end])?;
            at += 2 + end + 1;
            continue;
        }
        let ch = rest.chars().next()?;
        at += ch.len_utf8();
        if ch == '\n' {
            lines.push(Line::default());
        } else if ch != '\r' {
            if ch.is_control() {
                return None;
            }
            let line = lines.last_mut()?;
            line.text.push(ch);
            line.cells.push(Cell { ch, style });
        }
    }
    Some(lines)
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    fingerprint: u64,
    dimensions: (usize, usize),
}

/// Recognize only the latest red native error immediately above an empty composer.
fn capacity_failure(screen: &Screen) -> Option<Failure> {
    let m = &screen.meta;
    if m.dead || m.in_mode || !m.alternate || !m.cursor || m.width < 20 || m.y >= m.height {
        return None;
    }
    let lines = styled_lines(&screen.raw)?;
    let composer = lines.get(m.y)?;
    let leading = composer.text.chars().take_while(|c| *c == ' ').count();
    let prefix = composer.cells.get(leading..leading + 2)?;
    if !matches!(prefix[0].ch, '›' | '❯') || prefix[1].ch != ' ' || m.x != leading + 2 {
        return None;
    }
    let background = prefix[1].style.bg?;
    // Typed input is not dim, even if it exactly copies the placeholder and the cursor is at Home.
    if composer
        .cells
        .iter()
        .skip(leading + 2)
        .any(|cell| !cell.ch.is_whitespace() && !cell.style.dim)
    {
        return None;
    }
    let footer = lines.get(m.y + 1..)?;
    if footer.len() > 8 {
        return None;
    }
    let mut shortcuts = false;
    for line in footer {
        let text = line.text.trim();
        if line.cells.iter().any(|cell| {
            !cell.ch.is_whitespace() && cell.style.bg == Some(background) && !cell.style.dim
        }) {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        if lower.contains("esc to interrupt")
            || lower.contains("esc to cancel")
            || lower.contains("ctrl+c to interrupt")
        {
            return None;
        }
        if text.starts_with(['›', '❯']) {
            return None;
        }
        shortcuts |= lower.contains("? for shortcuts");
    }
    if !shortcuts {
        return None;
    }
    let mut end = m.y;
    while end > 0 && lines[end - 1].text.trim().is_empty() {
        end -= 1;
    }
    // An optional native background-terminal notice is informational, not model activity.
    let lower_bound = end.saturating_sub(4);
    if let Some(start) = (lower_bound..end).rev().find(|&i| {
        let text = lines[i].text.trim();
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && text[digits..].starts_with(" background terminal")
    }) {
        let notice = normalize(
            &lines[start..end]
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
        if !(notice.contains("/ps to view") && notice.ends_with("/stop to close")) {
            return None;
        }
        end = start;
        while end > 0 && lines[end - 1].text.trim().is_empty() {
            end -= 1;
        }
    }
    let start = (end.saturating_sub(6)..end).rev().find(|&i| {
        let t = lines[i].text.trim_start();
        t.starts_with('■')
            && lines[i]
                .cells
                .iter()
                .find(|c| !c.ch.is_whitespace())
                .is_some_and(|c| c.style.red)
    })?;
    let error = normalize(
        &lines[start..end]
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    if error.strip_prefix('■')?.trim() != ERROR {
        return None;
    }
    if lines[start..end]
        .iter()
        .flat_map(|l| &l.cells)
        .any(|c| !c.ch.is_whitespace() && !c.style.red)
    {
        return None;
    }
    // Only conversation preceding the error identifies its occurrence. Footer
    // clocks, cursor position, colors and line wrapping are excluded.
    let context: String = lines[..start]
        .iter()
        .flat_map(|l| l.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    let context: String = context.chars().rev().take(256).collect();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    context.hash(&mut hash);
    ERROR.hash(&mut hash);
    Some(Failure {
        fingerprint: hash.finish(),
        dimensions: (m.width, m.height),
    })
}

#[derive(Debug, Default)]
struct RetryState {
    observed: Option<(Failure, u64)>,
    dimensions: Option<(usize, usize)>,
    last_sent: Option<(u64, u64)>,
    attempts: u8,
}
impl RetryState {
    fn observe(&mut self, failure: Option<Failure>, now: u64) -> bool {
        let Some(failure) = failure else {
            self.observed = None;
            return false;
        };
        if let Some(previous) = self.dimensions {
            if previous != failure.dimensions {
                // A resize can expose a different amount of preceding history.
                // It never proves a new failure after a previous delivery.
                if let Some((_, sent)) = self.last_sent {
                    self.last_sent = Some((failure.fingerprint, sent));
                }
                self.observed = Some((failure, now));
                self.dimensions = self
                    .observed
                    .as_ref()
                    .map(|(failure, _)| failure.dimensions);
                return false;
            }
        }
        self.dimensions = Some(failure.dimensions);
        let first = match &self.observed {
            Some((previous, first)) if previous == &failure => *first,
            _ => {
                self.observed = Some((failure.clone(), now));
                return false;
            }
        };
        if self
            .last_sent
            .is_some_and(|(fingerprint, _)| fingerprint == failure.fingerprint)
        {
            return false;
        }
        let delay = if self.last_sent.is_none() {
            INITIAL_DELAY_MS
        } else {
            STABLE_MS
        };
        if now.saturating_sub(first) < delay {
            return false;
        }
        let backoff = (15_000u64.saturating_mul(1 << self.attempts.min(5))).min(300_000);
        !self
            .last_sent
            .is_some_and(|(_, sent)| now.saturating_sub(sent) < backoff)
    }
    fn attempted(&mut self, failure: &Failure, now: u64) {
        // Consume before writing: an ambiguous transport error must not cause duplicate input.
        self.last_sent = Some((failure.fingerprint, now));
        self.attempts = self.attempts.saturating_add(1);
    }
}

#[derive(Clone, Default)]
struct Tmux {
    socket: Option<PathBuf>,
}
impl Tmux {
    async fn command(&self, args: &[&str]) -> Result<String> {
        let mut command = Command::new("tmux");
        if let Some(socket) = &self.socket {
            command.arg("-f").arg("/dev/null").arg("-S").arg(socket);
        }
        command.args(args).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(3), command.output())
            .await
            .context("capacity recovery tmux timeout")??;
        if !output.status.success() {
            bail!("capacity recovery tmux command failed");
        }
        Ok(String::from_utf8(output.stdout)?)
    }
    async fn capture(&self, pane: &str) -> Result<Screen> {
        if !valid_pane(pane) {
            bail!("invalid capacity recovery pane");
        }
        let output = self
            .command(&[
                "display-message",
                "-p",
                "-t",
                pane,
                META_FORMAT,
                ";",
                "capture-pane",
                "-p",
                "-e",
                "-t",
                pane,
                ";",
                "display-message",
                "-p",
                "-t",
                pane,
                META_FORMAT,
            ])
            .await?;
        let (head, body) = output
            .split_once('\n')
            .context("missing capacity recovery metadata")?;
        let (raw, tail) = body
            .trim_end_matches('\n')
            .rsplit_once('\n')
            .context("missing capacity recovery capture")?;
        let meta = Meta::parse(head).context("invalid capacity recovery metadata")?;
        if Meta::parse(tail).as_ref() != Some(&meta) || meta.pane != pane {
            bail!("capacity recovery pane changed during capture");
        }
        Ok(Screen {
            meta,
            raw: raw.into(),
        })
    }
    async fn send(&self, pane: &str) -> Result<()> {
        if !valid_pane(pane) {
            bail!("invalid capacity recovery pane");
        }
        self.command(&[
            "send-keys",
            "-t",
            pane,
            "-l",
            "--",
            MESSAGE,
            ";",
            "send-keys",
            "-t",
            pane,
            "Enter",
        ])
        .await?;
        Ok(())
    }
}
fn valid_pane(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
}

fn candidates(inventory: &RuntimeInventory) -> BTreeMap<String, Identity> {
    let mut result = BTreeMap::new();
    for pane in &inventory.panes {
        let Some(root_start) = pane.process_started_at else {
            continue;
        };
        let candidates: Vec<_> = pane
            .candidates
            .iter()
            .filter(|p| p.provider == AgentProvider::Codex && !p.wrapper)
            .collect();
        if candidates.len() != 1 {
            continue;
        }
        let process = candidates[0];
        let Some(start) = process.process_started_at else {
            continue;
        };
        result.insert(
            pane.pane_id.clone(),
            Identity {
                pane: pane.pane_id.clone(),
                root_pid: pane.pid,
                root_start,
                pid: process.pid,
                start,
            },
        );
    }
    result
}
async fn alive(identity: &Identity) -> bool {
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || {
        identity_alive(identity.pid, Some(identity.start)) == Some(true)
            && identity_alive(identity.root_pid, Some(identity.root_start)) == Some(true)
    })
    .await
    .unwrap_or(false)
}

async fn delivery_ready(client: &Tmux, identity: &Identity, failure: &Failure) -> bool {
    if !alive(identity).await {
        return false;
    }
    let fresh = match client.capture(&identity.pane).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    fresh.meta.pid == identity.root_pid
        && capacity_failure(&fresh).as_ref() == Some(failure)
        && alive(identity).await
}

/// Default-on recovery uses runtime identity, not journal task classification.
pub async fn run(runtime: AgentRuntime) {
    let client = Tmux::default();
    let clock = Instant::now();
    let mut states: BTreeMap<String, (Identity, RetryState)> = BTreeMap::new();
    let mut timer = tokio::time::interval(TICK);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        let inventory = runtime.inventory().await;
        if !inventory.available
            || crate::agents::store::now_ms().saturating_sub(inventory.updated_at_ms) > 60_000
        {
            continue;
        }
        let candidates = candidates(&inventory);
        let active: BTreeSet<_> = inventory
            .panes
            .iter()
            .map(|pane| pane.pane_id.clone())
            .collect();
        states.retain(|pane, _| active.contains(pane));
        for (pane, identity) in candidates {
            let entry = states
                .entry(pane.clone())
                .or_insert_with(|| (identity.clone(), RetryState::default()));
            if entry.0 != identity {
                *entry = (identity.clone(), RetryState::default());
            }
            let now = clock.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let screen = match client.capture(&pane).await {
                Ok(s) if s.meta.pid == identity.root_pid => s,
                _ => {
                    entry.1.observed = None;
                    continue;
                }
            };
            let failure = capacity_failure(&screen);
            if !entry.1.observe(failure.clone(), now) {
                continue;
            }
            let Some(failure) = failure else {
                continue;
            };
            if !delivery_ready(&client, &identity, &failure).await {
                entry.1.observed = None;
                continue;
            }
            entry.1.attempted(&failure, now);
            match client.send(&pane).await {
                Ok(()) => {
                    tracing::info!(pane = %pane, pid = identity.pid, attempt = entry.1.attempts, "Codex capacity error: sent automatic continuation")
                }
                Err(error) => {
                    tracing::warn!(pane = %pane, %error, "Codex capacity continuation delivery uncertain; unchanged error will not be retried")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen(context: &str) -> Screen {
        let context = context.replace('\n', " ");
        Screen {
            meta: Meta { pane: "%1".into(), pid: 10, x: 2, y: 6, cursor: true,
                width: 100, height: 10, in_mode: false, alternate: true, dead: false },
            raw: format!("{context}\n\n\x1b[31m■ {ERROR}\x1b[0m\n\n1 background terminal running · /ps to view · /stop to close\n\n\x1b[1m›\x1b[0m\x1b[48;2;72;76;92m \x1b[2mAsk Codex to do anything\x1b[0m\n\n\x1b[2mweekly 96% left · GPT-6.1-Sol · Context 54% used · Goal stalled (/goal resume)\n← for agents · ? for shortcuts\x1b[0m"),
        }
    }
    #[test]
    fn screenshot_error_with_background_terminal_and_dim_composer_is_actionable() {
        assert!(capacity_failure(&screen("› previous request")).is_some());
        let mut theme = screen("› previous request");
        theme.raw = theme.raw.replace("\x1b[31m", "\x1b[38;2;233;144;169m");
        assert!(capacity_failure(&theme).is_some());
        let mut wrapped = screen("› previous request");
        wrapped.raw = wrapped.raw.replace(
            ERROR,
            "Selected model is at capacity.\nPlease try a different model.",
        );
        wrapped.meta.y += 1;
        wrapped.meta.height += 1;
        assert!(capacity_failure(&wrapped).is_some());
    }
    #[test]
    fn drafts_including_placeholder_text_and_multiline_home_cursor_are_preserved() {
        let base = screen("› previous request");
        for draft in [
            "my draft",
            "Ask Codex to do anything",
            "",
            "\x1b[48;2;72;76;92mAsk Codex to do anything",
        ] {
            let mut s = base.clone();
            s.raw = s.raw.replace("\x1b[2mAsk Codex to do anything", draft);
            if !draft.is_empty() {
                assert!(capacity_failure(&s).is_none(), "draft {draft:?}");
            }
        }
        let mut multiline = base.clone();
        multiline.raw = multiline.raw.replace(
            "\x1b[0m\n\n\x1b[2mweekly",
            "\x1b[0m\n\x1b[48;2;72;76;92msecond draft row\x1b[0m\n\x1b[2mweekly",
        );
        assert!(capacity_failure(&multiline).is_none());
        let mut moved = base;
        moved.meta.x = 9;
        assert!(capacity_failure(&moved).is_none());
    }
    #[test]
    fn stale_quotes_busy_shell_copy_and_hidden_cursor_do_not_qualify() {
        let base = screen("› previous request");
        for (from, to) in [
            ("\x1b[31m■ ", "■ "),
            ("\x1b[31m■ ", "\x1b[31m└ ■ "),
            (
                "\x1b[0m\n\n1 background",
                "\x1b[0m\nnew completed output\n1 background",
            ),
            ("? for shortcuts", "esc to interrupt · ? for shortcuts"),
        ] {
            let mut s = base.clone();
            s.raw = s.raw.replace(from, to);
            assert!(capacity_failure(&s).is_none(), "replacement {to:?}");
        }
        for index in 0..4 {
            let mut s = base.clone();
            match index {
                0 => s.meta.cursor = false,
                1 => s.meta.in_mode = true,
                2 => s.meta.alternate = false,
                _ => s.meta.dead = true,
            }
            assert!(capacity_failure(&s).is_none());
        }
    }
    #[test]
    fn truecolor_operands_never_enable_dim() {
        let mut style = Style::default();
        sgr(&mut style, "48;2;72;76;92").unwrap();
        assert!(!style.dim);
        assert!(style.bg.is_some());
        sgr(&mut style, "38;2;255;80;80;2").unwrap();
        assert!(style.dim && style.red);
        sgr(&mut style, "22").unwrap();
        assert!(!style.dim);
        sgr(&mut style, "0").unwrap();
        assert!(style.bg.is_none());
    }
    #[test]
    fn only_one_verified_native_codex_candidate_and_physical_pane_qualify() {
        use crate::agents::runtime::{RuntimePane, RuntimeProcess};
        let process = RuntimeProcess {
            pid: 11,
            parent_pid: 10,
            process_started_at: Some(101),
            provider: AgentProvider::Codex,
            native_ids: vec![],
            evidence: vec![],
            wrapper: false,
        };
        let pane = RuntimePane {
            pane_id: "%1".into(),
            tmux_session: "one".into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            pid: 10,
            process_started_at: Some(100),
            tty: "/dev/pts/1".into(),
            path: "/project".into(),
            active: false,
            candidates: vec![process.clone()],
        };
        let mut linked = pane.clone();
        linked.tmux_session = "linked".into();
        let mut inventory = RuntimeInventory {
            panes: vec![pane.clone(), linked],
            ..Default::default()
        };
        assert_eq!(candidates(&inventory).len(), 1);
        let mut ambiguous = process.clone();
        ambiguous.pid = 12;
        inventory.panes = vec![pane.clone()];
        inventory.panes[0].candidates.push(ambiguous);
        assert!(candidates(&inventory).is_empty());
        inventory.panes = vec![pane.clone()];
        inventory.panes[0].candidates[0].wrapper = true;
        assert!(candidates(&inventory).is_empty());
        inventory.panes = vec![pane.clone()];
        inventory.panes[0].candidates[0].provider = AgentProvider::Claude;
        assert!(candidates(&inventory).is_empty());
        inventory.panes = vec![pane];
        inventory.panes[0].process_started_at = None;
        assert!(candidates(&inventory).is_empty());
    }
    #[test]
    fn one_unchanged_error_is_sent_once_and_genuine_reerrors_back_off() {
        let a = capacity_failure(&screen("› first request")).unwrap();
        let b = capacity_failure(&screen("› first request\n› продолжи")).unwrap();
        let c = capacity_failure(&screen("› first request\n› продолжи\n› продолжи")).unwrap();
        let mut state = RetryState::default();
        assert!(!state.observe(Some(a.clone()), 0));
        assert!(!state.observe(Some(a.clone()), 14_999));
        assert!(state.observe(Some(a.clone()), 15_000));
        state.attempted(&a, 15_000);
        for now in [17_000, 300_000, 10_000_000] {
            assert!(!state.observe(Some(a.clone()), now));
        }
        // An entire busy turn and new native error can occur between polls.
        assert!(!state.observe(Some(b.clone()), 20_000));
        assert!(!state.observe(Some(b.clone()), 44_999));
        assert!(state.observe(Some(b.clone()), 45_000));
        state.attempted(&b, 45_000);
        assert!(!state.observe(Some(c.clone()), 47_000));
        assert!(!state.observe(Some(c.clone()), 104_999));
        assert!(state.observe(Some(c), 105_000));
    }
    #[test]
    fn footer_and_resize_changes_do_not_create_a_second_attempt() {
        let a = capacity_failure(&screen("› request")).unwrap();
        let mut changed = screen("› request");
        changed.raw = changed.raw.replace("96%", "95%").replace("54%", "55%");
        assert_eq!(a, capacity_failure(&changed).unwrap());
        let mut state = RetryState::default();
        state.observe(Some(a.clone()), 0);
        state.attempted(&a, 15_000);
        let mut resized =
            capacity_failure(&screen("newly visible historical rows › request")).unwrap();
        resized.dimensions = (130, 12);
        assert!(!state.observe(Some(resized.clone()), 30_000));
        assert!(!state.observe(Some(resized), 100_000));
    }
    #[test]
    fn resize_after_a_draft_or_copy_mode_gap_does_not_repeat_stale_error() {
        let a = capacity_failure(&screen("› request")).unwrap();
        let mut state = RetryState::default();
        state.observe(Some(a.clone()), 0);
        state.attempted(&a, 15_000);
        assert!(!state.observe(None, 20_000));
        let mut resized = capacity_failure(&screen("newly exposed history › request")).unwrap();
        resized.dimensions = (130, 12);
        assert!(!state.observe(Some(resized.clone()), 30_000));
        assert!(!state.observe(Some(resized), 500_000));
    }

    struct Server(Tmux);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(socket) = &self.0.socket {
                let _ = std::process::Command::new("tmux")
                    .arg("-S")
                    .arg(socket)
                    .arg("kill-server")
                    .output();
            }
        }
    }
    fn quote(path: &std::path::Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    #[tokio::test]
    async fn isolated_inactive_pane_rechecks_draft_and_receives_one_exact_message() {
        // Explicit private socket on EVERY command; this test cannot reach user panes.
        let dir = tempfile::tempdir().unwrap();
        let client = Tmux {
            socket: Some(dir.path().join("tmux.sock")),
        };
        let _server = Server(client.clone());
        let script = dir.path().join("fake_tui.py");
        let control = dir.path().join("mode");
        let received = dir.path().join("received");
        std::fs::write(&control, "empty").unwrap();
        std::fs::write(&script, r#"import os,sys,tty,select
tty.setraw(0)
control,received=sys.argv[1:]
last=None
while True:
    mode=open(control).read()
    if mode!=last:
        h=os.get_terminal_size().lines
        composer=h-4
        def row(y,text): sys.stdout.write(f'\x1b[{y+1};1H'+text)
        sys.stdout.write('\x1b[?1049h\x1b[?25h\x1b[2J\x1b[H')
        row(composer-4,'Native request before capacity error')
        row(composer-3,'\x1b[31m■ Selected model is at capacity. Please try a different model.\x1b[0m')
        row(composer-2,'1 background terminal running · /ps to view · /stop to close')
        row(composer,'\x1b[1m›\x1b[0m\x1b[48;2;72;76;92m '+('\x1b[2mAsk Codex to do anything' if mode=='empty' else 'my unsent draft')+'\x1b[0m')
        row(h-2,'weekly 96% left · GPT-6.1-Sol · Context 54% used · Goal stalled (/goal resume)')
        row(h-1,'← for agents · ? for shortcuts')
        sys.stdout.write(f'\x1b[{composer+1};3H');sys.stdout.flush();last=mode
    if select.select([0],[],[],0.02)[0]:
        data=os.read(0,4096)
        with open(received,'ab') as stream: stream.write(data)
"#).unwrap();
        client
            .command(&[
                "new-session",
                "-d",
                "-s",
                "capacity_test",
                "-n",
                "active",
                "-x",
                "110",
                "-y",
                "25",
                "sleep 60",
            ])
            .await
            .unwrap();
        let launch = format!(
            "exec python3 {} {} {}",
            quote(&script),
            quote(&control),
            quote(&received)
        );
        let pane = client
            .command(&[
                "new-window",
                "-d",
                "-t",
                "capacity_test:",
                "-n",
                "inactive",
                "-P",
                "-F",
                "#{pane_id}",
                &launch,
            ])
            .await
            .unwrap()
            .trim()
            .to_owned();
        let mut initial = None;
        for _ in 0..100 {
            if let Ok(screen) = client.capture(&pane).await {
                if capacity_failure(&screen).is_some() {
                    initial = Some(screen);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let initial = initial.expect("synthetic Codex TUI must be ready");
        let failure = capacity_failure(&initial).unwrap();
        let pid = initial.meta.pid;
        let start = crate::agents::runtime::process_started_at(pid).unwrap();
        let identity = Identity {
            pane: pane.clone(),
            root_pid: pid,
            root_start: start,
            pid,
            start,
        };
        assert!(delivery_ready(&client, &identity, &failure).await);
        std::fs::write(&control, "draft").unwrap();
        for _ in 0..100 {
            if capacity_failure(&client.capture(&pane).await.unwrap()).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!delivery_ready(&client, &identity, &failure).await);
        assert!(!received.exists(), "draft recheck must not write anything");
        std::fs::write(&control, "empty").unwrap();
        for _ in 0..100 {
            if delivery_ready(&client, &identity, &failure).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut state = RetryState::default();
        state.observe(Some(failure.clone()), 0);
        assert!(state.observe(Some(failure.clone()), 15_000));
        assert!(delivery_ready(&client, &identity, &failure).await);
        state.attempted(&failure, 15_000);
        client.send(&pane).await.unwrap();
        for _ in 0..100 {
            if std::fs::read(&received).is_ok_and(|bytes| bytes.ends_with(b"\r")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            std::fs::read(&received).unwrap(),
            format!("{MESSAGE}\r").as_bytes()
        );
        assert!(!state.observe(
            capacity_failure(&client.capture(&pane).await.unwrap()),
            600_000
        ));
        let active = client
            .command(&[
                "display-message",
                "-p",
                "-t",
                "capacity_test:",
                "#{window_name}",
            ])
            .await
            .unwrap();
        assert_eq!(
            active.trim(),
            "active",
            "inactive target must remain inactive"
        );
        let wrong_identity = Identity {
            start: start + 1,
            ..identity
        };
        assert!(!delivery_ready(&client, &wrong_identity, &failure).await);
    }
}
