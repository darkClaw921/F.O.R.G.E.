//! Построчный разбор одного транскрипта Claude Code (`.jsonl`).
//!
//! ### Формат источника
//!
//! Один `.jsonl` = одна сессия, имя файла = `sessionId`, файл append-only
//! (дописывается в конец, старые строки не меняются — на этом строится
//! инкрементальное дочитывание с оффсета). Каждая строка — самостоятельный
//! JSON-объект с полем `type`.
//!
//! Живые файлы содержат больше десятка типов записей
//! (`attachment`, `file-history-snapshot`, `file-history-delta`, `mode`,
//! `permission-mode`, `queue-operation`, `last-prompt`, `frame-link`,
//! `pr-link`, …), из которых аналитике нужны только шесть:
//! `user`, `assistant`, `system`, `cost-state`, `agent-name`, `ai-title`.
//! Остальные строки отсеиваются [`interesting_line`] ещё до
//! `serde_json::from_str` — в живых файлах их больше половины, и их разбор
//! съедал бы основное время индексации.
//!
//! ### Узкие структуры
//!
//! Записи транскрипта огромные (у `assistant` внутри лежат все блоки
//! контента с полными телами инструментов). Поэтому здесь описаны узкие
//! структуры, тянущие только нужные поля: все поля `Option`/`#[serde(default)]`,
//! неизвестные поля игнорируются. Живой архив содержит записи разных версий
//! CLI, и «строгая» схема на нём неизбежно ломается.
//!
//! Поля `durationMs` в структурах намеренно нет: у большинства записей оно
//! `null`, а там, где не `null` (`system`/`turn_duration`), дублирует
//! величину, которую мы и так считаем по таймстемпам.

use std::collections::BTreeMap;
use std::io::BufRead;

use serde::de::IgnoredAny;
use serde::Deserialize;

use super::rollup::{SessionRollup, SessionTotals};

/// Порог, после которого пауза человека считается не «чтением кода», а
/// «отошёл» (30 минут). Такие паузы ведутся отдельным счётчиком и не
/// попадают ни в медиану, ни в перцентили, ни в гистограмму.
pub const AWAY_THRESHOLD_MS: i64 = 30 * 60 * 1000;

/// Порог «ответил не читая» — 10 секунд.
pub const QUICK_REPLY_MS: i64 = 10_000;

// ---------------------------------------------------------------------------
// serde-структуры записей транскрипта
// ---------------------------------------------------------------------------

/// Одна строка транскрипта. Все поля опциональны: в файле соседствуют
/// записи разных типов и разных версий CLI.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Record {
    /// Тип записи: `user`, `assistant`, `system`, `cost-state`,
    /// `agent-name`, `ai-title`, …
    #[serde(rename = "type")]
    pub kind: String,
    /// Подтип системной записи: `turn_duration`, `compact_boundary`,
    /// `stop_hook_summary`, `local_command`, …
    pub subtype: Option<String>,
    /// ISO-8601 с миллисекундами (`2026-08-12T12:58:29.906Z`).
    /// У части служебных записей (`agent-name`, `ai-title`, `cost-state`)
    /// таймстемпа нет вовсе.
    pub timestamp: Option<String>,
    /// Рабочий каталог сессии — источник истины о «проекте».
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// Версия Claude Code CLI, писавшей запись.
    pub version: Option<String>,
    /// `true` у записей работы субагента.
    pub is_sidechain: bool,
    /// `true` у служебных записей, которые CLI подставляет от имени
    /// пользователя (не настоящее сообщение человека).
    pub is_meta: bool,
    pub message: Option<Msg>,
    /// Результат вызова инструмента; может быть как объектом, так и строкой.
    pub tool_use_result: Option<ToolUseResult>,
    /// Имя субагента (`type == "agent-name"`).
    pub agent_name: Option<String>,
    /// Заголовок сессии (`type == "ai-title"`); записей `summary` в файлах нет.
    pub ai_title: Option<String>,

    // --- поля записи cost-state (кумулятивные, брать только из последней) ---
    #[serde(rename = "totalCostUSD")]
    pub total_cost_usd: Option<f64>,
    #[serde(rename = "totalAPIDuration")]
    pub total_api_duration: Option<i64>,
    #[serde(rename = "totalToolDuration")]
    pub total_tool_duration: Option<i64>,
    pub total_lines_added: Option<i64>,
    pub total_lines_removed: Option<i64>,
    /// Unix-время старта сессии в миллисекундах.
    pub start_time: Option<i64>,
    pub model_usage: Option<BTreeMap<String, ModelUsage>>,
}

