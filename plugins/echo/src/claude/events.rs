//! Парсер NDJSON-стрима от `claude -p --output-format stream-json
//! --include-partial-messages --verbose`.
//!
//! ## Контракт стрима
//!
//! Каждая строка — JSON-объект с дискриминатором `type`. Известные типы:
//!
//! - `system`               — стартовое инфо (init), игнорируем.
//! - `message_start`        — начало message; обычно содержит `message.usage`
//!                            с input_tokens и cache_*. Из него выдаём
//!                            `ClaudeEvent::Result` ещё **нет** — usage здесь
//!                            частичный (только input + cache); финальный
//!                            usage приходит в типе `result` или `message_delta`.
//! - `content_block_start`  — старт блока (text/thinking/tool_use). Для
//!                            tool_use фиксируем имя в side-state нашего парсера
//!                            (см. `StatefulParser`), но в `parse_line` без
//!                            состояния просто игнорим — клиент получит
//!                            tool input в content_block_stop / message_delta.
//! - `content_block_delta`  — приращение блока:
//!     - `delta.type == "text_delta"`     → [`ClaudeEvent::TextDelta`]
//!     - `delta.type == "thinking_delta"` → [`ClaudeEvent::Thinking`]
//!     - `delta.type == "input_json_delta"` → пока пропускаем (Phase 3 не
//!       рендерит partial tool input).
//! - `content_block_stop`   — конец блока; для `tool_use` блока финальное
//!                            `input` обычно доступно отдельным сообщением.
//! - `message_delta`        — может содержать обновлённый `usage`. Не считаем
//!                            это `Result`-event'ом — финал придёт в `result`.
//! - `message_stop`         — конец одного assistant-сообщения, игнорируем
//!                            (Result-aggregator смотрит `result`).
//! - `result`               — финал всего run'а: `usage`, `total_cost_usd`,
//!                            `duration_ms` и т.п. → [`ClaudeEvent::Result`].
//! - `error`                → [`ClaudeEvent::Error`].
//!
//! Любой незнакомый `type` → `None` (skip, не падать).
//!
//! ## Толерантность к отсутствующим cache_*
//!
//! `cache_creation_input_tokens` и `cache_read_input_tokens` появились в Claude
//! относительно поздно. Старые модели/режимы могут их не возвращать. Поэтому в
//! [`Usage`] эти поля помечены `#[serde(default)]` (default = 0). Тесты
//! `parses_message_start_without_cache_fields` и
//! `parses_result_without_cache_fields` страхуют этот контракт.

use serde::{Deserialize, Serialize};

/// Высокоуровневое событие assistant-стрима.
///
/// Поглощается двумя потребителями:
/// - WS-loop (`plugins/echo/src/ws/mod.rs`) — конвертирует в `ServerMsg`.
/// - autonomous-runner (`plugins/echo/src/scheduler/runner.rs`) — собирает
///   финальный текст + usage для записи в `task_runs`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeEvent {
    /// Приращение текстового блока (assistant-ответ).
    TextDelta { text: String },
    /// Приращение thinking-блока (видимое только в --verbose + reasoning model).
    Thinking { text: String },
    /// Использование инструмента: имя + input JSON.
    /// Phase 3 — emit'ится один раз на блок, после `content_block_stop`
    /// с tool_use (см. [`parse_line`] комментарий).
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// Финальный результат run'а: usage и сырой объект для аудита.
    Result {
        usage: Usage,
        raw_json: serde_json::Value,
    },
    /// Ошибка от CLI (type=error в стриме).
    Error { message: String },
}

