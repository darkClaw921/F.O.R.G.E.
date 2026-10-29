//! Session attention and task activity are independent projections.
//!
//! Every 1.5 seconds the watcher lists all tmux sessions and physical panes,
//! captures each physical pane once, and detects pending native permission or
//! interactive-question overlays. Codex, Claude Code and verified OpenCode
//! layouts are supported. A normal composer, completed response or a question
//! mark in assistant prose does not by itself mean that input is required.
//!
//! The read-only prompt observer lives in attention_prompt.rs and retains an
//! uncertain capture only for the previously observed physical pane/process.
//! Linked aliases share one capture and use the existing primary-session
//! policy; independent sessions with identical prompt text remain independent.
//! It never sends keys, approves commands or answers questions. The existing
//! detect_claude_prompt contract is shared with autoapproval and stays intact.
//!
//! Working stars come from AgentStatsStore task metadata and exact bindings,
//! not terminal hashes. Echo idle is derived only from observed journal ends.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;

#[path = "attention_prompt.rs"]
mod prompts;

/// Сколько секунд непрерывного просмотра сессии (attached>0) достаточно, чтобы
/// считать её «просмотренной» и завершить idle-эпизод «следующего шага»:
/// голубое свечение гаснет и не возвращается при последующем уходе из сессии
/// (новое предложение появится только после новой серии генерации Claude).
///
/// Совпадает по значению с порогом затихания воркера
/// (`forge_echo::next_step::IDLE_THRESHOLD_SECS`), но семантически независим:
/// здесь это «сколько пользователь смотрел сессию», там — «сколько она молчала».
const VIEW_DISMISS_SECS: u64 = 10;

#[cfg(test)]
#[path = "attention_activity_tests.rs"]
mod agent_activity_tests;

/// One coherent journal-derived projection for the session tab DTO.
#[derive(Debug, Clone, Default)]
pub struct SessionAgentActivity {
    pub is_generating: bool,
    pub started_at_ms: Option<i64>,
    pub working_agent_count: usize,
    pub providers: Vec<crate::agents::AgentProvider>,
}
impl SessionAgentActivity {
    pub fn age_secs(&self, now_ms: i64) -> Option<u64> {
        self.is_generating.then(|| {
            now_ms
                .saturating_sub(self.started_at_ms.unwrap_or(now_ms))
                .max(0) as u64
                / 1000
        })
    }
}

