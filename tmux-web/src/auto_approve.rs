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
//! prompt.
//!
//! Если заголовок не найден, область выбирает [`danger_scope`]:
//! - prompt на правку файла («Do you want to overwrite hub.rs?» и родня) —
//!   ТОЛЬКО строка самого запроса, без diff'а над ней. Исполняемой команды в
//!   таком запросе нет, зато на экране показан код, и любое слово из списка
//!   внутри КОДА (`hub.shutdown().await`, `DELETE FROM` в SQL-строке,
//!   `| bash` в доке) молча блокировало правку — причём навсегда, потому что
//!   отказ LLM кэшировался. Найдено живым кейсом в окне `wt:1785950679`.
//!   Имя файла в строке запроса при этом проверяется: пользовательская запись
//!   вроде `secrets.rs` продолжает работать;
//! - иначе — узкое окно из [`FALLBACK_SCAN_LINES`] содержательных строк над
//!   меню. Это тот же консервативный fallback («заголовок уполз за верх
//!   капчи»), но без всего экрана: команда запроса всегда прилегает к меню,
//!   а выше лежит чужой вывод.
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
//! каждом тике. Положительный вердикт живёт до конца работы процесса,
//! ОТРИЦАТЕЛЬНЫЙ — только [`NEGATIVE_VERDICT_TTL`]: один неудачный ответ
//! модели (или ответ по мусорному тексту) иначе хоронил висящий prompt до
//! рестарта, и никакая смена обстоятельств уже не помогала.
//!
//! # Защита от ложных и повторных нажатий
//!
//! - **Стабильность.** Нажатие происходит только если один и тот же хэш
//!   наблюдался два тика подряд: TUI перерисовывается кадрами, и по
//!   недорисованному экрану можно попасть не в тот пункт. Хэшируется НЕ вся
//!   панель, а блок меню ([`stability_region`]): в окне с работающим
//!   суб-агентом строка прогресса над prompt'ом анимируется быстрее тика, и
//!   по полному хэшу гард не пропускал нажатие никогда.
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
    // `attention::prompt_at_bottom`: маркеры меню, показанные в diff'е
    // или cat'е исходника, не должны приводить к нажатиям в чужой ввод.
    if !crate::attention::prompt_at_bottom(pane) {
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

/// Заголовок блока запроса — верхняя граница текущего prompt'а.
///
/// Сравнение по ПРЕФИКСУ, а не по равенству: prompt, пришедший от суб-агента,
/// подписан источником — `Bash command · from the run-phase agent`. Точное
/// равенство `== "Bash command"` такой заголовок не находило, блок не
/// вычленялся, и блоклист откатывался на всю панель — а там, в выводе
/// предыдущих команд, легко находится `rm -rf`. В результате авто-апрув
/// молча блокировал каждый запрос суб-агента: ровно тот случай, ради
/// которого фича и нужна (человек не смотрит в это окно). Обнаружено живым
/// кейсом в окне `wt:1785950679`.
fn is_request_header(cleaned: &str) -> bool {
    cleaned.starts_with("Bash command") || cleaned.starts_with("Claude in Chrome")
}

/// Строка меню текущего запроса — якорь, от которого пляшут
/// [`extract_command_region`], [`extract_subject`] и [`stability_region`].
///
/// Ищется ПОСЛЕДНЕЕ вхождение `❯ 1.`, а не первое. Настоящий prompt всегда
/// внизу панели (тот же инвариант, что у `attention::prompt_at_bottom`), а
/// выше на экране легко остаётся ЧУЖОЕ такое же меню: `cat` собственных
/// тест-фикстур, diff, лог прошлого запроса. С первым вхождением область
/// проверки блоклиста строилась вокруг цитаты, а команда настоящего запроса
/// в неё не попадала — то есть `rm -rf` в реальном prompt'е мог быть
/// пропущен, если выше на экране висела цитата с меню.
fn menu_line(lines: &[&str]) -> Option<usize> {
    lines.iter().rposition(|l| l.contains("❯ 1."))
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
    let menu = menu_line(&lines)?;

    // Ближайший к меню заголовок блока — начало текущего запроса.
    let start = (0..menu).rev().find(|&i| {
        let c = clean_line(lines[i]);
        is_request_header(&c)
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
/// Область проверки — [`danger_scope`]; `None` оттуда означает «проверять
/// нечего» (prompt на правку файла) и трактуется как «не опасно». Текст
/// нормализуется (`normalize_ws`) и приводится к нижнему регистру: команда в
/// prompt'е может быть разорвана переносом рамки TUI, а регистр
/// (`DROP TABLE` / `drop table`) для опасности значения не имеет.
/// Пользовательские записи приходят уже в нижнем регистре — их приводит
/// `user_settings::sanitize_blocklist`; здесь на всякий случай тоже.
fn is_dangerous(pane: &str, extra: &[String]) -> bool {
    let Some(scope) = danger_scope(pane) else {
        return false;
    };
    let hay = normalize_ws(&scope).to_lowercase();
    DEFAULT_DANGEROUS.iter().any(|n| hay.contains(n))
        || extra
            .iter()
            .any(|n| !n.trim().is_empty() && hay.contains(&n.to_lowercase()))
}

/// Сколько содержательных строк над меню проверяется блоклистом, если
/// заголовок блока («Bash command» / «Claude in Chrome») в захвате не найден.
///
/// Столько же строк берёт [`extract_subject`]: суть запроса всегда прилегает к
/// меню снизу экрана. Раньше здесь была вся панель — и она приносила в
/// проверку прошлый вывод и diff редактируемого файла.
const FALLBACK_SCAN_LINES: usize = 3;

/// Текст, по которому работает блоклист, либо `None` — «команды в запросе нет,
/// проверять нечего».
///
/// Порядок: блок команды → prompt на правку файла (→ `None`) → узкое окно над
/// меню. Полная панель не берётся никогда: см. раздел «Блоклист» в шапке
/// модуля.
fn danger_scope(pane: &str) -> Option<String> {
    if let Some(region) = extract_command_region(pane) {
        return Some(region);
    }
    let lines: Vec<&str> = pane.lines().collect();
    let menu = menu_line(&lines)?;

    let mut picked: Vec<String> = Vec::new();
    for line in lines[..menu].iter().rev() {
        let cleaned = clean_line(line);
        if cleaned.is_empty() || is_noise(&cleaned) {
            continue;
        }
        // Запрос на правку файла: выше по экрану — diff, и он в область
        // проверки не входит. Остаётся сама строка запроса — в ней есть имя
        // файла, по которому пользовательский блоклист (`secrets.rs`) обязан
        // работать.
        if is_edit_request(&cleaned) {
            return Some(cleaned);
        }
        picked.push(cleaned);
        if picked.len() == FALLBACK_SCAN_LINES {
            break;
        }
    }
    picked.reverse();
    Some(picked.join("\n"))
}

/// Строка-заголовок prompt'а на изменение файла — единственная форма, у
/// которой нет исполняемой команды, зато на экране показан diff.
///
/// Формы перечислены явно, а не выводятся из префикса «Do you want to»:
/// им же начинается `Do you want to proceed?` — заголовок ИМЕННО bash-запроса,
/// и трактовать его как «команды нет» было бы дырой в блоклисте.
fn is_edit_request(cleaned: &str) -> bool {
    const FORMS: &[&str] = &[
        "do you want to make this edit to",
        "do you want to create",
        "do you want to overwrite",
        "do you want to write to",
        "do you want to apply this edit to",
    ];
    let lower = cleaned.to_lowercase();
    FORMS.iter().any(|f| lower.starts_with(f))
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
    let idx = menu_line(&lines).unwrap_or(lines.len());

    let mut picked: Vec<String> = Vec::new();
    for line in lines[..idx].iter().rev() {
        let cleaned = clean_line(line);
        if cleaned.is_empty() || is_noise(&cleaned) {
            continue;
        }
        // Заголовок блока — верхняя граница сути запроса. Всё, что выше
        // («Bash command» / «Claude in Chrome wants …»), к запросу уже не
        // относится: это предыдущий вывод, который просто остался на экране.
        let is_header = is_request_header(&cleaned);
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
    /// Хэш блока меню ([`stability_region`]) с прошлого тика — вход гарда
    /// стабильности.
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

/// Срок жизни ОТРИЦАТЕЛЬНОГО вердикта LLM.
///
/// Положительный вердикт кэшируется навсегда: разрешение на конкретный текст
/// команды со временем не портится. Отрицательный — нет: модель могла ответить
/// «нет» по мусорному тексту (см. [`danger_scope`]) или просто неудачно, и
/// вечный кэш означал, что висящий prompt уже никогда не будет переспрошен —
/// до перезапуска процесса. Пять минут — компромисс: человек успевает ответить
/// сам, а если prompt всё ещё висит, модель спрашивают заново, и это ~1 вызов
/// CLI в 5 минут на панель, а не на каждый тик.
const NEGATIVE_VERDICT_TTL: Duration = Duration::from_secs(300);

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
    // Кэш вердиктов: хэш текста команды → (approve, когда получен). Разрешение
    // живёт всё время работы процесса, отказ — `NEGATIVE_VERDICT_TTL`, чтобы
    // один неудачный ответ модели не хоронил висящий prompt до рестарта.
    let mut verdicts: HashMap<u64, (bool, u64)> = HashMap::new();

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
                // Хэш считается по блоку меню, а не по всей панели: над
                // prompt'ом может анимироваться строка суб-агента, и по
                // полному хэшу гард стабильности не срабатывал НИКОГДА (см.
                // `stability_region`).
                let hash = hash_pane(&stability_region(&pane));
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
                            let now = now_secs();
                            // Отказ считается годным только пока не истёк TTL;
                            // просроченный — как будто его и не было.
                            let cached = verdicts.get(&vkey).copied().filter(|&(allow, ts)| {
                                allow || now.saturating_sub(ts) < NEGATIVE_VERDICT_TTL.as_secs()
                            });
                            match cached {
                                Some((v, _)) => v,
                                None => match llm_blocklist_verdict(&region).await {
                                    LlmOutcome::Verdict(v) => {
                                        if verdicts.len() >= VERDICT_CACHE_CAP {
                                            verdicts.clear();
                                        }
                                        verdicts.insert(vkey, (v, now_secs()));
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

/// Сколько строк ВЫШЕ меню `❯ 1.` входит в [`stability_region`] — ровно
/// одна: строка-вопрос («Do you want to make this edit to config.rs?»,
/// «Do you want to proceed?»). Брать больше нельзя: на две строки выше меню
/// в окне с суб-агентом уже стоит анимированная строка статуса
/// («✶ Gesticulating… (1h 16m 12s · ↓ 25.8k tokens)») — ровно то, от чего
/// этот регион и защищает.
const STABILITY_LEAD_LINES: usize = 1;

/// Сколько последних строк берётся, если меню `❯ 1.` в панели не нашлось.
const STABILITY_TAIL_LINES: usize = 6;

/// Часть панели, по которой считается гард стабильности («экран не менялся»).
///
/// Хэшировать ВСЮ панель нельзя: если в окне работает суб-агент, Claude Code
/// рисует над prompt'ом живую строку прогресса — спиннер `⏺ run-phase(…)`,
/// таймер `6m 47s`, счётчик токенов. Она обновляется чаще, чем тик воркера
/// (2 с), поэтому хэш панели отличался КАЖДЫЙ тик, `seen` никогда не
/// доходило до 2, и prompt в таком окне не подтверждался вообще никогда.
/// Обнаружено живым кейсом в worktree-окнах `wt:<ts>`, где типично гоняют
/// `run-phase`: два подряд снятых `capture-pane` отличались ровно одним
/// символом спиннера в строке суб-агента.
///
/// Берётся блок меню: от строки `❯ 1.` минус [`STABILITY_LEAD_LINES`] (чтобы
/// заголовок запроса тоже участвовал — иначе два разных вопроса с
/// одинаковыми пунктами были бы неотличимы) и до конца панели. Если меню не
/// найдено (prompt'а нет — гард всё равно не сработает, но состояние
/// обновляется) — последние [`STABILITY_TAIL_LINES`] строк.
///
/// Результат нормализуется [`normalize_ws`]: перенос по словам в узком
/// терминале не должен считаться изменением экрана.
fn stability_region(pane: &str) -> String {
    let lines: Vec<&str> = pane.lines().collect();
    let start = match menu_line(&lines) {
        Some(menu) => menu.saturating_sub(STABILITY_LEAD_LINES),
        None => lines.len().saturating_sub(STABILITY_TAIL_LINES),
    };
    normalize_ws(&lines[start..].join("\n"))
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
";

    /// Claude in Chrome — навигация с расширенным пунктом.
    const ALLOW_BROAD: &str = "\
Claude in Chrome wants to navigate on 192.168.11.192:3001
http://192.168.11.192:3001/profi48/auto_call_bot
❯ 1. Allow
  2. Allow all actions on 192.168.11.192:3001 for this session
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

    /// Трёхопционная Chrome-форма: `Allow / Allow all … / Deny (esc)` —
    /// живой кейс с navigate+screenshot. Классифицируется как AllowBroad,
    /// footer'а у неё нет — панель заканчивается самим меню.
    #[test]
    fn classifies_chrome_three_option_broad() {
        let pane = "\
Claude in Chrome wants to navigate and take a screenshot on 109.172.47.224:4000
 http://109.172.47.224:4000/
❯ 1. Allow
  2. Allow all actions on 109.172.47.224:4000 for this session
  3. Deny (esc)
";
        assert_eq!(classify_prompt(pane), Some(PromptKind::AllowBroad));
        let s = settings(true, true, &[]);
        assert_eq!(decide(PromptKind::AllowBroad, pane, &s), Decision::Press("2"));
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
    fn blocklist_falls_back_to_lines_above_menu_without_header() {
        // Заголовок «Bash command» уполз за верх захвата — команда всё равно
        // прилегает к меню, и узкое окно её видит.
        let pane = "  rm -rf /home/user\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n";
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Blocked("1")
        );
    }

    #[test]
    fn fallback_scope_does_not_reach_old_output() {
        // Без заголовка берутся только FALLBACK_SCAN_LINES строк над меню:
        // `rm -rf` из прошлого вывода, отделённый от запроса, уже не считается.
        let pane = "\
running rm -rf /tmp/old-build
done
first
second
third
Do you want to proceed?
❯ 1. Yes
  2. No
";
        assert_eq!(
            decide(PromptKind::Permission2, pane, &settings(false, true, &[])),
            Decision::Press("1")
        );
    }

    /// Живой кейс окна `wt:1785950679`: prompt на перезапись файла, а в diff'е
    /// над ним — `hub.shutdown().await`. Слово из блоклиста внутри КОДА не
    /// должно блокировать правку: исполняемой команды в таком запросе нет.
    #[test]
    fn edit_prompt_ignores_blocklist_words_in_diff() {
        let pane = "\
  1490          hub.start(sink.clone(), r).await.unwrap();
  1492          hub.shutdown().await;
  1494          let logged = crate::testlog::since(mark);
 Do you want to overwrite hub.rs?
 ❯ 1. Yes
   2. Yes, allow all edits during this session (shift+tab)
   3. No

 Esc to cancel · Tab to amend
";
        assert_eq!(classify_prompt(pane), Some(PromptKind::Permission3));
        assert_eq!(
            decide(PromptKind::Permission3, pane, &settings(false, true, &[])),
            Decision::Press("1")
        );
    }

    #[test]
    fn edit_prompt_forms_are_recognized() {
        for head in [
            "Do you want to make this edit to config.rs?",
            "Do you want to create prompts.rs?",
            "Do you want to overwrite hub.rs?",
        ] {
            let pane = format!("  delete from users\n {head}\n ❯ 1. Yes\n   2. No\n");
            assert_eq!(
                decide(PromptKind::Permission2, &pane, &settings(false, true, &[])),
                Decision::Press("1"),
                "форма не опознана как правка файла: {head}"
            );
        }
    }

    /// `Do you want to proceed?` начинается так же, но это заголовок
    /// bash-запроса — область не схлопывается до одной строки, и команда
    /// над ней в проверку входит.
    #[test]
    fn proceed_header_is_not_treated_as_edit() {
        let pane = "  rm -rf /home/user\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n";
        let scope = danger_scope(pane).expect("меню есть");
        assert!(scope.contains("rm -rf"), "scope = {scope:?}");
    }

    #[test]
    fn edit_prompt_still_blocks_user_blocklist_hit_on_file_name() {
        // В область проверки входит строка самого запроса — имя файла в ней
        // видно, и пользовательская запись блоклиста по нему срабатывает.
        let pane = "  code\n Do you want to overwrite secrets.rs?\n ❯ 1. Yes\n   2. No\n";
        assert_eq!(
            decide(
                PromptKind::Permission2,
                pane,
                &settings(false, true, &["secrets.rs"])
            ),
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

    /// Живой кейс из окна `wt:1785950679`: запрос пришёл от суб-агента,
    /// поэтому заголовок подписан «· from the run-phase agent», а выше по
    /// экрану остался вывод предыдущей команды с `rm -rf`.
    const SUBAGENT_PROMPT: &str = "\
⏺ Bash(SP=/private/tmp/claude-501/x/scratchpad; HOMEDIR=$SP/home; rm -rf \"$HOMEDIR\";…)
  ⎿  running…
────────────────────────────────────────────
 Bash command · from the run-phase agent
   SP=/private/tmp/claude-501/x/scratchpad; cat $SP/ports.env
   2>/dev/null; pkill -f fakevk.py; echo killed
   Check ports and kill stray
 This command requires approval
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for: pkill -f fakevk.py
   3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    /// Регрессия: заголовок с подписью суб-агента должен вычленять блок, а не
    /// откатывать блоклист на всю панель (там `rm -rf` из прошлой команды).
    #[test]
    fn subagent_header_scopes_blocklist_to_its_own_block() {
        let region = extract_command_region(SUBAGENT_PROMPT).expect("блок должен вычлениться");
        assert!(region.contains("pkill -f fakevk.py"));
        assert!(!region.contains("rm -rf"), "region = {region}");

        assert_eq!(classify_prompt(SUBAGENT_PROMPT), Some(PromptKind::Permission3));
        assert_eq!(
            decide(PromptKind::Permission3, SUBAGENT_PROMPT, &settings(true, true, &[])),
            Decision::Press("2")
        );
    }

    /// Суть запроса берётся из блока текущего запроса, а не из вывода
    /// предыдущей команды, оставшегося выше на экране.
    #[test]
    fn subagent_header_bounds_subject() {
        let subject = extract_subject(SUBAGENT_PROMPT);
        assert!(subject.contains("pkill -f fakevk.py"), "{subject}");
        assert!(!subject.contains("rm -rf"), "{subject}");
    }

    /// Панель окна `wt:<ts>`, в котором работает суб-агент: над prompt'ом
    /// живая строка прогресса. `{}` — место спиннера/таймера, которые в
    /// живом капче менялись между соседними тиками воркера.
    fn pane_with_agent_spinner(spinner: &str, elapsed: &str) -> String {
        format!(
            "\
  {spinner} run-phase(Фаза 1: хранилище промптов)
  ⎿  Read(plugins/vk/src/config.rs)
     … +19 tool uses
✶ Gesticulating… ({elapsed} · ↓ 25.8k tokens)
 Do you want to make this edit to config.rs?
 ❯ 1. Yes
   2. Yes, allow all edits during this session (shift+tab)
   3. No
 Esc to cancel · Tab to amend
"
        )
    }

    /// Регрессия живого кейса: в окне с суб-агентом хэш ВСЕЙ панели
    /// отличался каждый тик (спиннер + таймер), поэтому `seen` не доходило
    /// до 2 и авто-апрув не нажимал НИКОГДА. Хэш блока меню совпадает.
    #[test]
    fn stability_region_ignores_agent_spinner_above_prompt() {
        let a = pane_with_agent_spinner(" ", "1h 16m 12s");
        let b = pane_with_agent_spinner("⏺", "1h 16m 14s");
        assert_ne!(hash_pane(&a), hash_pane(&b), "фикстура должна отличаться");
        assert_eq!(
            hash_pane(&stability_region(&a)),
            hash_pane(&stability_region(&b))
        );
        // И сам prompt при этом остаётся распознаваемым.
        assert_eq!(classify_prompt(&a), Some(PromptKind::Permission3));
    }

    /// Заголовок запроса входит в регион: два prompt'а с одинаковыми
    /// пунктами, но разными файлами, не должны считаться одним экраном.
    #[test]
    fn stability_region_keeps_prompt_header() {
        let a = pane_with_agent_spinner("⏺", "1m 0s");
        let b = a.replace("config.rs", "other.rs");
        assert_ne!(
            hash_pane(&stability_region(&a)),
            hash_pane(&stability_region(&b))
        );
    }

    /// Без меню `❯ 1.` регион — хвост панели: состояние обновляется, но
    /// нажимать всё равно нечего (`classify_prompt` вернёт `None`).
    #[test]
    fn stability_region_without_menu_uses_tail() {
        let pane = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n";
        assert_eq!(stability_region(pane), "line3 line4 line5 line6 line7 line8");
    }

    // ────────────────────────── краевые ситуации ──────────────────────────

    /// Вырожденные панели: пустая, из одних пробелов, из одной строки.
    /// Ни одна функция не должна паниковать (индексация срезов!) и ни одна
    /// не должна увидеть prompt.
    #[test]
    fn degenerate_panes_are_inert() {
        for pane in ["", "\n", "   \n\t\n", "hello", "❯"] {
            assert_eq!(classify_prompt(pane), None, "pane = {pane:?}");
            assert_eq!(extract_command_region(pane), None, "pane = {pane:?}");
            // Не паникует и не выдумывает содержимое.
            let _ = extract_subject(pane);
            let _ = stability_region(pane);
            assert!(!is_dangerous(pane, &[]), "pane = {pane:?}");
        }
    }

    /// Панель без завершающего перевода строки — `capture-pane` отдаёт
    /// именно так, если prompt дорисован до последней строки экрана.
    #[test]
    fn pane_without_trailing_newline_is_classified() {
        let pane = PERMISSION3.trim_end();
        assert_eq!(classify_prompt(pane), Some(PromptKind::Permission3));
    }

    /// Недорисованный кадр: пункт `❯ 1.` уже есть, а остальных ещё нет.
    /// Нажимать по такому экрану нельзя — попадём не в тот пункт.
    #[test]
    fn partially_drawn_menu_is_not_classified() {
        let yes_only = "Do you want to proceed?\n❯ 1. Yes\n Esc to cancel\n";
        let allow_only = "Claude in Chrome wants to navigate\n❯ 1. Allow\n Esc to cancel\n";
        assert_eq!(classify_prompt(yes_only), None);
        assert_eq!(classify_prompt(allow_only), None);
    }

    /// Оба footer'а на экране (перерисовка plan → question): порядок
    /// проверок отдаёт Plan, а план не подтверждается никогда.
    #[test]
    fn plan_footer_wins_over_question_footer() {
        let pane = "\
Would you like to proceed?
❯ 1. Yes
  2. No
 Enter to select · ↑/↓ to navigate · Tab/Arrow keys to navigate · Esc to cancel
";
        assert_eq!(classify_prompt(pane), Some(PromptKind::Plan));
        assert_eq!(
            decide(PromptKind::Plan, pane, &settings(true, true, &[])),
            Decision::Skip
        );
    }

    /// Question-footer сильнее permission-меню: у `AskUserQuestion` пункты
    /// тоже нумерованные, и без раннего выхода он классифицировался бы как
    /// permission — то есть подтверждался бы вопреки выключенному тумблеру.
    #[test]
    fn question_footer_wins_over_numbered_options() {
        let pane = "\
Какой вариант выбрать?
❯ 1. Yes
  2. No
 Enter to select · ↑/↓ to navigate · Esc to cancel
";
        assert_eq!(classify_prompt(pane), Some(PromptKind::Question));
        assert_eq!(
            decide(PromptKind::Question, pane, &settings(true, false, &[])),
            Decision::Skip
        );
    }

    /// Broad-режим не расширяет вопрос: у `AskUserQuestion` второй пункт —
    /// это другой ответ, а не «то же, но шире».
    #[test]
    fn question_ignores_broad_mode() {
        let pane = "\
Какой вариант выбрать?
❯ 1. Первый
  2. Второй
 Enter to select · ↑/↓ to navigate · Esc to cancel
";
        assert_eq!(
            decide(PromptKind::Question, pane, &settings(true, true, &[])),
            Decision::Press("1")
        );
    }

    /// Блоклист из пустых строк не должен блокировать всё подряд:
    /// `contains("")` истинно для любой панели.
    #[test]
    fn blank_blocklist_entries_are_ignored() {
        let s = settings(true, true, &["", "   ", "\t"]);
        assert_eq!(decide(PromptKind::Permission3, PERMISSION3, &s), Decision::Press("2"));
    }

    /// Опасная подстрока ТОЛЬКО в тексте пункта меню («…don't ask again for:
    /// rm -rf») блока команды не касается: пункты в регион не входят.
    /// Тест фиксирует границу области — сама команда при этом безобидна.
    #[test]
    fn blocklist_does_not_read_menu_options() {
        let pane = "\
Bash command
  ls -la
Do you want to proceed?
❯ 1. Yes
  2. Yes, and don't ask again for: rm -rf commands
  3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";
        assert_eq!(decide(PromptKind::Permission3, pane, &settings(true, true, &[])), Decision::Press("2"));
    }

    /// Панель, где ВЫШЕ настоящего запроса осталось чужое такое же меню
    /// (`cat` собственных тест-фикстур). Якорем должен быть НИЖНИЙ prompt:
    /// иначе блоклист проверял бы команду цитаты (`cat …`), а `rm -rf`
    /// настоящего запроса проскакивал бы как безопасный.
    const QUOTED_MENU_ABOVE: &str = "\
 Bash command
   cat tmux-web/src/auto_approve.rs
 Do you want to proceed?
⏺ Bash(cat tmux-web/src/auto_approve.rs)
  ⎿  const PERMISSION3: &str = \"...
     ❯ 1. Yes
       2. Yes, and don't ask again for: ls commands
       3. No
 Bash command · from the run-phase agent
   rm -rf /важное/место
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for: rm commands
   3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";

    #[test]
    fn quoted_menu_above_does_not_shadow_real_request() {
        let region = extract_command_region(QUOTED_MENU_ABOVE).expect("блок вычленяется");
        assert!(region.contains("rm -rf /важное/место"), "region = {region}");
        assert!(!region.contains("cat tmux-web"), "region = {region}");

        // И, как следствие, опасная команда действительно блокируется.
        assert_eq!(
            decide(PromptKind::Permission3, QUOTED_MENU_ABOVE, &settings(true, true, &[])),
            Decision::Blocked("2")
        );
    }

    #[test]
    fn subject_and_stability_anchor_to_the_bottom_menu() {
        let subject = extract_subject(QUOTED_MENU_ABOVE);
        assert!(subject.contains("rm -rf /важное/место"), "{subject}");
        assert!(!subject.contains("cat tmux-web"), "{subject}");

        let region = stability_region(QUOTED_MENU_ABOVE);
        assert!(region.starts_with("Do you want to proceed? ❯ 1. Yes"), "{region}");
    }

    /// Панель короче окна хвоста (`STABILITY_TAIL_LINES`) — срез не должен
    /// уходить в минус.
    #[test]
    fn stability_region_handles_short_pane() {
        assert_eq!(stability_region("a\nb\n"), "a b");
    }

    /// Типографский апостроф в пункте — именно так рисует Claude Code
    /// («don’t», U+2019). Классификация не должна на него опираться.
    #[test]
    fn typographic_apostrophe_does_not_break_classification() {
        let pane = "\
Bash command
  ls -la
Do you want to proceed?
❯ 1. Yes
  2. Yes, and don’t ask again for: ls commands
  3. No
 Esc to cancel · Tab to amend · ctrl+e to explain
";
        assert_eq!(classify_prompt(pane), Some(PromptKind::Permission3));
    }

    /// Prompt без меню в панели вообще (`Plan`/`Question` рисуют свои
    /// пункты иначе): `extract_subject` берёт хвост и не паникует.
    #[test]
    fn subject_without_menu_takes_tail() {
        let pane = "первая строка\nвторая строка\nтретья строка\n";
        let subject = extract_subject(pane);
        assert!(subject.contains("третья строка"), "{subject}");
    }
}
