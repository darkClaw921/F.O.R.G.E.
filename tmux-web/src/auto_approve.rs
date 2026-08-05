//! auto_approve — автоматическое подтверждение интерактивных prompt'ов
//! Claude Code в tmux-сессиях.
//!
//! # Назначение
//!
//! `attention::detect_claude_prompt` умеет **замечать** prompt и зажигать
//! оранжевое свечение на карточке сессии, но нажимать «1» всё равно должен
//! человек. При работе с несколькими сессиями агент простаивает на тривиальных
//! подтверждениях (`Bash command … Do you want to proceed?`, «Claude in Chrome
//! wants to navigate…»). Этот модуль закрывает разрыв: фоновый воркер
//! распознаёт prompt в панели и сам отправляет выбор через
//! [`crate::tmux::send_key`].
//!
//! # Инвариант «нулевая конфигурация = поведение как до фичи»
//!
//! Фича **opt-in**: `UserSettings::auto_approve_enabled` по умолчанию `false`,
//! и при выключенном флаге [`run`] не делает НИ ОДНОГО tmux-вызова (проверка
//! флага стоит до `list_sessions`). Это тот же инвариант, что у
//! `next_step_enabled` — см. шапку `user_settings.rs`.
//!
//! # Что подтверждается, а что нет
//!
//! | Тип ([`PromptKind`]) | Маркеры (после `normalize_ws`) | Выбор |
//! |---|---|---|
//! | `Permission3` | `❯ 1. Yes` + `2. Yes,` + `3. No` | `1`, в broad-режиме `2` |
//! | `Permission2` | `❯ 1. Yes` + `2. No` | всегда `1` |
//! | `AllowDeny` | `❯ 1. Allow` + `2. Deny` | всегда `1` |
//! | `AllowBroad` | `❯ 1. Allow` + `2. Allow all actions on` | `1`, в broad-режиме `2` |
//! | `Question` | `Enter to select` + `↑/↓ to navigate` | `1`, если `auto_approve_questions` |
//! | `Plan` | `Enter to select` + `Tab/Arrow keys to navigate` | **никогда** |
//!
//! Ключевое правило broad-режима: расширенный пункт жмётся ТОЛЬКО там, где
//! второй пункт действительно расширяющий (`2. Yes, and don't ask again`,
//! `2. Allow all actions on <host> for this session`). В двухопционных
//! `Yes/No` и `Allow/Deny` второй пункт — это отказ, и broad-режим на них не
//! распространяется. Отдельные варианты `Permission2`/`AllowDeny` существуют
//! именно ради этого различия.
//!
//! Утверждение плана (`ExitPlanMode`) не подтверждается автоматически никогда
//! и настройкой не управляется: план — это решение о том, что вообще будет
//! сделано, и цена ошибки несопоставима с ценой «Yes» на одну команду.
//!
//! # Блоклист
//!
//! Перед нажатием текст запроса проверяется на опасные подстроки: встроенный
//! [`DEFAULT_DANGEROUS`] плюс пользовательский
//! `UserSettings::auto_approve_blocklist` (дополнение, не замена).
//!
//! Область проверки — блок команды ([`extract_command_region`]): от заголовка
//! («Bash command» / «Claude in Chrome wants …») до списка пунктов. Раньше
//! проверялась вся панель, но это давало ложные блокировки: `rm -rf` в выводе
//! ПРЕДЫДУЩЕЙ команды, оставшемся на экране, блокировал следующий безобидный
//! prompt. Если заголовок не найден (уполз за верх капчи), fallback — вся
//! панель: лишняя строгость дешевле пропущенного `rm -rf`.
//!
//! # LLM-эскалация блоклиста
//!
//! Совпадение с блоклистом больше не приговор: команда отправляется в Claude
//! Sonnet через локальный CLI `claude -p --model sonnet` (используется
//! авторизация Claude Code на машине — API-ключ не нужен) с вопросом «это
//! безобидное удаление временных/тестовых данных или внутренних файлов
//! проекта (восстановимых через git)?». Ответ `approve=true` →
//! prompt подтверждается как обычно (в истории kind помечается `+LLM`);
//! `approve=false`, сбой вызова или отсутствие бинарника `claude` в PATH →
//! старое поведение, prompt остаётся человеку (отсутствие бинарника выключает
//! эскалацию до конца работы процесса). Вердикты кэшируются по хэшу текста
//! команды, чтобы один и тот же висящий prompt не порождал вызов CLI на
//! каждом тике.
//!
//! # Защита от ложных и повторных нажатий
//!
//! - **Стабильность.** Нажатие происходит только если один и тот же хэш
//!   панели наблюдался два тика подряд: TUI перерисовывается кадрами, и по
//!   недорисованному экрану можно попасть не в тот пункт.
//! - **Анти-луп.** Пока prompt непрерывно распознаётся на экране, допускается
//!   максимум 2 нажатия. Первое — цифра. Второе (если prompt всё ещё виден) —
//!   цифра + `Enter`, страховка на случай, если в этой версии CLI цифра лишь
//!   перемещает курсор по пунктам. Дальше сессия считается «залипшей» и
//!   пропускается до тех пор, пока `classify_prompt` не вернёт `None` —
//!   именно исчезновение prompt'а, а не смена содержимого панели, означает
//!   «сработало». Привязывать счётчик к хэшу панели нельзя: нажатая цифра
//!   сама печатается на экран и меняет хэш, так что счётчик обнулялся бы на
//!   каждом нажатии, а сессия, игнорирующая ввод, получала бы «1111…»
//!   (обнаружено живой проверкой, а не рассуждением).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Serialize;