/// Разделяемое состояние сессий, в которых агент ждёт разрешения или ответа.
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
    agent_activity: Arc<RwLock<HashMap<String, SessionAgentActivity>>>,
    agent_busy: Arc<RwLock<HashSet<String>>>,
    /// Legacy test-only hash state; production does not infer work from pixels.
    ///
    /// Используется в `update_generation` для получения «сырого» сигнала
    /// `changed = prev != current`. Само поле НЕ хранит финальный флаг
    /// `is_generating` — он пишется отдельно через `set_generating` после
    /// дедупликации в `watcher_loop`.
    ///
    /// При первом наблюдении сессии (нет prev) `changed` всегда `false`:
    /// нет точки сравнения. Это сознательное решение, чтобы избежать ложного
    /// срабатывания на самой первой итерации watcher'а.
    #[cfg(test)]
    last_gen_hash: Arc<RwLock<HashMap<String, u64>>>,
    /// Original journal task start projected onto a monotonic clock.
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
            agent_activity: Arc::new(RwLock::new(HashMap::new())),
            agent_busy: Arc::new(RwLock::new(HashSet::new())),
            #[cfg(test)]
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
    /// Значение поступает из журнала работающей задачи и точной привязки
    /// native session ID к панели. Только primary linked-сессии получает
    /// true; потоки build/tail и перерисовки терминала на сигнал не влияют.
    /// Для согласованного чтения возраста и счётчика используйте
    /// [`Self::agent_activity_snapshot`].
    pub async fn generating_snapshot(&self) -> HashMap<String, bool> {
        self.generating.read().await.clone()
    }
    pub async fn agent_activity_snapshot(&self) -> HashMap<String, SessionAgentActivity> {
        self.agent_activity.read().await.clone()
    }
    /// Silence, unknown probes and detach are visual changes, not task ends.
    async fn apply_agent_activity(
        &self,
        activity: HashMap<String, SessionAgentActivity>,
        busy: HashSet<String>,
        finished: HashSet<String>,
    ) {
        *self.agent_busy.write().await = busy.clone();
        let now_ms = crate::agents::store::now_ms();
        let now = Instant::now();
        *self.generating.write().await = activity
            .iter()
            .map(|(name, a)| (name.clone(), a.is_generating))
            .collect();
        *self.gen_started_at.write().await = activity
            .iter()
            .filter_map(|(name, a)| {
                a.age_secs(now_ms).map(|age| {
                    (
                        name.clone(),
                        now.checked_sub(std::time::Duration::from_secs(age))
                            .unwrap_or(now),
                    )
                })
            })
            .collect();
        *self.agent_activity.write().await = activity;
        let attached = self.attached_since.read().await;
        let mut idle = self.idle_started_at.write().await;
        for name in &busy {
            idle.remove(name);
        }
        for name in finished {
            if !busy.contains(&name) && !attached.contains_key(&name) {
                idle.insert(name, now);
            } else {
                idle.remove(&name);
            }
        }
    }

    /// Устанавливает финальный флаг `is_generating` для одной сессии.
    ///
    /// Legacy helper retained only for tests of the old boolean-front contract.
    /// Production writes coherent journal activity through apply_agent_activity
    /// and never derives Echo idle from a disappearing visual flag.
    ///
    /// Семантика записи аналогична [`AttentionState::set`]: ключ не удаляется
    /// при `false` (всегда `insert`), что позволяет фронтенду надёжно
    /// различать «никогда не видели сессию» (нет ключа) и «видели, флаг
    /// потушен» (есть ключ со значением `false`).
    #[cfg(test)]
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
        let busy = self.agent_busy.read().await;
        idle.iter()
            .filter(|(name, _)| {
                // Исключаем сессии с активным prompt'ом (needs_attention=true).
                !needs_attention.get(*name).copied().unwrap_or(false) && !busy.contains(*name)
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    if !prompt_at_bottom(pane) {
        return false;
    }
    let norm = normalize_ws(pane);
    // Permission разбирается ПОСТРОЧНО, поэтому получает сырую панель:
    // `normalize_ws` схлопывает переносы в пробелы, и всё меню слилось бы в
    // один «пункт». Перенос длинного пункта парсер склеивает сам, а plan и
    // question по-прежнему ищутся по footer'у — им нормализация нужна.
    detect_permission_prompt(pane) || detect_plan_prompt(&norm) || detect_question_prompt(&norm)
}

/// Гейт против «процитированного prompt'а»: настоящий prompt Claude Code
/// рисуется В САМОМ НИЗУ панели — поле ввода и статус-бар на это время
/// скрыты. Текст с теми же маркерами, просто показанный в терминале (diff с
/// тест-фикстурами, cat исходника, лог), стоит ВЫШЕ поля ввода и статус-бара.
///
/// «Низ панели — это prompt», если последние непустые строки — одно из двух:
/// - footer-подсказка («Esc to cancel · Tab to amend · ctrl+e to explain» у
///   permission, «Enter to select · … · Esc to cancel» у plan/question);
/// - пункт меню вида `2. …` / `3. Deny (esc)` — prompt'ы Claude in Chrome
///   отдельного footer'а НЕ рисуют и заканчиваются самим меню (esc указан
///   прямо в пункте Deny). Обнаружено живым кейсом: footer-гейт первой
///   редакции перекрыл авто-апрув настоящих Chrome-prompt'ов.
///
/// Без этого гейта воркер авто-апрува однажды принял diff собственных тестов
/// за AllowBroad-prompt и напечатал «2» в поле ввода чужой сессии. Строки
/// diff'а под пункт меню не маскируются: у них префикс `+`/`-`, и главное —
/// цитата не может оказаться ПОД полем ввода и статус-баром.
///
/// Берутся последние 4 непустые строки: в узком терминале footer переносится
/// по словам, и «Esc to cancel» может уехать со своей строки на соседнюю.
/// Перед этим с конца срезаются виджеты, которые Claude Code рисует ПОД
/// prompt'ом и которые иначе выталкивают footer за это окно, — см.
/// [`trim_bottom_widgets`]. Набор искомых footer-фраз —
/// [`BOTTOM_FOOTER_MARKERS`].
pub(crate) fn prompt_at_bottom(pane: &str) -> bool {
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    // Всё, что ниже виджетов, — не часть prompt'а.
    let end = trim_bottom_widgets(&lines);
    let mut tail: Vec<&str> = lines[..end].iter().rev().take(4).copied().collect();
    // Возвращаем исходный порядок строк: footer мог перенестись («… Esc to» /
    // «cancel»), и склейка в обратном порядке разорвала бы искомую фразу.
    tail.reverse();
    let joined = normalize_ws(&tail.join(" "));
    if BOTTOM_FOOTER_MARKERS.iter().any(|m| joined.contains(m)) {
        return true;
    }
    // Chrome-форма: меню заканчивает панель. Пункт мог перенестись по словам,
    // поэтому смотрим две последние строки — при переносе номер остаётся на
    // предпоследней, а последняя несёт хвост текста пункта.
    tail.iter().rev().take(2).any(|line| {
        let t = line.trim_start().trim_start_matches('❯').trim_start();
        let mut c = t.chars();
        matches!(
            (c.next(), c.next(), c.next()),
            (Some(d), Some('.'), Some(' ')) if d.is_ascii_digit()
        )
    })
}

/// Footer-фразы, по которым [`prompt_at_bottom`] признаёт низ панели
/// prompt'ом.
///
/// - `Esc to cancel` — permission, plan и question в формах с общим footer'ом;
/// - `to edit in Vim` и `shift+tab to approve` — plan-prompt Claude Code 2.1.x
///   (`ctrl+g to edit in Vim · ~/.claude/plans/<файл>.md`), у которого footer'а
///   `Enter to select · Tab/Arrow keys to navigate · Esc to cancel` больше НЕТ.
///   Без них панель с планом не признавалась нижней: последние строки — пункт
///   `3. Tell Claude what to change`, подсказка `shift+tab …` и путь к файлу
///   плана. Живой кейс — сессия с планом «лёгкая CRM», свечение не горело.
///
/// Chrome-форма (панель заканчивается самим меню) footer'а не имеет вовсе и
/// разбирается отдельной веткой [`prompt_at_bottom`].
const BOTTOM_FOOTER_MARKERS: &[&str] = &["Esc to cancel", "to edit in Vim", "shift+tab to approve"];

/// Граница «настоящего низа панели» для [`prompt_at_bottom`]: индекс первой
/// строки хвостовых виджетов в списке непустых строк, либо `lines.len()`,
/// если виджетов внизу нет.
///
/// Claude Code рисует под prompt'ом сразу несколько блоков, и они идут в
/// произвольном сочетании: список задач ([`todo_widget_start`]) и полоса
/// фоновых агентов ([`agents_widget_start`]). Поэтому срез итеративный —
/// каждый распознанный виджет отодвигает границу вверх, и следующий
/// разбирается уже под новой границей. Цикл сходится: обе функции
/// возвращают индекс СТРОГО меньше переданной длины, иначе граница не
/// двигается и цикл выходит.
fn trim_bottom_widgets(lines: &[&str]) -> usize {
    let mut end = lines.len();
    loop {
        let head = &lines[..end];
        let next = todo_widget_start(head).or_else(|| agents_widget_start(head));
        match next {
            Some(i) if i < end => end = i,
            _ => return end,
        }
    }
}

/// Максимум строк полосы фоновых агентов, которые [`agents_widget_start`]
/// готов срезать с конца панели.
///
/// Развёрнутая полоса (`ctrl+o`) показывает по строке на агента; больше
/// пары десятков одновременных агентов в панель физически не помещается, а
/// всё, что глубже, — уже чужой текст.
const AGENTS_WIDGET_MAX_LINES: usize = 24;

/// Маркер строки фонового агента в полосе: `◯ run-phase (+6) …`.
///
/// Взят отдельно от [`TODO_ITEM_MARKERS`] намеренно: `◯` встречается ТОЛЬКО
/// в этой полосе, тогда как `⏺` — ещё и маркер обычного сообщения ассистента
/// в потоке вывода. Именно поэтому строка `⏺` засчитывается лишь как
/// заголовок полосы, и лишь когда под ней уже нашлась хотя бы одна строка
/// `◯` (см. [`agents_widget_start`]).
const AGENT_ROW_MARKER: char = '◯';

/// Маркер строки-заголовка полосы фоновых агентов — основной диалог
/// (`⏺ main`), который Claude Code рисует первой строкой блока.
const AGENT_HEAD_MARKER: char = '⏺';

/// Маркеры строки режима — второй формы заголовка полосы агентов.
///
/// В 2.1.26x полосу, поднятую агентами workflow, Claude Code рисует БЕЗ
/// строки `⏺ main`: над ней оказывается сама строка режима со счётчиком —
/// `⏸ plan mode on · 1 shell · ← 2 agents` или `⏵⏵ auto mode on (shift+tab to
/// cycle) · ← 2 agents · /diff to hide diff`. Пока заголовком считалась
/// только `⏺`, полоса не срезалась, футер prompt'а уезжал за окно из четырёх
/// хвостовых строк, и `prompt_at_bottom` отвечал `false`: свечение не
/// загоралось, авто-апрув молчал. Живой кейс — сессия `main` (apistend) с
/// двумя агентами workflow и висящим MCP-разрешением.
const MODE_LINE_MARKERS: &[char] = &['⏵', '⏸'];

/// Строка-заголовок полосы фоновых агентов: либо `⏺ main`, либо строка
/// режима, в которой Claude Code показывает счётчик агентов.
///
/// Счётчик обязателен: строка режима без агентов — обычный статус-бар, и
/// срезать его нельзя, иначе за виджет примут любой низ экрана.
fn is_agents_head(t: &str) -> bool {
    if t.starts_with(AGENT_HEAD_MARKER) {
        return true;
    }
    t.starts_with(MODE_LINE_MARKERS) && t.contains("agents")
}

/// Индекс первой строки полосы фоновых агентов в хвосте `lines` (только
/// непустые строки), либо `None` — полосы внизу нет.
///
/// Полоса — это заголовок `⏺ main`, под которым идут только строки агентов
/// с маркером [`AGENT_ROW_MARKER`]:
///
/// ```text
///   ⏺ main
///   ◯ run-phase (+6)  Delegating CrmRecurrenceRules…   3h 41m · ↓ 718.3k tokens
/// ```
///
/// Claude Code держит её ПОД областью ввода всё время, пока живы фоновые
/// агенты, из-за чего footer prompt'а уезжает за окно из четырёх хвостовых
/// строк и гейт [`prompt_at_bottom`] переставал признавать prompt нижним:
/// авто-апрув молчал, свечение не загоралось. Живой кейс — сессия `213`
/// (`aira`) с backgrounded `run-phase` и шестью потомками: полоса из двух
/// строк плюс двухстрочный статус-бар выталкивали `Esc to cancel` на пятую
/// позицию снизу.
///
/// Защита от процитированного prompt'а сохраняется: требование «под
/// заголовком ТОЛЬКО строки `◯`» плюс сам заголовок означают, что одиночная
/// строка `⏺ Готово` из потока вывода полосой не считается, а под цитатой в
/// diff/логе всегда идут поле ввода и статус-бар (`───`, `⏵⏵ auto mode on`),
/// которые под маркеры не подходят — срез не происходит и гейт по-прежнему
/// отвечает `false`.
fn agents_widget_start(lines: &[&str]) -> Option<usize> {
    let mut rows = 0usize;
    for (offset, line) in lines.iter().rev().enumerate() {
        if offset >= AGENTS_WIDGET_MAX_LINES {
            return None;
        }
        let t = line.trim_start();
        if t.starts_with(AGENT_ROW_MARKER) {
            rows += 1;
            continue;
        }
        // Заголовок засчитывается только вместе со строками агентов: `⏺` —
        // ещё и маркер обычного сообщения ассистента, и срезать такую строку
        // саму по себе означало бы принять УЖЕ ОТВЕЧЕННЫЙ prompt, оставшийся
        // выше на экране, за активный. То же и для строки режима: без строк
        // `◯` под ней это просто статус-бар.
        if rows > 0 && is_agents_head(t) {
            return Some(lines.len() - 1 - offset);
        }
        return None;
    }
    None
}

/// Максимум строк TODO-виджета, которые [`todo_widget_start`] готов срезать
/// с конца панели.
///
/// Ограничение нужно, чтобы поиск заголовка не уходил вглубь истории вывода:
/// список задач Claude Code показывает целиком, но панель высотой в экран
/// физически не вмещает больше пары десятков пунктов, а всё, что глубже, —
/// уже чужой текст.
const TODO_WIDGET_MAX_LINES: usize = 24;

/// Маркеры пунктов TODO-виджета: `◼` — задача в работе, `◻` — открытая,
/// `☑`/`☐`/`✓`/`✔` — формы «сделано/не сделано» из соседних версий CLI.
///
/// `⏺` и `◯` в набор НЕ входят намеренно: `⏺` — маркер обычного сообщения
/// ассистента в потоке вывода, и срезать такие строки означало бы принять
/// УЖЕ ОТВЕЧЕННЫЙ prompt, оставшийся выше на экране, за активный. Полосу
/// фоновых агентов, где эти два символа и встречаются, разбирает отдельная
/// [`agents_widget_start`] — с обязательным заголовком `⏺` над строками `◯`.
const TODO_ITEM_MARKERS: &[char] = &['◼', '◻', '☑', '☐', '✓', '✔'];

/// Индекс первой строки TODO-виджета в хвосте `lines` (только непустые
/// строки), либо `None` — виджета внизу нет.
///
/// Виджет — это заголовок вида `5 tasks (0 done, 1 in progress, 4 open)`,
/// под которым идут только пункты с маркерами из [`TODO_ITEM_MARKERS`].
/// Claude Code рисует его ПОД prompt'ом, из-за чего footer уезжает за окно
/// из четырёх хвостовых строк и гейт [`prompt_at_bottom`] переставал
/// признавать prompt нижним: авто-апрув молчал, свечение не загоралось
/// (живой кейс — сессия с пятью задачами и висящим `Do you want to proceed?`).
///
/// Требование «под заголовком ТОЛЬКО пункты» и лимит
/// [`TODO_WIDGET_MAX_LINES`] держат защиту от процитированного prompt'а: под
/// цитатой в diff/логе всегда есть поле ввода и статус-бар (рамка `───`,
/// `⏵⏵ auto mode on`), они под маркеры не подходят, срез не происходит и
/// гейт по-прежнему отвечает `false`.
fn todo_widget_start(lines: &[&str]) -> Option<usize> {
    let mut items = 0usize;
    for (offset, line) in lines.iter().rev().enumerate() {
        if offset >= TODO_WIDGET_MAX_LINES {
            return None;
        }
        let t = line.trim_start();
        if t.starts_with(TODO_ITEM_MARKERS) {
            items += 1;
            continue;
        }
        // Заголовок засчитывается только вместе с пунктами: одинокая строка
        // «5 tasks (…)» в выводе команды виджетом не является.
        if items > 0 && is_todo_header(t) {
            return Some(lines.len() - 1 - offset);
        }
        return None;
    }
    None
}

/// Заголовок TODO-виджета — строка, начинающаяся с числа и слова `task`
/// (`5 tasks (0 done, 1 in progress, 4 open)`, `1 task (…)`).
fn is_todo_header(trimmed: &str) -> bool {
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && trimmed[digits..].trim_start().starts_with("task")
}

/// Схлопывает все последовательности whitespace (пробелы, табы, переносы
/// строк) в один пробел. См. раздел «Нормализация whitespace» в
/// [`detect_claude_prompt`] — нужно для устойчивости детекции к переносу
/// длинного footer в узких терминалах.
pub(crate) fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ==========================================================================
// Разбор меню prompt'а
// ==========================================================================

/// Роль пункта меню — что произойдёт, если его выбрать.
///
/// Роль определяется по ТЕКСТУ пункта, а не по его номеру. Номера в Claude
/// Code не стабильны: за месяц отказ переезжал с третьего пункта на
/// четвёртый (добавился «Yes, and switch to auto mode»), а расширяющих «Yes»
/// стало два. Каждая такая редакция ломала детекторы, привязанные к номеру,
/// и авто-апрув молча переставал работать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemRole {
    /// Разовое согласие: `Yes`, `Allow`.
    Approve,
    /// Согласие, расширяющее ЭТО разрешение: `Yes, and don't ask again …`,
    /// `Yes, and always allow access to <путь> from this project`,
    /// `Allow all actions on <host> for this session`.
    ApproveBroad,
    /// Согласие, меняющее РЕЖИМ работы CLI: `Yes, and switch to auto mode`,
    /// пункты плана `auto-accept edits` / `manually approve edits`.
    ///
    /// Автоматически не нажимается никогда, даже в broad-режиме: такой пункт
    /// меняет поведение и для будущих запросов, которых человек ещё не видел.
    ModeSwitch,
    /// Отказ: `No`, `Deny`, `No, keep planning`.
    Refuse,
    /// Ничего из перечисленного — форма незнакомая.
    Unknown,
}

