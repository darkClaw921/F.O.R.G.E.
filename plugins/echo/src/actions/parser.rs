//! Парсер `forge-actions`-блоков из markdown-ответа Claude.
//!
//! Поддерживаемый формат — fenced code block с языком `forge-actions`,
//! внутри — JSON-массив (или single-object) [`Action`]. См.
//! [`crate::actions`] module-level doc для примеров.
//!
//! Парсер автоматически оборачивает single-object в Vec.
//!
//! ## Толерантность
//!
//! - Текст без блока → пустой `Vec`.
//! - Несколько блоков → конкатенация в один `Vec` (по порядку появления).
//! - Невалидный JSON → warn + skip конкретного блока, остальные парсятся.
//! - Action с неизвестным `name` для `kind=system` (или невалидной структурой)
//!   → warn + skip (serde возвращает Err при парсинге `SystemActionKind`).
//!
//! Используется в `ws::mod` после `assistant_done` (Phase 5b).

use super::Action;

/// Маркер языка fenced-block'а. Совпадение чувствительно к регистру —
/// мы намеренно требуем нижний регистр.
const LANG_TAG: &str = "forge-actions";

/// Извлекает все валидные actions из text'а. Не паникует на любом вводе.
pub fn extract(text: &str) -> Vec<Action> {
    let mut out = Vec::new();
    for body in iter_fenced_blocks(text, LANG_TAG) {
        let trimmed = body.trim();
        // Поддерживаем и array, и single object.
        let val: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, body = %trimmed, "actions::parser: invalid JSON in forge-actions block");
                continue;
            }
        };
        let items: Vec<serde_json::Value> = match val {
            serde_json::Value::Array(arr) => arr,
            other @ serde_json::Value::Object(_) => vec![other],
            other => {
                tracing::warn!(?other, "actions::parser: forge-actions body must be array or object");
                continue;
            }
        };
        for item in items {
            match serde_json::from_value::<Action>(item.clone()) {
                Ok(a) => out.push(a),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        item = ?item,
                        "actions::parser: skip invalid action"
                    );
                }
            }
        }
    }
    out
}

/// Итерируется по `body` всех fenced-блоков с указанным `lang`.
///
/// Простая state-machine без зависимостей: ищем строку, начинающуюся с
/// `` ```<lang> `` (с возможными trailing-пробелами), затем — следующую
/// строку, начинающуюся с ` ``` `. Всё между ними — body.
///
/// Это не полный markdown-парсер — нам и не нужно. forge-actions блок мы
/// сами форматируем мета-prompt'ом, и Claude должен соблюдать формат.
fn iter_fenced_blocks<'a>(text: &'a str, lang: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut block_start: Option<usize> = None;
    let mut byte_cursor = 0usize;
    for line in text.split_inclusive('\n') {
        let line_no_nl = line.trim_end_matches('\n').trim_end_matches('\r');
        let trimmed = line_no_nl.trim_start();
        if !in_block {
            if let Some(rest) = trimmed.strip_prefix("```") {
                if rest.trim() == lang {
                    in_block = true;
                    block_start = Some(byte_cursor + line.len());
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("```") {
            // close — игнорируем «```lang» как вложенный open (не наш случай).
            if rest.trim().is_empty() {
                if let Some(s) = block_start {
                    out.push(&text[s..byte_cursor]);
                }
                in_block = false;
                block_start = None;
            }
        }
        byte_cursor += line.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::SystemActionKind;

    #[test]
    fn empty_text_returns_empty() {
        assert!(extract("").is_empty());
    }

    #[test]
    fn no_fenced_block_returns_empty() {
        let t = "Just plain text without any blocks. Maybe `inline` code.";
        assert!(extract(t).is_empty());
    }

    #[test]
    fn block_with_other_lang_ignored() {
        let t = "Here is some Python:\n```python\nprint('hi')\n```\n";
        assert!(extract(t).is_empty());
    }

    #[test]
    fn single_prompt_block() {
        let t = "Sure!\n```forge-actions\n[{\"id\":\"r1\",\"label\":\"Hi\",\"kind\":\"prompt\",\"text\":\"hello\"}]\n```\nEnd.";
        let v = extract(t);
        assert_eq!(v.len(), 1);
        match &v[0] {
            Action::Prompt { id, label, text } => {
                assert_eq!(id, "r1");
                assert_eq!(label, "Hi");
                assert_eq!(text, "hello");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn multiple_actions_in_one_block() {
        let body = r#"[
          {"id":"p1","label":"P","kind":"prompt","text":"do X"},
          {"id":"s1","label":"S","kind":"system","name":"create_task","params":{"title":"T"}}
        ]"#;
        let t = format!("Ok:\n```forge-actions\n{body}\n```");
        let v = extract(&t);
        assert_eq!(v.len(), 2);
        assert!(matches!(v[0], Action::Prompt { .. }));
        match &v[1] {
            Action::System { name, params, .. } => {
                assert_eq!(*name, SystemActionKind::CreateTask);
                assert_eq!(params["title"], "T");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_system_name_skipped_with_warning() {
        let body = r#"[
          {"id":"a","label":"A","kind":"prompt","text":"X"},
          {"id":"b","label":"B","kind":"system","name":"format_disk","params":{}}
        ]"#;
        let t = format!("```forge-actions\n{body}\n```");
        let v = extract(&t);
        // Только первый Action валиден.
        assert_eq!(v.len(), 1);
        assert!(matches!(v[0], Action::Prompt { .. }));
    }

    #[test]
    fn invalid_json_block_skipped() {
        let t = "```forge-actions\nnot a json\n```";
        assert!(extract(t).is_empty());
    }

    #[test]
    fn single_object_body_is_wrapped_as_one_item_vec() {
        let body = r#"{"id":"only","label":"Only","kind":"prompt","text":"X"}"#;
        let t = format!("```forge-actions\n{body}\n```");
        let v = extract(&t);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn multiple_blocks_concatenate() {
        let t = "```forge-actions\n[{\"id\":\"1\",\"label\":\"A\",\"kind\":\"prompt\",\"text\":\"x\"}]\n```\n\
                 middle text\n\
                 ```forge-actions\n[{\"id\":\"2\",\"label\":\"B\",\"kind\":\"prompt\",\"text\":\"y\"}]\n```";
        let v = extract(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id(), "1");
        assert_eq!(v[1].id(), "2");
    }

    #[test]
    fn tolerates_whitespace_around_body() {
        let t = "```forge-actions\n\n   [\n  {\"id\":\"1\",\"label\":\"A\",\"kind\":\"prompt\",\"text\":\"x\"}\n]   \n\n```";
        let v = extract(t);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn fenced_block_with_leading_spaces_in_open() {
        let t = "  ```forge-actions\n[{\"id\":\"1\",\"label\":\"A\",\"kind\":\"prompt\",\"text\":\"x\"}]\n  ```";
        let v = extract(t);
        assert_eq!(v.len(), 1);
    }
}