use crate::attention::normalize_ws;
use crate::user_settings::UserSettings;
use crate::AppState;

/// Интервал опроса сессий. Чуть больше тика `attention::watcher_loop`
/// (1.5 с) — авто-апрув не гонка, а лишние `capture-pane` стоят процессов.
const TICK: Duration = Duration::from_millis(2000);

/// Максимум попыток нажатия на один и тот же хэш панели (см. «Анти-луп»).
const MAX_ATTEMPTS: u8 = 2;

/// Встроенный блоклист: подстроки, при которых авто-апрув отказывается
/// подтверждать prompt.
///
/// Сравнение регистронезависимое (панель приводится к нижнему регистру), все
/// записи здесь — уже в нижнем регистре. Список намеренно короткий и грубый:
/// это не полноценный анализ команд, а сито на необратимые операции. Тонкую
/// настройку пользователь делает через `auto_approve_blocklist`.
pub const DEFAULT_DANGEROUS: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "git push --force",
    "git push -f",
    "git reset --hard",
    "git clean -fd",
    "drop table",
    "drop database",
    "truncate table",
    "delete from",
    "mkfs",
    "dd if=",
    "chmod 777",
    "chown -r",
    "shutdown",
    "reboot",
    "kill -9 -1",
    "| sh",
    "| bash",
    "> /dev/sd",
];

/// Сколько записей истории апрувов держим в памяти.
///
/// История нужна для ответа на вопрос «что оно там подтвердило, пока меня не
/// было» — это журнал последних действий, а не архив. Двести записей
/// покрывают несколько дней активной работы и стоят десятки килобайт.
const HISTORY_CAP: usize = 200;

/// Одна запись журнала авто-апрува — то, что показывает кнопка «История»
/// в настройках интерфейса.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRecord {
    /// Unix-время (секунды) момента нажатия.
    pub ts: u64,
    /// Имя tmux-сессии.
    pub session: String,
    /// ИМЯ окна (`#{window_name}`) — то самое «в каком окне». Именно имя, а
    /// не индекс: человек узнаёт своё окно по названию. Может быть пустым,
    /// если tmux его не отдал — тогда UI показывает `pane`.
    pub window: String,
    /// Панель в формате `"win.pane"` — точный адрес нажатия (окно.панель).
    /// Пустая строка означает активную панель сессии (Claude не был опознан
    /// по имени процесса, см. `tmux::list_claude_pane_targets`).
    pub pane: String,
    /// Тип prompt'а (`Permission3`, `AllowDeny`, …) строкой — фронтенд
    /// показывает его как есть.
    pub kind: String,
    /// Что было нажато: `"1"` или `"2"`.
    pub choice: String,
    /// Номер попытки в серии (1 — цифра, 2 — цифра + Enter).
    pub attempt: u8,
    /// Суть запроса: команда из `Bash command`, адрес из запроса Claude in
    /// Chrome или заголовок вопроса — см. [`extract_subject`].
    pub subject: String,
}

/// Журнал последних апрувов. `Arc<RwLock<VecDeque>>` — дёшево клонируется,
/// кладётся в `AppState.approvals`, пишется воркером [`run`] и читается
/// handler'ом `GET /api/auto-approve/history`.
///
/// Только в памяти: при рестарте процесса журнал пуст. Персист не делается
/// осознанно — это оперативный след последних действий, а не аудит, который
/// надо переживать перезапуск; файл пришлось бы ротировать и чистить.
#[derive(Clone, Default)]
pub struct ApprovalHistory {
    inner: Arc<RwLock<VecDeque<ApprovalRecord>>>,
}

impl ApprovalHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавляет запись, вытесняя самую старую при переполнении
    /// [`HISTORY_CAP`].
    pub fn push(&self, rec: ApprovalRecord) {
        let mut q = self.inner.write().expect("ApprovalHistory lock poisoned");
        if q.len() >= HISTORY_CAP {
            q.pop_front();
        }
        q.push_back(rec);
    }

    /// Снимок журнала, НОВЫЕ ЗАПИСИ ПЕРВЫМИ — в таком порядке его и рисует
    /// UI, поэтому разворот делается здесь, а не на фронтенде.
    pub fn snapshot(&self) -> Vec<ApprovalRecord> {
        let q = self.inner.read().expect("ApprovalHistory lock poisoned");
        q.iter().rev().cloned().collect()
    }
}

/// Тип распознанного prompt'а. См. таблицу в шапке модуля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Permission с тремя пунктами: `Yes` / `Yes, and don't ask again` / `No`.
    Permission3,
    /// Permission с двумя пунктами: `Yes` / `No`.
    Permission2,
    /// Claude in Chrome: `Allow` / `Deny`.
    AllowDeny,
    /// Claude in Chrome: `Allow` / `Allow all actions on <host> for this session`.
    AllowBroad,
    /// `AskUserQuestion` — выбор из вариантов.
    Question,
    /// `ExitPlanMode` — утверждение плана.
    Plan,
}