/// Полезная нагрузка сообщения (`user` / `assistant`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Msg {
    pub role: Option<String>,
    pub model: Option<String>,
    pub content: Option<Content>,
    pub usage: Option<Usage>,
}

/// Контент сообщения: у человека часто просто строка, у ассистента и у
/// `tool_result`-ответов — массив блоков.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<Block>),
    /// Любая другая форма (`null`, число, объект) — не роняем разбор.
    Other(IgnoredAny),
}

/// Блок контента. Нужны только `type` (различить `text` / `tool_use` /
/// `tool_result`), `name` (имя инструмента) и `text` (для длины сообщения).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Block {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    pub text: Option<String>,
}

/// Счётчики токенов из `assistant.message.usage`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OutputTokensDetails {
    pub thinking_tokens: Option<u64>,
}

/// Разбивка итогов сессии по моделям из записи `cost-state`.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
}

/// `toolUseResult` бывает и объектом, и строкой — untagged-enum позволяет
/// вытащить `interrupted` из объекта и молча проглотить всё остальное.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResult {
    Obj {
        #[serde(default)]
        interrupted: bool,
    },
    Other(IgnoredAny),
}

impl ToolUseResult {
    /// Был ли вызов инструмента прерван пользователем.
    pub fn interrupted(&self) -> bool {
        matches!(self, ToolUseResult::Obj { interrupted: true })
    }
}

impl Record {
    /// Настоящее сообщение человека?
    ///
    /// Правило (проверено на живых данных): `type == "user"`, `isMeta != true`,
    /// `isSidechain == false`, и `message.content` — строка либо массив, в
    /// котором НЕТ ни одного блока `tool_result`.
    ///
    /// Фильтр по `tool_result` критичен: в одной живой сессии 81 настоящее
    /// сообщение против 881 ответа инструментов — без него метрика завышена
    /// примерно в десять раз.
    pub fn is_human_message(&self) -> bool {
        if self.kind != "user" || self.is_meta || self.is_sidechain {
            return false;
        }
        match self.message.as_ref().and_then(|m| m.content.as_ref()) {
            Some(Content::Text(_)) => true,
            Some(Content::Blocks(blocks)) => !blocks.iter().any(|b| b.kind == "tool_result"),
            _ => false,
        }
    }

