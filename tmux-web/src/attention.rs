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
use std::time::Instant;

use tokio::sync::RwLock;

/// Сколько секунд непрерывного просмотра сессии (attached>0) достаточно, чтобы
/// считать её «просмотренной» и завершить idle-эпизод «следующего шага»:
/// голубое свечение гаснет и не возвращается при последующем уходе из сессии
/// (новое предложение появится только после новой серии генерации Claude).
///
/// Совпадает по значению с порогом затихания воркера
/// (`forge_echo::next_step::IDLE_THRESHOLD_SECS`), но семантически независим:
/// здесь это «сколько пользователь смотрел сессию», там — «сколько она молчала».
const VIEW_DISMISS_SECS: u64 = 10;

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
    generating: Arc<RwLock<HashMap<String, bool>>>,
    /// Последний наблюдённый хэш pane на сессию.
    ///
    /// Используется в `update_generation` для получения «сырого» сигнала
    /// `changed = prev != current`. Само поле НЕ хранит финальный флаг
    /// `is_generating` — он пишется отдельно через `set_generating` после
    /// дедупликации в `watcher_loop`.
    ///
    /// При первом наблюдении сессии (нет prev) `changed` всегда `false`:
    /// нет точки сравнения. Это сознательное решение, чтобы избежать ложного
    /// срабатывания на самой первой итерации watcher'а.
    last_gen_hash: Arc<RwLock<HashMap<String, u64>>>,
    /// Момент начала текущей непрерывной серии генерации на сессию.
    ///
    /// Ставится при переходе финального флага `is_generating` `false→true`
    /// (см. `set_generating`) и удаляется когда флаг гаснет. Используется
    /// только для UI-tooltip синего индикатора работы: по нему вычисляется
    /// «генерация идёт уже N секунд» через [`AttentionState::generating_age_snapshot`].
    /// На саму логику дедупа/детекта не влияет.
    gen_started_at: Arc<RwLock<HashMap<String, Instant>>>,
    /// Момент, когда индикатор генерации сессии погас (фронт `is_generating`
    /// `true→false`) — начало периода «idle».
    ///
    /// Ставится в [`set_generating`] на фронте `true→false` и удаляется на
    /// фронте `false→true` (новая серия генерации — сессия больше не idle).
    /// Idle отсчитывается ТОЛЬКО для сессий, которые реально генерировали и
    /// затихли: при первом наблюдении сессии (флаг сразу `false`) отметка НЕ
    /// ставится. Используется [`AttentionState::idle_snapshot`] для фичи
    /// «Следующий шаг» (Phase 2): по этой карте Echo-воркер находит сессии,
    /// в которых Claude закончил генерацию.
    idle_started_at: Arc<RwLock<HashMap<String, Instant>>>,
    /// Момент, когда сессию начал непрерывно смотреть пользователь
    /// (`attached>0`).
    ///
    /// Обновляется каждый тик watcher'а через [`AttentionState::update_attached`]:
    /// `Instant` фиксируется при появлении сессии в attached-множестве и
    /// держится стабильным, пока сессия непрерывно attached; снимается при
    /// detach. Нужен для фичи «Следующий шаг», чтобы не зажигать голубое
    /// свечение на сессиях, которые пользователь сейчас просматривает:
    /// - пока сессия attached, фронт `is_generating` `true→false` (перерисовка
    ///   при переключении/просмотре) НЕ ставит idle-метку (см. `set_generating`);
    /// - если сессию смотрят >= [`VIEW_DISMISS_SECS`], её idle-эпизод
    ///   завершается (idle-метка удаляется в [`AttentionState::idle_snapshot`]).
    attached_since: Arc<RwLock<HashMap<String, Instant>>>,
}