/// Классифицирует содержимое панели.
///
/// Возвращает `None`, если prompt'а нет (обычный вывод shell, работающий
/// Claude без вопроса и т.п.).
///
/// Панель прогоняется через `attention::normalize_ws` — ровно тот же приём,
/// что в `attention::detect_claude_prompt`: Claude Code переносит длинный
/// footer по словам в узком терминале, и без нормализации маркеры
/// разрывались бы переносом.
///
/// Порядок проверок значим: `Plan` и `Question` определяются по footer'у и
/// проверяются первыми, потому что prompt плана тоже содержит нумерованные
/// пункты и без раннего выхода мог бы совпасть с permission-веткой.
pub fn classify_prompt(pane: &str) -> Option<PromptKind> {
    // Гейт против «процитированного prompt'а» — см.
    // `attention::prompt_footer_visible`: маркеры меню, показанные в diff'е
    // или cat'е исходника, не должны приводить к нажатиям в чужой ввод.
    if !crate::attention::prompt_footer_visible(pane) {
        return None;
    }
    let norm = normalize_ws(pane);

    if norm.contains("Enter to select") {
        if norm.contains("Tab/Arrow keys to navigate") {
            return Some(PromptKind::Plan);
        }
        if norm.contains("↑/↓ to navigate") {
            return Some(PromptKind::Question);
        }
    }

    if norm.contains("❯ 1. Allow") {
        if norm.contains("2. Allow all") {
            return Some(PromptKind::AllowBroad);
        }
        if norm.contains("2. Deny") {
            return Some(PromptKind::AllowDeny);
        }
        return None;
    }

    if norm.contains("❯ 1. Yes") {
        // `2. Yes,` — это «Yes, and don't ask again …»: единственная форма, в
        // которой второй пункт расширяет разрешение, а не отказывает.
        if norm.contains("2. Yes,") && norm.contains("3. No") {
            return Some(PromptKind::Permission3);
        }
        if norm.contains("2. No") {
            return Some(PromptKind::Permission2);
        }
        return None;
    }

    None
}

