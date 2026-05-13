//! Интеграция с tmux CLI: листинг, создание и убийство сессий.
//!
//! Все вызовы — через `tokio::process::Command`, чтобы не блокировать
//! async-runtime. Парсинг строго по формату, заданному в `-F`.
//!
//! Особенность: tmux при отсутствии запущенного сервера выдаёт ошибку
//! `"no server running on /tmp/tmux-1000/default"`. Это НЕ ошибка для
//! нашего web-viewer — мы трактуем её как «сессий нет» и возвращаем
//! пустой список.

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Метаданные одной tmux-сессии для отдачи во фронтенд.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    /// Имя сессии (`#{session_name}`), уникально в рамках tmux-сервера.
    pub name: String,
    /// Внутренний id сессии (`#{session_id}`), вида `$0`, `$1`, ...
    pub id: String,
    /// Сколько клиентов сейчас прикреплено к сессии.
    pub attached: u32,
    /// Количество окон в сессии.
    pub windows: u32,
    /// Unix-таймстамп создания сессии (`#{session_created}`).
    pub created: i64,
    /// Стартовый cwd сессии (`#{session_path}`). Используется для
    /// группировки сессий по реальной папке-проекту, независимо от
    /// `tmux_prefix`.
    pub path: String,
    /// Имя tmux session-group, к которой привязана сессия
    /// (`#{session_group}`).
    ///
    /// tmux позволяет создавать «linked» сессии, которые делят одни и те же
    /// окна (`tmux new-session -t <existing>`). Все сессии одной группы
    /// получают одинаковое значение `#{session_group}`. Если сессия не входит
    /// ни в какую группу — tmux возвращает пустую строку, что мапится в
    /// `None`.
    ///
    /// Используется в `attention::watcher_loop` для дедупликации: сессии
    /// одной группы рендерят одну и ту же логическую работу, поэтому
    /// `needs_attention=true` должен подсвечиваться только у одной из них.
    #[serde(default)]
    pub session_group: Option<String>,
}

/// Формат вывода для `tmux list-sessions -F`. Поля разделены `|`.
///
/// Порядок полей: `name | id | attached | windows | created | path | session_group`.
/// `#{session_group}` идёт последним, чтобы старый формат без этого поля (6
/// колонок) оставался парсибельным — см. `parse_session_line`.
const LS_FORMAT: &str =
    "#{session_name}|#{session_id}|#{session_attached}|#{session_windows}|#{session_created}|#{session_path}|#{session_group}";

/// Возвращает список активных tmux-сессий.
///
/// - Если tmux-сервер не запущен — `Ok(vec![])`.
/// - Если tmux отсутствует в `$PATH` — `Err`.
/// - Битые строки (несовпадение колонок) пропускаются с warning'ом в лог.
pub async fn list_sessions() -> anyhow::Result<Vec<SessionInfo>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", LS_FORMAT])
        .output()
        .await
        .context("failed to spawn `tmux list-sessions` (is tmux installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "no server running on /tmp/..." → это нормальная ситуация, нет сессий.
        if stderr.contains("no server running") {
            tracing::debug!("tmux server not running, returning empty session list");
            return Ok(Vec::new());
        }
        return Err(anyhow!(
            "tmux list-sessions failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut sessions = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        match parse_session_line(line) {
            Some(s) => sessions.push(s),
            None => tracing::warn!(line = %line, "skipping malformed tmux list-sessions line"),
        }
    }
    Ok(sessions)
}

