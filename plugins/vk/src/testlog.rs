//! Захват `tracing` для юнит-тестов крейта.
//!
//! Существует ради одного инварианта: **секрет не появляется в логах**. Ради
//! него не заводится dev-зависимость `tracing-subscriber` — подписчик на
//! 40 строк пишется руками и складывает события в общий буфер процесса.
//!
//! # Почему подписчик ГЛОБАЛЬНЫЙ и ставится один раз
//!
//! `tracing` кэширует «интерес» к callsite ОДИН раз на весь процесс. Если
//! первый же тест выполнит `tracing::warn!` без установленного подписчика,
//! callsite навсегда пометится неинтересным, и захват в соседнем тесте молча
//! получит пустоту — падало бы только при параллельном прогоне и только
//! иногда. Поэтому [`install`] ставится через `set_global_default` внутри
//! `OnceLock`, а тесты читают ОБЩИЙ буфер, отмеряя свой отрезок по длине.
//!
//! Тот же приём используется в интеграционном стенде
//! `plugins/vk/tests/vk_stub.rs` (у него собственная копия: интеграционный
//! тест — отдельный бинарь и `#[cfg(test)]`-модули крейта ему не видны).

use std::sync::{Arc, Mutex, OnceLock};

/// Общий буфер строк лога на процесс.
static LINES: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();

/// Подписчик, складывающий поля события в строку.
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

/// Склейка всех полей события в одну строку `имя=значение`.
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={:?} ", field.name(), value));
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Ставит захват (идемпотентно) и возвращает общий буфер.
pub fn install() -> Arc<Mutex<Vec<String>>> {
    LINES
        .get_or_init(|| {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let _ = tracing::subscriber::set_global_default(Capture {
                lines: lines.clone(),
            });
            lines
        })
        .clone()
}

/// Снимок буфера целиком.
pub fn lines() -> Vec<String> {
    install().lock().unwrap().clone()
}

/// Сколько строк уже накоплено — курсор для «своего» отрезка.
pub fn mark() -> usize {
    install().lock().unwrap().len()
}

/// Строки, записанные ПОСЛЕ отметки [`mark`], одним текстом.
pub fn since(mark: usize) -> String {
    let all = lines();
    all.get(mark..).unwrap_or(&[]).join("\n")
}
