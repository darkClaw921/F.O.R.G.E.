//! auto_promote — in-memory состояние цепочки авто-промоута TODO-карточек.
//!
//! # Назначение
//!
//! Модуль обслуживает фичу «авто-промоут TODO по очереди»: пользователь
//! помечает TODO-карточку флагом `auto_promote` (см. `todos::Todo`), и после
//! закрытия текущей задачи цепочки фоновый воркер автоматически промоутит
//! следующую верхнюю TODO с этим флагом — без ручного нажатия «promote».
//!
//! Этот файл — подготовительная часть фичи (типы + состояние). Он сознательно
//! вынесен ДО рефакторинга `promote_todo_core` (Фаза 3), потому что
//! `promote_todo_core` будет писать в [`AutoChainMap`] (фиксировать «голову»
//! цепочки), а значит тип и поле состояния обязаны существовать раньше — иначе
//! возникает цикл зависимостей между задачами рефакторинга.
//!
//! # Состояние цепочки
//!
//! Состояние — это отображение `root_path -> [`AutoChainEntry`]` (см.
//! [`AutoChainMap`]). Ключ — строковый путь корня (`paths::resolve_root` от cwd
//! сессии), потому что TODO привязаны к корню, а не к проекту (концепция Project
//! удалена, см. `remove-projects-concept.md`). Для каждого корня хранится одна
//! «голова» цепочки — последняя промоутнутая задача и сессия для уведомлений.
//!
//! Состояние держится **только в памяти** (`Arc<RwLock<HashMap<..>>>`): persist
//! на диск не делается — это осознанное MVP-решение. При рестарте процесса
//! цепочка обнуляется; self-heal происходит при следующем ручном `promote`
//! (который заново запишет «голову» цепочки). Терять при рестарте здесь нечего
//! критичного: незавершённые TODO остаются в `todos.json`, и пользователь
//! просто промоутит верхнюю вручную.
//!
//! # Concurrency
//!
//! [`AutoChainMap`] — `Arc<RwLock<HashMap<..>>>`: дёшево клонируется (Arc
//! внутри), кладётся в `AppState.auto_chain` и шарится между HTTP-handler'ами
//! (`promote_todo_core` берёт write-lock при фиксации головы) и фоновым
//! воркером (Фаза 4b: `run`, который читает голову, ждёт закрытия задачи и
//! промоутит следующую). RwLock допускает параллельные чтения и
//! сериализованные записи — узких мест нет, мутации редки (раз на промоут).
//!
//! # Воркер цепочки
//!
//! [`run`] — фоновый воркер: слушает broadcast [`crate::tasks::TaskEvent`] и на
//! каждое закрытие задачи (`Upsert{status=="closed"}`) проверяет, не была ли она
//! «головой» цепочки. Если да — извлекает и СРАЗУ удаляет голову под одним
//! write-lock'ом (анти-гонка до `await` на `br create`), выбирает верхнюю
//! TODO-карточку корня через [`pick_top`] и, если у неё стоит флаг
//! `auto_promote`, промоутит её через [`crate::promote_todo_core`] (mode
//! `Immediate`, сессия протягивается по цепочке). Иначе — барьер: цепочка тихо
//! останавливается. Spawn'ится в `main.rs` рядом с `tasks_watcher` (subscribe
//! ДО move'а `tasks_tx` в watcher).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tokio::sync::broadcast;

use crate::tasks::TaskEvent;
use crate::todos::Todo;
use crate::AppState;

/// Запись о текущей «голове» цепочки авто-промоута для одного `root_path`.
///
/// - `active_task_id` — bd-id последней промоутнутой задачи цепочки (ручной или
///   авто). При её закрытии воркер (Фаза 4b) промоутит следующую верхнюю TODO с
///   флагом `auto_promote`.
/// - `session` — tmux-сессия, в которую слать уведомление о новой задаче. Она
///   протягивается по цепочке (наследуется от ручного промоута, запустившего
///   цепочку). `None` означает фолбэк на `cfg.session` из
///   `notifier_config::NotifierConfig`.
// Поля пишутся в promote_todo_core (голова цепочки) и читаются в auto_promote::run.
#[derive(Debug, Clone)]
pub struct AutoChainEntry {
    pub active_task_id: String,
    pub session: Option<String>,
}

