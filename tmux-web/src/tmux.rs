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
/// Эквивалент `tmux capture-pane -p -J -t <session>` — только current visible
/// pane без scrollback. Используется `attention::watcher_loop` для детекции
/// Claude permission prompt.
///
/// ### Почему `-J`
///
/// Флаг `-J` (join wrapped lines) склеивает строки, перенесённые самим tmux,
/// в одну логическую — чтобы длинный footer prompt'а (`Enter to select ·
/// Tab/Arrow keys to navigate · Esc to cancel`) не разрывался посередине
/// маркера. Это defense-in-depth к нормализации whitespace в
/// `attention::detect_claude_prompt` (которая покрывает word-wrap от самого
/// Claude TUI, не помеченный tmux как wrapped).
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
    // Двоеточие в конце таргета указывает tmux, что это session-target, а не
    // target-window. Критично для числовых имён сессий (`0`, `1`, …): без `:`
    // tmux резолвит `-t 1` как ОКНО с индексом 1 в текущей активной сессии, а
    // не как сессию с именем `1`. На практике это приводило к тому, что
    // watcher захватывал чужой pane (окно соседней сессии) → индикатор работы
    // и оранжевое свечение «дублировались» в неправильную сессию.
    let target = format!("{session}:");
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-J", "-t", &target])
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

/// Захватывает содержимое панели с включённым scrollback (`-S -<lines>`).
///
/// Эквивалент `tmux capture-pane -p -t <session> -S -<lines>`. Используется
/// Echo плагином (`prompt_builder`) для подмешивания контекста tmux-сессий в
/// prompt к Claude — там нужен расширенный буфер, в отличие от watcher'а
/// (см. [`capture_pane`]), которому достаточно «что видно на экране».
///
/// ### Поведение
///
/// - `lines < 0` — ошибка (отрицательное окно неосмысленно).
/// - `lines > 10000` — clamp до 10_000 (защита от случайного OOM на больших
///   историях).
/// - `lines == 0` — равнозначно «только видимая часть» (флаг `-S 0`).
/// - При отсутствующей сессии или non-running tmux-сервере — `Ok("")`,
///   как и в [`capture_pane`]; calling-сторона (prompt-builder) пропускает
///   эту сессию и продолжает с остальными.
pub async fn capture_pane_full(session: &str, lines: i32) -> anyhow::Result<String> {
    if lines < 0 {
        bail!("capture_pane_full: lines must be >= 0, got {lines}");
    }
    let clamped: i32 = lines.min(10_000);
    // tmux хочет `-S -N` где N — отступ от конца назад. `-N` строго отрицательное
    // число, иначе синтаксис не подходит. Формируем строку явно.
    let start_arg = format!("-{clamped}");

    // См. `capture_pane`: суффикс `:` делает таргет session-target, иначе для
    // числовых имён сессий tmux захватывает окно с тем же индексом в текущей
    // активной сессии вместо нужной сессии.
    let target = format!("{session}:");
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-p",
            "-t",
            &target,
            "-S",
            &start_arg,
        ])
        .output()
        .await
        .context("failed to spawn `tmux capture-pane -S` (is tmux installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("can't find session") {
            tracing::debug!(
                session = %session,
                lines = clamped,
                "tmux capture-pane -S: session/server absent, returning empty pane"
            );
            return Ok(String::new());
        }
        return Err(anyhow!(
            "tmux capture-pane -S failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Формат строки `list-panes` для [`find_claude_pane`] и
/// [`list_claude_pane_targets`]:
/// `win.pane|команда|окно_активно|панель_активна|имя_окна`.
///
/// `#{window_name}` идёт ПОСЛЕДНИМ и добавлен позже остальных: [`pick_claude_pane`]
/// читает только первые четыре поля, поэтому дописывание пятого его не задело.
/// Имя окна нужно истории авто-апрува — человеку понятно «vim» или «server»,
/// а не индекс `1.0`.
const LP_FORMAT: &str = "#{window_index}.#{pane_index}|#{pane_current_command}|#{window_active}|#{pane_active}|#{window_name}";

/// Похоже ли `pane_current_command` на запущенный Claude Code CLI.
///
/// Claude Code переименовывает свой процесс в строку версии (`2.1.172`),
/// поэтому матчим либо буквальное `claude` (старые/альтернативные установки),
/// либо version-like строку: только ASCII-цифры и точки, минимум одна точка.
/// Команды вида `python3.11` не проходят (содержат буквы), `zsh`/`node` — тоже.
fn is_claude_command(cmd: &str) -> bool {
    if cmd.eq_ignore_ascii_case("claude") {
        return true;
    }
    !cmd.is_empty()
        && cmd.contains('.')
        && cmd.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Выбирает из вывода `list-panes -s` (формат [`LP_FORMAT`]) панель с Claude.
///
/// Возвращает target-суффикс `"win.pane"` (без имени сессии) либо `None`,
/// если ни одна панель не похожа на Claude. При нескольких кандидатах
/// приоритет: активная панель активного окна > активное окно > первая по
/// порядку листинга. Чистая функция — вынесена ради юнит-тестов.
fn pick_claude_pane(list_panes_output: &str) -> Option<String> {
    let mut best: Option<(u8, String)> = None;
    for line in list_panes_output.lines() {
        let mut parts = line.split('|');
        let (Some(target), Some(cmd), Some(win_active), Some(pane_active)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !is_claude_command(cmd) {
            continue;
        }
        let score =
            u8::from(win_active == "1") * 2 + u8::from(win_active == "1" && pane_active == "1");
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, target.to_string()));
        }
    }
    best.map(|(_, target)| target)
}

/// Панель сессии, пригодная для опроса авто-апрувом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTarget {
    /// Target-суффикс `"win.pane"` для `-t <session>:<target>`. Пустая строка
    /// означает активную панель сессии.
    pub target: String,
    /// Имя окна (`#{window_name}`) — то, что видит человек в статусбаре tmux
    /// и в истории апрувов. Может быть пустым, если tmux его не отдал.
    pub window: String,
}

