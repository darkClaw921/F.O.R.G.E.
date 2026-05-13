//! Backend для Tasks-таба: passthrough вызов CLI `br` из beads_rust.
//!
//! ### Назначение
//!
//! Read-only прокси `GET /api/tasks` поверх `br list --json --all --limit 0`.
//! Stdout `br` уже отдаёт JSON в формате
//! `{issues: [...], total, limit, offset, has_more}` — мы парсим его в
//! `serde_json::Value` и возвращаем «как есть» во фронтенд.
//!
//! ### Почему через CLI, а не SQLite напрямую
//!
//! Beads хранит данные в `.beads/beads.db` (sqlite + WAL). Прямой доступ
//! к БД параллельно с `br` потребовал бы либо подсадить sqlite-крейт и
//! дублировать схему, либо подключаться в read-only WAL-режиме — это
//! лишние зависимости и риск рассинхронизации схемы при апгрейде `br`.
//! Поэтому Phase 6.A намеренно идёт самым простым путём: spawn'им `br`
//! как subprocess через `tokio::process::Command` (не блокирующий runtime,
//! см. паттерн `tmux::list_sessions`).
//!
//! ### Отказы
//!
//! - `br` не найден в PATH → ошибка spawn → `Err`.
//! - non-zero exit (битая БД, неподдерживаемые флаги, нет `.beads/`) →
//!   `Err` со stderr внутри (хендлер маппит в 500).
//! - stdout не парсится как JSON → `Err`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context};
use serde::Serialize;
use tokio::process::Command;

/// Автодетект какой CLI использовать в данном проекте: `bd` (go-beads) или `br` (beads_rust).
///
/// Если в `<cwd>/.beads/` есть unix-socket `bd.sock` — это маркер живого
/// go-beads daemon, используем `bd` чтобы не конфликтовать с его write-lock.
/// Иначе fallback на `br` (поведение по умолчанию). Сокет создаётся при
/// старте `bd daemon --start` и удаляется при graceful shutdown, поэтому
/// его наличие достаточно надёжный сигнал — даже если daemon упал, обращение
/// через `bd` CLI отработает (с фоновой ошибкой соединения), а `br` тут
/// заблокировался бы на write-lock на 30с.
fn detect_cli(cwd: &Path) -> &'static str {
    if cwd.join(".beads").join("bd.sock").exists() {
        "bd"
    } else {
        "br"
    }
}

/// Возвращает полный snapshot задач из `br list` в виде
/// `{issues, total, limit, offset, has_more}`.
///
/// `--limit 0` означает «все задачи без отсечки», `--all` подтягивает
/// также закрытые. Этот endpoint предназначен для kanban-board, поэтому
/// нам нужен полный набор статусов.
///
/// # Параметры
/// - `project_root` — рабочая директория для `br`. Должна содержать
///   `.beads/` (или родительский каталог с ним) — `br` сам поднимется
///   до корня репозитория.
pub async fn list_tasks(project_root: &Path) -> anyhow::Result<serde_json::Value> {
    let cli = detect_cli(project_root);
    let output = Command::new(cli)
        .args(["list", "--json", "--all", "--limit", "0"])
        .current_dir(project_root)
        .output()
        .await
        .with_context(|| format!("failed to spawn `{cli} list` (is beads installed?)"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "{cli} list failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("{cli} list returned non-JSON stdout"))?;
    Ok(value)
}

/// Универсальный хелпер: запускает `br <args>` в указанной cwd, парсит stdout
/// как JSON и возвращает [`serde_json::Value`]. Используется всеми write-эндпоинтами
/// CRUD (create/update/close/reopen) — каждая операция строит свой массив `args`
/// с уже добавленным `--json` и передаёт сюда.
///
/// # Поведение
/// - При non-zero exit `br` — возвращает `Err(anyhow!(...))` с включённым
///   stderr (хендлер маппит в 500/400).
/// - При пустом stdout — возвращает `serde_json::Value::Null` (некоторые
///   подкоманды `br` могут не выдавать вывод даже с `--json`, например при
///   тихих режимах).
/// - При непарсимом stdout — `Err` с контекстом первых ~200 байт ответа.
///
/// # Параметры
/// - `args` — slice CLI-аргументов после `br` (например `["create", "--json", "--title", "..."]`).
/// - `cwd` — рабочая директория, где `br` найдёт `.beads/`.
pub async fn run_br(args: &[&str], cwd: &Path) -> anyhow::Result<serde_json::Value> {
    let cli = detect_cli(cwd);
    let output = Command::new(cli)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .with_context(|| format!("failed to spawn `{cli} {}`", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "{cli} {} failed (exit {:?}): {}",
            args.join(" "),
            output.status.code(),
            stderr.trim()
        ));
    }

    if output.stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }

    serde_json::from_slice::<serde_json::Value>(&output.stdout).with_context(|| {
        let preview = String::from_utf8_lossy(&output.stdout);
        let snippet: String = preview.chars().take(200).collect();
        format!("{cli} {} returned non-JSON stdout: {}", args.join(" "), snippet)
    })
}

// =============================================================================
// Phase 6.D — Realtime watcher: snapshot + diff
// =============================================================================

/// Событие изменения отдельной задачи в beads. Сериализуется как
/// `{"kind":"upsert","issue":{...}}` или `{"kind":"removed","id":"..."}`.
///
/// Используется фоновым watcher'ом и WS-handler'ом `/ws/tasks`: после каждого
/// диффа `diff_issues(prev, next)` событие бродкастится подписчикам, JS-клиент
/// применяет его in-place к своему `state.tasksData`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TaskEvent {
    /// Issue появилась или изменилась. `issue` — тот же объект, что лежит
    /// внутри массива `issues` ответа `br list --json`.
    Upsert { issue: serde_json::Value },
    /// Issue полностью пропала из `br list --json --all` (физически удалена
    /// из БД — `br close` НЕ генерирует Removed, только Upsert со статусом
    /// `closed`). На практике `br` редко удаляет issues, но мы обрабатываем
    /// этот случай для полноты.
    Removed { id: String },
}