/// Отображение `root_path -> [`AutoChainEntry`]` — состояние всех активных
/// цепочек авто-промоута. In-memory: persist намеренно не делаем (MVP, self-heal
/// через ручной `promote`, см. doc-комментарий модуля). Cheap-clonable
/// (`Arc<RwLock<..>>`), живёт в `AppState.auto_chain`.
pub type AutoChainMap = Arc<RwLock<HashMap<String, AutoChainEntry>>>;

/// Выбирает «верхнюю» карточку канбана из списка TODO одного корня — ту, что
/// окажется первой в колонке UI после сортировки [`compareIssues`] (см.
/// `static/js/tasks/render.js`). Чистая функция (без async/IO), вынесена ради
/// тестируемости барьерной логики воркера.
///
/// Порядок сортировки повторяет фронтенд один-в-один:
/// 1. `priority` ASC (`u8`, меньше = выше приоритет, идёт первым);
/// 2. при равном `priority` — `updated_at` DESC. Строки `updated_at` — RFC3339,
///    сравниваются лексикографически (новее = лексикографически больше), поэтому
///    DESC означает «больший `updated_at` первым».
///
/// Возвращает `Some(top)` — клон верхней карточки, либо `None`, если список пуст.
/// Фильтрации по `auto_promote` здесь НЕТ: барьер цепочки (стоп, если у верхней
/// карточки `auto_promote == false`) проверяется в [`run`]/`handle_closed` уже
/// после выбора top — это сознательно, чтобы цепочка реагировала именно на
/// текущую верхнюю карточку, а не «перепрыгивала» через неё к следующей флагнутой.
pub fn pick_top(mut todos: Vec<Todo>) -> Option<Todo> {
    todos.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            // updated_at DESC: больший (новее) первым -> b vs a.
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    todos.into_iter().next()
}

/// Фоновый воркер цепочки авто-промоута.
///
/// # Назначение
///
/// Слушает поток [`TaskEvent`] (тот же broadcast, что питает `tasks_watcher` и
/// `notifier`) и на КАЖДОЕ закрытие задачи (`Upsert{status=="closed"}`) проверяет,
/// не была ли эта задача «головой» какой-либо активной цепочки авто-промоута. Если
/// да — промоутит следующую верхнюю TODO-карточку этого корня (при условии, что у
/// неё стоит флаг `auto_promote`). Так реализуется «очередь»: задачи цепочки
/// запускаются последовательно, каждая после закрытия предыдущей.
///
/// # Параметры
///
/// - `state` — владелец [`AppState`] (cheap-clone, Arc-поля). Воркер живёт всю
///   жизнь процесса, поэтому берёт `state` по значению (а не `&`).
/// - `tasks_rx` — приёмник broadcast'а [`TaskEvent`]. Subscribe ОБЯЗАН быть сделан
///   в `main.rs` ДО передачи `tasks_tx` в `tasks_watcher::run_watcher` (там sender
///   move'ится), иначе ранние события потеряются.
///
/// # Логика цикла
///
/// На каждой итерации `tasks_rx.recv().await`:
/// - `Ok(Upsert{issue})` — извлекаем `id`/`status` (как `notifier`). Если
///   `status != "closed"` → `continue`. Иначе → [`handle_closed`].
/// - `Ok(Removed{..})` → `continue` (физическое удаление цепочку не двигает).
/// - `Err(Lagged(n))` → `warn` + `continue` (broadcast переполнился; пропущенные
///   closed-события «само-лечатся» при следующем ручном промоуте).
/// - `Err(Closed)` → `info` + `return` (sender дропнут — процесс завершается).
///
/// # Связанные элементы
///
/// [`handle_closed`], [`pick_top`], [`crate::promote_todo_core`], [`AutoChainMap`],
/// [`AutoChainEntry`], [`crate::tasks::TaskEvent`], [`crate::notifier::NotifyMode`].
pub async fn run(state: AppState, mut tasks_rx: broadcast::Receiver<TaskEvent>) {
    loop {
        match tasks_rx.recv().await {
            Ok(TaskEvent::Upsert { issue }) => {
                let id_opt = issue
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let status_opt = issue
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let (Some(task_id), Some(status)) = (id_opt, status_opt) {
                    if status == "closed" {
                        handle_closed(&state, &task_id).await;
                    }
                }
            }
            Ok(TaskEvent::Removed { .. }) => {}
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "auto_promote task_events channel lagged");
            }
            Err(broadcast::error::RecvError::Closed) => {
                tracing::info!("auto_promote::run: task_events channel closed, exiting");
                return;
            }
        }
    }
}

