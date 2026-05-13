//! Attention-watcher для tmux-сессий: следит за наличием Claude permission
//! prompt в панелях и поддерживает разделяемое состояние «нужно внимание».
//!
//! ### Назначение
//!
//! Раз в 1.5 секунды watcher (см. `watcher_loop`) обходит все tmux-сессии
//! активного проекта, дёргает `tmux::capture_pane` и применяет строгий
//! паттерн `detect_claude_prompt`. Результат пишется в `AttentionState`,
//! откуда фронтенд получает флаг для оранжевой подсветки вкладки.
//!
//! ### Контракт детектора
//!
//! `detect_claude_prompt` срабатывает когда в панели одновременно присутствуют
//! **три** маркера структуры меню выбора Claude Code:
//! - `❯ 1. Yes` — курсор на первом варианте;
//! - `2. Yes,` — второй вариант (любой sub-text: «and don't ask again»,
//!   «allow all edits during this session» и пр.);
//! - `3. No` — третий вариант (любой sub-text: «», «tell Claude what to do» и пр.).
//!
//! AND-семантика выбрана сознательно: каждый из маркеров может встретиться
//! и в обычном выводе (например, в логах), но все три вместе — практически
//! однозначно prompt. Маркеры намеренно сделаны «короткими», чтобы покрыть
//! разные варианты Claude UI (file create, edit, bash, и т.п.).
//!
//! ### Состояние
//!
//! `AttentionState` хранит `HashMap<session_name, bool>` под `tokio::RwLock`.
//! Async-замок обязателен: и watcher_loop, и axum-хендлеры (Phase 3) дёргают
//! `snapshot`/`set` из tokio runtime.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Разделяемое состояние «у каких сессий сейчас открыт Claude permission
/// prompt».
///
/// Ключ — имя tmux-сессии (то же, что `tmux::SessionInfo::name`). Значение
/// `true` означает «нужно подсветить вкладку оранжевым».
///
/// `Clone` дешёвый: это лишь клонирование `Arc`. Конкурентное чтение через
/// `snapshot` не блокирует пишущих надолго — мы возвращаем владеющую копию
/// мапы, а не ссылку.
#[derive(Debug, Clone, Default)]
pub struct AttentionState {
    map: Arc<RwLock<HashMap<String, bool>>>,
}

impl AttentionState {
    /// Создаёт пустое состояние.
    pub fn new() -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Возвращает копию текущего состояния.
    ///
    /// Используется axum-хендлером `/api/attention` (Phase 3) и broadcast'ером
    /// в WebSocket. Копия позволяет отдать данные наружу без удержания
    /// read-lock'а.
    pub async fn snapshot(&self) -> HashMap<String, bool> {
        self.map.read().await.clone()
    }

    /// Устанавливает флаг для одной сессии.
    ///
    /// Перезаписывает предыдущее значение. Не удаляет ключи при `false` —
    /// это позволяет фронтенду надёжно различать «никогда не видели» и
    /// «видели, prompt закрыт».
    pub async fn set(&self, name: &str, flag: bool) {
        let mut guard = self.map.write().await;
        guard.insert(name.to_string(), flag);
    }
}

/// Строгий детектор Claude Code permission prompt.
///
/// Возвращает `true` если в `pane` присутствует один из трёх типов prompt'а:
///
/// 1. **Permission prompt** (Yes/Yes/No) — все три маркера сразу:
///    `❯ 1. Yes`, `2. Yes,`, `3. No`.
/// 2. **Plan prompt** (`ExitPlanMode`) — footer `Enter to select · Tab/Arrow keys to navigate · Esc to cancel`.
/// 3. **Question prompt** (`AskUserQuestion`) — footer `Enter to select · ↑/↓ to navigate · Esc to cancel`.
///
/// Для permission используется AND-семантика по трём маркерам, чтобы избежать
/// ложных срабатываний на обычный вывод. Plan и question prompt'ы детектятся
/// по уникальной footer-строке, которая в обычном shell-выводе не встречается.
pub fn detect_claude_prompt(pane: &str) -> bool {
    detect_permission_prompt(pane) || detect_plan_prompt(pane) || detect_question_prompt(pane)
}