/// Пункт меню: номер (то, что надо нажать) и его роль.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuItem {
    pub(crate) number: u8,
    pub(crate) text: String,
    pub(crate) role: ItemRole,
}

/// Меню prompt'а: пункты в порядке показа плюс признак курсора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Menu {
    pub(crate) items: Vec<MenuItem>,
    /// Был ли в блоке маркер `❯`. У активного меню он есть всегда; его
    /// отсутствие означает обрывок текста, а не вопрос к человеку.
    pub(crate) has_cursor: bool,
}

impl Menu {
    /// Первый пункт заданной роли.
    pub(crate) fn first(&self, role: ItemRole) -> Option<&MenuItem> {
        self.items.iter().find(|i| i.role == role)
    }

    /// Есть ли в меню отказ.
    pub(crate) fn has_refusal(&self) -> bool {
        self.items.iter().any(|i| i.role == ItemRole::Refuse)
    }

    /// Номера идут подряд с единицы — защита от обрывка.
    ///
    /// `2. Yes, and don't ask` + `3. No`, оставшиеся на экране от прошлого
    /// запроса или попавшие в diff, номерами себя и выдают: настоящее меню
    /// всегда начинается с первого пункта.
    pub(crate) fn numbering_is_intact(&self) -> bool {
        self.items
            .iter()
            .enumerate()
            .all(|(i, it)| it.number as usize == i + 1)
    }