/// Парсит одну строку формата `name|id|attached|windows|created|path|session_group`.
///
/// Возвращает `None` если первых пять колонок отсутствуют или числа не парсятся.
/// Поля `path` и `session_group` опциональны для обратной совместимости со
/// старым форматом (5 или 6 колонок). Пустой `session_group` мапится в `None`.
fn parse_session_line(line: &str) -> Option<SessionInfo> {
    let mut parts = line.splitn(7, '|');
    let name = parts.next()?.to_string();
    let id = parts.next()?.to_string();
    let attached = parts.next()?.parse().ok()?;
    let windows = parts.next()?.parse().ok()?;
    let created = parts.next()?.parse().ok()?;
    let path = parts.next().unwrap_or("").to_string();
    let session_group = match parts.next() {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    if name.is_empty() || id.is_empty() {
        return None;
    }
    Some(SessionInfo {
        name,
        id,
        attached,
        windows,
        created,
        path,
        session_group,
    })
}

/// Создаёт новую detached tmux-сессию с заданной рабочей директорией.
///
/// Эквивалент `tmux new-session -d -s <name> -c <cwd>`. Флаг `-c` задаёт
/// startup-cwd: внутри сессии все шеллы будут стартовать в `cwd`. Это нужно
/// для multi-project режима (Phase 6.B), чтобы сессии активного проекта
/// открывались в его корне.
///
/// Если сессия с таким именем уже существует — tmux вернёт ненулевой exit,
/// мы маппим это в `Err`.
pub async fn new_session(name: &str, cwd: &std::path::Path) -> anyhow::Result<()> {
    if !is_valid_session_name(name) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            name
        );
    }

    let cwd_str = cwd
        .to_str()
        .ok_or_else(|| anyhow!("cwd path is not valid UTF-8: {}", cwd.display()))?;

    let output = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-c", cwd_str])
        .output()
        .await
        .context("failed to spawn `tmux new-session`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux new-session failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Захватывает содержимое **видимой** части активной панели сессии.
///
/// Эквивалент `tmux capture-pane -p -t <session>` — только current visible
/// pane без scrollback. Используется `attention::watcher_loop` для детекции
/// Claude permission prompt.
///
/// ### Почему без `-S -30`
///
/// Раньше захватывались последние 30 строк истории, что приводило к
/// false-positive: старый prompt из scrollback продолжал триггерить
/// `detect_claude_prompt`, даже когда юзер уже ответил и Claude отрисовал
/// что-то другое. Видимая часть всегда отражает «что сейчас на экране у
/// пользователя» — это и есть истинное состояние «нужно внимание».
///
/// Юнит-тесты детектора (`attention.rs::tests`) остаются валидны: их фикстуры
/// представляют собой именно видимую часть pane, scrollback там не задействован.
///
/// ### Гонка между list_sessions и capture-pane
///
/// Между листингом и захватом сессия может исчезнуть (юзер убил
/// `tmux kill-session`), а tmux-сервер мог упасть полностью. Оба случая —
/// **не ошибка** для watcher'а: возвращаем `Ok(String::new())`, чтобы loop
/// продолжил итерацию.
///
/// Маркеры распознаваемых не-ошибок в stderr:
/// - `"no server running"` — сервер упал/не запущен;
/// - `"can't find session"` — конкретная сессия исчезла.
///
/// Прочие сбои tmux (например, отсутствие бинаря в PATH) — `Err`.
#[allow(dead_code)]
pub async fn capture_pane(session: &str) -> anyhow::Result<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", session])
        .output()
        .await
        .context("failed to spawn `tmux capture-pane` (is tmux installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("can't find session") {
            tracing::debug!(
                session = %session,
                "tmux capture-pane: session/server absent, returning empty pane"
            );
            return Ok(String::new());
        }
        return Err(anyhow!(
            "tmux capture-pane failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Отправляет текст в активное окно tmux-сессии и нажимает Enter.
///
/// Используется фоновым notifier'ом для доставки текста промоутнутого
/// TODO в указанную tmux-сессию (см. Phase 2). Реализация:
///
/// 1. Имя сессии валидируется через [`is_valid_session_name`] —
///    запрещены пробелы, `:`, `.` и не-ASCII, чтобы tmux не интерпретировал
///    их как target syntax.
/// 2. Если `text` пуст — возвращаем `Ok(())` без действий.
/// 3. Многострочный текст разбивается по символу новой строки. Для каждой строки
///    выполняется `tmux send-keys -t <session> -l <line>`, после неё —
///    отдельный `tmux send-keys -t <session> Enter`. Это эквивалентно
///    набору пользователем строки за строкой и нажатию Enter после
///    каждой.
/// 4. Не запускаем shell — каждый аргумент передаётся отдельно через
///    `Command::args`, поэтому никакой интерпретации `text` как shell не
///    происходит. Безопасно для произвольных пользовательских строк.
///
/// Маркеры не-ошибок (как в [`capture_pane`]):
/// - `"no server running"` и `"can't find session"` остаются ошибками,
///   потому что вызов send_keys без активной сессии — это явная попытка
///   доставки сообщения, и calling-сторона обязана узнать о провале.
#[allow(dead_code)]
pub async fn send_keys(session: &str, text: &str) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    if text.is_empty() {
        return Ok(());
    }

    for line in text.split('\n') {
        if !line.is_empty() {
            let output = Command::new("tmux")
                .args(["send-keys", "-t", session, "-l", line])
                .output()
                .await
                .context("failed to spawn `tmux send-keys -l`")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow!(
                    "tmux send-keys -l failed (exit {:?}): {}",
                    output.status.code(),
                    stderr.trim()
                ));
            }
        }
        let enter = Command::new("tmux")
            .args(["send-keys", "-t", session, "Enter"])
            .output()
            .await
            .context("failed to spawn `tmux send-keys Enter`")?;
        if !enter.status.success() {
            let stderr = String::from_utf8_lossy(&enter.stderr);
            return Err(anyhow!(
                "tmux send-keys Enter failed (exit {:?}): {}",
                enter.status.code(),
                stderr.trim()
            ));
        }
    }
    Ok(())
}