/// Структура usage-объекта Claude. Все поля `Option<u64>` либо
/// `#[serde(default)]` u64 — Anthropic меняет схему ответа без bump'а
/// версии, и `cache_*` поля исторически отсутствовали.
///
/// `default = 0` для cache-полей — это намеренное упрощение: prompt-caching
/// не используется → 0 cache-tokens, что эквивалентно отсутствию поля.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// Парсит одну NDJSON-строку и возвращает событие.
///
/// Возвращает `None` для:
/// - невалидного JSON,
/// - известных, но не интересных нам типов (system/init/message_start/
///   content_block_start/content_block_stop/message_stop),
/// - неизвестных типов (forward-compat: новый Claude может добавить новый
///   `type` — мы должны не падать).
///
/// Толерантен к отсутствующим cache_* полям в Usage (через
/// `#[serde(default)]`).
pub fn parse_line(line: &str) -> Option<ClaudeEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let raw: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return None,
    };
    // Claude CLI с --include-partial-messages оборачивает каждое API-event
    // в `{"type":"stream_event","event":{...}}`. Разворачиваем обёртку.
    let v: serde_json::Value = if raw.get("type").and_then(|t| t.as_str()) == Some("stream_event") {
        match raw.get("event") {
            Some(inner) => inner.clone(),
            None => return None,
        }
    } else {
        raw
    };
    let ty = v.get("type")?.as_str()?;
    match ty {
        "content_block_delta" => {
            let delta = v.get("delta")?;
            let dty = delta.get("type")?.as_str()?;
            match dty {
                "text_delta" => {
                    let text = delta.get("text")?.as_str()?.to_string();
                    Some(ClaudeEvent::TextDelta { text })
                }
                "thinking_delta" => {
                    let text = delta.get("thinking")?.as_str()?.to_string();
                    Some(ClaudeEvent::Thinking { text })
                }
                // input_json_delta — partial tool input; Phase 3 не показывает.
                _ => None,
            }
        }
        "content_block_start" => {
            // Если это полностью сформированный tool_use (есть input) —
            // эмитим сразу. Для частичных tool_use'ов реальный input приходит
            // позже в input_json_delta'ах + content_block_stop; в первом
            // приближении Phase 3 этим пренебрегает.
            let block = v.get("content_block")?;
            let btype = block.get("type")?.as_str()?;
            if btype == "tool_use" {
                let name = block.get("name")?.as_str()?.to_string();
                let input = block.get("input").cloned().unwrap_or(serde_json::Value::Null);
                Some(ClaudeEvent::ToolUse { name, input })
            } else {
                None
            }
        }
        "result" => {
            let usage_val = v.get("usage").cloned().unwrap_or(serde_json::Value::Null);
            let usage: Usage = serde_json::from_value(usage_val).unwrap_or_default();
            Some(ClaudeEvent::Result {
                usage,
                raw_json: v,
            })
        }
        "error" => {
            let message = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Some(ClaudeEvent::Error { message })
        }
        // Все остальные — system, message_start, content_block_stop,
        // message_delta, message_stop, неизвестные → пропускаем.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_delta() {
        let line = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let ev = parse_line(line).expect("must parse");
        assert_eq!(ev, ClaudeEvent::TextDelta { text: "Hello".into() });
    }

    #[test]
    fn parse_text_delta_wrapped_in_stream_event() {
        // Реальный формат от `claude -p --include-partial-messages`: каждое
        // API-event обёрнуто в {"type":"stream_event","event":{...}}.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"ОК"}},"session_id":"s","uuid":"u"}"#;
        let ev = parse_line(line).expect("must parse wrapped event");
        assert_eq!(ev, ClaudeEvent::TextDelta { text: "ОК".into() });
    }

    #[test]
    fn parse_thinking_delta_wrapped() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}"#;
        let ev = parse_line(line).expect("must parse");
        assert_eq!(ev, ClaudeEvent::Thinking { text: "hmm".into() });
    }

    #[test]
    fn parse_thinking_delta() {
        let line = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#;
        let ev = parse_line(line).expect("must parse");
        assert_eq!(ev, ClaudeEvent::Thinking { text: "hmm".into() });
    }

    #[test]
    fn parse_tool_use_inline_input() {
        let line = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}}"#;
        let ev = parse_line(line).expect("must parse");
        match ev {
            ClaudeEvent::ToolUse { name, input } => {
                assert_eq!(name, "Bash");
                assert_eq!(input.get("command").and_then(|v| v.as_str()), Some("ls"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_result_with_full_usage() {
        let line = r#"{"type":"result","subtype":"success","duration_ms":12,"is_error":false,"usage":{"input_tokens":42,"output_tokens":7,"cache_creation_input_tokens":3,"cache_read_input_tokens":5}}"#;
        let ev = parse_line(line).expect("must parse");
        match ev {
            ClaudeEvent::Result { usage, .. } => {
                assert_eq!(usage.input_tokens, 42);
                assert_eq!(usage.output_tokens, 7);
                assert_eq!(usage.cache_creation_input_tokens, 3);
                assert_eq!(usage.cache_read_input_tokens, 5);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_result_without_cache_fields() {
        // Старый формат без cache_* — cache_* должны быть 0.
        let line = r#"{"type":"result","usage":{"input_tokens":10,"output_tokens":2}}"#;
        let ev = parse_line(line).expect("must parse");
        match ev {
            ClaudeEvent::Result { usage, .. } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(usage.cache_creation_input_tokens, 0);
                assert_eq!(usage.cache_read_input_tokens, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_result_without_usage_field() {
        // Если usage вообще отсутствует — заполняем дефолтом (всё 0).
        let line = r#"{"type":"result","duration_ms":3}"#;
        let ev = parse_line(line).expect("must parse");
        match ev {
            ClaudeEvent::Result { usage, .. } => {
                assert_eq!(usage, Usage::default());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_message_start_without_cache_fields() {
        // message_start раньше игнорировался — проверим, что система
        // тёрпит missing cache_* в любых местах, где встречается Usage.
        // (Прямой вызов parse_line на message_start вернёт None — это ок,
        // но usage внутри парсится в нашем тесте десериализации напрямую.)
        let raw = r#"{"input_tokens":1,"output_tokens":0}"#;
        let usage: Usage = serde_json::from_str(raw).unwrap();
        assert_eq!(usage.input_tokens, 1);
        assert_eq!(usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn parse_error_event() {
        let line = r#"{"type":"error","message":"overloaded_error"}"#;
        let ev = parse_line(line).expect("must parse");
        assert_eq!(
            ev,
            ClaudeEvent::Error {
                message: "overloaded_error".into()
            }
        );
    }

    #[test]
    fn empty_or_whitespace_returns_none() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("\n").is_none());
    }

    #[test]
    fn invalid_json_returns_none() {
        assert!(parse_line("not json at all").is_none());
        assert!(parse_line("{").is_none());
        assert!(parse_line("{\"type\": }").is_none());
    }

    #[test]
    fn unknown_type_returns_none() {
        let line = r#"{"type":"some_brand_new_type_v999","data":42}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn known_but_uninteresting_types_return_none() {
        for line in [
            r#"{"type":"system","subtype":"init","model":"sonnet"}"#,
            r#"{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":1,"output_tokens":0}}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            assert!(parse_line(line).is_none(), "expected None for {line}");
        }
    }

    #[test]
    fn ndjson_stream_round_trip() {
        // Симулируем реалистичный поток NDJSON.
        let stream = r#"{"type":"system","subtype":"init","model":"sonnet"}
{"type":"message_start","message":{"id":"m1"}}
{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}
{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi "}}
{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"there"}}
{"type":"content_block_stop","index":0}
{"type":"message_stop"}
{"type":"result","duration_ms":42,"usage":{"input_tokens":15,"output_tokens":2}}
"#;
        let events: Vec<ClaudeEvent> = stream
            .lines()
            .filter_map(parse_line)
            .collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], ClaudeEvent::TextDelta { text: "Hi ".into() });
        assert_eq!(events[1], ClaudeEvent::TextDelta { text: "there".into() });
        match &events[2] {
            ClaudeEvent::Result { usage, .. } => {
                assert_eq!(usage.input_tokens, 15);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }
}