    /// Форма Claude in Chrome (`Allow` / `Allow all actions on …`).
    ///
    /// Отдельный признак нужен потому, что у неё НЕТ пункта отказа: выйти
    /// предлагается по `esc`, и требовать отказ, как у `Yes`-форм, нельзя.
    pub(crate) fn is_chrome_form(&self) -> bool {
        self.items
            .first()
            .is_some_and(|i| i.text.trim().to_lowercase().starts_with("allow"))
    }

    /// Меню — это активный вопрос «делать или нет», а не обрывок текста.
    pub(crate) fn is_actionable(&self) -> bool {
        let approves = self
            .items
            .iter()
            .any(|i| matches!(i.role, ItemRole::Approve | ItemRole::ApproveBroad));
        if !approves || !self.has_cursor || !self.numbering_is_intact() {
            return false;
        }
        // У `Yes`-форм отказ есть всегда; у Chrome-формы его нет вовсе.
        self.has_refusal() || (self.is_chrome_form() && self.items.len() >= 2)
    }
}

/// Маркер курсора в меню Claude Code.
const CURSOR_MARKER: char = '❯';

/// Строки, на которых разбор меню останавливается: ниже них пунктов уже нет.
const MENU_STOP_MARKERS: &[&str] = &["Esc to cancel", "Enter to select", "to edit in Vim"];

/// Разбирает меню в НИЖНЕЙ части панели.
///
/// Возвращает `None`, если нумерованных пунктов внизу нет.
///
/// Разбор идёт от последней строки с курсором `❯`, а не от первой найденной:
/// настоящий prompt всегда внизу, а выше на экране легко остаётся чужое такое
/// же меню — `cat` фикстуры, diff, лог прошлого запроса.
///
/// Пункт может занимать НЕСКОЛЬКО строк: длинный путь в «Yes, and always allow
/// access to /very/long/path from this project» переносится по словам. Строки
/// без номера, идущие за пунктом, присоединяются к нему.
pub(crate) fn parse_menu(pane: &str) -> Option<Menu> {
    let lines: Vec<&str> = pane.lines().collect();

    // Якорь — последняя СТРОКА-ПУНКТ, а не последний `❯`: курсором помечено
    // поле ввода под prompt'ом, и поиск по нему уводил разбор ниже меню.
    let is_item = |i: usize| parse_item_head(strip_cursor(lines[i])).is_some();
    let anchor = (0..lines.len()).rev().find(|&i| {
        if MENU_STOP_MARKERS.iter().any(|m| lines[i].contains(m)) {
            return false;
        }
        is_item(i)
    })?;

    // Вниз: продолжения последнего пункта (перенос длинного пути по словам).
    let mut end = anchor;
    for (idx, line) in lines.iter().enumerate().skip(anchor + 1) {
        if parse_item_head(strip_cursor(line)).is_some() || is_continuation(line) {
            end = idx;
            continue;
        }
        break;
    }

    // Вверх: предыдущие пункты. Курсор может стоять НЕ на первом пункте —
    // человек подвинул выбор стрелками, — поэтому подъём идёт по номерам, а
    // не по `❯`. Строка-продолжение между пунктами подъём не прерывает.
    let mut start = anchor;
    let mut i = anchor;
    while i > 0 {
        let j = i - 1;
        if is_item(j) {
            start = j;
            i = j;
            continue;
        }
        if is_continuation(lines[j]) && j > 0 && is_item(j - 1) {
            i = j;
            continue;
        }
        break;
    }

    let mut items: Vec<MenuItem> = Vec::new();
    let has_cursor = lines[start..=end].iter().any(|l| l.contains(CURSOR_MARKER));
    for raw in &lines[start..=end] {
        let t = strip_cursor(raw);
        match parse_item_head(t) {
            Some((number, text)) => items.push(MenuItem {
                number,
                text: text.to_string(),
                role: ItemRole::Unknown,
            }),
            None => {
                if let Some(last) = items.last_mut() {
                    let cont = t.trim();
                    if !cont.is_empty() {
                        last.text.push(' ');
                        last.text.push_str(cont);
                    }
                }
            }
        }
    }

    if items.is_empty() {
        return None;
    }
    for item in &mut items {
        item.role = classify_item(&item.text);
    }
    Some(Menu { items, has_cursor })
}

/// Снимает отступ и маркер курсора: `  ❯ 1. Yes` → `1. Yes`.
fn strip_cursor(line: &str) -> &str {
    line.trim_start()
        .trim_start_matches(CURSOR_MARKER)
        .trim_start()
}

/// Строка-продолжение пункта: непустая, без номера, не рамка и не футер.
fn is_continuation(line: &str) -> bool {
    let t = strip_cursor(line);
    let trimmed = t.trim();
    if trimmed.is_empty() || MENU_STOP_MARKERS.iter().any(|m| trimmed.contains(m)) {
        return false;
    }
    !trimmed.starts_with('─') && !trimmed.starts_with('│') && parse_item_head(t).is_none()
}