/// Убивает существующую сессию (`tmux kill-session -t <name>`).
///
/// Возвращает `Err` если сессии нет или tmux-сервер не запущен.
pub async fn kill_session(name: &str) -> anyhow::Result<()> {
    if !is_valid_session_name(name) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            name
        );
    }

    let output = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .await
        .context("failed to spawn `tmux kill-session`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux kill-session failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Проверяет имя сессии: только `[A-Za-z0-9_-]+`, непустое.
///
/// tmux семантически плохо переваривает имена с `:` (target syntax) и `.`
/// (window-target). Пробелы и спецсимволы тоже отметаем — даже если
/// пробрасываем через args (а не shell), tmux может ругаться.
pub fn is_valid_session_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_line_ok() {
        let s = parse_session_line("work|$0|1|3|1715250000|/home/u/proj|").expect("must parse");
        assert_eq!(
            s,
            SessionInfo {
                name: "work".to_string(),
                id: "$0".to_string(),
                attached: 1,
                windows: 3,
                created: 1_715_250_000,
                path: "/home/u/proj".to_string(),
                session_group: None,
            }
        );
    }

    #[test]
    fn parse_session_line_missing_path_ok() {
        // Старый формат без session_path и session_group — оба пустые.
        let s = parse_session_line("work|$0|1|3|1715250000").expect("must parse");
        assert_eq!(s.path, "");
        assert_eq!(s.session_group, None);
    }

    #[test]
    fn parse_session_line_zero_attached() {
        let s = parse_session_line("dev|$2|0|1|1700000000|/tmp|").expect("must parse");
        assert_eq!(s.attached, 0);
        assert_eq!(s.windows, 1);
        assert_eq!(s.id, "$2");
        assert_eq!(s.path, "/tmp");
        assert_eq!(s.session_group, None);
    }

    #[test]
    fn parse_session_line_with_session_group() {
        // Linked-сессии из одной группы получают одинаковый session_group.
        let s = parse_session_line("ui|$0|1|3|1715250000|/home/u/proj|grp42")
            .expect("must parse");
        assert_eq!(s.session_group, Some("grp42".to_string()));
    }

    #[test]
    fn parse_session_line_empty_session_group_is_none() {
        // tmux возвращает пустую строку для не-linked сессий → None.
        let s = parse_session_line("solo|$1|0|1|1715250000|/tmp|").expect("must parse");
        assert_eq!(s.session_group, None);
    }

    #[test]
    fn parse_session_line_legacy_six_columns_ok() {
        // Старый формат без session_group (6 колонок) — backward compat.
        let s = parse_session_line("work|$0|1|3|1715250000|/home/u/proj").expect("must parse");
        assert_eq!(s.path, "/home/u/proj");
        assert_eq!(s.session_group, None);
    }

    #[test]
    fn parse_session_line_too_few_columns() {
        assert!(parse_session_line("work|$0|1|3").is_none());
        assert!(parse_session_line("").is_none());
    }

    #[test]
    fn parse_session_line_bad_numbers() {
        assert!(parse_session_line("work|$0|x|3|1715250000").is_none());
        assert!(parse_session_line("work|$0|1|y|1715250000").is_none());
        assert!(parse_session_line("work|$0|1|3|notanumber").is_none());
    }

    #[test]
    fn parse_session_line_empty_name() {
        // Если tmux каким-то образом отдал пустое имя — отбрасываем.
        assert!(parse_session_line("|$0|1|3|1715250000").is_none());
    }

    #[test]
    fn valid_session_names() {
        assert!(is_valid_session_name("foo"));
        assert!(is_valid_session_name("foo_bar"));
        assert!(is_valid_session_name("foo-bar"));
        assert!(is_valid_session_name("Foo123"));
        assert!(is_valid_session_name("a"));
    }

    #[test]
    fn invalid_session_names() {
        assert!(!is_valid_session_name(""));
        assert!(!is_valid_session_name("foo:bar"));
        assert!(!is_valid_session_name("foo.bar"));
        assert!(!is_valid_session_name("foo bar"));
        assert!(!is_valid_session_name("foo/bar"));
        assert!(!is_valid_session_name("foo$"));
        assert!(!is_valid_session_name("привет"));
    }
}