    /// Текст сообщения человека (для массива — конкатенация `text`-блоков
    /// через перевод строки).
    ///
    /// Текст нужен ровно на один шаг — классификацию
    /// ([`super::text::classify`]); ни в роллап, ни в снимок кэша, ни в API
    /// он не попадает.
    pub fn human_text(&self) -> String {
        match self.message.as_ref().and_then(|m| m.content.as_ref()) {
            Some(Content::Text(s)) => s.clone(),
            Some(Content::Blocks(blocks)) => blocks
                .iter()
                .filter(|b| b.kind == "text")
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    /// Таймстемп записи в миллисекундах Unix-эпохи.
    pub fn ts_ms(&self) -> Option<i64> {
        self.timestamp.as_deref().and_then(parse_ts_ms)
    }
}

/// Разбирает ISO-8601 таймстемп с миллисекундами в Unix-мс.
pub fn parse_ts_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

// ---------------------------------------------------------------------------
// Префильтр
// ---------------------------------------------------------------------------

/// Типы записей, интересные аналитике.
const WANTED: [&str; 6] = [
    "user",
    "assistant",
    "system",
    "cost-state",
    "agent-name",
    "ai-title",
];

/// Дешёвый префильтр: стоит ли вообще отдавать строку в `serde_json`.
///
/// Ищем подстроку `"type":"<нужный>"` (и вариант с пробелом после
/// двоеточия — на случай другого форматтера). Ложноположительные
/// срабатывания безвредны: у распарсенной записи всё равно проверяется
/// настоящее поле `type`. Ложноотрицательных быть не может — CLI всегда
/// пишет `type` первым полем и без экранирования.
pub fn interesting_line(line: &str) -> bool {
    WANTED.iter().any(|t| {
        memfind(line, &format!("\"type\":\"{t}\"")) || memfind(line, &format!("\"type\": \"{t}\""))
    })
}

#[inline]
fn memfind(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

// ---------------------------------------------------------------------------
// Разбор
// ---------------------------------------------------------------------------

/// Разбирает транскрипт из `reader` в новый [`SessionRollup`].
///
/// `session_id` — имя файла без расширения; `start_offset` — байтовый
/// оффсет, с которого начинается чтение (для полного разбора — `0`).
///
/// # Правила вычисления
///
/// * **Сообщение человека** — см. [`Record::is_human_message`].
/// * **Конец хода ассистента** — запись `system`/`turn_duration` (ровно одна
///   на ход). Fallback, если её нет: таймстемп последней `assistant`-записи
///   перед следующим сообщением человека.
/// * **Пауза «читает ли код»** = таймстемп следующего сообщения человека −
///   конец хода. Паузы > [`AWAY_THRESHOLD_MS`] — «отошёл»: отдельный
///   счётчик, в медиану/перцентили/гистограмму не входят.
/// * **Длительность хода** = конец хода − предшествующее сообщение человека.
/// * **Токены** — `assistant.message.usage.*`, по дням и по моделям.
/// * **Стоимость и итоги сессии** — только из ПОСЛЕДНЕЙ записи `cost-state`:
///   промежуточные кумулятивны, суммировать их нельзя.
/// * **Инструменты** — блоки `tool_use` в контенте ассистента; прерывания —
///   `toolUseResult.interrupted == true`.
/// * **Компакты** — `system`/`compact_boundary`. **Субагенты** —
///   `isSidechain == true` и записи `agent-name`.
///
/// # Устойчивость
///
/// Чтение построчное (`read_line`), файл целиком в память не попадает.
/// Битая или усечённая строка (обрыв записи при дописывании) не роняет
/// разбор: строка пропускается, увеличивается `skipped_lines`, а
/// `parse_offset` остаётся на конце последней ПОЛНОЙ строки — чтобы Фаза 2
/// дочитала хвост заново.
pub fn parse_session<R: BufRead>(
    reader: R,
    session_id: &str,
    start_offset: u64,
) -> std::io::Result<SessionRollup> {
    let mut rollup = SessionRollup::new(session_id);
    rollup.parse_offset = start_offset;
    append_session(&mut rollup, reader)?;
    Ok(rollup)
}

/// Дочитывает продолжение файла в уже собранный роллап.
///
/// Счётчики складываются, итоги `cost-state` ПЕРЕЗАПИСЫВАЮТСЯ, а состояние
/// автомата ходов (`turn_state`) живёт в самом роллапе — поэтому ход,
/// разрезанный границей дочитывания, не теряется и не дублируется.
pub fn append_session<R: BufRead>(
    rollup: &mut SessionRollup,
    mut reader: R,
) -> std::io::Result<()> {
    let mut line = String::new();
    let mut offset = rollup.parse_offset;

    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => n,
            // Невалидный UTF-8 в середине файла: пропускаем и идём дальше не
            // можем (позиция потеряна) — прекращаем разбор на последней
            // полной строке.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                rollup.skipped_lines += 1;
                break;
            }
            Err(e) => return Err(e),
        };

        // Строка без завершающего `\n` — файл оборван на середине записи
        // (или дописывается прямо сейчас). Не двигаем offset: хвост будет
        // дочитан следующим проходом.
        if !line.ends_with('\n') {
            if !line.trim().is_empty() {
                rollup.skipped_lines += 1;
            }
            break;
        }
        offset += read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !interesting_line(trimmed) {
            continue;
        }
        match serde_json::from_str::<Record>(trimmed) {
            Ok(rec) => {
                rollup.parsed_lines += 1;
                apply_record(rollup, &rec);
            }
            Err(_) => rollup.skipped_lines += 1,
        }
    }