/// Снимок задач активного проекта в виде map'ы id → issue-JSON.
///
/// Внутри вызывает [`list_tasks`] и распаковывает массив `issues` из ответа
/// `br list --json --all --limit 0` в HashMap по полю `id`. Issues без `id`
/// (теоретически быть не должно) игнорируются с предупреждением в лог.
///
/// # Применение
///
/// Watcher держит prev-snapshot, после каждого debounce-burst'а берёт новый
/// и считает [`diff_issues`]. Также при первом старте watcher делает
/// initial snapshot и НЕ бродкастит — используется только как baseline для
/// последующих диффов.
pub async fn snapshot(cwd: &Path) -> anyhow::Result<HashMap<String, serde_json::Value>> {
    let value = list_tasks(cwd).await?;
    let mut map: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(arr) = value.get("issues").and_then(|v| v.as_array()) {
        for issue in arr {
            if let Some(id) = issue.get("id").and_then(|v| v.as_str()) {
                map.insert(id.to_string(), issue.clone());
            } else {
                tracing::warn!(?issue, "snapshot: issue without id field — skipped");
            }
        }
    } else {
        tracing::warn!(
            "snapshot: br list returned no `issues` array (got: {})",
            value
        );
    }
    Ok(map)
}

/// Diff между предыдущим и новым snapshot'ом. Возвращает список событий,
/// которые надо отправить подписчикам.
///
/// Семантика:
/// - id присутствует в new, но не в old → `Upsert(new[id])`.
/// - id есть в обоих, но JSON-объекты не равны → `Upsert(new[id])`.
/// - id есть в old, но не в new → `Removed(id)`.
/// - id есть в обоих и объекты равны → событий не порождается.
///
/// Сравнение глубокое (`PartialEq` на `serde_json::Value`), что включает
/// `updated_at`, `status`, `priority`, `labels` и прочие поля. Beads сам
/// обновляет `updated_at` при любом изменении, так что это надёжный признак
/// фактической мутации issue.
pub fn diff_issues(
    old: &HashMap<String, serde_json::Value>,
    new: &HashMap<String, serde_json::Value>,
) -> Vec<TaskEvent> {
    let mut events = Vec::new();

    // Upserts: новые или изменённые.
    for (id, issue) in new {
        match old.get(id) {
            None => events.push(TaskEvent::Upsert { issue: issue.clone() }),
            Some(prev) if prev != issue => {
                events.push(TaskEvent::Upsert { issue: issue.clone() })
            }
            _ => {}
        }
    }

    // Removed: были в old, но исчезли в new.
    for id in old.keys() {
        if !new.contains_key(id) {
            events.push(TaskEvent::Removed { id: id.clone() });
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(id: &str, status: &str, updated: &str) -> serde_json::Value {
        json!({
            "id": id,
            "status": status,
            "updated_at": updated,
        })
    }

    #[test]
    fn diff_empty() {
        let old: HashMap<String, serde_json::Value> = HashMap::new();
        let new: HashMap<String, serde_json::Value> = HashMap::new();
        assert!(diff_issues(&old, &new).is_empty());
    }

    #[test]
    fn diff_added() {
        let old: HashMap<String, serde_json::Value> = HashMap::new();
        let mut new: HashMap<String, serde_json::Value> = HashMap::new();
        new.insert("a-1".into(), issue("a-1", "open", "t1"));
        let events = diff_issues(&old, &new);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TaskEvent::Upsert { issue } => {
                assert_eq!(issue["id"], "a-1");
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
    }

    #[test]
    fn diff_changed() {
        let mut old: HashMap<String, serde_json::Value> = HashMap::new();
        old.insert("a-1".into(), issue("a-1", "open", "t1"));
        let mut new: HashMap<String, serde_json::Value> = HashMap::new();
        new.insert("a-1".into(), issue("a-1", "in_progress", "t2"));
        let events = diff_issues(&old, &new);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TaskEvent::Upsert { issue } => {
                assert_eq!(issue["status"], "in_progress");
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
    }

    #[test]
    fn diff_unchanged() {
        let mut old: HashMap<String, serde_json::Value> = HashMap::new();
        old.insert("a-1".into(), issue("a-1", "open", "t1"));
        let mut new: HashMap<String, serde_json::Value> = HashMap::new();
        new.insert("a-1".into(), issue("a-1", "open", "t1"));
        assert!(diff_issues(&old, &new).is_empty());
    }

    #[test]
    fn diff_removed() {
        let mut old: HashMap<String, serde_json::Value> = HashMap::new();
        old.insert("a-1".into(), issue("a-1", "open", "t1"));
        let new: HashMap<String, serde_json::Value> = HashMap::new();
        let events = diff_issues(&old, &new);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TaskEvent::Removed { id } => assert_eq!(id, "a-1"),
            other => panic!("expected Removed, got {other:?}"),
        }
    }

    #[test]
    fn task_event_serialization() {
        let upsert = TaskEvent::Upsert { issue: json!({"id":"a-1","status":"open"}) };
        let s = serde_json::to_string(&upsert).unwrap();
        assert!(s.contains("\"kind\":\"upsert\""));
        assert!(s.contains("\"issue\""));

        let removed = TaskEvent::Removed { id: "a-1".into() };
        let s = serde_json::to_string(&removed).unwrap();
        assert!(s.contains("\"kind\":\"removed\""));
        assert!(s.contains("\"id\":\"a-1\""));
    }
}