/// Результат [`decide`]: что делать с распознанным prompt'ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Не трогать: план или выключенный автоответ на вопросы.
    Skip,
    /// Нажать пункт (`"1"` или `"2"`).
    Press(&'static str),
    /// Сработал блоклист. Внутри — пункт, который был бы нажат, не будь
    /// блокировки: он нужен LLM-эскалации, чтобы после `approve=true` нажать
    /// ровно то же самое.
    Blocked(&'static str),
}

/// Решает, какую клавишу отправить для распознанного prompt'а.
///
/// Блоклист проверяется ПОСЛЕ раннего выхода по `Plan`/`Question`, но ДО
/// выбора пункта: опасная команда блокирует любой апрув, включая broad.
/// Совпадение возвращается как [`Decision::Blocked`], а не тихий `Skip`, —
/// воркер по нему запускает LLM-эскалацию.
pub fn decide(kind: PromptKind, pane: &str, s: &UserSettings) -> Decision {
    match kind {
        PromptKind::Plan => return Decision::Skip,
        PromptKind::Question => {
            return if s.auto_approve_questions {
                Decision::Press("1")
            } else {
                Decision::Skip
            };
        }
        _ => {}
    }

    let broad =
        s.auto_approve_broad && matches!(kind, PromptKind::Permission3 | PromptKind::AllowBroad);
    let choice = if broad { "2" } else { "1" };

    if is_dangerous(pane, &s.auto_approve_blocklist) {
        return Decision::Blocked(choice);
    }
    Decision::Press(choice)
}

/// Вырезает из панели блок текущего запроса — от заголовка («Bash command» /
/// «Claude in Chrome wants …») до списка пунктов `❯ 1.`.
///
/// Именно по этому блоку идёт проверка блоклиста: всё, что выше заголовка, —
/// прошлый вывод, оставшийся на экране, и `rm -rf` в нём не должен блокировать
/// следующий безобидный prompt. `None` — заголовок не найден (уполз за верх
/// захвата); вызывающий обязан откатиться к проверке всей панели.
pub fn extract_command_region(pane: &str) -> Option<String> {
    let lines: Vec<&str> = pane.lines().collect();
    let menu = lines.iter().position(|l| l.contains("❯ 1."))?;

    // Ближайший к меню заголовок блока — начало текущего запроса.
    let start = (0..menu).rev().find(|&i| {
        let c = clean_line(lines[i]);
        c == "Bash command" || c.starts_with("Claude in Chrome")
    })?;

    let mut out: Vec<String> = Vec::new();
    for line in &lines[start..menu] {
        let c = clean_line(line);
        if c.is_empty() || is_noise(&c) {
            continue;
        }
        out.push(c);
    }
    Some(out.join("\n"))
}

/// Проверяет текст запроса на опасные подстроки: [`DEFAULT_DANGEROUS`] плюс
/// пользовательский список (дополнение, не замена).
///
/// Область проверки — [`extract_command_region`]; если блок не вычленяется,
/// консервативный fallback на всю панель. Текст нормализуется (`normalize_ws`)
/// и приводится к нижнему регистру: команда в prompt'е может быть разорвана
/// переносом рамки TUI, а регистр (`DROP TABLE` / `drop table`) для опасности
/// значения не имеет. Пользовательские записи приходят уже в нижнем регистре —
/// их приводит `user_settings::sanitize_blocklist`; здесь на всякий случай
/// тоже.
fn is_dangerous(pane: &str, extra: &[String]) -> bool {
    let scope = extract_command_region(pane).unwrap_or_else(|| pane.to_string());
    let hay = normalize_ws(&scope).to_lowercase();
    DEFAULT_DANGEROUS.iter().any(|n| hay.contains(n))
        || extra
            .iter()
            .any(|n| !n.trim().is_empty() && hay.contains(&n.to_lowercase()))
}

/// Максимальная длина `subject` в записи истории (в символах).
const SUBJECT_MAX_CHARS: usize = 200;

/// Убирает из строки панели рамку TUI и лишние пробелы.
fn clean_line(line: &str) -> String {
    let stripped: String = line
        .chars()
        .filter(|c| !matches!(c, '─' | '│' | '╭' | '╮' | '╰' | '╯' | '━' | '┃' | '█' | '▌'))
        .collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Служебные строки, которые не несут сути запроса.
fn is_noise(line: &str) -> bool {
    line == "Do you want to proceed?"
        || line.starts_with("Enter to select")
        || line.starts_with("Esc to cancel")
        || line.starts_with('❯')
        || line.starts_with(char::is_numeric) && line.contains(". ")
}

/// Вытаскивает из панели суть запроса — то, что показывается в истории
/// апрувов колонкой «команда».
///
/// Никакого структурированного источника у нас нет: Claude Code — TUI, и
/// единственный доступный текст это то, что нарисовано на экране. Поэтому
/// берутся до трёх последних содержательных строк ПЕРЕД списком пунктов
/// (`❯ 1. …`): для `Bash command` это «Bash command» + сама команда, для
/// Claude in Chrome — «… wants to navigate on <хост>» + URL, для вопроса —
/// его заголовок.
///
/// Отбрасываются рамки TUI ([`clean_line`]), пустые строки и служебные
/// ([`is_noise`]): «Do you want to proceed?», footer'ы и сами пункты меню.
/// Результат обрезается до [`SUBJECT_MAX_CHARS`] символов (по символам, а не
/// по байтам — в командах бывает кириллица).
pub fn extract_subject(pane: &str) -> String {
    let lines: Vec<&str> = pane.lines().collect();
    let idx = lines
        .iter()
        .position(|l| l.contains("❯ 1."))
        .unwrap_or(lines.len());

    let mut picked: Vec<String> = Vec::new();
    for line in lines[..idx].iter().rev() {
        let cleaned = clean_line(line);
        if cleaned.is_empty() || is_noise(&cleaned) {
            continue;
        }
        // Заголовок блока — верхняя граница сути запроса. Всё, что выше
        // («Bash command» / «Claude in Chrome wants …»), к запросу уже не
        // относится: это предыдущий вывод, который просто остался на экране.
        let is_header = cleaned == "Bash command" || cleaned.starts_with("Claude in Chrome");
        picked.push(cleaned);
        if is_header || picked.len() == 3 {
            break;
        }
    }
    picked.reverse();
    let joined = picked.join(" ");

    if joined.chars().count() > SUBJECT_MAX_CHARS {
        let head: String = joined.chars().take(SUBJECT_MAX_CHARS).collect();
        format!("{head}…")
    } else {
        joined
    }
}

/// Текущее unix-время в секундах (для [`ApprovalRecord::ts`]).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Состояние одной сессии между тиками: какой хэш панели видели, сколько
/// тиков подряд он держится и сколько нажатий сделали в текущей серии
/// «prompt всё ещё на экране».
#[derive(Debug, Clone, Copy)]
struct SessionState {
    /// Хэш панели с прошлого тика — вход гарда стабильности.
    pane_hash: u64,
    /// Сколько тиков подряд наблюдается этот хэш (1 = увидели впервые).
    seen: u32,
    /// Сколько нажатий отправлено с тех пор, как на экране непрерывно
    /// распознаётся prompt. Обнуляется, когда [`classify_prompt`] вернул
    /// `None` — то есть prompt действительно ушёл.
    ///
    /// Привязка именно к серии, а НЕ к хэшу панели, — результат живой
    /// проверки: нажатие цифры само меняет экран (символ печатается в поле
    /// ввода), хэш становится новым, и счётчик, привязанный к хэшу,
    /// обнулялся бы на каждом нажатии. В сессии, где prompt по какой-то
    /// причине не реагирует, это дало бы бесконечную очередь «1111…».
    attempts: u8,
}

/// Модель для LLM-эскалации блоклиста — alias для `claude -p --model`.
/// Sonnet: классификация одной команды — не та задача, ради которой стоит
/// платить за Opus.
const LLM_MODEL: &str = "sonnet";

/// Таймаут одного вызова `claude -p`. CLI стартует и отвечает за 5–20 с;
/// вызов идёт внутри тика воркера и на время ожидания задерживает обход
/// остальных панелей — но prompt всё равно никуда не денется до следующего
/// тика.
const LLM_TIMEOUT: Duration = Duration::from_secs(90);

/// Максимум закэшированных вердиктов. При переполнении кэш просто чистится:
/// это защита от бесконечного роста, а не LRU — повторный вопрос по живому
/// prompt'у стоит один вызов CLI.
const VERDICT_CACHE_CAP: usize = 512;

/// Исход обращения к `claude -p` — см. [`llm_blocklist_verdict`].
enum LlmOutcome {
    /// Модель ответила: `true` — апрувить, `false` — оставить человеку.
    Verdict(bool),
    /// Бинарник `claude` не найден в PATH — эскалацию можно выключать до
    /// конца работы процесса, повторные попытки бессмысленны.
    Unavailable,
    /// Разовый сбой (таймаут, не-нулевой exit, кривой вывод) — не кэшировать,
    /// на следующем тике попробуем ещё раз.
    Failed,
}

/// Спрашивает у Sonnet через локальный `claude -p`, безопасно ли подтвердить
/// команду из блоклиста.
///
/// CLI использует авторизацию Claude Code на машине, поэтому API-ключ не
/// нужен. Рабочая директория — временная папка: `claude -p` в корне проекта
/// потянул бы CLAUDE.md и память проекта, что тут не нужно и замедляет ответ.
/// Модель просят ответить строго одним JSON-объектом; из вывода на всякий
/// случай вырезается диапазон от первой `{` до последней `}` (модель может
/// обернуть ответ в markdown-фенс).
async fn llm_blocklist_verdict(command: &str) -> LlmOutcome {
    let prompt = format!(
        "Ты — фильтр безопасности авто-подтверждения команд в среде разработчика.\n\
         Команда попала в блоклист авто-апрува по опасной подстроке (rm -rf, git reset --hard, drop table и т.п.).\n\
         Реши, можно ли подтвердить её автоматически.\n\n\
         approve=true, если операция затрагивает только:\n\
         - временные файлы, тестовые данные, артефакты сборки или scratch-директории \
         (/tmp, target/, dist/, node_modules, кэши, временные переменные вроде $SP);\n\
         - либо внутренние файлы и директории самого проекта — пути внутри рабочей копии \
         репозитория (src/, docs/, конфиги проекта и т.п.): они под git и восстановимы.\n\n\
         approve=false, если команда может задеть: файлы ВНЕ проекта (домашняя директория, \
         системные пути, чужие проекты), сам .git или git-историю (rm -rf .git, git push --force, \
         git reset --hard с потерей чужих коммитов), боевые базы данных (drop/truncate/delete), \
         пользовательские данные или систему — и во всех сомнительных случаях.\n\n\
         Не используй инструменты. Ответь СРАЗУ и ТОЛЬКО одним JSON-объектом вида \
         {{\"approve\": true|false, \"reason\": \"коротко почему\"}} — без markdown и пояснений.\n\n\
         Запрос на подтверждение:\n{command}"
    );

    let result = tokio::time::timeout(
        LLM_TIMEOUT,
        tokio::process::Command::new("claude")
            .arg("-p")
            .arg("--model")
            .arg(LLM_MODEL)
            .arg(prompt)
            .current_dir(std::env::temp_dir())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;

    let out = match result {
        Err(_) => {
            tracing::warn!("auto-approve LLM: claude -p timed out");
            return LlmOutcome::Failed;
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!("auto-approve LLM: `claude` not found in PATH, escalation disabled");
            return LlmOutcome::Unavailable;
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "auto-approve LLM: claude -p spawn failed");
            return LlmOutcome::Failed;
        }
        Ok(Ok(out)) => out,
    };

    if !out.status.success() {
        tracing::warn!(
            status = %out.status,
            stderr = %String::from_utf8_lossy(&out.stderr),
            "auto-approve LLM: claude -p non-zero exit"
        );
        return LlmOutcome::Failed;
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_slice = match (stdout.find('{'), stdout.rfind('}')) {
        (Some(a), Some(b)) if a < b => &stdout[a..=b],
        _ => {
            tracing::warn!(output = %stdout, "auto-approve LLM: no JSON in claude output");
            return LlmOutcome::Failed;
        }
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_slice) else {
        tracing::warn!(output = %stdout, "auto-approve LLM: bad JSON in claude output");
        return LlmOutcome::Failed;
    };
    let Some(approve) = parsed["approve"].as_bool() else {
        tracing::warn!(output = %stdout, "auto-approve LLM: no `approve` field");
        return LlmOutcome::Failed;
    };
    tracing::info!(
        approve,
        reason = %parsed["reason"].as_str().unwrap_or(""),
        "auto-approve LLM verdict"
    );
    LlmOutcome::Verdict(approve)
}

/// Фоновый воркер авто-апрува.
///
/// Спавнится в `main.rs` рядом с `attention::watcher_loop`. Принимает
/// `AppState` целиком (cheap-clone), потому что нужен `user_settings` — этим и
/// отличается от `watcher_loop`, который намеренно берёт только
/// `Arc<AttentionState>`, чтобы не тянуть зависимость на `AppState`.
///
/// Цикл тика:
/// 1. читаем настройки; выключено → спим дальше, tmux не трогаем вообще;
/// 2. `list_sessions`, затем по каждой сессии — `list_claude_pane_targets` и
///    `capture_pane_target` по КАЖДОЙ её Claude-панели (см. «Все окна» ниже);
/// 3. [`classify_prompt`] → [`decide`];
/// 4. гард стабильности (тот же хэш два тика подряд) и анти-луп
///    (не более [`MAX_ATTEMPTS`] нажатий за серию, пока prompt на экране);
/// 5. `send_key_target` в ту же панель + `tracing::info!` для аудита;
/// 6. чистка карты состояний от панелей, которых не было в этом тике.
///
/// # Все окна, а не только активное
///
/// Состояние ключуется парой «сессия + панель», обход идёт по всем панелям
/// всех окон сессии, и нажатие уходит ровно в ту панель, где был найден
/// prompt. Это принципиально: пользователь смотрит одно окно, а Claude
/// спрашивает разрешение в соседнем — и такой prompt должен подтверждаться
/// так же, как в активном.
pub async fn run(state: AppState) {
    let mut seen: HashMap<String, SessionState> = HashMap::new();

    // Флаг доступности LLM-эскалации: гаснет навсегда, если `claude` не
    // найден в PATH — плодить попытки спавна на каждом тике бессмысленно.
    let mut llm_available = true;
    // Кэш вердиктов: хэш текста команды → approve. Живёт всё время работы
    // процесса; один и тот же висящий prompt опрашивает модель ровно один раз.
    let mut verdicts: HashMap<u64, bool> = HashMap::new();

    loop {
        tokio::time::sleep(TICK).await;

        let settings = state.user_settings.get();
        if !settings.auto_approve_enabled {
            // Тумблер выключен — ни одного tmux-вызова. Карту состояний
            // чистим, чтобы после повторного включения не сработал гард
            // стабильности по устаревшему хэшу.
            if !seen.is_empty() {
                seen.clear();
            }
            continue;
        }

        let sessions = crate::tmux::list_sessions().await.unwrap_or_default();
        let mut live_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

        for s in sessions.iter() {
            // Обходим ВСЕ Claude-панели сессии, а не только активную: prompt
            // может висеть в фоновом окне (пользователь работает в соседнем),
            // и такой запрос тоже должен подтверждаться. `capture_pane`
            // здесь не годится — он всегда смотрит активную панель активного
            // окна.
            let panes = crate::tmux::list_claude_pane_targets(&s.name)
                .await
                .unwrap_or_default();

            for pt in panes.iter() {
                let pane_target = &pt.target;
                let pane = crate::tmux::capture_pane_target(&s.name, pane_target)
                    .await
                    .unwrap_or_default();
                if pane.is_empty() {
                    continue;
                }
                let hash = hash_pane(&pane);
                // Ключ состояния — панель, а не сессия: у окон независимые
                // prompt'ы, и счётчик попыток одного не должен глушить другой.
                let key = format!("{}\u{0}{}", s.name, pane_target);
                live_keys.insert(key.clone());

                let entry = seen.entry(key).or_insert(SessionState {
                    pane_hash: hash,
                    seen: 0,
                    attempts: 0,
                });
                if entry.pane_hash == hash {
                    entry.seen = entry.seen.saturating_add(1);
                } else {
                    entry.pane_hash = hash;
                    entry.seen = 1;
                }

                let Some(kind) = classify_prompt(&pane) else {
                    // Prompt ушёл с экрана — серия закончилась, счётчик
                    // попыток можно обнулять. Именно этот момент, а не смена
                    // хэша, означает «наши нажатия сработали».
                    entry.attempts = 0;
                    continue;
                };

                // Гард стабильности: первый тик с новым хэшем пропускаем.
                if entry.seen < 2 {
                    continue;
                }
                if entry.attempts >= MAX_ATTEMPTS {
                    continue;
                }

                // `llm_ok` — прошёл ли prompt через LLM-эскалацию блоклиста
                // (для пометки `+LLM` в истории и логе).
                let (choice, llm_ok) = match decide(kind, &pane, &settings) {
                    Decision::Press(c) => (c, false),
                    Decision::Skip => {
                        tracing::debug!(
                            session = %s.name,
                            pane = %pane_target,
                            kind = ?kind,
                            "auto-approve skipped (plan / questions off)"
                        );
                        continue;
                    }
                    Decision::Blocked(c) => {
                        // Блоклист. Пробуем LLM-эскалацию: вердикт из кэша
                        // или один вызов `claude -p`.
                        let region = extract_command_region(&pane)
                            .unwrap_or_else(|| extract_subject(&pane));
                        let allow = if !llm_available || region.is_empty() {
                            false
                        } else {
                            let vkey = hash_pane(&region);
                            match verdicts.get(&vkey) {
                                Some(&v) => v,
                                None => match llm_blocklist_verdict(&region).await {
                                    LlmOutcome::Verdict(v) => {
                                        if verdicts.len() >= VERDICT_CACHE_CAP {
                                            verdicts.clear();
                                        }
                                        verdicts.insert(vkey, v);
                                        v
                                    }
                                    LlmOutcome::Unavailable => {
                                        llm_available = false;
                                        false
                                    }
                                    // Разовый сбой не кэшируем: на следующем
                                    // тике попробуем ещё раз.
                                    LlmOutcome::Failed => false,
                                },
                            }
                        };
                        if !allow {
                            tracing::debug!(
                                session = %s.name,
                                pane = %pane_target,
                                kind = ?kind,
                                "auto-approve skipped (blocklist, LLM not approving)"
                            );
                            continue;
                        }
                        (c, true)
                    }
                };

                let attempt = entry.attempts + 1;
                entry.attempts = attempt;

                // Жмём строго в ту панель, в которой УВИДЕЛИ prompt.
                // `send_key` здесь был бы ошибкой: он сам выбирает «лучшую»
                // Claude-панель, и в сессии с несколькими окнами клавиша
                // ушла бы не туда.
                if let Err(e) = crate::tmux::send_key_target(&s.name, pane_target, choice).await {
                    tracing::warn!(
                        session = %s.name,
                        pane = %pane_target,
                        error = ?e,
                        "auto-approve send_key failed"
                    );
                    continue;
                }
                // Вторая попытка по тому же prompt'у — цифра не сработала:
                // дожимаем Enter (в этой версии CLI цифра могла лишь
                // переместить курсор на пункт). На первой попытке Enter не
                // шлём — если цифра уже применила выбор, лишний Enter улетел
                // бы в поле ввода пустым сообщением.
                if attempt > 1 {
                    if let Err(e) =
                        crate::tmux::send_key_target(&s.name, pane_target, "Enter").await
                    {
                        tracing::warn!(
                            session = %s.name,
                            pane = %pane_target,
                            error = ?e,
                            "auto-approve Enter failed"
                        );
                    }
                }

                let subject = extract_subject(&pane);
                // `+LLM` в kind помечает апрув, прошедший через эскалацию
                // блоклиста, — в истории видно, что решение принимала модель.
                let kind_label = if llm_ok {
                    format!("{kind:?}+LLM")
                } else {
                    format!("{kind:?}")
                };
                state.approvals.push(ApprovalRecord {
                    ts: now_secs(),
                    session: s.name.clone(),
                    window: pt.window.clone(),
                    pane: pane_target.clone(),
                    kind: kind_label,
                    choice: choice.to_string(),
                    attempt,
                    subject: subject.clone(),
                });

                tracing::info!(
                    session = %s.name,
                    window = %pt.window,
                    pane = %pane_target,
                    kind = ?kind,
                    llm = llm_ok,
                    choice = %choice,
                    attempt,
                    subject = %subject,
                    "auto-approved Claude Code prompt"
                );
            }
        }

        // Prune: панель могла закрыться, сессия — исчезнуть. Ключи, которых
        // не было в этом тике, больше не нужны (иначе переиспользование
        // индекса окна унаследовало бы чужой счётчик попыток).
        seen.retain(|k, _| live_keys.contains(k));
    }
}

/// Хэш содержимого панели — детектор «экран не менялся».
///
/// Своя копия вместо `attention::hash_pane`: там функция приватна и служит
/// дедупу индикаторов, здесь — гарду стабильности. Требование одно и то же
/// («один текст → один хэш в рамках процесса»), криптостойкость не нужна.
fn hash_pane(pane: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    pane.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Bash command … 1. Yes / 2. Yes, and don't ask again / 3. No`.
    const PERMISSION3: &str = "\
Bash command
  ls -la
Do you want to proceed?
❯ 1. Yes
  2. Yes, and don't ask again for: ls commands
  3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    /// Короткий permission без «don't ask again».
    const PERMISSION2: &str = "\
Do you want to proceed?
❯ 1. Yes
  2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    /// Claude in Chrome — чтение вкладок.
    const ALLOW_DENY: &str = "\
Claude in Chrome wants to create a browser window and read your tabs
❯ 1. Allow
  2. Deny (esc)
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    /// Claude in Chrome — навигация с расширенным пунктом.
    const ALLOW_BROAD: &str = "\
Claude in Chrome wants to navigate on 192.168.11.192:3001
http://192.168.11.192:3001/profi48/auto_call_bot
❯ 1. Allow
  2. Allow all actions on 192.168.11.192:3001 for this session
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    const PLAN: &str = "\
Ready to code?
❯ 1. Yes, and auto-accept edits
  2. Yes, and manually approve edits
  3. No, keep planning
Enter to select · Tab/Arrow keys to navigate · Esc to cancel
";

    const QUESTION: &str = "\
Which approach?
❯ 1. Option A
  2. Option B
Enter to select · ↑/↓ to navigate · Esc to cancel
";

    const SHELL: &str = "\
$ ls -la
total 42
drwxr-xr-x  5 user staff 160 Jan  1 00:00 .
";

    /// Тот же PERMISSION3, но с word-wrap длинного второго пункта.
    const PERMISSION3_WRAPPED: &str = "\
❯ 1. Yes
  2. Yes, and don't ask again for: ffmpeg -y -v error -ss 9.0 -i
     /tmp/final.mp4 -frames:v 1 -q:v 3 out.jpg
  3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    fn settings(broad: bool, questions: bool, blocklist: &[&str]) -> UserSettings {
        UserSettings {
            auto_approve_enabled: true,
            auto_approve_broad: broad,
            auto_approve_questions: questions,
            auto_approve_blocklist: blocklist.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn classifies_all_known_forms() {
        assert_eq!(classify_prompt(PERMISSION3), Some(PromptKind::Permission3));
        assert_eq!(classify_prompt(PERMISSION2), Some(PromptKind::Permission2));
        assert_eq!(classify_prompt(ALLOW_DENY), Some(PromptKind::AllowDeny));
        assert_eq!(classify_prompt(ALLOW_BROAD), Some(PromptKind::AllowBroad));
        assert_eq!(classify_prompt(PLAN), Some(PromptKind::Plan));
        assert_eq!(classify_prompt(QUESTION), Some(PromptKind::Question));
    }

    #[test]
    fn ignores_plain_shell_output() {
        assert_eq!(classify_prompt(SHELL), None);
    }

    #[test]
    fn classifies_wrapped_permission() {
        assert_eq!(
            classify_prompt(PERMISSION3_WRAPPED),
            Some(PromptKind::Permission3)
        );
    }

    #[test]
    fn plan_is_never_auto_approved() {
        let permissive = settings(true, true, &[]);
        assert_eq!(decide(PromptKind::Plan, PLAN, &permissive), Decision::Skip);
    }

    #[test]
    fn question_respects_toggle() {
        assert_eq!(
            decide(PromptKind::Question, QUESTION, &settings(false, true, &[])),
            Decision::Press("1")
        );
        assert_eq!(
            decide(PromptKind::Question, QUESTION, &settings(false, false, &[])),
            Decision::Skip
        );
    }

    #[test]
    fn narrow_mode_picks_first_option() {
        let s = settings(false, true, &[]);
        assert_eq!(
            decide(PromptKind::Permission3, PERMISSION3, &s),
            Decision::Press("1")
        );
        assert_eq!(
            decide(PromptKind::AllowBroad, ALLOW_BROAD, &s),
            Decision::Press("1")
        );
    }

    #[test]
    fn broad_mode_picks_second_only_where_it_widens() {
        let s = settings(true, true, &[]);
        assert_eq!(
            decide(PromptKind::Permission3, PERMISSION3, &s),
            Decision::Press("2")
        );
        assert_eq!(
            decide(PromptKind::AllowBroad, ALLOW_BROAD, &s),
            Decision::Press("2")
        );
        // Здесь второй пункт — отказ, broad-режим на него не распространяется.
        assert_eq!(
            decide(PromptKind::Permission2, PERMISSION2, &s),
            Decision::Press("1")
        );
        assert_eq!(
            decide(PromptKind::AllowDeny, ALLOW_DENY, &s),
            Decision::Press("1")
        );
    }

    #[test]
    fn builtin_blocklist_blocks_destructive_command() {
        let pane =
            "Bash command\n  rm -rf /tmp/build\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n Esc to cancel · Tab to amend · ctrl+e to explain\n";
        assert_eq!(classify_prompt(pane), Some(PromptKind::Permission2));
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Blocked("1")
        );
    }

    #[test]
    fn blocklist_is_case_insensitive() {
        let pane = "Bash command\n  psql -c 'DROP TABLE users'\n❯ 1. Yes\n  2. No\n";
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Blocked("1")
        );
    }

    #[test]
    fn custom_blocklist_entry_blocks() {
        let pane = "Bash command\n  ./release.sh\n❯ 1. Yes\n  2. No\n";
        // Без стоп-слова — подтверждаем.
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Press("1")
        );
        // Со стоп-словом — блокируем (кандидат на LLM-эскалацию).
        assert_eq!(
            decide(
                PromptKind::Permission2,
                pane,
                &settings(false, true, &["release.sh"])
            ),
            Decision::Blocked("1")
        );
    }

    #[test]
    fn blocklist_survives_tui_line_wrap() {
        // Команда разорвана переносом рамки — normalize_ws склеивает её.
        let pane = "Bash command\n  git push\n  --force origin master\n❯ 1. Yes\n  2. No\n";
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Blocked("1")
        );
    }

    #[test]
    fn blocklist_ignores_scrollback_above_header() {
        // `rm -rf` в выводе ПРЕДЫДУЩЕЙ команды, оставшемся на экране, не
        // должен блокировать следующий безобидный prompt: проверка идёт по
        // блоку текущего запроса (от заголовка до пунктов), а не по панели.
        let pane = "\
$ cleanup.sh
running rm -rf /tmp/old-build
done
Bash command
  ls -la
Do you want to proceed?
❯ 1. Yes
  2. No
";
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Press("1")
        );
    }

    #[test]
    fn blocklist_falls_back_to_whole_pane_without_header() {
        // Заголовок «Bash command» уполз за верх захвата — консервативный
        // откат к проверке всей панели: строгость дешевле пропущенного rm -rf.
        let pane = "  rm -rf /home/user\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n";
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Blocked("1")
        );
    }

    #[test]
    fn extract_command_region_takes_current_block() {
        let pane = "\
старый вывод rm -rf где-то тут
Bash command
  rm -rf $SP/cand2_render
  docker run --rm reels-maker:latest sh
Do you want to proceed?
❯ 1. Yes
  2. No
";
        let region = extract_command_region(pane).expect("header present");
        assert!(region.starts_with("Bash command"), "region = {region:?}");
        assert!(region.contains("rm -rf $SP/cand2_render"));
        assert!(!region.contains("старый вывод"));
    }

    #[test]
    fn extract_subject_takes_bash_command() {
        let s = extract_subject(PERMISSION3);
        assert_eq!(s, "Bash command ls -la");
    }

    #[test]
    fn extract_subject_stops_at_header() {
        // Всё, что выше «Bash command», — это прошлый вывод на экране, и в
        // историю ему попадать незачем.
        let pane = "какой-то прошлый вывод\nещё строка\nBash command\n  git status\n❯ 1. Yes\n  2. No\n";
        assert_eq!(extract_subject(pane), "Bash command git status");
    }

    #[test]
    fn extract_subject_takes_chrome_target() {
        let s = extract_subject(ALLOW_BROAD);
        assert!(s.contains("192.168.11.192:3001"), "subject = {s:?}");
    }

    #[test]
    fn extract_subject_strips_tui_frame() {
        let pane = "╭──────────────╮\n│ Bash command │\n│   git status │\n╰──────────────╯\n❯ 1. Yes\n  2. No\n";
        let s = extract_subject(pane);
        assert_eq!(s, "Bash command git status");
    }

    #[test]
    fn extract_subject_truncates_long_command() {
        let long = "x".repeat(400);
        let pane = format!("Bash command\n  {long}\n❯ 1. Yes\n  2. No\n");
        let s = extract_subject(&pane);
        assert!(s.chars().count() <= SUBJECT_MAX_CHARS + 1, "len = {}", s.chars().count());
        assert!(s.ends_with('…'));
    }

    #[test]
    fn history_keeps_newest_first_and_caps() {
        let h = ApprovalHistory::new();
        for i in 0..(HISTORY_CAP + 5) {
            h.push(ApprovalRecord {
                ts: i as u64,
                session: format!("s{i}"),
                window: "main".into(),
                pane: "0.0".into(),
                kind: "Permission2".into(),
                choice: "1".into(),
                attempt: 1,
                subject: "ls".into(),
            });
        }
        let snap = h.snapshot();
        assert_eq!(snap.len(), HISTORY_CAP);
        // Новые первыми, старые вытеснены.
        assert_eq!(snap[0].ts, (HISTORY_CAP + 4) as u64);
        assert_eq!(snap[HISTORY_CAP - 1].ts, 5);
    }

    #[test]
    fn hash_pane_is_stable_and_distinguishes() {
        assert_eq!(hash_pane(PERMISSION3), hash_pane(PERMISSION3));
        assert_ne!(hash_pane(PERMISSION3), hash_pane(PERMISSION2));
    }
}