impl AttentionState {
    /// Создаёт пустое состояние.
    pub fn new() -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
            generating: Arc::new(RwLock::new(HashMap::new())),
            last_gen_hash: Arc::new(RwLock::new(HashMap::new())),
            gen_started_at: Arc::new(RwLock::new(HashMap::new())),
            idle_started_at: Arc::new(RwLock::new(HashMap::new())),
            attached_since: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Обновляет множество сессий, которые сейчас просматривает пользователь
    /// (`attached>0`). Вызывается каждый тик `watcher_loop` с актуальным
    /// набором имён attached-сессий.
    ///
    /// Момент начала просмотра (`Instant`) держится СТАБИЛЬНЫМ, пока сессия
    /// непрерывно attached (используется `entry().or_insert`), и снимается,
    /// как только сессия выпала из набора (`retain`). Это даёт корректный
    /// `elapsed()` «сколько уже смотрят» для порога [`VIEW_DISMISS_SECS`].
    pub async fn update_attached(&self, attached_names: &std::collections::HashSet<String>) {
        let mut map = self.attached_since.write().await;
        for name in attached_names {
            map.entry(name.clone()).or_insert_with(Instant::now);
        }
        map.retain(|k, _| attached_names.contains(k));
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

    /// Возвращает копию состояния «генерирует / работает» по всем сессиям.
    ///
    /// Семантика `is_generating`: за прошедший тик watcher'а (1.5с) содержимое
    /// последних 50 строк pane изменилось, и этот сырой сигнал прошёл через
    /// дедупликацию `deduplicate_generating` (group by `session_group`).
    /// Только primary linked-сессии получает `true`, остальные — `false`,
    /// чтобы индикатор не дублировался во всех вкладках одной группы.
    /// Независимые сессии (`session_group=None`) не группируются и светятся
    /// каждая сама. На практике это означает, что что-то рисуется на экране
    /// — Claude печатает, выводится stream tool output, идёт `tail -f`, и т.п.
    /// Frontend подсвечивает такие сессии пульсирующим значком.
    pub async fn generating_snapshot(&self) -> HashMap<String, bool> {
        self.generating.read().await.clone()
    }

    /// Устанавливает финальный флаг `is_generating` для одной сессии.
    ///
    /// Это «писатель» карты `generating` — единственное место, через которое
    /// финальное значение попадает в наблюдаемое состояние (что видят
    /// `generating_snapshot` и `/api/sessions`). Сюда пишет дедуп-фаза
    /// `watcher_loop` уже после того, как сырые `changed`-сигналы от
    /// `update_generation` свёрнуты с учётом `session_group`
    /// (linked-сессии меняются одновременно и должны давать общий флаг,
    /// иначе индикатор горит во всех вкладках одной группы).
    ///
    /// Семантика записи аналогична [`AttentionState::set`]: ключ не удаляется
    /// при `false` (всегда `insert`), что позволяет фронтенду надёжно
    /// различать «никогда не видели сессию» (нет ключа) и «видели, флаг
    /// потушен» (есть ключ со значением `false`).
    pub async fn set_generating(&self, name: &str, flag: bool) {
        let prev = {
            let mut map = self.generating.write().await;
            map.insert(name.to_string(), flag).unwrap_or(false)
        };

        // Поддерживаем `gen_started_at` строго по фронтам флага:
        // `false→true` — фиксируем начало серии «сейчас»; `*→false` —
        // удаляем отметку (серия закончилась). Пока флаг непрерывно `true`,
        // отметку НЕ трогаем, чтобы tooltip показывал реальную длительность,
        // а не сбрасывал её каждый тик watcher'а.
        let mut started = self.gen_started_at.write().await;
        if flag {
            if !prev {
                started.insert(name.to_string(), Instant::now());
            }
        } else {
            started.remove(name);
        }
        drop(started);

        // Поддерживаем `idle_started_at` строго по фронтам флага, зеркально
        // `gen_started_at`: фронт `true→false` — сессия затихла, фиксируем
        // начало idle «сейчас»; фронт `false→true` — началась новая серия
        // генерации, idle сброшен (удаляем отметку). Idle ставится ТОЛЬКО
        // когда сессия реально генерировала (prev=true): при первом
        // наблюдении (флаг сразу false, prev=false) отметку НЕ ставим.
        let mut idle = self.idle_started_at.write().await;
        if flag {
            // false→true или true→true: генерация идёт, сессия не idle.
            idle.remove(name);
        } else if prev {
            // true→false: фронт затухания. НО если сессию сейчас СМОТРИТ
            // пользователь (attached) — это его взаимодействие/перерисовка
            // при переключении или просмотре, а не автономная работа Claude:
            // idle-метку НЕ ставим. Иначе простое переключение между сессиями
            // зажигало бы «следующий шаг» на каждой посещённой сессии, и
            // свечение появлялось бы прямо на той сессии, в которой
            // пользователь сейчас работает.
            let attached = self.attached_since.read().await;
            if !attached.contains_key(name) {
                idle.insert(name.to_string(), Instant::now());
            }
        }
        // prev=false && flag=false (первое наблюдение / повторный false):
        // отметку idle не трогаем — сессия не генерировала, нечему затихать.
    }

    /// Возвращает «затихшие» (idle) сессии: для каждой записи в
    /// `idle_started_at` отдаёт `Instant::elapsed().as_secs()` от момента, когда
    /// индикатор генерации погас.
    ///
    /// Сессии с `needs_attention=true` (показан Claude permission/plan/question
    /// prompt — см. `self.map`) **исключаются**: там нужен ответ пользователя,
    /// а не «следующий шаг», поэтому idle для них не имеет смысла.
    ///
    /// Сессии, которые пользователь непрерывно смотрит уже >= [`VIEW_DISMISS_SECS`]
    /// секунд (см. `attached_since`), **завершают idle-эпизод**: их idle-метка
    /// удаляется насовсем. Логика: пользователь уже находится в сессии и видит
    /// её состояние — голубое свечение «следующего шага» ему не нужно и не
    /// должно возвращаться при уходе (новое появится лишь после новой генерации).
    ///
    /// Используется хост-адаптером [`crate::echo_host::EchoHostAdapter`] для
    /// реализации `HostApi::idle_sessions` (фича «Следующий шаг», Phase 2).
    pub async fn idle_snapshot(&self) -> HashMap<String, u64> {
        // Завершаем idle-эпизод для сессий, которые пользователь смотрит уже
        // >= VIEW_DISMISS_SECS: он их «просмотрел», предложение больше не нужно.
        // Удаляем idle-метку НАСОВСЕМ (а не просто фильтруем вывод), чтобы
        // свечение не вернулось, когда пользователь уйдёт из сессии: новое
        // предложение появится только после новой серии генерации Claude.
        {
            let attached = self.attached_since.read().await;
            let dismissed: Vec<String> = attached
                .iter()
                .filter(|(_, t)| t.elapsed().as_secs() >= VIEW_DISMISS_SECS)
                .map(|(name, _)| name.clone())
                .collect();
            drop(attached);
            if !dismissed.is_empty() {
                let mut idle = self.idle_started_at.write().await;
                for name in dismissed {
                    idle.remove(&name);
                }
            }
        }

        let idle = self.idle_started_at.read().await;
        let needs_attention = self.map.read().await;
        idle.iter()
            .filter(|(name, _)| {
                // Исключаем сессии с активным prompt'ом (needs_attention=true).
                !needs_attention.get(*name).copied().unwrap_or(false)
            })
            .map(|(name, t)| (name.clone(), t.elapsed().as_secs()))
            .collect()
    }

    /// Возвращает длительность текущей серии генерации (в секундах) для всех
    /// сессий, у которых она сейчас идёт.
    ///
    /// Ключ — имя сессии, значение — `Instant::elapsed().as_secs()` от момента
    /// начала серии (см. `gen_started_at`). Сессии без активной генерации в
    /// карту не попадают. Используется хендлером `/api/sessions` для поля
    /// `SessionDto::generating_since_secs`, на основе которого фронтенд строит
    /// tooltip синего индикатора работы.
    pub async fn generating_age_snapshot(&self) -> HashMap<String, u64> {
        let started = self.gen_started_at.read().await;
        started
            .iter()
            .map(|(name, t)| (name.clone(), t.elapsed().as_secs()))
            .collect()
    }

    /// Возвращает «сырой» сигнал `changed = prev != current` по pane-хэшу
    /// одной сессии и сохраняет новый хэш как `prev` для следующего тика.
    ///
    /// Первое наблюдение сессии (когда `prev` отсутствует) всегда возвращает
    /// `false`: точки сравнения ещё нет, поэтому однозначно сказать «было
    /// изменение» нельзя — избегаем ложного срабатывания на первой итерации
    /// watcher'а.
    ///
    /// **НЕ пишет** в `self.generating` — это критично. Финальный флаг
    /// `is_generating` определяется отдельно: watcher собирает `changed` со
    /// всех сессий, применяет дедупликацию по `session_group`
    /// (linked-сессии меняются одновременно и не должны давать множественные
    /// сигналы) и затем пишет результат через [`AttentionState::set_generating`].
    ///
    /// Эта развязка позволяет дедуп-фазе видеть «сырой» сигнал без побочного
    /// влияния на финальное состояние.
    pub async fn update_generation(&self, name: &str, current_hash: u64) -> bool {
        let mut map = self.last_gen_hash.write().await;
        let prev = map.insert(name.to_string(), current_hash);
        let changed = prev.map(|p| p != current_hash).unwrap_or(false);

        match prev {
            Some(p) if p != current_hash => {
                tracing::debug!(
                    session = %name,
                    prev = p,
                    current = current_hash,
                    changed = true,
                    "is_generating raw signal: pane hash changed"
                );
            }
            Some(p) => {
                tracing::debug!(
                    session = %name,
                    prev = p,
                    current = current_hash,
                    changed = false,
                    "is_generating raw signal: pane hash unchanged"
                );
            }
            None => {
                tracing::debug!(
                    session = %name,
                    current = current_hash,
                    changed = false,
                    "is_generating raw signal: first observation, no prev hash"
                );
            }
        }

        changed
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
///
/// ### Нормализация whitespace
///
/// Перед поиском маркеров pane прогоняется через [`normalize_ws`] — все
/// последовательности whitespace (включая переносы строк) схлопываются в один
/// пробел. Это нужно, потому что Claude Code — full-screen TUI: в недостаточно
/// широком терминале он переносит длинный footer (`Enter to select · Tab/Arrow
/// keys to navigate · Esc to cancel`, ~60 символов) по словам на несколько
/// строк. Без нормализации `contains("Tab/Arrow keys to navigate")` не находил
/// бы разорванный переносом маркер → оранжевое свечение не загоралось на
/// detached-сессиях с plan/question prompt (короткие маркеры permission-prompt
/// не переносятся, поэтому раньше работал только он). Нормализация покрывает
/// word-wrap (перенос по границам слов — штатное поведение TUI).
pub fn detect_claude_prompt(pane: &str) -> bool {
    let norm = normalize_ws(pane);
    detect_permission_prompt(&norm) || detect_plan_prompt(&norm) || detect_question_prompt(&norm)
}

/// Схлопывает все последовательности whitespace (пробелы, табы, переносы
/// строк) в один пробел. См. раздел «Нормализация whitespace» в
/// [`detect_claude_prompt`] — нужно для устойчивости детекции к переносу
/// длинного footer в узких терминалах.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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

    /// Фикстура — plan prompt в УЗКОМ терминале: Claude TUI перенёс footer
    /// по словам на несколько строк. Без нормализации whitespace маркер
    /// `Tab/Arrow keys to navigate` разорван и не детектится.
    const PLAN_WRAPPED_FIXTURE: &str = "\
Here is my plan:
  ❯ Approve
    Edit
Enter to select · Tab/Arrow
keys to navigate · Esc to
cancel
";

    /// Фикстура — question prompt в узком терминале с перенесённым footer.
    const QUESTION_WRAPPED_FIXTURE: &str = "\
Which approach do you prefer?
  ❯ Option A
    Option B
Enter to select · ↑/↓ to
navigate · Esc to cancel
";

    /// Фикстура — permission prompt с лишними пробелами и переносом внутри
    /// блока опций (нормализация должна схлопнуть whitespace).
    const PERMISSION_WRAPPED_FIXTURE: &str = "\
Do you want to make this edit?
  ❯ 1. Yes
    2. Yes, and don't ask
       again this session
    3. No
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

    /// Регрессия: plan prompt с перенесённым по словам footer (узкий терминал)
    /// должен детектиться благодаря нормализации whitespace в
    /// `detect_claude_prompt`. Это главный кейс бага «свечение не срабатывает
    /// на такой тип» — раньше разорванный `Tab/Arrow keys to navigate` не
    /// находился через `contains`.
    #[test]
    fn detects_wrapped_plan_prompt() {
        // Без нормализации сырой footer разорван — суб-детектор не сработал бы.
        assert!(
            !detect_plan_prompt(PLAN_WRAPPED_FIXTURE),
            "sanity: на сыром (ненормализованном) тексте маркер разорван"
        );
        // detect_claude_prompt нормализует whitespace → маркер целый.
        assert!(detect_claude_prompt(PLAN_WRAPPED_FIXTURE));
    }

    /// Регрессия: question prompt с перенесённым footer.
    #[test]
    fn detects_wrapped_question_prompt() {
        assert!(
            !detect_question_prompt(QUESTION_WRAPPED_FIXTURE),
            "sanity: на сыром тексте маркер `↑/↓ to navigate` разорван"
        );
        assert!(detect_claude_prompt(QUESTION_WRAPPED_FIXTURE));
    }

    /// Permission prompt с лишними пробелами/переносом внутри опций —
    /// нормализация whitespace не должна ломать детекцию.
    #[test]
    fn detects_permission_with_extra_whitespace() {
        assert!(detect_claude_prompt(PERMISSION_WRAPPED_FIXTURE));
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

    #[tokio::test]
    async fn generating_age_tracks_streak_fronts() {
        // gen_started_at заводится при false→true, держится пока true,
        // удаляется при →false. Это питает SessionDto::generating_since_secs
        // и tooltip синего индикатора работы.
        let s = AttentionState::new();

        // Нет генерации — снапшот длительностей пуст.
        assert!(s.generating_age_snapshot().await.is_empty());

        // false→true: серия началась, отметка появилась (0+ секунд).
        s.set_generating("forge", true).await;
        assert!(
            s.generating_age_snapshot().await.contains_key("forge"),
            "после false→true должна появиться отметка начала серии"
        );

        // true→true: отметка НЕ сбрасывается (длительность продолжает расти).
        s.set_generating("forge", true).await;
        assert!(
            s.generating_age_snapshot().await.contains_key("forge"),
            "повторный true не должен ронять серию"
        );

        // →false: серия закончилась, отметка удалена.
        s.set_generating("forge", false).await;
        assert!(
            !s.generating_age_snapshot().await.contains_key("forge"),
            "после →false отметка должна исчезнуть"
        );
    }

    #[tokio::test]
    async fn idle_snapshot_tracks_generating_fronts() {
        // idle_started_at заводится на фронте true→false (сессия реально
        // генерировала и затихла) и снимается на false→true (новая серия).
        // Это питает HostApi::idle_sessions для фичи «Следующий шаг».
        let s = AttentionState::new();

        // Нет генерации никогда — idle-снапшот пуст (нечему затихать).
        assert!(
            s.idle_snapshot().await.is_empty(),
            "сессия без генерации не должна попадать в idle"
        );

        // false→true: генерация пошла, сессия не idle.
        s.set_generating("forge", true).await;
        assert!(
            !s.idle_snapshot().await.contains_key("forge"),
            "пока флаг true сессия не idle"
        );

        // true→false: серия закончилась — сессия стала idle (метка появилась).
        s.set_generating("forge", false).await;
        assert!(
            s.idle_snapshot().await.contains_key("forge"),
            "после true→false должна появиться idle-метка"
        );

        // false→true: новая серия генерации — idle-метка снимается.
        s.set_generating("forge", true).await;
        assert!(
            !s.idle_snapshot().await.contains_key("forge"),
            "после false→true idle-метка должна исчезнуть"
        );
    }

    #[tokio::test]
    async fn idle_snapshot_not_set_on_first_false_observation() {
        // Первое наблюдение сессии, флаг сразу false (prev=false): сессия
        // никогда не генерировала, idle-метку ставить нельзя.
        let s = AttentionState::new();
        s.set_generating("forge", false).await;
        assert!(
            !s.idle_snapshot().await.contains_key("forge"),
            "первый false (без предшествующей генерации) не должен ставить idle"
        );

        // Повторный false (prev=false) — тоже не ставит метку.
        s.set_generating("forge", false).await;
        assert!(
            !s.idle_snapshot().await.contains_key("forge"),
            "повторный false без генерации тоже не ставит idle"
        );
    }

    #[tokio::test]
    async fn idle_snapshot_excludes_needs_attention() {
        // Сессия затихла (idle-метка стоит), но у неё показан Claude prompt
        // (needs_attention=true в self.map) — её НЕЛЬЗЯ отдавать как idle:
        // там нужен ответ пользователя, а не «следующий шаг».
        let s = AttentionState::new();

        // Заводим idle-метку через фронт true→false.
        s.set_generating("forge", true).await;
        s.set_generating("forge", false).await;
        assert!(
            s.idle_snapshot().await.contains_key("forge"),
            "предусловие: idle-метка должна стоять"
        );

        // Поднимаем needs_attention — сессия должна выпасть из idle-снапшота.
        s.set("forge", true).await;
        assert!(
            !s.idle_snapshot().await.contains_key("forge"),
            "needs_attention=true исключает сессию из idle, даже если idle-метка стоит"
        );

        // Снимаем needs_attention — сессия снова считается idle.
        s.set("forge", false).await;
        assert!(
            s.idle_snapshot().await.contains_key("forge"),
            "после снятия needs_attention сессия снова idle"
        );
    }

    #[tokio::test]
    async fn idle_marker_not_set_while_attached() {
        // Регрессия на баг «переключение сессий зажигает свечение каждый раз».
        // Пока сессию смотрит пользователь (attached), фронт is_generating
        // true→false (перерисовка при переключении/просмотре) НЕ должен
        // ставить idle-метку — иначе посещение сессии создаёт ложный
        // «следующий шаг».
        let s = AttentionState::new();
        let mut attached = std::collections::HashSet::new();
        attached.insert("forge".to_string());
        s.update_attached(&attached).await;

        s.set_generating("forge", true).await;
        s.set_generating("forge", false).await; // true→false, но сессия attached
        assert!(
            !s.idle_snapshot().await.contains_key("forge"),
            "attached-сессия не должна попадать в idle (подавление навигационного шума)"
        );
    }

    #[tokio::test]
    async fn detached_session_still_marks_idle() {
        // Контроль к предыдущему тесту: БЕЗ attached фронт true→false штатно
        // ставит idle-метку (фоновая сессия, где Claude реально доработал).
        let s = AttentionState::new();
        s.set_generating("forge", true).await;
        s.set_generating("forge", false).await;
        assert!(
            s.idle_snapshot().await.contains_key("forge"),
            "не-attached сессия после true→false должна стать idle"
        );
    }

    #[tokio::test]
    async fn update_attached_clears_on_detach() {
        // attached_since держит сессию, пока она в наборе, и снимает при detach.
        // После detach сессия снова способна стать idle (новая генерация).
        let s = AttentionState::new();
        let mut attached = std::collections::HashSet::new();
        attached.insert("a".to_string());
        s.update_attached(&attached).await;
        // Пока attached — true→false не ставит idle.
        s.set_generating("a", true).await;
        s.set_generating("a", false).await;
        assert!(!s.idle_snapshot().await.contains_key("a"));

        // Detach: набор пуст.
        s.update_attached(&std::collections::HashSet::new()).await;
        // Новая серия генерации в уже не-attached сессии → idle ставится.
        s.set_generating("a", true).await;
        s.set_generating("a", false).await;
        assert!(
            s.idle_snapshot().await.contains_key("a"),
            "после detach новая серия генерации снова делает сессию idle"
        );
    }

    #[tokio::test]
    async fn idle_snapshot_returns_elapsed_secs() {
        // idle_snapshot отдаёт elapsed().as_secs() — для свежей метки это 0,
        // значение присутствует и детерминировано (без sleep, без флейков).
        let s = AttentionState::new();
        s.set_generating("forge", true).await;
        s.set_generating("forge", false).await;

        let snap = s.idle_snapshot().await;
        assert_eq!(
            snap.get("forge"),
            Some(&0u64),
            "свежая idle-метка → elapsed=0 секунд"
        );
    }

    #[tokio::test]
    async fn update_generation_first_call_returns_false() {
        // Первое наблюдение сессии — нет точки сравнения (prev отсутствует
        // в `last_gen_hash`), `changed = prev != current` не определён,
        // поэтому возвращается false. Хэш сохраняется как prev для
        // следующего тика.
        let s = AttentionState::new();
        let flag = s.update_generation("forge", 42).await;
        assert!(!flag, "первый вызов — нет prev hash, changed=false");
    }

    #[tokio::test]
    async fn update_generation_same_hash_returns_false() {
        // Содержимое не менялось → prev == current → changed=false.
        let s = AttentionState::new();
        s.update_generation("forge", 42).await; // первый тик: stash hash
        let flag = s.update_generation("forge", 42).await;
        assert!(!flag, "одинаковый хэш → changed=false");
    }

    #[tokio::test]
    async fn update_generation_different_hash_returns_true() {
        // Любое изменение хэша (prev != current) → changed=true. Это
        // «сырой» сигнал для дедуп-фазы; финальный флаг is_generating
        // пишется через set_generating только после deduplicate_generating.
        let s = AttentionState::new();
        s.update_generation("forge", 1).await; // первый тик: stash prev=1
        let flag = s.update_generation("forge", 2).await; // prev=1, current=2 → true
        assert!(flag, "разные хэши (1 → 2) должны давать changed=true");
    }

    #[tokio::test]
    async fn update_generation_does_not_write_to_generating_map() {
        // Контракт: update_generation НИКОГДА не пишет в карту `generating`.
        // Единственный writer — set_generating, который вызывается из
        // watcher_loop после дедуп-фазы. Это нужно проверить и для случая
        // changed=false (первый вызов / одинаковый хэш), и для changed=true
        // (разные хэши), чтобы исключить регрессию.
        let s = AttentionState::new();

        // Случай 1: первый вызов (prev отсутствует) → changed=false, но
        // даже false не должен попасть в карту generating.
        s.update_generation("forge", 42).await;
        assert!(
            s.generating_snapshot().await.get("forge").is_none(),
            "после первого update_generation в карте generating не должно быть ключа forge"
        );

        // Случай 2: повторный вызов с тем же хэшем → changed=false.
        s.update_generation("forge", 42).await;
        assert!(
            s.generating_snapshot().await.get("forge").is_none(),
            "после повторного update_generation с тем же хэшем карта generating должна оставаться пустой"
        );

        // Случай 3: вызов с другим хэшем → changed=true, но всё равно
        // карта generating не трогается — финальный флаг идёт через set_generating.
        let flag = s.update_generation("forge", 99).await;
        assert!(flag, "разные хэши → changed=true (sanity check)");
        assert!(
            s.generating_snapshot().await.get("forge").is_none(),
            "даже при changed=true update_generation не должен писать в generating"
        );
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
    /// - `detected` параметризуется — это исходный результат
    ///   `detect_claude_prompt`, который дедуп будет нормализовать.
    fn mk_session(
        name: &str,
        attached: u32,
        group: Option<&str>,
        detected: bool,
    ) -> SessionAttention {
        SessionAttention {
            name: name.to_string(),
            id: format!("${}", name),
            attached,
            session_group: group.map(|s| s.to_string()),
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

    /// Кейс 1: две сессии с одинаковым `pane_hash`, обе `detected=true`, но
    /// БЕЗ `session_group` (None) — после удаления оси `pane_hash` дедуп их
    /// больше НЕ объединяет, обе остаются с `needs_attention=true`.
    ///
    /// Это регрессия на баг «вкладка не светится оранжевым, пока в неё не
    /// перейдёшь»: раньше совпавший `pane_hash` гасил одну из двух
    /// независимых сессий с реальным Claude-prompt'ом.
    #[test]
    fn dedup_same_pane_hash_no_grouping_without_group() {
        // Оба detached (attached=0), session_group=None. Раньше совпадавший
        // pane_hash их объединял и гасил одну — теперь ось pane_hash убрана,
        // значит это две разные независимые сессии и обе светятся.
        let items = vec![
            mk_session("alpha", 0, None, true),
            mk_session("beta", 0, None, true),
        ];

        let out = deduplicate_attention(&items);

        assert!(
            flag_of(&out, "alpha"),
            "alpha сохраняет флаг — независимая сессия (out={out:?})"
        );
        assert!(
            flag_of(&out, "beta"),
            "beta сохраняет флаг — независимая сессия (out={out:?})"
        );
    }

    /// Кейс 2: две сессии БЕЗ session_group (None) — дедуп их не объединяет
    /// (единственная ось — session_group), обе остаются с
    /// `needs_attention=true`.
    #[test]
    fn dedup_no_group_keeps_each_independent() {
        let items = vec![
            mk_session("alpha", 0, None, true),
            mk_session("beta", 0, None, true),
        ];

        let out = deduplicate_attention(&items);

        assert!(
            flag_of(&out, "alpha"),
            "alpha сохраняет флаг (независимая сессия, group=None)"
        );
        assert!(
            flag_of(&out, "beta"),
            "beta сохраняет флаг (независимая сессия, group=None)"
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
            mk_session("alpha", 1, Some("grp1"), true),
            mk_session("beta", 0, Some("grp1"), true),
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

    /// Кейс 4: совпадает `session_group` — дедуп срабатывает по группе
    /// (linked-сессии должны давать одного primary независимо от содержимого
    /// панелей).
    #[test]
    fn dedup_same_group_unifies() {
        let items = vec![
            mk_session("alpha", 0, Some("grp7"), true),
            mk_session("beta", 0, Some("grp7"), true),
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
            mk_session("alpha", 1, Some("grp1"), false),
            mk_session("beta", 0, Some("grp1"), false),
            mk_session("gamma", 0, None, false),
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
            mk_session("alpha", 0, Some("g"), true),
            mk_session("beta", 0, Some("g"), true),
            mk_session("gamma", 0, Some("g"), true),
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
        let items = vec![mk_session("solo", 0, None, true)];
        let out = deduplicate_attention(&items);
        assert!(flag_of(&out, "solo"));
    }

    /// Регрессионный тест на `hash_pane`: одинаковый текст → одинаковый хэш,
    /// разный текст → (с очень высокой вероятностью) разный хэш. Это контракт,
    /// на котором держится сигнал `is_generating` (`update_generation`
    /// сравнивает хэши последних 50 строк pane между тиками).
    #[test]
    fn hash_pane_is_deterministic_and_collision_free_for_distinct_inputs() {
        let h1 = hash_pane("hello world");
        let h2 = hash_pane("hello world");
        let h3 = hash_pane("hello world!");
        assert_eq!(h1, h2, "одинаковый ввод → одинаковый hash");
        assert_ne!(h1, h3, "разный ввод → разный hash (DefaultHasher достаточен)");
    }

    // ===========================================================================
    // Юнит-тесты дедуп-логики is_generating (deduplicate_generating).
    //
    // Структурно-парные тестам `dedup_*` для attention, но работают с другим
    // снимком (`GenSnapshot`) и сырым сигналом `changed` вместо `detected`.
    // ===========================================================================

    /// Вспомогательный конструктор `GenSnapshot` для тестов.
    ///
    /// Аналогично `mk_session`: `id` собирается как `"$<name>"` чтобы быть
    /// уникальным и воспроизводимым (нужно для tie-break по id в
    /// `pick_primary_gen`). `changed` — сырой сигнал от `update_generation`
    /// (prev != current).
    fn mk_gen(
        name: &str,
        attached: u32,
        group: Option<&str>,
        changed: bool,
    ) -> GenSnapshot {
        GenSnapshot {
            name: name.to_string(),
            id: format!("${}", name),
            attached,
            session_group: group.map(|s| s.to_string()),
            changed,
        }
    }

    /// Хелпер: возвращает финальный флаг is_generating для сессии по имени.
    fn gen_flag_of(out: &[(String, bool)], name: &str) -> bool {
        out.iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| *f)
            .unwrap_or_else(|| panic!("session {name} not found in dedup_generating output"))
    }

    /// Кейс 1: две сессии без session_group с одинаковым `gen_hash`, обе
    /// `changed=true` — после удаления оси `gen_hash` дедуп их больше НЕ
    /// объединяет, обе остаются с `is_generating=true`.
    ///
    /// Симметрично `dedup_same_pane_hash_no_grouping_without_group`: без
    /// linked-сессий совпавший `gen_hash` — лишь случайное совпадение двух
    /// независимых сессий, гасить индикатор у одной из них нельзя.
    #[test]
    fn dedup_generating_same_gen_hash_no_grouping_without_group() {
        let items = vec![
            mk_gen("alpha", 0, None, true),
            mk_gen("beta", 0, None, true),
        ];

        let out = deduplicate_generating(&items);

        assert!(
            gen_flag_of(&out, "alpha"),
            "alpha сохраняет флаг — независимая сессия (out={out:?})"
        );
        assert!(
            gen_flag_of(&out, "beta"),
            "beta сохраняет флаг — независимая сессия (out={out:?})"
        );
    }

    /// Кейс 2: две сессии БЕЗ session_group — дедуп их не объединяет
    /// (единственная ось — session_group), обе остаются с
    /// `is_generating=true`.
    #[test]
    fn dedup_generating_no_group_keeps_each_independent() {
        let items = vec![
            mk_gen("alpha", 0, None, true),
            mk_gen("beta", 0, None, true),
        ];

        let out = deduplicate_generating(&items);

        assert!(
            gen_flag_of(&out, "alpha"),
            "alpha сохраняет флаг (независимая сессия, group=None)"
        );
        assert!(
            gen_flag_of(&out, "beta"),
            "beta сохраняет флаг (независимая сессия, group=None)"
        );
    }

    /// Кейс 3: две сессии в одной session_group, обе `changed=true`. У одной
    /// `attached=1`, у другой `0` — primary всегда attached, даже если её
    /// id/имя лексикографически меньше.
    #[test]
    fn dedup_generating_attached_wins_over_detached() {
        // Без правила attached primary стал бы beta (наибольший id). Но
        // attached=1 у alpha → она primary.
        let items = vec![
            mk_gen("alpha", 1, Some("grp1"), true),
            mk_gen("beta", 0, Some("grp1"), true),
        ];

        let out = deduplicate_generating(&items);

        assert!(
            gen_flag_of(&out, "alpha"),
            "alpha (attached=1) должна быть primary в группе grp1"
        );
        assert!(
            !gen_flag_of(&out, "beta"),
            "beta (detached) должна быть подавлена несмотря на больший id"
        );
    }

    /// Кейс 4: совпадает `session_group` — дедуп срабатывает по группе
    /// (linked-сессии должны давать одного primary). Только primary получает
    /// `true`.
    #[test]
    fn dedup_generating_same_group_unifies() {
        let items = vec![
            mk_gen("alpha", 0, Some("grp7"), true),
            mk_gen("beta", 0, Some("grp7"), true),
        ];

        let out = deduplicate_generating(&items);

        let primary_count = out.iter().filter(|(_, f)| *f).count();
        assert_eq!(
            primary_count, 1,
            "linked-сессии одной группы должны схлопываться в одного primary (out={out:?})"
        );
        // Tie-break — наибольший id ($beta > $alpha).
        assert!(gen_flag_of(&out, "beta"), "beta primary по наибольшему id");
        assert!(!gen_flag_of(&out, "alpha"));
    }

    /// Кейс 5: ни одна сессия не имеет `changed=true` — флаги остаются
    /// false, дедуп не зажигает индикатор «из ничего».
    #[test]
    fn dedup_generating_no_change_keeps_all_false() {
        let items = vec![
            mk_gen("alpha", 1, Some("grp1"), false),
            mk_gen("beta", 0, Some("grp1"), false),
            mk_gen("gamma", 0, None, false),
        ];

        let out = deduplicate_generating(&items);

        for (name, flag) in &out {
            assert!(
                !*flag,
                "session {name}: changed=false везде → флаг должен быть false (got true)"
            );
        }
        assert_eq!(
            out.len(),
            3,
            "все сессии должны присутствовать в результате"
        );
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
        // Параллельно собираем `gens` — снапшоты сырого сигнала `is_generating`,
        // которые после цикла пройдут через `deduplicate_generating` (linked-
        // сессии и одинаковый pane должны давать общий флаг).
        let mut collected: Vec<SessionAttention> = Vec::with_capacity(sessions.len());
        let mut gens: Vec<GenSnapshot> = Vec::with_capacity(sessions.len());
        for s in sessions.iter() {
            let pane = crate::tmux::capture_pane(&s.name).await.unwrap_or_default();
            let detected = detect_claude_prompt(&pane);
            let pane_hash = hash_pane(&pane);

            // Generation-detector: захват последних 50 строк pane (включая
            // scrollback) и сравнение хэша с предыдущим тиком. Если
            // содержимое менялось — Claude/процесс что-то рисует.
            //
            // Отдельный capture с lines=50 (а не переиспользование `pane`)
            // выбран сознательно: `capture_pane` хэширует **видимую**
            // часть (без scrollback) ради дедупа prompt-детектора, а здесь
            // нужны именно последние 50 строк истории — иначе быстрый
            // ввод/вывод, выходящий за пределы экрана, мог бы пропуститься.
            //
            // `update_generation` возвращает «сырой» сигнал `changed = prev !=
            // current` и НЕ пишет финальный флаг — финал решается в дедуп-фазе
            // ниже через `deduplicate_generating` + `set_generating`.
            let pane50 = crate::tmux::capture_pane_full(&s.name, 50)
                .await
                .unwrap_or_default();
            let gen_hash = hash_pane(&pane50);
            let changed = attention.update_generation(&s.name, gen_hash).await;

            tracing::debug!(
                session = %s.name,
                group = ?s.session_group,
                pane_hash,
                detected,
                pane_len = pane.len(),
                gen_hash,
                changed,
                "attention check"
            );

            collected.push(SessionAttention {
                name: s.name.clone(),
                id: s.id.clone(),
                attached: s.attached,
                session_group: s.session_group.clone(),
                detected,
            });
            gens.push(GenSnapshot {
                name: s.name.clone(),
                id: s.id.clone(),
                attached: s.attached,
                session_group: s.session_group.clone(),
                changed,
            });
        }

        // Шаг 2a: дедупликация needs_attention. Только primary получает true.
        let final_flags = deduplicate_attention(&collected);
        for (name, flag) in final_flags {
            attention.set(&name, flag).await;
        }

        // Обновляем множество просматриваемых (attached>0) сессий ДО записи
        // is_generating: set_generating подавляет постановку idle-метки для
        // attached-сессий (см. его docstring) — чтобы переключение/просмотр
        // сессий не зажигали голубое свечение «следующего шага».
        {
            use std::collections::HashSet;
            let attached_names: HashSet<String> = sessions
                .iter()
                .filter(|s| s.attached > 0)
                .map(|s| s.name.clone())
                .collect();
            attention.update_attached(&attached_names).await;
        }

        // Шаг 2b: дедупликация is_generating. Свёртка сырых `changed`-сигналов
        // по двум осям группировки (gen_hash + session_group), затем запись
        // финального флага во всех сессиях — в т.ч. явный `false` для тех,
        // кто не получил primary (сбрасывает индикатор при стабилизации).
        let final_gens = deduplicate_generating(&gens);
        for (name, flag) in final_gens {
            attention.set_generating(&name, flag).await;
        }

        // Шаг 3: cleanup карт для исчезнувших сессий. tmux может убить сессию
        // между тиками — без prune карты `last_gen_hash` и `generating` будут
        // вечно хранить мёртвые ключи, а next-tick сравнения по хэшу могут
        // выдать ложный `changed=true` при переиспользовании имени.
        {
            use std::collections::HashSet;
            let live: HashSet<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
            // needs_attention-карта: без prune мёртвая сессия вечно «требует
            // внимания» и переиспользование имени унаследует чужой флаг.
            attention
                .map
                .write()
                .await
                .retain(|k, _| live.contains(k.as_str()));
            attention
                .last_gen_hash
                .write()
                .await
                .retain(|k, _| live.contains(k.as_str()));
            attention
                .generating
                .write()
                .await
                .retain(|k, _| live.contains(k.as_str()));
            attention
                .gen_started_at
                .write()
                .await
                .retain(|k, _| live.contains(k.as_str()));
            attention
                .idle_started_at
                .write()
                .await
                .retain(|k, _| live.contains(k.as_str()));
            attention
                .attached_since
                .write()
                .await
                .retain(|k, _| live.contains(k.as_str()));
        }

        // Сводный лог по индикаторам всех сессий за тик. Уровень debug: тик
        // идёт каждые ~1.5с, на info это засоряло лог и без ротации раздувало
        // файл. На debug включается прицельно через RUST_LOG=...=debug.
        let attn_snap = attention.snapshot().await;
        let gen_snap = attention.generating_snapshot().await;
        let summary: Vec<String> = sessions
            .iter()
            .map(|s| {
                let na = attn_snap.get(&s.name).copied().unwrap_or(false);
                let gn = gen_snap.get(&s.name).copied().unwrap_or(false);
                format!("{}[a={},g={}]", s.name, na as u8, gn as u8)
            })
            .collect();
        if !summary.is_empty() {
            tracing::debug!(tick = %summary.join(" "), "indicator summary");
        }
    }
}

/// Снимок состояния одной сессии для дедупликации в одной итерации
/// `watcher_loop`.
///
/// Содержит всё, что нужно для выбора primary в группе: имя (ключ итогового
/// флага), tmux id (`$0`), attached (число прикреплённых клиентов),
/// `session_group` (имя tmux session-group — единственная ось группировки
/// после удаления `pane_hash`) и `detected` (исходный результат
/// `detect_claude_prompt`).
#[derive(Debug, Clone)]
struct SessionAttention {
    name: String,
    id: String,
    attached: u32,
    session_group: Option<String>,
    detected: bool,
}

/// Снимок состояния одной сессии для дедупликации сигнала `is_generating`
/// в одной итерации `watcher_loop`.
///
/// Структурно-парный к `SessionAttention`, но описывает другой сигнал:
/// «pane менялся за последний тик». Содержит всё, что нужно для выбора
/// primary в группе:
/// - `name` — ключ итогового флага в `AttentionState::generating`;
/// - `id` — tmux id вида `$0` (используется для tie-break — наибольший id
///   лексикографически);
/// - `attached` — число прикреплённых клиентов (приоритет в pick_primary_gen);
/// - `session_group` — имя tmux session-group (`Some(_)` означает linked-
///   сессии, которые меняются одновременно и должны давать общий флаг) —
///   единственная ось группировки после удаления `gen_hash`;
/// - `changed` — «сырой» сигнал от `AttentionState::update_generation`:
///   `prev != current` хэш pane. Это исходное состояние ДО дедупа, которое
///   `deduplicate_generating` свернёт по группам (linked-сессии должны
///   подсветиться только в primary, иначе индикатор горит во всех вкладках
///   одной группы).
#[derive(Debug, Clone)]
struct GenSnapshot {
    name: String,
    id: String,
    attached: u32,
    session_group: Option<String>,
    changed: bool,
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
/// Сессии группируются по **одной** оси:
/// - **session_group** — `Some(g)` означает linked-сессии tmux (`new-session
///   -t`): они делят окна и рендерят одно и то же, поэтому `needs_attention`
///   должен подсвечиваться только у одной из них.
///
/// Ось `pane_hash` (совпадение содержимого видимой панели) **убрана**:
/// текущий `spawn_tmux_attach` делает прямой `tmux attach -t`, а не
/// `new-session -t`, поэтому linked-сессий нет и `session_group` всегда
/// `None`. Совпавший `pane_hash` означал бы лишь случайное совпадение
/// содержимого двух НЕЗАВИСИМЫХ сессий — гасить у одной из них реальный
/// Claude-prompt неверно (вкладка не светилась, пока в неё не перейдёшь).
///
/// Группы объединяются (union-find по `session_group`). Внутри каждой
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

    // Объединяем ТОЛЬКО по session_group (для Some(_)). Ось pane_hash
    // убрана сознательно: linked-сессий больше нет (`spawn_tmux_attach`
    // делает прямой `tmux attach -t`, а не `new-session -t`), поэтому
    // совпавший pane_hash означал лишь случайное совпадение содержимого
    // двух НЕЗАВИСИМЫХ сессий — гасить у одной из них реальный prompt
    // неверно. См. deduplicate_attention doc-комментарий.
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

/// Выбирает primary-индекс среди `members` для дедупликации `is_generating`.
///
/// Структурно-парная функция к `pick_primary`, но критерий «кандидата» —
/// `items[i].changed == true` вместо `detected`. Это даёт независимую от
/// permission-prompt'а ось выбора: дедуп `deduplicate_generating` решает,
/// в какой именно сессии группы зажечь индикатор «генерирует».
///
/// Не пытается обобщить `pick_primary` через предикат — просто копирует
/// и адаптирует, чтобы не ломать существующие тесты `pick_primary` и
/// держать обе функции независимыми друг от друга при будущих изменениях
/// семантики.
///
/// ### Приоритет (применяется по порядку — первое сработавшее правило выбирает primary)
///
/// 1. среди элементов с `changed=true` и `attached>0` — наибольший `id`
///    лексикографически (приоритет тому, что кто-то реально смотрит);
/// 2. иначе среди всех `changed=true` — наибольший `id` (свежесозданная
///    сессия предпочтительнее);
/// 3. fallback: лексикографически наибольшее имя среди `changed=true`
///    (на практике недостижимо, т.к. tmux session id уникальны).
///
/// Возвращает `None` если ни у одной сессии в группе `changed != true` —
/// дедупликация не должна зажигать индикатор «из ничего».
fn pick_primary_gen(items: &[GenSnapshot], members: &[usize]) -> Option<usize> {
    // Кандидаты — только те, у кого changed=true.
    let mut changed_idx: Vec<usize> =
        members.iter().copied().filter(|&i| items[i].changed).collect();
    if changed_idx.is_empty() {
        return None;
    }

    // (a) attached > 0.
    let attached_idx: Vec<usize> = changed_idx
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
    if let Some(&p) = changed_idx
        .iter()
        .max_by(|&&a, &&b| items[a].id.cmp(&items[b].id))
    {
        return Some(p);
    }

    // (c) fallback — лексикографически наибольшее имя.
    changed_idx.sort_by(|&a, &b| items[a].name.cmp(&items[b].name));
    changed_idx.last().copied()
}

/// Дедуплицирует флаги `is_generating` среди сессий одной итерации.
///
/// Возвращает вектор `(session_name, final_flag)` — по одной записи на каждую
/// входную сессию (в т.ч. явный `false` для тех, кто не получил primary).
///
/// Структурно-парная функция к `deduplicate_attention`, работает с сырым
/// сигналом `changed` (вместо `detected`).
///
/// ### Алгоритм
///
/// Сессии группируются по **одной** оси:
/// - **session_group** — `Some(g)` означает linked-сессии tmux: они делят
///   окна и должны давать общий сигнал «генерации».
///
/// Ось `gen_hash` (совпадение последних 50 строк pane) **убрана**
/// симметрично `deduplicate_attention`: без linked-сессий (`spawn_tmux_attach`
/// делает прямой `tmux attach -t`, `session_group` всегда `None`) совпавший
/// `gen_hash` — лишь случайное совпадение содержимого двух НЕЗАВИСИМЫХ сессий,
/// и гасить индикатор «генерации» у одной из них неверно.
///
/// Группы объединяются (union-find по `session_group`). Внутри каждой
/// объединённой группы:
/// - если ни одна сессия не имеет `changed=true` — все остаются `false`;
/// - если хотя бы одна имеет `changed=true` — выбирается **primary** через
///   `pick_primary_gen`, ему `true`, остальным `false`.
///
/// Только primary получает `true`, остальные сессии группы — `false`,
/// даже если их собственный `changed=true`. Это и есть подавление
/// «множественного индикатора» в linked-группе.
fn deduplicate_generating(items: &[GenSnapshot]) -> Vec<(String, bool)> {
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

    // Объединяем ТОЛЬКО по session_group (для Some(_)). Ось gen_hash убрана
    // симметрично deduplicate_attention: без linked-сессий совпавший gen_hash
    // — лишь случайное совпадение последних 50 строк двух НЕЗАВИСИМЫХ сессий,
    // и гасить индикатор «генерации» у одной из них неверно.
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
        let any_changed = members.iter().any(|&i| items[i].changed);
        if !any_changed {
            continue; // все остаются false
        }

        let primary = pick_primary_gen(items, members);
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
