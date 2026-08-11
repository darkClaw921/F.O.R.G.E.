//! Накопительный [`AnalysisSink`] для тестов — ядра и плагинов.
//!
//! Публичный, а не `#[cfg(test)]`, намеренно: тем же накопителем проверяются
//! и хаб внутри крейта, и роуты плагина («стартовали run — пришёл ли
//! `started`»). До выноса тесты хаба разбирали `broadcast`-канал VK и
//! `VkServerMsg`, то есть тянули за собой весь WS-протокол ради проверки
//! арифметики кадров.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use crate::sink::AnalysisSink;

/// Кадр, как его увидел приёмник.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// [`AnalysisSink::started`].
    Started {
        /// Идентификатор run'а.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// `analyze` | `ask` | `refresh`.
        kind: String,
        /// Сколько сообщений попало в транскрипт.
        messages_used: u32,
        /// Часть истории отброшена потолком символов.
        truncated: bool,
    },
    /// [`AnalysisSink::delta`].
    Delta {
        /// Идентификатор run'а.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Приращение текста.
        text: String,
    },
    /// [`AnalysisSink::done`].
    Done {
        /// Идентификатор run'а.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Штатное ли завершение.
        ok: bool,
        /// Причина неуспеха.
        reason: Option<String>,
        /// Токенов на входе.
        input_tokens: u64,
        /// Токенов на выходе.
        output_tokens: u64,
    },
    /// [`AnalysisSink::error`].
    Error {
        /// Идентификатор run'а.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Текст ошибки как есть.
        message: String,
    },
}

impl Frame {
    /// Это финальный кадр?
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done { .. })
    }

    /// Это кусок ответа?
    #[must_use]
    pub fn is_delta(&self) -> bool {
        matches!(self, Self::Delta { .. })
    }
}

/// Накопитель кадров.
#[derive(Debug, Default)]
pub struct RecordingSink {
    frames: Mutex<Vec<Frame>>,
    /// Будит [`RecordingSink::wait_for`] на каждом новом кадре: опрос со `sleep`
    /// сделал бы тесты либо медленными, либо флакующими.
    bell: Notify,
}

impl RecordingSink {
    /// Пустой накопитель, готовый к передаче в [`crate::AnalysisHub::start`].
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Снимок всех кадров в порядке получения.
    #[must_use]
    pub fn frames(&self) -> Vec<Frame> {
        self.frames.lock().unwrap().clone()
    }

    /// Склейка всех дельт — то, что увидел бы пользователь на экране.
    #[must_use]
    pub fn text(&self) -> String {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter_map(|f| match f {
                Frame::Delta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Сколько кадров дельт пришло — ось, по которой меряется коалесцинг.
    #[must_use]
    pub fn delta_frames(&self) -> usize {
        self.frames.lock().unwrap().iter().filter(|f| f.is_delta()).count()
    }

    /// Первый кадр, удовлетворяющий предикату, среди уже полученных.
    #[must_use]
    pub fn find(&self, pred: impl Fn(&Frame) -> bool) -> Option<Frame> {
        self.frames.lock().unwrap().iter().find(|f| pred(f)).cloned()
    }

    /// Ждёт кадр, удовлетворяющий предикату, не дольше `timeout`.
    ///
    /// `None` означает «за это время не пришёл» — и это ПОЛНОЦЕННЫЙ результат:
    /// тест отмены проверяет именно отсутствие финального кадра.
    pub async fn wait_for(
        &self,
        timeout: Duration,
        pred: impl Fn(&Frame) -> bool,
    ) -> Option<Frame> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Подписка ДО осмотра: иначе кадр, пришедший между осмотром и
            // `await`, разбудил бы никого.
            let bell = self.bell.notified();
            if let Some(f) = self.find(&pred) {
                return Some(f);
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            let _ = tokio::time::timeout(left, bell).await;
        }
    }

    fn push(&self, frame: Frame) {
        self.frames.lock().unwrap().push(frame);
        self.bell.notify_waiters();
    }
}

impl AnalysisSink for RecordingSink {
    fn started(
        &self,
        run_id: &str,
        dialog_id: &str,
        kind: &str,
        messages_used: u32,
        truncated: bool,
    ) {
        self.push(Frame::Started {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            kind: kind.to_string(),
            messages_used,
            truncated,
        });
    }

    fn delta(&self, run_id: &str, dialog_id: &str, text: &str) {
        self.push(Frame::Delta {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            text: text.to_string(),
        });
    }

    fn done(
        &self,
        run_id: &str,
        dialog_id: &str,
        ok: bool,
        reason: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        self.push(Frame::Done {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            ok,
            reason: reason.map(str::to_string),
            input_tokens,
            output_tokens,
        });
    }

    fn error(&self, run_id: &str, dialog_id: &str, message: &str) {
        self.push(Frame::Error {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            message: message.to_string(),
        });
    }
}