/// Permission prompt: `❯ 1. Yes` + `2. Yes,` + `3. No`.
fn detect_permission_prompt(pane: &str) -> bool {
    pane.contains("❯ 1. Yes") && pane.contains("2. Yes,") && pane.contains("3. No")
}

/// Plan prompt (`ExitPlanMode`): footer с `Tab/Arrow keys to navigate`.
fn detect_plan_prompt(pane: &str) -> bool {
    pane.contains("Enter to select") && pane.contains("Tab/Arrow keys to navigate")
}

/// Question prompt (`AskUserQuestion`): footer с `↑/↓ to navigate`.
fn detect_question_prompt(pane: &str) -> bool {
    pane.contains("Enter to select") && pane.contains("↑/↓ to navigate")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фикстура — типовой permission prompt Claude Code (edit-вариант).
    const PROMPT_FIXTURE: &str = "\
Do you want to make this edit to file.rs?
  ❯ 1. Yes
    2. Yes, and don't ask again this session
    3. No, tell Claude what to do differently
";

    /// Фикстура — file-create prompt Claude Code (другой вариант текста
    /// в options 2 и 3).
    const FILE_CREATE_FIXTURE: &str = "\
Do you want to create mandelbrot.py?
 ❯ 1. Yes
   2. Yes, allow all edits during this session (shift+tab)
   3. No
";

    /// Фикстура — обычный shell output без признаков prompt'а.
    const SHELL_FIXTURE: &str = "\
$ ls -la
total 24
drwxr-xr-x  3 user  staff   96 May 10 12:00 .
drwxr-xr-x  5 user  staff  160 May 10 12:00 ..
-rw-r--r--  1 user  staff  120 May 10 12:00 file.txt
$
";

    /// Фикстура — plan prompt (`ExitPlanMode`).
    const PLAN_FIXTURE: &str = "\
Here is my plan:
  ❯ Approve
    Edit
    Cancel
Enter to select · Tab/Arrow keys to navigate · Esc to cancel
";

    /// Фикстура — question prompt (`AskUserQuestion`).
    const QUESTION_FIXTURE: &str = "\
Which approach do you prefer?
  ❯ Option A
    Option B
    Other
Enter to select · ↑/↓ to navigate · Esc to cancel
";

    #[test]
    fn detects_full_prompt() {
        assert!(detect_claude_prompt(PROMPT_FIXTURE));
    }

    #[test]
    fn detects_file_create_prompt() {
        assert!(detect_claude_prompt(FILE_CREATE_FIXTURE));
    }

    #[test]
    fn ignores_plain_shell_output() {
        assert!(!detect_claude_prompt(SHELL_FIXTURE));
        assert!(!detect_claude_prompt(""));
    }

    #[test]
    fn detects_plan_prompt() {
        assert!(detect_claude_prompt(PLAN_FIXTURE));
        assert!(detect_plan_prompt(PLAN_FIXTURE));
        // Не должен путаться с question prompt.
        assert!(!detect_question_prompt(PLAN_FIXTURE));
        assert!(!detect_permission_prompt(PLAN_FIXTURE));
    }

    #[test]
    fn detects_question_prompt() {
        assert!(detect_claude_prompt(QUESTION_FIXTURE));
        assert!(detect_question_prompt(QUESTION_FIXTURE));
        // Не должен путаться с plan prompt.
        assert!(!detect_plan_prompt(QUESTION_FIXTURE));
        assert!(!detect_permission_prompt(QUESTION_FIXTURE));
    }

    #[test]
    fn plan_prompt_requires_both_markers() {
        // Только `Enter to select` — false (типичная справка в help-выводе).
        assert!(!detect_plan_prompt("Enter to select something"));
        // Только `Tab/Arrow keys to navigate` — false.
        assert!(!detect_plan_prompt("Tab/Arrow keys to navigate"));
    }

    #[test]
    fn question_prompt_requires_both_markers() {
        assert!(!detect_question_prompt("Enter to select foo"));
        assert!(!detect_question_prompt("↑/↓ to navigate"));
    }

    #[test]
    fn requires_all_three_markers() {
        // Только первый маркер — false.
        assert!(!detect_claude_prompt("❯ 1. Yes\nsome other text"));
        // Первый + второй, без третьего — false.
        assert!(!detect_claude_prompt("❯ 1. Yes\n2. Yes, and don't ask again"));
        // Второй + третий, без первого — false.
        assert!(!detect_claude_prompt("2. Yes, and don't ask\n3. No"));
        // Первый + третий, без второго — false.
        assert!(!detect_claude_prompt("❯ 1. Yes\n3. No"));
    }

    #[tokio::test]
    async fn attention_state_snapshot_and_set() {
        let s = AttentionState::new();
        assert!(s.snapshot().await.is_empty());

        s.set("forge-web", true).await;
        s.set("forge-cli", false).await;

        let snap = s.snapshot().await;
        assert_eq!(snap.get("forge-web"), Some(&true));
        assert_eq!(snap.get("forge-cli"), Some(&false));
        assert_eq!(snap.len(), 2);

        // Перезапись.
        s.set("forge-web", false).await;
        let snap2 = s.snapshot().await;
        assert_eq!(snap2.get("forge-web"), Some(&false));
    }

    #[tokio::test]
    async fn attention_state_is_cheaply_cloneable() {
        // Clone должен делить общий map (через Arc).
        let s1 = AttentionState::new();
        let s2 = s1.clone();
        s1.set("a", true).await;
        let snap = s2.snapshot().await;
        assert_eq!(snap.get("a"), Some(&true));
    }

    // ===========================================================================
    // Юнит-тесты дедуп-логики (forge-bjm.2.1).
    //
    // Тестируют чистые функции `deduplicate_attention` и `pick_primary` без
    // зависимостей на tmux/IO: оперируют напрямую структурой `SessionAttention`.
    // ===========================================================================

    /// Вспомогательный конструктор `SessionAttention` для тестов.
    ///
    /// Поля, не значимые для конкретного кейса, выставляются дефолтами:
    /// - `id` собирается как `"$<name>"` чтобы быть уникальным и
    ///   воспроизводимым (нужно для tie-break по id).
    /// - `pane_hash` параметризуется отдельно для имитации совпадающего/
    ///   разного содержимого панели.
    /// - `detected` параметризуется — это исходный результат
    ///   `detect_claude_prompt`, который дедуп будет нормализовать.
    fn mk_session(
        name: &str,
        attached: u32,
        group: Option<&str>,
        pane_hash: u64,
        detected: bool,
    ) -> SessionAttention {
        SessionAttention {
            name: name.to_string(),
            id: format!("${}", name),
            attached,
            session_group: group.map(|s| s.to_string()),
            pane_hash,
            detected,
        }
    }

    /// Хелпер: возвращает финальный флаг для сессии по имени.
    fn flag_of(out: &[(String, bool)], name: &str) -> bool {
        out.iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| *f)
            .unwrap_or_else(|| panic!("session {name} not found in dedup output"))
    }

    /// Кейс 1: две сессии с одинаковым `pane_hash`, обе `detected=true` —
    /// после дедупа ровно одна сохраняет флаг, обе detached → tie-break
    /// по наибольшему id (правило `pick_primary`).
    #[test]
    fn dedup_same_pane_hash_keeps_only_primary() {
        // Оба detached (attached=0), session_group=None → группа собирается
        // по pane_hash. Идентичный pane_hash объединяет их.
        let items = vec![
            mk_session("alpha", 0, None, 42, true),
            mk_session("beta", 0, None, 42, true),
        ];

        let out = deduplicate_attention(&items);

        let primary_count = out.iter().filter(|(_, f)| *f).count();
        assert_eq!(
            primary_count, 1,
            "ровно одна сессия должна остаться с needs_attention=true (out={out:?})"
        );

        // Tie-break: id это `$<name>` → `$beta` > `$alpha` лексикографически
        // (символ 'b' > 'a'), значит beta становится primary.
        assert!(
            flag_of(&out, "beta"),
            "beta должна быть primary по наибольшему id ($beta > $alpha)"
        );
        assert!(
            !flag_of(&out, "alpha"),
            "alpha должна быть подавлена дедупом"
        );
    }

    /// Кейс 2: две сессии БЕЗ session_group (None) и с РАЗНЫМИ `pane_hash` —
    /// дедуп их не объединяет, обе остаются с `needs_attention=true`.
    #[test]
    fn dedup_different_pane_hash_no_grouping() {
        let items = vec![
            mk_session("alpha", 0, None, 11, true),
            mk_session("beta", 0, None, 22, true),
        ];

        let out = deduplicate_attention(&items);

        assert!(
            flag_of(&out, "alpha"),
            "alpha сохраняет флаг (своя группа по pane_hash=11)"
        );
        assert!(
            flag_of(&out, "beta"),
            "beta сохраняет флаг (своя группа по pane_hash=22)"
        );
    }

    /// Кейс 3: две сессии в одной session_group, обе detected. У одной
    /// `attached=1`, у другой `0` — primary всегда attached, даже если её
    /// id/имя лексикографически меньше.
    #[test]
    fn dedup_attached_wins_over_detached() {
        // id будет $alpha vs $beta. Без правила attached primary стал бы
        // beta (наибольший id). Но attached=1 у alpha → она primary.
        let items = vec![
            mk_session("alpha", 1, Some("grp1"), 100, true),
            mk_session("beta", 0, Some("grp1"), 200, true),
        ];

        let out = deduplicate_attention(&items);

        assert!(
            flag_of(&out, "alpha"),
            "alpha (attached=1) должна быть primary в группе grp1"
        );
        assert!(
            !flag_of(&out, "beta"),
            "beta (detached) должна быть подавлена несмотря на больший id"
        );
    }

    /// Кейс 4: совпадает `session_group`, но `pane_hash` РАЗНЫЕ — дедуп
    /// всё равно срабатывает по группе (linked-сессии с лёгкой
    /// расходимостью рендеринга, например cursor-blink).
    #[test]
    fn dedup_same_group_unifies_even_with_different_pane_hash() {
        let items = vec![
            mk_session("alpha", 0, Some("grp7"), 1, true),
            mk_session("beta", 0, Some("grp7"), 2, true),
        ];

        let out = deduplicate_attention(&items);

        let primary_count = out.iter().filter(|(_, f)| *f).count();
        assert_eq!(
            primary_count, 1,
            "linked-сессии одной группы должны схлопываться в одного primary (out={out:?})"
        );
        // Tie-break — наибольший id ($beta > $alpha).
        assert!(flag_of(&out, "beta"), "beta primary по наибольшему id");
        assert!(!flag_of(&out, "alpha"));
    }

    /// Кейс 5 (бонус): ни одна сессия не имеет detected=true — флаги
    /// остаются false, никаких изменений группы не вносится.
    #[test]
    fn dedup_no_detection_keeps_all_false() {
        let items = vec![
            mk_session("alpha", 1, Some("grp1"), 50, false),
            mk_session("beta", 0, Some("grp1"), 50, false),
            mk_session("gamma", 0, None, 99, false),
        ];

        let out = deduplicate_attention(&items);

        for (name, flag) in &out {
            assert!(
                !*flag,
                "session {name}: detected=false везде → флаг должен быть false (got true)"
            );
        }
        assert_eq!(out.len(), 3, "все сессии должны присутствовать в результате");
    }

    /// Дополнительный кейс: 3 detached-сессии в одной группе, все detected
    /// → primary выбирается по правилу «наибольший id» (для одинаковых
    /// `$<name>`-id это лексикографически наибольшее имя).
    #[test]
    fn dedup_three_detached_picks_largest_id() {
        let items = vec![
            mk_session("alpha", 0, Some("g"), 5, true),
            mk_session("beta", 0, Some("g"), 5, true),
            mk_session("gamma", 0, Some("g"), 5, true),
        ];

        let out = deduplicate_attention(&items);

        let primary_count = out.iter().filter(|(_, f)| *f).count();
        assert_eq!(primary_count, 1, "ровно один primary (out={out:?})");
        assert!(flag_of(&out, "gamma"), "gamma — наибольший id ($gamma)");
        assert!(!flag_of(&out, "alpha"));
        assert!(!flag_of(&out, "beta"));
    }

    /// Edge case: пустой ввод — пустой выход.
    #[test]
    fn dedup_empty_input_returns_empty() {
        let out = deduplicate_attention(&[]);
        assert!(out.is_empty());
    }

    /// Edge case: одиночная detected-сессия — без изменений, флаг остаётся true.
    #[test]
    fn dedup_single_detected_session_unchanged() {
        let items = vec![mk_session("solo", 0, None, 7, true)];
        let out = deduplicate_attention(&items);
        assert!(flag_of(&out, "solo"));
    }

    /// Регрессионный тест на `hash_pane`: одинаковый текст → одинаковый хэш,
    /// разный текст → (с очень высокой вероятностью) разный хэш. Это контракт,
    /// на котором держится dedup-ось «по pane_hash».
    #[test]
    fn hash_pane_is_deterministic_and_collision_free_for_distinct_inputs() {
        let h1 = hash_pane("hello world");
        let h2 = hash_pane("hello world");
        let h3 = hash_pane("hello world!");
        assert_eq!(h1, h2, "одинаковый ввод → одинаковый hash");
        assert_ne!(h1, h3, "разный ввод → разный hash (DefaultHasher достаточен)");
    }
}