    rollup.parse_offset = offset;
    rollup.redistribute_totals();
    Ok(())
}

/// Учитывает одну разобранную запись в роллапе.
fn apply_record(rollup: &mut SessionRollup, rec: &Record) {
    // Идентификация сессии и проекта — из первой встреченной записи с cwd.
    if rollup.cwd.is_none() {
        if let Some(cwd) = rec.cwd.as_deref() {
            rollup.set_cwd(cwd);
        }
    }
    if rollup.git_branch.is_none() {
        rollup.git_branch = rec.git_branch.clone();
    }
    if let Some(title) = rec.ai_title.as_deref() {
        // Заголовок переписывается — CLI дописывает уточнённые варианты.
        rollup.ai_title = Some(title.to_string());
    }

    let ts = rec.ts_ms();
    if let Some(ts) = ts {
        rollup.observe_ts(ts);
        if let Some(v) = rec.version.as_deref() {
            rollup.cli_versions.insert(v.to_string());
            rollup.day_mut(ts).versions.insert(v.to_string());
        }
    }

    match rec.kind.as_str() {
        "user" => {
            if rec.is_sidechain {
                if let Some(ts) = ts {
                    rollup.day_mut(ts).subagent_records += 1;
                }
                return;
            }
            if rec.is_human_message() {
                if let Some(ts) = ts {
                    match super::text::classify_user_text(&rec.human_text()) {
                        super::text::UserRecord::Message(text) => {
                            rollup.on_human_message(ts, &text)
                        }
                        // Прерывание не открывает ход и не мерит паузу: человек
                        // не формулировал задачу, а обрывал ответ.
                        super::text::UserRecord::Interruption => rollup.on_interruption(ts),
                        // Вывод слэш-команды — не реплика человека вовсе.
                        super::text::UserRecord::CliOutput => {}
                    }
                }
            }
            // Исторический маркер прерывания. В живом архиве не встречается ни
            // разу (CLI пишет прерывание текстом, см. `text::INTERRUPT_PREFIX`),
            // но записи старых версий CLI ловятся именно так.
            if let Some(res) = rec.tool_use_result.as_ref() {
                if res.interrupted() {
                    if let Some(ts) = ts {
                        rollup.on_interruption(ts);
                    }
                }
            }
        }
        "assistant" => {
            if let Some(ts) = ts {
                rollup.on_assistant(ts, rec);
            }
        }
        "system" => match rec.subtype.as_deref() {
            Some("turn_duration") => {
                if let Some(ts) = ts {
                    rollup.on_turn_end(ts);
                }
            }
            Some("compact_boundary") => {
                if let Some(ts) = ts {
                    rollup.day_mut(ts).compacts += 1;
                }
            }
            _ => {}
        },
        "cost-state" => {
            rollup.totals = SessionTotals {
                present: true,
                total_cost_usd: rec.total_cost_usd.unwrap_or(0.0),
                total_api_duration_ms: rec.total_api_duration.unwrap_or(0).max(0) as u64,
                total_tool_duration_ms: rec.total_tool_duration.unwrap_or(0).max(0) as u64,
                lines_added: rec.total_lines_added.unwrap_or(0).max(0) as u64,
                lines_removed: rec.total_lines_removed.unwrap_or(0).max(0) as u64,
                start_time_ms: rec.start_time,
                model_usage: rec.model_usage.clone().unwrap_or_default(),
            };
        }
        "agent-name" => {
            if let Some(name) = rec.agent_name.as_deref() {
                rollup.on_subagent_start(name);
            }
        }
        _ => {}
    }
}

