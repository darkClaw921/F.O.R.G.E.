//! Куда ядро вещает кадры расшифровки.
//!
//! Это ЕДИНСТВЕННАЯ развязка крейта от плагина, и сделана она по образцу
//! `forge-chat-analysis::sink`. Если бы [`crate::hub`] знал `VkServerMsg`,
//! вместе с ним в ядро переехали бы и WS-протокол VK, и его broadcast-канал —
//! то есть Telegram получил бы чужой enum кадров. Вместо этого ядро зовёт
//! четыре метода, а плагин в четыре строки перекладывает их в СВОЙ кадр
//! (`VkServerMsg` / `TgServerMsg` / `B24ServerMsg`) и вещает как ему удобно.
//!
//! # Почему методы синхронные
//!
//! Их зовёт задача прогона, в том числе в цикле прогресса. Всякая реализация,
//! которую мы знаем, — это `broadcast::Sender::send`, то есть мгновенная
//! неблокирующая операция; сделать методы `async` значило бы разрешить
//! реализации ЖДАТЬ внутри прогона, а прогон ждать не имеет права: пока он
//! стои́т, тикает его собственный таймаут.
//!
//! # Порядок и обязательность кадров
//!
//! - [`VoiceSink::started`] — ровно один, синхронно в [`crate::hub::VoiceHub`],
//!   ДО первого прогресса;
//! - [`VoiceSink::progress`] — ноль и больше, проценты не убывают;
//! - [`VoiceSink::error`] — ноль или один, всегда ПЕРЕД финальным кадром;
//! - [`VoiceSink::done`] — ровно один на run.
//!
//! # Один run — НЕСКОЛЬКО подписчиков
//!
//! Повторный запрос на уже идущую расшифровку подключается к существующему
//! run'у ([`crate::hub`]), а не запускает второй. Поэтому кадры адресованы
//! КЛЮЧОМ вложения, а не подписчиком: плагин вещает их всем, кто смотрит на
//! это сообщение, и второй запросивший видит ту же полосу прогресса с середины.

use crate::store::{Transcript, VoiceKey};

/// Приёмник кадров расшифровки.
pub trait VoiceSink: Send + Sync + 'static {
    /// Расшифровка началась.
    ///
    /// `engine` — `"local"` или `"cloud"`: человек имеет право знать, ушло ли
    /// его голосовое в чужое облако, ДО того, как оно туда ушло.
    fn started(&self, run_id: &str, key: &VoiceKey, engine: &str);

    /// Прогресс, проценты `0..=100`.
    fn progress(&self, run_id: &str, key: &VoiceKey, percent: u8);

    /// Ошибка run'а: машинный код и текст с хвостом stderr.
    ///
    /// Код передаётся ОТДЕЛЬНО от текста, потому что по нему фронт различает
    /// случаи: `metal_unavailable` — это «включите облако или почините GPU», а
    /// `cloud_no_funds` — «пополните счёт», и разбирать это из русской фразы
    /// плагин не должен.
    fn error(&self, run_id: &str, key: &VoiceKey, code: &str, message: &str);

    /// Run завершён успешно; расшифровка уже лежит в сторе.
    fn done(&self, run_id: &str, key: &VoiceKey, transcript: &Transcript);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Engine;
    use std::sync::{Arc, Mutex};

    /// Приёмник, который просто записывает всё, что ему сказали.
    #[derive(Default)]
    pub struct Recorder {
        pub frames: Mutex<Vec<String>>,
    }

    impl VoiceSink for Recorder {
        fn started(&self, run_id: &str, key: &VoiceKey, engine: &str) {
            self.frames
                .lock()
                .unwrap()
                .push(format!("started {run_id} {} {engine}", key.message_id));
        }
        fn progress(&self, _run_id: &str, _key: &VoiceKey, percent: u8) {
            self.frames.lock().unwrap().push(format!("progress {percent}"));
        }
        fn error(&self, _run_id: &str, _key: &VoiceKey, code: &str, _message: &str) {
            self.frames.lock().unwrap().push(format!("error {code}"));
        }
        fn done(&self, _run_id: &str, _key: &VoiceKey, t: &Transcript) {
            self.frames.lock().unwrap().push(format!("done {}", t.text));
        }
    }

    /// Трейт объектно-безопасен: ядро держит его за `Arc<dyn VoiceSink>`, и
    /// иначе реестр не собрался бы вовсе.
    #[test]
    fn the_sink_is_object_safe() {
        let r = Arc::new(Recorder::default());
        let dyn_sink: Arc<dyn VoiceSink> = r.clone();
        let key = VoiceKey::new("peer", "1", 0);
        dyn_sink.started("run-1", &key, "cloud");
        dyn_sink.progress("run-1", &key, 50);
        dyn_sink.done("run-1", &key, &Transcript::new("готово", Engine::Cloud));

        let frames = r.frames.lock().unwrap().clone();
        assert_eq!(frames, vec!["started run-1 1 cloud", "progress 50", "done готово"]);
    }
}