/// Фоновый watcher-loop: каждые 1500мс обходит ВСЕ tmux-сессии,
/// захватывает их панели через [`crate::tmux::capture_pane`] и обновляет
/// `attention` соответствующим флагом.
///
/// ### Параметры
///
/// Принимает только `attention` — состояние, в которое пишутся флаги. Раньше
/// watcher фильтровал сессии по `tmux_prefix` активного проекта, но в рамках
/// фичи cross-project sessions visibility этот фильтр снят: фронтенду нужны
/// флаги для всех сессий, чтобы корректно подсвечивать вкладки в любом
/// проекте (а также orphan-сессии без проекта).
///
/// ### Устойчивость
///
/// - Сбой `tmux::list_sessions` или `tmux::capture_pane` не валит loop:
///   используется `unwrap_or_default`. Это правильное поведение, т.к. tmux-
///   сервер может отсутствовать (нет сессий — пустой вектор) и сессия может
///   исчезнуть между `list` и `capture` (`capture_pane` сама вернёт
///   `Ok(String::new())`).
/// - Loop никогда не завершается штатно — он жив до завершения процесса.
///
/// ### Запуск
///
/// Стартуется через `tokio::spawn(watcher_loop(attention))` из `main.rs`.
pub async fn watcher_loop(attention: Arc<AttentionState>) {
    use std::time::Duration;

    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let sessions = crate::tmux::list_sessions().await.unwrap_or_default();

        // Шаг 1: собрать состояние всех сессий (имя, attached, id, hash, detected).
        let mut collected: Vec<SessionAttention> = Vec::with_capacity(sessions.len());
        for s in sessions.iter() {
            let pane = crate::tmux::capture_pane(&s.name).await.unwrap_or_default();
            let detected = detect_claude_prompt(&pane);
            let pane_hash = hash_pane(&pane);

            tracing::debug!(
                session = %s.name,
                group = ?s.session_group,
                pane_hash,
                detected,
                pane_len = pane.len(),
                "attention check"
            );

            collected.push(SessionAttention {
                name: s.name.clone(),
                id: s.id.clone(),
                attached: s.attached,
                session_group: s.session_group.clone(),
                pane_hash,
                detected,
            });
        }

        // Шаг 2: дедупликация. Только primary получает true.
        let final_flags = deduplicate_attention(&collected);

        // Шаг 3: записать финальные флаги.
        for (name, flag) in final_flags {
            attention.set(&name, flag).await;
        }
    }
}