/// Начало пункта: `1. текст` (номер 1..9). Возвращает номер и остаток строки.
fn parse_item_head(t: &str) -> Option<(u8, &str)> {
    let mut chars = t.char_indices();
    let (_, first) = chars.next()?;
    let digit = first.to_digit(10)?;
    if digit == 0 {
        return None;
    }
    let (dot_at, dot) = chars.next()?;
    if dot != '.' {
        return None;
    }
    let rest = t[dot_at + 1..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((digit as u8, rest))
}

/// Признаки пункта, меняющего режим CLI, а не одно разрешение.
const MODE_SWITCH_MARKERS: &[&str] = &[
    "switch to auto mode",
    "auto mode handles",
    "auto-accept edits",
    "manually approve edits",
    "use auto mode",
];

/// Признаки расширяющего согласия — оно раздвигает границы ЭТОГО разрешения.
const BROAD_MARKERS: &[&str] = &[
    "don't ask again",
    "dont ask again",
    "always allow",
    "allow all",
    "for this session",
    "from this project",
    "during this session",
];

/// Определяет роль пункта по его тексту.
///
/// Апострофы нормализуются: Claude Code рисует типографский `don’t` (U+2019),
/// а не ASCII-кавычку, и сравнение с `don't ask again` без этого промахивалось
/// — расширяющий пункт выглядел как обычное «Yes».
fn classify_item(text: &str) -> ItemRole {
    let t = text
        .trim()
        .to_lowercase()
        .replace(['\u{2019}', '\u{2018}', '\u{02BC}'], "'");
    if t.starts_with("no") || t.starts_with("deny") || t.starts_with("cancel") {
        return ItemRole::Refuse;
    }
    let affirmative = t.starts_with("yes") || t.starts_with("allow");
    if !affirmative {
        return ItemRole::Unknown;
    }
    // Смена режима проверяется РАНЬШЕ расширения: «Yes, and switch to auto
    // mode · auto mode handles these prompts for you» подходит и под то, и
    // под другое, но нажимать его автоматически нельзя.
    if MODE_SWITCH_MARKERS.iter().any(|m| t.contains(m)) {
        return ItemRole::ModeSwitch;
    }
    if BROAD_MARKERS.iter().any(|m| t.contains(m)) {
        return ItemRole::ApproveBroad;
    }
    ItemRole::Approve
}

/// Permission prompt во всех известных формах:
/// - трёхопционный: `❯ 1. Yes` + `2. Yes,` + `3. No`;
/// - четырёхопционный: `❯ 1. Yes` + `2. Yes,` + `3. Yes, and switch to auto
///   mode` + `4. No`;
/// - двухопционный: `❯ 1. Yes` + `2. No` (короткий `Do you want to proceed?`);
/// - Claude in Chrome: `❯ 1. Allow` + `2. Deny` / `2. Allow all`.
///
/// Двухопционные формы добавлены после живого кейса: prompt `1. Yes / 2. No`,
/// оставленный человеку блоклистом авто-апрува, не зажигал свечение — тот же
/// набор форм распознаёт `auto_approve::classify_prompt`.
fn detect_permission_prompt(pane: &str) -> bool {
    // РАЗБОР МЕНЮ, А НЕ ПОИСК НОМЕРОВ. Прежняя проверка искала дословные
    // `❯ 1. Yes` + `2. Yes,` + `3. No`, и каждая редакция CLI, добавлявшая
    // пункт, гасила и свечение, и авто-апрув: в 2.1.26x между согласием и
    // отказом появился `3. Yes, and switch to auto mode`, отказ уехал на
    // четвёртый пункт, и форма перестала опознаваться. Теперь форма — это
    // «есть согласие И есть отказ», независимо от их номеров и количества.
    parse_menu(pane).is_some_and(|m| m.is_actionable())
}

/// Plan prompt (`ExitPlanMode`) в обеих известных редакциях: старой — footer
/// `Enter to select · Tab/Arrow keys to navigate`, и нумерованной
/// ([`plan_menu_prompt`]), у которой этого footer'а нет.
fn detect_plan_prompt(pane: &str) -> bool {
    (pane.contains("Enter to select") && pane.contains("Tab/Arrow keys to navigate"))
        || plan_menu_prompt(pane)
}

/// Маркеры нумерованного plan-prompt'а: заголовок либо любой пункт меню,
/// каких нет ни у одной permission-формы.
///
/// `manually approve edits` и `auto-accept edits` — пункты «Yes, …» именно
/// плана; `keep planning` — третий пункт редакции с `Ready to code?`;
/// `use auto mode` — редакции 2.1.x. Статус-бар `⏵⏵ auto mode on (shift+tab to
/// cycle)` под них не подходит — там `auto mode on`, а не `use auto mode`.
const PLAN_MENU_MARKERS: &[&str] = &[
    "written up a plan",
    "Ready to code?",
    "use auto mode",
    "auto-accept edits",
    "manually approve edits",
    "keep planning",
];

/// Нумерованный plan-prompt: меню `❯ 1.` плюс маркер из
/// [`PLAN_MENU_MARKERS`]. Принимает НОРМАЛИЗОВАННУЮ панель.
///
/// Отдельная функция, потому что её использует и
/// `auto_approve::classify_prompt` — и использует ДО permission-веток.
/// Это инвариант безопасности, а не косметика: в редакции с `Ready to code?`
/// меню выглядит как `❯ 1. Yes, and auto-accept edits` / `2. Yes, and manually
/// approve edits` / `3. No, keep planning`, то есть подходит под шаблон
/// трёхопционного permission (`2. Yes,` + `3. No`). Пока plan опознавался
/// ТОЛЬКО по footer'у `Enter to select · Tab/Arrow keys to navigate`, которого
/// в новых редакциях нет, авто-апрув принял бы план за permission и нажал бы
/// «Yes» — ровно то, что плану запрещено.
pub(crate) fn plan_menu_prompt(norm: &str) -> bool {
    norm.contains("❯ 1.") && PLAN_MENU_MARKERS.iter().any(|m| norm.contains(m))
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
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    /// Фикстура — file-create prompt Claude Code (другой вариант текста
    /// в options 2 и 3).
    const FILE_CREATE_FIXTURE: &str = "\
Do you want to create mandelbrot.py?
 ❯ 1. Yes
   2. Yes, allow all edits during this session (shift+tab)
   3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
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
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    /// Живой кейс 07.09.2026, сессия `main` (проект apistend): разрешение на
    /// MCP-инструмент, поднятое агентом workflow, и полоса из двух фоновых
    /// агентов ПОД prompt'ом.
    ///
    /// Отличие от [`AGENTS_WIDGET_FIXTURE`] — заголовок полосы. Строки
    /// `⏺ main` здесь НЕТ: над агентами идёт сама строка режима со счётчиком
    /// (`⏸ plan mode on · 1 shell · ← 2 agents`). Пока заголовком считалась
    /// только `⏺`, полоса не срезалась, footer уезжал за окно из четырёх
    /// хвостовых строк — свечение не горело, авто-апрув молчал.
    const AGENTS_MODE_HEAD_FIXTURE: &str = "\
 Tool use · from the \"apistend-research\" workflow

   b24-dev-mcp — Bitrix-Method-Details Tool: (MCP)

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for b24-dev-mcp — Bitrix-Method-Details commands
      in /Users/igorgerasimov/claudeWorkspace/apistend
   3. No

 Esc to cancel · Tab to amend
  …/claudeWorkspace/apistend/design-handoff │ 20%: 171k[▓▓░░░░░░░░]663k │ ~$27.70
  ⏸ plan mode on · 1 shell · ← 2 agents

  ◯ apistend-research  Исследование API Bitrix24/Ozon/WB и точных значений макета
  ◯ apistend-specs     Проверка доступности машиночитаемых спецификаций Ozon/WB
";

    /// Строка режима БЕЗ счётчика агентов — обычный статус-бар. Срезать его
    /// нельзя: иначе за виджет принимается любой низ экрана, и процитированный
    /// в diff'е prompt снова становится «нижним».
    const MODE_LINE_NO_AGENTS_FIXTURE: &str = "\
   ❯ 1. Yes
     2. No
   Esc to cancel
 diff --git a/src/x.rs b/src/x.rs
 +  ❯ 1. Yes
 +  2. No
────────────────────────────────────────────────────────────────────────────────
❯ расскажи про это
────────────────────────────────────────────────────────────────────────────────
  ~/project │ main
  ⏵⏵ auto mode on (shift+tab to cycle)
";

    /// Полоса агентов под prompt'ом со СТРОКОЙ РЕЖИМА вместо `⏺ main`
    /// срезается, и footer снова попадает в окно гейта.
    #[test]
    fn the_agents_strip_is_trimmed_when_the_mode_line_heads_it() {
        assert!(
            prompt_at_bottom(AGENTS_MODE_HEAD_FIXTURE),
            "полоса агентов workflow с заголовком-строкой режима должна срезаться"
        );
        assert!(detect_claude_prompt(AGENTS_MODE_HEAD_FIXTURE));
    }

    /// Строка режима без счётчика агентов заголовком не считается: низ экрана
    /// не срезается, и цитата prompt'а из diff'а не принимается за активную.
    #[test]
    fn a_mode_line_without_agents_is_not_a_widget_head() {
        assert!(!prompt_at_bottom(MODE_LINE_NO_AGENTS_FIXTURE));
        assert!(!detect_claude_prompt(MODE_LINE_NO_AGENTS_FIXTURE));
    }

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

    /// Двухопционный permission (`Do you want to proceed?` без «don't ask
    /// again») — живой кейс: такой prompt, оставленный человеку блоклистом
    /// авто-апрува, не зажигал свечение.
    #[test]
    fn detects_two_option_permission_prompt() {
        let pane = "\
Bash command
  rm -rf src/old
Do you want to proceed?
❯ 1. Yes
  2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";
        assert!(detect_claude_prompt(pane));
    }

    /// Вырожденные панели: пустая, из одних пробелов, однострочная. Гейт
    /// индексирует хвост срезами — паниковать он не должен ни на одной.
    #[test]
    fn bottom_gate_handles_degenerate_panes() {
        for pane in ["", "\n", "   \n\t\n", "just one line"] {
            assert!(!prompt_at_bottom(pane), "pane = {pane:?}");
            assert!(!detect_claude_prompt(pane), "pane = {pane:?}");
        }
    }

    /// `capture-pane` дорисовывает панель до полной высоты окна, поэтому под
    /// footer'ом почти всегда идут пустые строки. Гейт обязан их пропускать —
    /// иначе настоящий prompt не признавался бы «нижним».
    #[test]
    fn bottom_gate_ignores_trailing_blank_lines() {
        let pane = format!("{PROMPT_FIXTURE}\n   \n\t\n\n");
        assert!(prompt_at_bottom(&pane));
        assert!(detect_claude_prompt(&pane));
    }

    /// Живой кейс: полоса фоновых агентов под prompt'ом. Claude Code рисует
    /// её ПОД областью ввода всё время, пока живы backgrounded-агенты, и она
    /// уводила footer за окно из четырёх хвостовых строк — авто-апрув молчал
    /// и свечение не загоралось. Срезается [`agents_widget_start`].
    #[test]
    fn bottom_gate_ignores_agents_widget_below_prompt() {
        let pane = format!(
            "{PROMPT_FIXTURE}⏺ main\n◯ run-phase Фаза 1\n◯ e2e-writer тесты\n◯ security-audit\n"
        );
        assert!(prompt_at_bottom(&pane));
        assert!(detect_claude_prompt(&pane));
    }

    /// Полная раскладка сессии `213` (`aira`): под prompt'ом остаются ДВЕ
    /// строки статус-бара плюс двухстрочная полоса агентов — footer уезжал
    /// на пятую позицию снизу. Именно этот кейс и не подтверждался.
    #[test]
    fn bottom_gate_ignores_agents_widget_under_status_bar() {
        let pane = "\
Bash command · from the general-purpose agent
  cd ~/claudeWorkspace/aira && grep -n '\\.vk\\b' Sources/Crm/*.swift
Do you want to proceed?
❯ 1. Yes
  2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
  …/claudeWorkspace/airo │ feat/aira-native-backend ✗ │ 28%: 242k
  ⏵⏵ auto mode on · 5 shells · ← 2 agents

  ⏺ main
  ◯ run-phase (+6)  Delegating CrmRecurrenceRules   3h 41m · ↓ 718.3k tokens
";
        assert!(prompt_at_bottom(pane));
        assert!(detect_claude_prompt(pane));
    }

    /// Полоса и TODO-виджет одновременно: срез итеративный, граница едет
    /// вверх через оба блока (см. [`trim_bottom_widgets`]).
    #[test]
    fn bottom_gate_ignores_todo_widget_and_agents_widget_together() {
        let pane = format!(
            "{PROMPT_FIXTURE}\
  2 tasks (0 done, 1 in progress, 1 open)
  ◼ Починить гейт
  ◻ Прогнать тесты
  ⏺ main
  ◯ run-phase Фаза 17
"
        );
        assert!(prompt_at_bottom(&pane));
    }

    /// Одинокая строка `⏺ …` внизу — обычное сообщение ассистента, а не
    /// полоса агентов. Срезать её нельзя: выше на экране может висеть УЖЕ
    /// ОТВЕЧЕННЫЙ prompt, и срез принял бы его за активный.
    #[test]
    fn agents_widget_needs_rows_under_head() {
        let pane = format!(
            "{PROMPT_FIXTURE}\n\n⏺ Готово, тесты зелёные\n⏺ Ещё сообщение\n⏺ И ещё\n⏺ И ещё одно\n"
        );
        assert!(!prompt_at_bottom(&pane));
    }

    /// Процитированный prompt: под ним поле ввода, статус-бар и полоса
    /// агентов. Полоса срезается, но открывшийся низ — статус-бар, а не
    /// footer, поэтому гейт по-прежнему отвечает `false`.
    #[test]
    fn agents_widget_trim_keeps_quoted_prompt_protection() {
        let pane = "\
Do you want to proceed?
❯ 1. Yes
  2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
───────────────────────────────────────────────
❯ покажи фикстуру
───────────────────────────────────────────────
  ~/proj │ master ✗ │ ⏵⏵ auto mode on · ← 2 agents
  ⏺ main
  ◯ run-phase Фаза 1
";
        assert!(!prompt_at_bottom(pane));
    }

    /// Живой кейс: plan-prompt редакции 2.1.x. Footer'а `Enter to select ·
    /// Tab/Arrow keys to navigate · Esc to cancel` у него нет — внизу пункт
    /// меню, подсказка `shift+tab …` и путь к файлу плана, поэтому не
    /// срабатывали ни гейт, ни детектор плана: сессия с планом «лёгкая CRM»
    /// не горела оранжевым.
    #[test]
    fn detects_numbered_plan_prompt_without_footer() {
        let pane = "\
 ───────────────────────────────────────────────────────
  Claude has written up a plan and is ready to execute. Would you like to proceed?

  ❯ 1. Yes, and use auto mode
    2. Yes, manually approve edits
    3. Tell Claude what to change
       shift+tab to approve with this feedback

  ctrl+g to edit in Vim · ~/.claude/plans/stateful-frolicking-meadow.md
";
        assert!(prompt_at_bottom(pane));
        assert!(detect_claude_prompt(pane));
    }

    /// Редакция `Ready to code?`: меню (`2. Yes, …` + `3. No, keep planning`)
    /// подходит под шаблон трёхопционного permission — детектор обязан
    /// признать его планом, иначе авто-апрув подтвердит план.
    #[test]
    fn detects_ready_to_code_plan_prompt() {
        let pane = "\
Ready to code?

Here is Claude's plan:
  Переписать роутер на axum
❯ 1. Yes, and auto-accept edits
  2. Yes, and manually approve edits
  3. No, keep planning
";
        assert!(detect_claude_prompt(pane));
        assert!(plan_menu_prompt(&normalize_ws(pane)));
    }

    /// Статус-бар обычной сессии содержит `auto mode on` — под маркер
    /// `use auto mode` он подходить не должен, иначе любая панель с меню
    /// выше по экрану считалась бы планом.
    #[test]
    fn auto_mode_status_bar_is_not_a_plan() {
        let pane = "\
❯ 1. Yes
  2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
  ~/proj │ master ✗ │ ⏵⏵ auto mode on (shift+tab to cycle) · ← 2 agents
";
        assert!(!plan_menu_prompt(&normalize_ws(pane)));
    }

    /// Живой кейс: под prompt'ом Claude Code рисует список задач, и footer
    /// уезжает за окно из четырёх хвостовых строк — свечение не загоралось,
    /// авто-апрув молчал, хотя `Do you want to proceed?` висел на экране.
    #[test]
    fn bottom_gate_ignores_todo_widget_below_prompt() {
        let pane = "\
Bash command
  curl -s https://api.telegram.org/bot$t/getMe
Do you want to proceed?
❯ 1. Yes
  2. No
 Esc to cancel · Tab to amend · ctrl+e to explain

  5 tasks (0 done, 1 in progress, 4 open)
  ◼ Изучить репозитории anti_chaos и hormonal_bot
  ◻ Подготовить сервер (Docker, базовые пакеты)
  ◻ Развернуть anti_chaos бот (@antichaos_check_bot)
  ◻ Развернуть hormonal_bot (@powercouplesyncbot)
  ◻ Найти исходники HR-нумеролог бота (@HRnumerolog_bot)
";
        assert!(prompt_at_bottom(pane));
        assert!(detect_claude_prompt(pane));
    }

    /// TODO-виджет под Chrome-prompt'ом (у него нет footer'а — панель
    /// заканчивалась самим меню): после среза виджета внизу снова меню.
    #[test]
    fn bottom_gate_ignores_todo_widget_below_chrome_prompt() {
        let pane = "\
Claude in Chrome wants to navigate
❯ 1. Allow
  2. Deny (esc)
  2 tasks (0 done, 1 in progress, 1 open)
  ◼ Открыть отчёт
  ◻ Снять скриншот
";
        assert!(prompt_at_bottom(pane));
        assert!(detect_claude_prompt(pane));
    }

    /// Срез TODO-виджета не должен открывать дорогу процитированному
    /// prompt'у: под цитатой остаются поле ввода и статус-бар, они виджетом
    /// не являются — гейт обязан молчать.
    #[test]
    fn todo_widget_strip_keeps_quoted_prompt_rejected() {
        let pane = "\
+❯ 1. Allow
+  2. Allow all actions on host for this session
────────────────────────────────
❯ покажи diff
────────────────────────────────
  ~/proj │ master ✗ │ ⏵⏵ auto mode on (shift+tab to cycle)
  3 tasks (1 done, 1 in progress, 1 open)
  ◼ Сделать diff
  ◻ Проверить тесты
";
        assert!(!prompt_at_bottom(pane));
        assert!(!detect_claude_prompt(pane));
    }

    /// Одинокая строка «N tasks (…)» в выводе команды — не виджет: без
    /// пунктов под ней срез не срабатывает, и вывод не выдаёт себя за prompt.
    #[test]
    fn todo_header_without_items_is_not_a_widget() {
        let pane = format!(
            "{PROMPT_FIXTURE}$ ./report.sh\nbuilding report\nreport written\ndone\n5 tasks (0 done, 5 open)\n"
        );
        assert!(!prompt_at_bottom(&pane));
    }

    /// Процитированный prompt (diff/cat с маркерами меню) НАД полем ввода и
    /// статус-баром — не настоящий prompt: footer'а внизу нет, детект молчит.
    /// Живой кейс: воркер авто-апрува принял diff собственных тест-фикстур за
    /// AllowBroad и напечатал «2» в поле ввода чужой сессии.
    #[test]
    fn ignores_quoted_prompt_above_input_box() {
        let pane = "\
+❯ 1. Allow
+  2. Allow all actions on host for this session
+Do you want to proceed?
+❯ 1. Yes
+  2. No
───────────────
❯ 
───────────────
  ~/project │ master │ 30%: 257k
  ⏵⏵ accept edits on
";
        assert!(!detect_claude_prompt(pane));
    }

    /// Prompt'ы Claude in Chrome: `Allow/Deny` и `Allow/Allow all`.
    #[test]
    fn detects_chrome_allow_prompts() {
        assert!(detect_claude_prompt(
            "Claude in Chrome wants to read your tabs\n❯ 1. Allow\n  2. Deny (esc)\n"
        ));
        assert!(detect_claude_prompt(
            "Claude in Chrome wants to navigate\n❯ 1. Allow\n  2. Allow all actions on host for this session\n"
        ));
        // Трёхопционная форма Allow / Allow all / Deny — тоже без footer'а.
        assert!(detect_claude_prompt(
            "Claude in Chrome wants to navigate and take a screenshot on host\n❯ 1. Allow\n  2. Allow all actions on host for this session\n  3. Deny (esc)\n"
        ));
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
        assert!(!detect_claude_prompt(
            "❯ 1. Yes\n2. Yes, and don't ask again"
        ));
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
            !s.generating_snapshot().await.contains_key("forge"),
            "после первого update_generation в карте generating не должно быть ключа forge"
        );

        // Случай 2: повторный вызов с тем же хэшем → changed=false.
        s.update_generation("forge", 42).await;
        assert!(
            !s.generating_snapshot().await.contains_key("forge"),
            "после повторного update_generation с тем же хэшем карта generating должна оставаться пустой"
        );

        // Случай 3: вызов с другим хэшем → changed=true, но всё равно
        // карта generating не трогается — финальный флаг идёт через set_generating.
        let flag = s.update_generation("forge", 99).await;
        assert!(flag, "разные хэши → changed=true (sanity check)");
        assert!(
            !s.generating_snapshot().await.contains_key("forge"),
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
        assert_eq!(
            out.len(),
            3,
            "все сессии должны присутствовать в результате"
        );
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
        assert_ne!(
            h1, h3,
            "разный ввод → разный hash (DefaultHasher достаточен)"
        );
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
    fn mk_gen(name: &str, attached: u32, group: Option<&str>, changed: bool) -> GenSnapshot {
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

/// Refresh all-session attention and journal-derived activity every 1500ms.
/// The prompt observer inventories all windows/panes once, deduplicates shared
/// physical pane captures, and aggregates waiting permissions/questions. A
/// failed read preserves only that pane's known signal; normal answered output
/// clears it. Copy mode is unknown and cannot create attention from history.
/// This pass is independent of the autoapproval toggle and statistics config.
/// Existing linked-primary selection and explicit-only Echo idle are retained.
/// Session-inventory failure preserves the previous coherent state until the
/// next successful tick; deleted sessions are pruned after successful listing.
pub async fn watcher_loop(attention: Arc<AttentionState>, agents: crate::agents::AgentStatsStore) {
    use std::time::Duration;
    let mut observer = AgentActivityObserver::new(crate::agents::store::now_ms());
    let mut prompt_observer = prompts::Observer::default();

    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let sessions = match crate::tmux::list_sessions().await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::debug!(%error,"attention session inventory unavailable");
                continue;
            }
        };

        // Terminal capture is used only by the existing orange prompt detector.
        // Journal activity is projected after the independent prompt pass.
        let collected = prompt_observer.collect(&sessions, &attention.snapshot().await).await;
        let mut gens: Vec<GenSnapshot> = Vec::with_capacity(sessions.len());
        for s in sessions.iter() {
            gens.push(GenSnapshot {
                name: s.name.clone(),
                id: s.id.clone(),
                attached: s.attached,
                session_group: s.session_group.clone(),
                changed: false,
            });
        }

        // Шаг 2a: дедупликация needs_attention. Только primary получает true.
        let final_flags = deduplicate_attention(&collected);
        for (name, flag) in final_flags {
            attention.set(&name, flag).await;
        }

        // Viewing a session suppresses Echo idle even after a real task end.
        {
            use std::collections::HashSet;
            let attached_names: HashSet<String> = sessions
                .iter()
                .filter(|s| s.attached > 0)
                .map(|s| s.name.clone())
                .collect();
            attention.update_attached(&attached_names).await;
        }

        let live = agents.snapshot().await;
        let recent = agents
            .recent_tasks(crate::agents::store::now_ms() - 24 * 3_600_000)
            .await;
        let (activity, busy, finished) = observer.update(&gens, &live, &recent);
        attention
            .apply_agent_activity(activity, busy, finished)
            .await;

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
            #[cfg(test)]
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
/// native interactive prompt detection).
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
/// «на панели работает задача агента». Содержит всё для выбора
/// primary в группе:
/// - `name` — ключ итогового флага в `AttentionState::generating`;
/// - `id` — tmux id вида `$0` (используется для tie-break — наибольший id
///   лексикографически);
/// - `attached` — число прикреплённых клиентов (приоритет в pick_primary_gen);
/// - `session_group` — имя tmux session-group (`Some(_)` означает linked-
///   сессии, которые меняются одновременно и должны давать общий флаг) —
///   единственная ось группировки после удаления `gen_hash`;
/// - `changed` — исходное наличие работающей задачи ДО дедупа, которое
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

/// A completion is news only after this observer actually saw that task work.
/// Historical replay, quiet waiting and unavailable sources never create idle.
struct AgentActivityObserver {
    started_at_ms: i64,
    seen: HashMap<String, (i64, HashSet<String>)>,
    finished: HashSet<String>,
}
impl AgentActivityObserver {
    fn new(started_at_ms: i64) -> Self {
        Self {
            started_at_ms,
            seen: HashMap::new(),
            finished: HashSet::new(),
        }
    }
    fn update(
        &mut self,
        sessions: &[GenSnapshot],
        snapshot: &crate::agents::AgentSnapshot,
        recent: &[crate::agents::AgentTask],
    ) -> (
        HashMap<String, SessionAgentActivity>,
        HashSet<String>,
        HashSet<String>,
    ) {
        let live_names: HashSet<_> = sessions.iter().map(|s| s.name.clone()).collect();
        let mut activity: HashMap<String, SessionAgentActivity> = sessions
            .iter()
            .map(|s| (s.name.clone(), SessionAgentActivity::default()))
            .collect();
        let mut busy = HashSet::new();
        let mut observed = HashSet::new();
        let mut per_session: HashMap<
            String,
            (HashSet<String>, BTreeSet<crate::agents::AgentProvider>),
        > = HashMap::new();
        for task in snapshot
            .live
            .iter()
            .chain(&snapshot.waiting)
            .chain(&snapshot.unknown)
        {
            observed.insert(task.session_id.clone());
            let names: HashSet<String> = task
                .bindings
                .iter()
                .map(|b| b.tmux_session.clone())
                .filter(|name| live_names.contains(name))
                .collect();
            busy.extend(names.iter().cloned());
            self.seen
                .insert(task.session_id.clone(), (task.started_at_ms, names.clone()));
            if task.state != crate::agents::AgentTaskState::Working {
                continue;
            }
            for name in names {
                let a = activity.get_mut(&name).expect("known session");
                a.started_at_ms = Some(
                    a.started_at_ms
                        .map_or(task.started_at_ms, |at| at.min(task.started_at_ms)),
                );
                let (ids, providers) = per_session.entry(name).or_default();
                ids.insert(task.id.clone());
                providers.insert(task.provider);
            }
        }
        for (name, (ids, providers)) in per_session {
            let a = activity.get_mut(&name).expect("known session");
            a.working_agent_count = ids.len();
            a.providers = providers.into_iter().collect();
            a.is_generating = !ids.is_empty();
        }
        let flags: HashMap<_, _> = deduplicate_generating(
            &sessions
                .iter()
                .cloned()
                .map(|mut s| {
                    s.changed = activity[&s.name].is_generating;
                    s
                })
                .collect::<Vec<_>>(),
        )
        .into_iter()
        .collect();
        for (name, a) in &mut activity {
            if !flags.get(name).copied().unwrap_or(false) {
                *a = SessionAgentActivity::default();
            }
        }
        // A disappeared unended task was forgotten/expired/offline, not done.
        let open_sources: HashSet<_> = recent
            .iter()
            .filter(|t| t.ended_at_ms.is_none())
            .map(|t| t.session_id.clone())
            .collect();
        let known_sources: HashSet<_> = recent.iter().map(|t| t.session_id.clone()).collect();
        self.seen.retain(|id, _| {
            observed.contains(id) || known_sources.contains(id) && !open_sources.contains(id)
        });
        let mut ended_names = HashSet::new();
        for task in recent {
            let Some(end) = task.ended_at_ms else {
                continue;
            };
            if task.completed.is_none()
                || end < self.started_at_ms
                || !self.finished.insert(task.id.clone())
            {
                continue;
            }
            if let Some((started, names)) = self.seen.get(&task.session_id) {
                if end >= *started {
                    ended_names.extend(
                        names
                            .iter()
                            .filter(|name| live_names.contains(*name) && !busy.contains(*name))
                            .cloned(),
                    );
                }
            }
        }
        let finished = deduplicate_generating(
            &sessions
                .iter()
                .cloned()
                .map(|mut s| {
                    s.changed = ended_names.contains(&s.name);
                    s
                })
                .collect::<Vec<_>>(),
        )
        .into_iter()
        .filter_map(|(name, flag)| flag.then_some(name))
        .collect();
        let recent_ids: HashSet<_> = recent.iter().map(|t| t.id.as_str()).collect();
        self.finished.retain(|id| recent_ids.contains(id.as_str()));
        (activity, busy, finished)
    }
}

/// Хэширует содержимое панели в стабильный `u64`.
///
/// Использует `std::collections::hash_map::DefaultHasher`: достаточно для
/// эквивалентности «один и тот же текст → один хэш» в рамках одного процесса.
/// Криптостойкость не требуется.
#[cfg(test)]
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
        let mut by_group: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
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
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
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
    let mut detected_idx: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&i| items[i].detected)
        .collect();
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
    let mut changed_idx: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&i| items[i].changed)
        .collect();
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
        let mut by_group: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
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
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
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