/// Обрабатывает закрытие задачи `closed_id`: если она была «головой» цепочки —
/// промоутит следующую верхнюю TODO (при стоящем у неё флаге `auto_promote`).
///
/// # Анти-гонка / анти-повтор
///
/// Поиск головы и её удаление делаются под ОДНИМ write-lock'ом
/// `state.auto_chain`: запись извлекается (find + remove) ДО любого `await` на
/// `br create`. Это гарантирует, что параллельный дубль closed-события (или
/// re-touch старой задачи) не запустит вторую промоут-операцию для той же головы.
/// Poisoned lock обрабатывается мягко (без паники): просто выходим.
///
/// # Барьер цепочки
///
/// После удаления головы берём список TODO корня и выбираем верхнюю карточку через
/// [`pick_top`] (БЕЗ фильтра по `auto_promote`). Цепочка ПРОДОЛЖАЕТСЯ только если у
/// верхней карточки `auto_promote == true`. Иначе (список пуст ИЛИ верхняя не
/// флагнута) — барьер: запись уже удалена, ничего не промоутим, цепочка тихо
/// останавливается (перезапустится при следующем ручном `promote`).
///
/// # Промоут
///
/// При продолжении цепочки вызываем [`crate::promote_todo_core`] с
/// `target_session = entry.session` (сессия протягивается по цепочке от ручного
/// промоута; `None` → фолбэк на `cfg.session` внутри core) и `mode_override =
/// Some(NotifyMode::Immediate)`. Immediate ОБЯЗАТЕЛЕН: использовать
/// `cfg.wait_previous` нельзя — иначе двойная сериализация ожидания закрытия задачи
/// (воркер уже ждёт closed-событие) приведёт к дедлоку цепочки. `promote_todo_core`
/// сам перезапишет `auto_chain[root]` на новый `task_id` (новая голова). Ошибка
/// `br create` логируется как `error` и обрывает цепочку (ок: перезапуск ручным
/// промоутом).
async fn handle_closed(state: &AppState, closed_id: &str) {
    // 1. Найти root, где голова == closed_id, и СРАЗУ удалить запись под одним
    //    write-lock'ом (анти-гонка: до await на br create).
    let found: Option<(String, AutoChainEntry)> = {
        match state.auto_chain.write() {
            Ok(mut chain) => {
                let root = chain
                    .iter()
                    .find(|(_, entry)| entry.active_task_id == closed_id)
                    .map(|(root, _)| root.clone());
                match root {
                    Some(root) => chain.remove(&root).map(|entry| (root, entry)),
                    None => None,
                }
            }
            Err(_) => {
                tracing::warn!(
                    closed_id = %closed_id,
                    "auto_chain lock poisoned; skipped chain advance"
                );
                None
            }
        }
    };

    let (root, entry) = match found {
        Some(pair) => pair,
        // Посторонняя задача (не голова цепочки) или re-touch уже снятой головы.
        None => return,
    };

    // 2. Выбрать верхнюю карточку корня (канбан-порядок), БЕЗ фильтра.
    let top = pick_top(state.todos.list(&root));

    // 3. Барьер: пусто ИЛИ верхняя без флага -> стоп (запись уже удалена в п.1).
    let top = match top {
        Some(t) if t.auto_promote => t,
        Some(_) => {
            tracing::debug!(
                root = %root,
                closed_id = %closed_id,
                "auto_promote chain barrier: top card has auto_promote=false, stopping chain"
            );
            return;
        }
        None => {
            tracing::debug!(
                root = %root,
                closed_id = %closed_id,
                "auto_promote chain barrier: no TODO cards left, stopping chain"
            );
            return;
        }
    };

    // 4. Промоут следующей: session протягивается, mode=Immediate (НЕ wait_previous).
    match crate::promote_todo_core(
        state,
        &top,
        entry.session.clone(),
        Some(crate::notifier::NotifyMode::Immediate),
    )
    .await
    {
        Ok(outcome) => {
            tracing::info!(
                root = %root,
                closed_id = %closed_id,
                new_task_id = %outcome.task_id,
                session = ?entry.session,
                "auto_promote chain advanced to next TODO"
            );
        }
        Err(e) => {
            tracing::error!(
                root = %root,
                closed_id = %closed_id,
                error = %e,
                "auto_promote chain advance failed (br create); chain broken, restart via manual promote"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(id: &str, priority: u8, updated_at: &str, auto_promote: bool) -> Todo {
        Todo {
            id: id.to_string(),
            root_path: "/root".to_string(),
            title: format!("title-{id}"),
            description: None,
            priority,
            issue_type: "task".to_string(),
            labels: Vec::new(),
            plan_mode: false,
            auto_promote,
            created_at: "2026-05-27T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            origin: "local".to_string(),
        }
    }

    #[test]
    fn pick_top_empty_is_none() {
        assert!(pick_top(Vec::new()).is_none());
    }

    #[test]
    fn pick_top_orders_by_priority_asc() {
        // priority меньше = выше; updated_at одинаков, выбираем по приоритету.
        let todos = vec![
            todo("c", 3, "2026-05-27T00:00:00Z", true),
            todo("a", 0, "2026-05-27T00:00:00Z", true),
            todo("b", 1, "2026-05-27T00:00:00Z", true),
        ];
        assert_eq!(pick_top(todos).unwrap().id, "a");
    }

    #[test]
    fn pick_top_breaks_ties_by_updated_at_desc() {
        // Одинаковый priority -> новее (больший updated_at) первым.
        let todos = vec![
            todo("old", 1, "2026-05-20T10:00:00Z", true),
            todo("new", 1, "2026-05-27T10:00:00Z", true),
            todo("mid", 1, "2026-05-25T10:00:00Z", true),
        ];
        assert_eq!(pick_top(todos).unwrap().id, "new");
    }

    #[test]
    fn pick_top_priority_dominates_updated_at() {
        // Низкий приоритет (число больше) не обгоняет высокий, даже будучи новее.
        let todos = vec![
            todo("newer_low_prio", 2, "2026-05-27T23:59:59Z", true),
            todo("older_high_prio", 0, "2026-05-01T00:00:00Z", true),
        ];
        assert_eq!(pick_top(todos).unwrap().id, "older_high_prio");
    }

    #[test]
    fn pick_top_ignores_auto_promote_flag_in_selection() {
        // Верхняя выбирается без фильтра: даже если у неё auto_promote=false,
        // именно она возвращается (барьер проверяется уже после, в handle_closed).
        let todos = vec![
            todo("top_unflagged", 0, "2026-05-27T00:00:00Z", false),
            todo("lower_flagged", 1, "2026-05-27T00:00:00Z", true),
        ];
        let top = pick_top(todos).unwrap();
        assert_eq!(top.id, "top_unflagged");
        assert!(!top.auto_promote, "barrier (auto_promote=false) decided by caller, not pick_top");
    }
}