/// Снимок состояния одной сессии для дедупликации в одной итерации
/// `watcher_loop`.
///
/// Содержит всё, что нужно для выбора primary в группе: имя (ключ итогового
/// флага), tmux id (`$0`), attached (число прикреплённых клиентов),
/// `session_group` (имя tmux session-group), `pane_hash` (DefaultHasher по
/// содержимому pane) и `detected` (исходный результат `detect_claude_prompt`).
#[derive(Debug, Clone)]
struct SessionAttention {
    name: String,
    id: String,
    attached: u32,
    session_group: Option<String>,
    pane_hash: u64,
    detected: bool,
}

/// Хэширует содержимое панели в стабильный `u64`.
///
/// Использует `std::collections::hash_map::DefaultHasher`: достаточно для
/// эквивалентности «один и тот же текст → один хэш» в рамках одного процесса.
/// Криптостойкость не требуется.
fn hash_pane(pane: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    pane.hash(&mut hasher);
    hasher.finish()
}

/// Дедуплицирует флаги `needs_attention` среди сессий одной итерации.
///
/// Возвращает вектор `(session_name, final_flag)` — по одной записи на каждую
/// входную сессию.
///
/// ### Алгоритм
///
/// Сессии группируются по двум осям:
/// 1. **pane_hash** — точное совпадение содержимого видимой панели.
/// 2. **session_group** — `Some(g)` означает linked-сессии tmux: они делят
///    окна, но рендеринг может отличаться на пару символов (cursor),
///    поэтому дедуп нужен независимо от pane_hash.
///
/// Группы объединяются (union-find по обеим осям). Внутри каждой
/// объединённой группы:
/// - если ни одна сессия не имеет `detected=true` — все остаются `false`;
/// - если хотя бы одна имеет `detected=true` — выбирается **primary**:
///   1. `attached > 0` (кто-то реально смотрит);
///   2. среди `attached > 0` или среди всех (если все 0) — наибольший
///      `session_id` (как строка): свежее созданная сессия предпочтительнее;
///   3. ничья по id невозможна (id уникальны), но как fallback —
///      лексикографически наибольшее имя.
///
/// Только primary получает `true`, остальные сессии группы — `false`,
/// даже если их собственный `detected=true`. Это и есть подавление
/// «оранжевого отображения всей группы».
fn deduplicate_attention(items: &[SessionAttention]) -> Vec<(String, bool)> {
    if items.is_empty() {
        return Vec::new();
    }

    // Union-find по индексам в `items`.
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        // path compression
        let mut cur = x;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    // Объединяем по pane_hash.
    {
        let mut by_hash: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
        for (i, it) in items.iter().enumerate() {
            match by_hash.get(&it.pane_hash) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_hash.insert(it.pane_hash, i);
                }
            }
        }
    }

    // Объединяем по session_group (только для Some(_)).
    {
        let mut by_group: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (i, it) in items.iter().enumerate() {
            if let Some(g) = &it.session_group {
                match by_group.get(g.as_str()) {
                    Some(&j) => union(&mut parent, i, j),
                    None => {
                        by_group.insert(g.as_str(), i);
                    }
                }
            }
        }
    }

    // Группируем индексы по корню.
    let mut groups: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    // Для каждой группы выбираем primary.
    let mut result: Vec<(String, bool)> = Vec::with_capacity(n);
    // Сначала заготавливаем все false.
    for it in items.iter() {
        result.push((it.name.clone(), false));
    }

    for (_root, members) in groups.iter() {
        let any_detected = members.iter().any(|&i| items[i].detected);
        if !any_detected {
            continue; // все остаются false
        }

        let primary = pick_primary(items, members);
        if let Some(p) = primary {
            // Найти соответствующую запись в result и установить true.
            let name = &items[p].name;
            for r in result.iter_mut() {
                if r.0 == *name {
                    r.1 = true;
                    break;
                }
            }
        }
    }

    result
}