/// Собирает из вывода `list-panes -s` (формат [`LP_FORMAT`]) ВСЕ панели с
/// Claude — в порядке листинга tmux (окно за окном, панель за панелью).
///
/// Отличие от [`pick_claude_pane`], который возвращает ОДНУ «лучшую» панель:
/// авто-апруву нужны все, потому что prompt может висеть в неактивном окне
/// (пользователь работает в другом окне той же сессии), и такой prompt тоже
/// должен подтверждаться.
///
/// Если Claude-панелей не нашлось — возвращается одна запись с ПУСТЫМ
/// `target` (= активная панель сессии, target `"<session>:"`) и именем
/// активного окна, если его удалось прочитать. Это тот же фолбэк, что у
/// [`resolve_target`]: Claude может не опознаться по `pane_current_command`
/// (обёртки, нестандартная установка), и терять сессию целиком нельзя.
///
/// Чистая функция — вынесена ради юнит-тестов.
fn collect_claude_panes(list_panes_output: &str) -> Vec<PaneTarget> {
    let mut out = Vec::new();
    let mut active_window = String::new();
    for line in list_panes_output.lines() {
        let mut parts = line.split('|');
        let (Some(target), Some(cmd), Some(win_active), Some(_pane_active)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let window = parts.next().unwrap_or("").to_string();
        if win_active == "1" && active_window.is_empty() {
            active_window = window.clone();
        }
        if is_claude_command(cmd) {
            out.push(PaneTarget {
                target: target.to_string(),
                window,
            });
        }
    }
    if out.is_empty() {
        out.push(PaneTarget {
            target: String::new(),
            window: active_window,
        });
    }
    out
}

/// Перечисляет панели сессии, в которых имеет смысл искать prompt Claude Code.
///
/// Возвращает список [`PaneTarget`] — суффикс `"win.pane"` для
/// [`capture_pane_target`] / [`send_key_target`] плюс имя окна для истории
/// апрувов. Пустой `target` означает «активная панель сессии» — см.
/// [`collect_claude_panes`].
///
/// Нужна авто-апруву ([`crate::auto_approve`]): [`capture_pane`] смотрит
/// только активную панель активного окна, поэтому prompt в фоновом окне
/// оставался бы незамеченным.
///
/// При исчезнувшей сессии / незапущенном сервере — `Ok(vec![])` (нечего
/// обходить), прочие сбои tmux — `Err`.
pub async fn list_claude_pane_targets(session: &str) -> anyhow::Result<Vec<PaneTarget>> {
    // `:` в конце — session-target, см. `capture_pane`.
    let target = format!("{session}:");
    let output = Command::new("tmux")
        .args(["list-panes", "-s", "-t", &target, "-F", LP_FORMAT])
        .output()
        .await
        .context("failed to spawn `tmux list-panes`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("can't find session") {
            return Ok(Vec::new());
        }
        return Err(anyhow!(
            "tmux list-panes failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    Ok(collect_claude_panes(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Захватывает видимую часть КОНКРЕТНОЙ панели сессии.
///
/// `pane` — суффикс вида `"win.pane"` из [`list_claude_pane_targets`]; пустая
/// строка означает активную панель сессии (target `"<session>:"`).
///
/// В остальном идентична [`capture_pane`] (флаг `-J`, «сессия исчезла» →
/// `Ok("")`), от которой отличается только адресацией: та всегда смотрит
/// активную панель активного окна.
pub async fn capture_pane_target(session: &str, pane: &str) -> anyhow::Result<String> {
    let target = format!("{session}:{pane}");
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-J", "-t", &target])
        .output()
        .await
        .context("failed to spawn `tmux capture-pane` (is tmux installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running")
            || stderr.contains("can't find session")
            || stderr.contains("can't find pane")
            || stderr.contains("can't find window")
        {
            tracing::debug!(
                session = %session,
                pane = %pane,
                "tmux capture-pane: target absent, returning empty pane"
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

/// Ищет в сессии панель, в которой запущен Claude Code CLI.
///
/// Перечисляет ВСЕ панели всех окон сессии (`tmux list-panes -s`) и выбирает
/// ту, чей `pane_current_command` похож на Claude (см. [`is_claude_command`]).
/// Возвращает `Ok(Some("win.pane"))` — суффикс для target `session:win.pane`,
/// `Ok(None)` если Claude-панель не найдена, сессия исчезла или tmux-сервер
/// не запущен (доставку решает caller), `Err` — прочие сбои tmux.
pub async fn find_claude_pane(session: &str) -> anyhow::Result<Option<String>> {
    // `:` в конце — session-target (см. `capture_pane`): критично для числовых
    // имён сессий, иначе tmux резолвит `-t 8` как окно 8 текущей сессии.
    let target = format!("{session}:");
    let output = Command::new("tmux")
        .args(["list-panes", "-s", "-t", &target, "-F", LP_FORMAT])
        .output()
        .await
        .context("failed to spawn `tmux list-panes`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("can't find session") {
            return Ok(None);
        }
        return Err(anyhow!(
            "tmux list-panes failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    Ok(pick_claude_pane(&String::from_utf8_lossy(&output.stdout)))
}

/// Отправляет текст в tmux-сессию и нажимает Enter.
///
/// Используется фоновым notifier'ом для доставки текста промоутнутого
/// TODO в указанную tmux-сессию (см. Phase 2). Реализация:
///
/// 1. Имя сессии валидируется через [`is_valid_session_name`] —
///    запрещены пробелы, `:`, `.` и не-ASCII, чтобы tmux не интерпретировал
///    их как target syntax.
/// 2. Если `text` пуст — возвращаем `Ok(())` без действий.
/// 3. Целевая панель резолвится через [`find_claude_pane`]: текст идёт в
///    панель с запущенным Claude CLI, в каком бы окне сессии она ни была.
///    Раньше target был просто именем сессии — tmux слал в активное окно, и
///    при нескольких окнах текст попадал в шелл вместо Claude (баг
///    forge-v6pw: «доставка работает только при одном окне»). Если
///    Claude-панель не найдена — фолбэк на `session:` (активная панель;
///    `:` обязателен — для числовых имён без него tmux резолвит target как
///    окно чужой сессии, тот же баг, что был у `capture_pane`).
/// 4. Многострочный текст разбивается по символу новой строки. Для каждой строки
///    выполняется `tmux send-keys -t <target> -l -- <line>` (разделитель `--`
///    защищает строки, начинающиеся с `-`, от трактовки как флаг), после неё —
///    отдельный `tmux send-keys -t <target> Enter`. Это эквивалентно
///    набору пользователем строки за строкой и нажатию Enter после
///    каждой.
/// 5. Не запускаем shell — каждый аргумент передаётся отдельно через
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

    let target = resolve_target(session).await;

    for line in text.split('\n') {
        if !line.is_empty() {
            // `--` завершает разбор опций: без него строка, начинающаяся с
            // `-` (например `-t`, `--help`), парсилась бы tmux как флаг даже
            // при наличии `-l`. Литеральный текст идёт строго после `--`.
            let output = Command::new("tmux")
                .args(["send-keys", "-t", &target, "-l", "--", line])
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
            .args(["send-keys", "-t", &target, "Enter"])
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

/// Резолвит tmux-target для доставки ввода в сессию.
///
/// Возвращает `"<session>:<win>.<pane>"` — панель с запущенным Claude CLI
/// (см. [`find_claude_pane`]), либо `"<session>:"` (активная панель) если
/// Claude-панель не найдена или tmux вернул ошибку. Завершающее `:`
/// обязательно: без него tmux резолвит числовое имя сессии как номер окна
/// текущей сессии (тот же баг, что был у [`capture_pane`]).
///
/// Общий шаг [`send_keys`] и [`send_key`] — вынесен, чтобы правило выбора
/// панели было ровно в одном месте.
async fn resolve_target(session: &str) -> String {
    match find_claude_pane(session).await {
        Ok(Some(pane)) => format!("{session}:{pane}"),
        Ok(None) => format!("{session}:"),
        Err(e) => {
            tracing::warn!(
                session = %session,
                error = ?e,
                "find_claude_pane failed; falling back to session active pane"
            );
            format!("{session}:")
        }
    }
}

/// Именованные клавиши tmux, которые отправляются БЕЗ флага `-l`.
///
/// `tmux send-keys Enter` нажимает Enter, а `tmux send-keys -l -- Enter`
/// напечатал бы пять букв «Enter». Всё, чего нет в этом списке, считается
/// литералом и уходит с `-l`.
const NAMED_KEYS: &[&str] = &[
    "Enter", "Escape", "Space", "Tab", "BSpace", "Up", "Down", "Left", "Right", "Home", "End",
    "C-c", "C-d",
];

/// Отправляет ОДНУ клавишу в конкретную панель сессии — без добавления Enter.
///
/// Нужна авто-апруву ([`crate::auto_approve`]): пункт меню Claude Code
/// выбирается нажатием цифры, и лишний Enter после выбора улетел бы уже в
/// поле ввода как пустое сообщение. [`send_keys`] для этого не годится — он
/// по контракту дожимает Enter после каждой строки.
///
/// `pane` — суффикс вида `"win.pane"` из [`list_claude_pane_targets`]; пустая
/// строка означает активную панель сессии. Панель задаёт ВЫЗЫВАЮЩИЙ, а не
/// [`resolve_target`]: авто-апрув обязан нажать ровно в ту панель, в которой
/// увидел prompt ([`capture_pane_target`]), иначе в сессии с несколькими
/// окнами клавиша уходила бы в другое окно.
///
/// - имя сессии валидируется через [`is_valid_session_name`];
/// - `key` из [`NAMED_KEYS`] уходит как именованная клавиша, всё остальное —
///   литералом через `-l --` (разделитель защищает ключи, начинающиеся с `-`);
/// - пустой `key` → `Ok(())` без действий.
pub async fn send_key_target(session: &str, pane: &str, key: &str) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    if key.is_empty() {
        return Ok(());
    }
    send_key_to(&format!("{session}:{pane}"), key).await
}

/// Общая реализация отправки одной клавиши в готовый tmux-target.
///
/// `key` из [`NAMED_KEYS`] уходит как именованная клавиша, всё остальное —
/// литералом через `-l --`.
async fn send_key_to(target: &str, key: &str) -> anyhow::Result<()> {
    let output = if NAMED_KEYS.contains(&key) {
        Command::new("tmux")
            .args(["send-keys", "-t", target, key])
            .output()
            .await
            .context("failed to spawn `tmux send-keys <named>`")?
    } else {
        Command::new("tmux")
            .args(["send-keys", "-t", target, "-l", "--", key])
            .output()
            .await
            .context("failed to spawn `tmux send-keys -l`")?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux send-keys failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
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

/// Переименовывает существующую сессию (`tmux rename-session -t <old> <new>`).
///
/// Оба имени валидируются через [`is_valid_session_name`]. Если сессии с
/// именем `old` нет, либо `new` уже занято — tmux вернёт ненулевой exit,
/// который мапится в `Err`.
pub async fn rename_session(old: &str, new: &str) -> anyhow::Result<()> {
    if !is_valid_session_name(old) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            old
        );
    }
    if !is_valid_session_name(new) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            new
        );
    }
    if old == new {
        return Ok(());
    }

    let output = Command::new("tmux")
        .args(["rename-session", "-t", old, new])
        .output()
        .await
        .context("failed to spawn `tmux rename-session`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux rename-session failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Метаданные одного окна tmux внутри сессии.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowInfo {
    /// Индекс окна в сессии (`#{window_index}`). tmux нумерует с base-index
    /// (по умолчанию 0). Используется как target после `:`.
    pub index: u32,
    /// Имя окна (`#{window_name}`).
    pub name: String,
    /// `true` если это активное окно сессии (`#{window_active}` = `1`).
    pub active: bool,
    /// Количество панелей в окне (`#{window_panes}`).
    pub panes: u32,
}

const LW_FORMAT: &str = "#{window_index}|#{window_name}|#{window_active}|#{window_panes}";

/// Перечисляет окна одной tmux-сессии (`tmux list-windows -t <session> -F ...`).
///
/// Возвращает `Err`, если сессии нет или tmux-сервер не запущен — это
/// сознательное решение: вызывающая сторона запрашивает окна конкретной
/// сессии, и отсутствие сессии — это ошибка, а не «пустой список».
pub async fn list_windows(session: &str) -> anyhow::Result<Vec<WindowInfo>> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }

    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", LW_FORMAT])
        .output()
        .await
        .context("failed to spawn `tmux list-windows`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux list-windows failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut wins = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        match parse_window_line(line) {
            Some(w) => wins.push(w),
            None => tracing::warn!(line = %line, "skipping malformed tmux list-windows line"),
        }
    }
    Ok(wins)
}

/// Парсит одну строку формата `index|name|active|panes`.
fn parse_window_line(line: &str) -> Option<WindowInfo> {
    let mut parts = line.splitn(4, '|');
    let index = parts.next()?.parse().ok()?;
    let name = parts.next()?.to_string();
    let active = matches!(parts.next()?, "1");
    let panes = parts.next()?.parse().ok()?;
    Some(WindowInfo { index, name, active, panes })
}

/// Создаёт новое окно в существующей сессии (`tmux new-window -t <session>`).
///
/// По умолчанию tmux назначает следующий свободный индекс и сразу делает
/// окно активным. Если передано непустое `name` — окно создаётся с `-n <name>`.
pub async fn new_window(session: &str, name: Option<&str>) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }

    // Двоеточие в конце таргета указывает tmux, что это session-target
    // (а не target-window). Без двоеточия tmux резолвит `-t <session>`
    // в текущее активное окно этой сессии и пытается создать новое
    // окно ровно по тому же индексу — что приводит к ошибке
    // "create window failed: index N in use".
    let target = format!("{session}:");
    // `-c "#{pane_current_path}"` — новое окно открывается в той же рабочей
    // директории, что и текущая активная панель сессии. Без этого флага
    // tmux использует cwd самого сервера (как правило — $HOME), и новое
    // окно «теряет» путь, в котором была создана сессия.
    let mut args: Vec<&str> = vec![
        "new-window",
        "-t",
        &target,
        "-c",
        "#{pane_current_path}",
    ];
    if let Some(n) = name {
        if !n.is_empty() {
            args.push("-n");
            args.push(n);
        }
    }

    let output = Command::new("tmux")
        .args(&args)
        .output()
        .await
        .context("failed to spawn `tmux new-window`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux new-window failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Создаёт окно по ЯВНОМУ индексу (`tmux new-window -t <session>:<index>`).
///
/// Используется при восстановлении сессии из истории, чтобы сохранить
/// оригинальные (возможно разрежённые) индексы окон — например `0, 2, 5`.
/// Обычный [`new_window`] назначил бы следующий свободный индекс и потерял бы
/// исходную раскладку.
///
/// Требует, чтобы целевой индекс был свободен (иначе tmux вернёт
/// "index N in use"). Caller обязан это гарантировать.
pub async fn new_window_at(
    session: &str,
    index: u32,
    name: Option<&str>,
) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    let target = format!("{session}:{index}");
    let mut args: Vec<&str> = vec![
        "new-window",
        "-t",
        &target,
        "-c",
        "#{pane_current_path}",
    ];
    if let Some(n) = name {
        if !n.is_empty() {
            args.push("-n");
            args.push(n);
        }
    }

    let output = Command::new("tmux")
        .args(&args)
        .output()
        .await
        .context("failed to spawn `tmux new-window` (explicit index)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux new-window -t {target} failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Перемещает окно на новый индекс (`tmux move-window -s <src> -t <dst>`).
/// Используется при восстановлении, когда авто-созданное окно 0 нужно сдвинуть
/// на исторический индекс (если 0 отсутствует в истории).
pub async fn move_window(session: &str, from: u32, to: u32) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    let src = format!("{session}:{from}");
    let dst = format!("{session}:{to}");
    let output = Command::new("tmux")
        .args(["move-window", "-s", &src, "-t", &dst])
        .output()
        .await
        .context("failed to spawn `tmux move-window`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux move-window {src}->{dst} failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Делает указанное окно активным (`tmux select-window -t <session>:<index>`).
///
/// Все прикреплённые клиенты автоматически переключатся — в том числе наш
/// WS-attach (он использует `tmux attach -t <session>` и следует за активным
/// окном сессии).
pub async fn select_window(session: &str, index: u32) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    let target = format!("{session}:{index}");
    let output = Command::new("tmux")
        .args(["select-window", "-t", &target])
        .output()
        .await
        .context("failed to spawn `tmux select-window`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux select-window failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Создаёт новое окно в заданной рабочей директории
/// (`tmux new-window -t <session>: -c <cwd>`).
///
/// Аналог [`new_window`], но вместо `-c "#{pane_current_path}"` (cwd активной
/// панели) новое окно открывается в ЯВНО переданном `cwd`. Используется фичей
/// «Новое окно в git worktree»: окно создаётся в каталоге свежесозданной
/// рабочей копии (`<repo>/.forge-worktrees/<имя>/`), а не там, где находится
/// текущая панель сессии.
///
/// Как и [`new_window`], таргет формируется как `<session>:` (с двоеточием в
/// конце), чтобы tmux трактовал `-t` как session-target и назначил следующий
/// свободный индекс окна, а не пытался пересоздать текущее.
///
/// # Параметры
/// - `session` — имя сессии; валидируется [`is_valid_session_name`].
/// - `name` — имя окна: при `Some(непустое)` добавляется `-n <name>`; при
///   `None` или пустой строке имя не задаётся (tmux назовёт окно сам).
/// - `cwd` — рабочая директория нового окна (передаётся в `-c <cwd>`).
///
/// # Возврат
/// `Ok(())` при успехе; `Err` при невалидном имени сессии (без spawn) или при
/// ненулевом exit `tmux` (с обрезанным stderr).
pub async fn new_window_in(session: &str, name: Option<&str>, cwd: &str) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }

    // Двоеточие в конце — session-target (см. пояснение в `new_window`),
    // чтобы tmux назначил следующий свободный индекс, а не пересоздавал окно.
    let target = format!("{session}:");
    let mut args: Vec<&str> = vec!["new-window", "-t", &target, "-c", cwd];
    if let Some(n) = name {
        if !n.is_empty() {
            args.push("-n");
            args.push(n);
        }
    }

    let output = Command::new("tmux")
        .args(&args)
        .output()
        .await
        .context("failed to spawn `tmux new-window` (explicit cwd)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux new-window failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Возвращает cwd текущей активной панели сессии
/// (`tmux display-message -p -t "<session>:" -F "#{pane_current_path}"`).
///
/// Таргет формируется как `<session>:` (двоеточие в конце) — session-target,
/// который tmux резолвит в активное окно/панель сессии. Двоеточие ОБЯЗАТЕЛЬНО:
/// для сессий с числовыми именами (`0`, `1`, …) таргет без `:` tmux истолковал
/// бы как индекс окна и захватил бы чужое окно (см. регресс с числовыми
/// именами сессий).
///
/// # Параметры
/// - `session` — имя сессии; валидируется [`is_valid_session_name`].
///
/// # Возврат
/// `Ok(String)` — обрезанный (без `\n`) абсолютный путь. `Err` при невалидном
/// имени сессии (без spawn), ненулевом exit `tmux`, или если путь пуст.
pub async fn session_cwd(session: &str) -> anyhow::Result<String> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    let target = format!("{session}:");
    display_pane_current_path(&target).await
}

/// Возвращает cwd панели конкретного окна сессии
/// (`tmux display-message -p -t "<session>:<index>" -F "#{pane_current_path}"`).
///
/// Аналог [`session_cwd`], но таргет указывает конкретное окно по индексу:
/// `<session>:<index>`. Используется, чтобы узнать рабочую директорию окна
/// (например, worktree-окна) — в частности, при удалении рабочей копии, чтобы
/// понять, какой каталог `.forge-worktrees/<имя>` за окном закреплён.
///
/// # Параметры
/// - `session` — имя сессии; валидируется [`is_valid_session_name`].
/// - `index` — индекс окна в сессии.
///
/// # Возврат
/// `Ok(String)` — обрезанный абсолютный путь. `Err` при невалидном имени
/// сессии (без spawn), ненулевом exit `tmux`, или если путь пуст.
pub async fn window_cwd(session: &str, index: u32) -> anyhow::Result<String> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    let target = format!("{session}:{index}");
    display_pane_current_path(&target).await
}

/// Общий хелпер: `tmux display-message -p -t <target> -F "#{pane_current_path}"`.
///
/// Спавнит tmux, проверяет `status.success()` и возвращает обрезанный stdout.
/// Пустой результат (окно без панели / неожиданный вывод) трактуется как
/// ошибка. Выделен из [`session_cwd`]/[`window_cwd`], чтобы не дублировать
/// вызов и обработку ошибок — target формирует вызывающая функция.
async fn display_pane_current_path(target: &str) -> anyhow::Result<String> {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            target,
            "-F",
            "#{pane_current_path}",
        ])
        .output()
        .await
        .context("failed to spawn `tmux display-message`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux display-message failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = stdout.trim().to_string();
    if path.is_empty() {
        return Err(anyhow!(
            "tmux display-message returned empty pane_current_path for target `{}`",
            target
        ));
    }
    Ok(path)
}

/// Убивает указанное окно (`tmux kill-window -t <session>:<index>`).
///
/// Если это было последнее окно сессии — tmux убьёт и саму сессию.
pub async fn kill_window(session: &str, index: u32) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    let target = format!("{session}:{index}");
    let output = Command::new("tmux")
        .args(["kill-window", "-t", &target])
        .output()
        .await
        .context("failed to spawn `tmux kill-window`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux kill-window failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Переименовывает указанное окно (`tmux rename-window -t <session>:<index> <name>`).
pub async fn rename_window(session: &str, index: u32, name: &str) -> anyhow::Result<()> {
    if !is_valid_session_name(session) {
        bail!(
            "invalid session name `{}` (allowed: [A-Za-z0-9_-]+, non-empty)",
            session
        );
    }
    if name.is_empty() {
        bail!("window name must not be empty");
    }
    let target = format!("{session}:{index}");
    let output = Command::new("tmux")
        .args(["rename-window", "-t", &target, name])
        .output()
        .await
        .context("failed to spawn `tmux rename-window`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux rename-window failed (exit {:?}): {}",
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
    fn is_claude_command_matches_version_string() {
        // Claude Code переименовывает процесс в строку версии.
        assert!(is_claude_command("2.1.172"));
        assert!(is_claude_command("2.1.170"));
        assert!(is_claude_command("10.0"));
    }

    #[test]
    fn is_claude_command_matches_literal_claude() {
        assert!(is_claude_command("claude"));
        assert!(is_claude_command("Claude"));
    }

    #[test]
    fn is_claude_command_rejects_other_commands() {
        assert!(!is_claude_command("zsh"));
        assert!(!is_claude_command("node"));
        assert!(!is_claude_command("python3.11")); // буквы + точка — не версия
        assert!(!is_claude_command("123")); // цифры без точки — не версия
        assert!(!is_claude_command(""));
    }

    #[test]
    fn pick_claude_pane_finds_claude_in_inactive_window() {
        // Главный кейс forge-v6pw: Claude в НЕактивном окне (активно окно с zsh).
        let out = "0.0|zsh|1|1\n1.0|2.1.170|0|1\n";
        assert_eq!(pick_claude_pane(out).as_deref(), Some("1.0"));
    }

    #[test]
    fn pick_claude_pane_prefers_active_window() {
        // Два Claude — выбираем тот, что в активном окне.
        let out = "0.0|2.1.170|0|1\n1.0|2.1.172|1|1\n";
        assert_eq!(pick_claude_pane(out).as_deref(), Some("1.0"));
    }

    #[test]
    fn pick_claude_pane_prefers_active_pane_within_active_window() {
        // Сплит в активном окне: zsh-панель активна, Claude — нет, но из двух
        // Claude-панелей выигрывает та, что активна в своём окне.
        let out = "0.0|2.1.170|1|0\n0.1|zsh|1|1\n1.0|2.1.170|0|1\n";
        assert_eq!(pick_claude_pane(out).as_deref(), Some("0.0"));
    }

    #[test]
    fn pick_claude_pane_none_when_no_claude() {
        let out = "0.0|zsh|1|1\n1.0|vim|0|1\n";
        assert_eq!(pick_claude_pane(out), None);
        assert_eq!(pick_claude_pane(""), None);
    }

    #[test]
    fn pick_claude_pane_skips_malformed_lines() {
        let out = "garbage\n1.0|2.1.170|1|1\n";
        assert_eq!(pick_claude_pane(out).as_deref(), Some("1.0"));
    }

    /// Компактный конструктор ожидаемого [`PaneTarget`] для тестов.
    fn pt(target: &str, window: &str) -> PaneTarget {
        PaneTarget {
            target: target.to_string(),
            window: window.to_string(),
        }
    }

    #[test]
    fn collect_claude_panes_returns_every_window() {
        // Ключевой кейс авто-апрува: Claude в активном И в фоновом окне —
        // обойти нужно оба, а не только активное.
        let out = "0.0|2.1.170|1|1|main\n1.0|zsh|0|1|shell\n2.0|2.1.172|0|1|agent\n";
        assert_eq!(
            collect_claude_panes(out),
            vec![pt("0.0", "main"), pt("2.0", "agent")]
        );
    }

    #[test]
    fn collect_claude_panes_includes_splits_in_same_window() {
        let out = "0.0|2.1.170|1|1|main\n0.1|2.1.170|1|0|main\n";
        assert_eq!(
            collect_claude_panes(out),
            vec![pt("0.0", "main"), pt("0.1", "main")]
        );
    }

    #[test]
    fn collect_claude_panes_falls_back_to_active_pane_with_window_name() {
        // Claude не опознан по имени команды — не теряем сессию целиком, а
        // смотрим активную панель (пустой суффикс = `<session>:`) и всё же
        // подставляем имя АКТИВНОГО окна, чтобы история осталась читаемой.
        let out = "0.0|zsh|1|1|editor\n1.0|vim|0|1|logs\n";
        assert_eq!(collect_claude_panes(out), vec![pt("", "editor")]);
        assert_eq!(collect_claude_panes(""), vec![pt("", "")]);
    }

    #[test]
    fn collect_claude_panes_skips_malformed_lines() {
        let out = "garbage\n1.0|2.1.170|1|1|main\n";
        assert_eq!(collect_claude_panes(out), vec![pt("1.0", "main")]);
    }

    #[test]
    fn collect_claude_panes_tolerates_missing_window_name() {
        // Старый формат из пяти полей (без #{window_name}) не должен ломать
        // разбор — имя окна просто пустое.
        let out = "1.0|2.1.170|1|1\n";
        assert_eq!(collect_claude_panes(out), vec![pt("1.0", "")]);
    }

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

    #[tokio::test]
    async fn capture_pane_full_rejects_negative_lines() {
        let err = capture_pane_full("any", -1).await.unwrap_err();
        assert!(err.to_string().contains("lines must be >= 0"));
    }

    #[tokio::test]
    async fn capture_pane_full_missing_session_returns_empty() {
        // Нет сессии или нет tmux-сервера → Ok("") — это контракт для
        // prompt-builder'а (см. doc-комментарий функции).
        let out = capture_pane_full("__definitely_missing_echo_test__", 50).await;
        match out {
            Ok(s) => assert!(s.is_empty(), "expected empty pane, got {s:?}"),
            // tmux может полностью отсутствовать в test-окружении CI —
            // допускаем оба варианта, лишь бы не паниковали.
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("failed to spawn") || msg.contains("tmux"),
                    "unexpected error: {msg}"
                );
            }
        }
    }

    #[tokio::test]
    async fn capture_pane_full_clamps_large_lines() {
        // Просто проверяем что функция не паникует и не виснет на огромном
        // запросе — clamp защищает от OOM. Сессии нет → должно вернуть "" /
        // понятный Err про отсутствующую сессию или отсутствующий tmux.
        let out = capture_pane_full("__missing_clamp_test__", 1_000_000).await;
        match out {
            Ok(s) => assert!(s.is_empty()),
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("failed to spawn") || msg.contains("tmux"));
            }
        }
    }
}