/// Выбирает primary-индекс среди `members`.
///
/// Приоритет:
/// 1. среди элементов с `detected=true` и `attached>0` — наибольший `id` лексикографически;
/// 2. иначе среди всех `detected=true` — наибольший `id`;
/// 3. fallback: лексикографически наибольшее имя среди `detected=true`.
fn pick_primary(items: &[SessionAttention], members: &[usize]) -> Option<usize> {
    // Кандидаты — только те, у кого detected=true.
    let mut detected_idx: Vec<usize> =
        members.iter().copied().filter(|&i| items[i].detected).collect();
    if detected_idx.is_empty() {
        return None;
    }

    // (a) attached > 0.
    let attached_idx: Vec<usize> = detected_idx
        .iter()
        .copied()
        .filter(|&i| items[i].attached > 0)
        .collect();

    if !attached_idx.is_empty() {
        return attached_idx
            .into_iter()
            .max_by(|&a, &b| items[a].id.cmp(&items[b].id));
    }

    // (b) все attached=0 → берём по наибольшему id.
    if let Some(&p) = detected_idx
        .iter()
        .max_by(|&&a, &&b| items[a].id.cmp(&items[b].id))
    {
        return Some(p);
    }

    // (c) fallback — лексикографически наибольшее имя.
    detected_idx.sort_by(|&a, &b| items[a].name.cmp(&items[b].name));
    detected_idx.last().copied()
}
