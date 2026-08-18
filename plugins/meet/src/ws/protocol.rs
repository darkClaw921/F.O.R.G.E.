//! Типизированный WS-протокол плагина встреч (`/ws/meet`).
//!
//! Формат кадра — JSON с внешним тегом `type` в snake_case:
//!
//! ```json
//! {"type":"snapshot","meetings":[…],"deps":{…},"limits":{…}}
//! {"type":"meeting_update","meeting":{…}}
//! {"type":"progress","meeting_id":"…","phase":"transcribing","queued":true,"queue_position":2}
//! {"type":"ping"}
//! ```
//!
//! # Весь набор кадров объявлен СРАЗУ
//!
//! Включая те, что начнут отправляться только в Ф3–Ф6. Причина та же, что у
//! соседних плагинов: фронтовый обработчик пишется один раз по этой таблице и
//! не переписывается на каждой фазе, а незнакомый кадр в старом фронте из кэша
//! PWA не должен выглядеть ошибкой.
//!
//! # Кто и когда шлёт каждый кадр
//!
//! | кадр | источник | фаза |
//! |---|---|---|
//! | `snapshot` | первый кадр каждого соединения | **Ф2** |
//! | `meeting_update` | `repo::meetings::set_status` — ЕДИНСТВЕННАЯ точка | **Ф2** |
//! | `deps` | пробы внешних бинарей и их перепроверка | Ф5 |
//! | `progress` | этапы пайплайна, включая позицию в очереди whisper | Ф5 |
//! | `transcript_delta` | живая расшифровка по мере готовности сегментов | Ф5 |
//! | `participants` | каскад привязки имён | Ф4 |
//! | `needs_human` | капча, логин, лобби — ассистированный вход | Ф3 |
//! | `error` | воркеры | Ф3+ |
//! | `resync` | `broadcast::Lagged` | **Ф2** |
//! | `ping` | heartbeat | **Ф2** |
//!
//! # Фильтрации по конкретной встрече НЕТ
//!
//! Сервер вещает все события всем подписчикам. Индикатор записи обязан
//! работать с ЛЮБОЙ вкладки, а не только с той, где встречу запустили: человек
//! нажал кнопку в Telegram, ушёл в терминал, и запись должна быть видна ему и
//! там. Решает, что показать, фронт.
//!
//! # Очередь видна честно
//!
//! Когда whisper занят, статус встречи остаётся `transcribing`, но кадр
//! [`MeetServerMsg::Progress`] несёт `queued: true` и `queue_position`.
//! Отдельного статуса «в очереди» нет намеренно: он размножил бы таблицу
//! переходов вдвое ради информации, которая по природе своей временная.

use serde::{Deserialize, Serialize};

use crate::db::repo::{MeetingBadge, MeetingRow};
use crate::deps::DepsReport;

/// Лимиты параллелизма — уезжают в снапшот и в `GET /api/meet/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub recordings: usize,
    /// Всегда 1. Metal занимается процессом целиком: два прогона дерутся за
    /// GPU и суммарно идут медленнее одного.
    pub whisper: usize,
    pub summaries: usize,
}

/// Этап пайплайна для кадра прогресса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Joining,
    Recording,
    Transcoding,
    Transcribing,
    Summarizing,
}

/// То, что летит в broadcast и дальше каждому WS-подписчику.
///
/// Обёртки над кадром нет намеренно: рассылка идёт без фильтрации.
pub type MeetServerEvent = MeetServerMsg;

/// Сообщения сервер → клиент.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeetServerMsg {
    /// **Первый кадр каждого соединения.** Активные встречи, состояние
    /// внешних зависимостей и лимиты.
    ///
    /// Только АКТИВНЫЕ, а не все: у пользователя могут быть тысячи записанных
    /// встреч, и слать их в снапшоте означало бы мегабайты на каждый открытый
    /// сокет. Состояния завершённых встреч приезжают батчем ленты.
    Snapshot {
        meetings: Vec<MeetingRow>,
        deps: DepsReport,
        limits: Limits,
    },

    /// Встреча сменила статус. Единственный отправитель —
    /// `repo::meetings::set_status`.
    MeetingUpdate {
        // Box: вариант с полной строкой встречи вчетверо крупнее остальных, а
        // размер enum'а равен размеру самого крупного варианта — и этот enum
        // копируется на каждого подписчика broadcast'а.
        meeting: Box<MeetingRow>,
    },

    /// Перепроверка внешних зависимостей (`POST /api/meet/deps/recheck`, Ф5).
    Deps { deps: DepsReport },

    /// Прогресс этапа. `queued: true` означает «этап не начат, ждём слот».
    Progress {
        meeting_id: String,
        phase: Phase,
        #[serde(skip_serializing_if = "Option::is_none")]
        done: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(default)]
        queued: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        queue_position: Option<usize>,
    },

    /// Готовые сегменты расшифровки — дельтами, а не целиком: час встречи это
    /// 600–1200 реплик.
    TranscriptDelta {
        meeting_id: String,
        segments: Vec<crate::db::repo::segments::Segment>,
    },

    /// Список участников изменился (каскад привязки имён, Ф4).
    Participants {
        meeting_id: String,
        participants: Vec<crate::db::repo::participants::Participant>,
    },

    /// Нужен человек: капча, логин, лобби. Капчу мы НЕ обходим —
    /// показываем скриншот и ждём.
    NeedsHuman {
        meeting_id: String,
        /// `captcha` | `login` | `lobby` | `denied` | `unknown`.
        ///
        /// `denied` — площадка ОТКАЗАЛА во входе (Google Meet при
        /// ограниченном доступе хоста отклоняет анонимную заявку сам).
        /// От `lobby` отличается тем, что ждать бесполезно.
        kind: String,
    },

    /// Ошибка воркера. Не рвёт соединение.
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        meeting_id: Option<String>,
        code: String,
        message: String,
    },

    /// Прогресс скачивания модели whisper.
    ///
    /// Отдельный кадр, а не [`MeetServerMsg::Progress`], потому что у него
    /// нет и не может быть `meeting_id`: модель качают из настроек, когда
    /// никакой встречи ещё нет. Подставлять сюда пустую строку значило бы
    /// заставить фронт отличать «встреча без имени» от «это вообще не про
    /// встречу».
    ///
    /// `status`: `downloading` | `ready` | `failed`.
    ModelProgress {
        name: String,
        done: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        status: String,
        /// Текст ошибки — только у `failed`.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Подписчик отстал (`broadcast::Lagged`) — пусть перечитает состояние по
    /// REST. Разрывать соединение при отставании НЕЛЬЗЯ: реконнект дороже
    /// перечитывания, а отставание случается именно в момент пиковой
    /// нагрузки, когда лишний handshake нужен меньше всего.
    Resync,

    /// Heartbeat.
    Ping,
}

/// Сообщения клиент → сервер.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeetClientMsg {
    /// Ответ на [`MeetServerMsg::Ping`] — сбрасывает idle-таймер.
    Pong,
}

/// Батч состояний для ленты — тело ответа `GET /api/meet/badges`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BadgeEntry {
    pub chat_id: String,
    pub message_id: i64,
    #[serde(flatten)]
    pub badge: MeetingBadge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_an_external_type_tag_in_snake_case() {
        let json = serde_json::to_string(&MeetServerMsg::Ping).unwrap();
        assert_eq!(json, r#"{"type":"ping"}"#);
        let json = serde_json::to_string(&MeetServerMsg::Resync).unwrap();
        assert_eq!(json, r#"{"type":"resync"}"#);
        let json = serde_json::to_string(&MeetServerMsg::Error {
            meeting_id: None,
            code: "chrome_unavailable".into(),
            message: "нет Chrome".into(),
        })
        .unwrap();
        assert!(json.starts_with(r#"{"type":"error""#), "{json}");
        assert!(!json.contains("meeting_id"), "пустой meeting_id не сериализуется: {json}");
    }

    #[test]
    fn the_client_only_ever_says_pong() {
        assert_eq!(
            serde_json::from_str::<MeetClientMsg>(r#"{"type":"pong"}"#).unwrap(),
            MeetClientMsg::Pong
        );
        assert!(serde_json::from_str::<MeetClientMsg>(r#"{"type":"start"}"#).is_err());
    }

    #[test]
    fn the_progress_frame_can_say_it_is_only_queued() {
        let json = serde_json::to_string(&MeetServerMsg::Progress {
            meeting_id: "m1".into(),
            phase: Phase::Transcribing,
            done: None,
            total: None,
            queued: true,
            queue_position: Some(2),
        })
        .unwrap();
        assert!(json.contains(r#""queued":true"#), "{json}");
        assert!(json.contains(r#""queue_position":2"#), "{json}");
        assert!(!json.contains("done"), "пустые счётчики не сериализуются: {json}");
    }

    /// Кадр батча ленты сплющивает бейдж, чтобы фронт не разворачивал вложенный
    /// объект на каждую строку.
    #[test]
    fn the_badge_entry_is_flat_on_the_wire() {
        let entry = BadgeEntry {
            chat_id: "7".into(),
            message_id: 42,
            badge: MeetingBadge {
                meeting_id: "m1".into(),
                status: crate::status::MeetStatus::Recording,
                summary_status: crate::status::SummaryStatus::None,
                platform: crate::providers::Platform::Telemost,
                started_at: Some(1),
                ended_at: None,
                duration_ms: None,
            },
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""message_id":42"#), "{json}");
        assert!(json.contains(r#""status":"recording""#), "{json}");
        assert!(!json.contains(r#""badge""#), "бейдж обязан быть плоским: {json}");
    }

    /// Перечень кадров — контракт с фронтом. Тест ловит переименование
    /// варианта, которое иначе прошло бы молча и сломало бы обработчик.
    #[test]
    fn the_frame_names_are_frozen() {
        let names: Vec<String> = vec![
            MeetServerMsg::Resync,
            MeetServerMsg::Ping,
            MeetServerMsg::Error {
                meeting_id: None,
                code: String::new(),
                message: String::new(),
            },
            MeetServerMsg::NeedsHuman {
                meeting_id: String::new(),
                kind: String::new(),
            },
            MeetServerMsg::TranscriptDelta {
                meeting_id: String::new(),
                segments: vec![],
            },
            MeetServerMsg::Participants {
                meeting_id: String::new(),
                participants: vec![],
            },
            MeetServerMsg::ModelProgress {
                name: String::new(),
                done: 0,
                total: None,
                status: String::new(),
                message: None,
            },
        ]
        .into_iter()
        .map(|m| {
            serde_json::to_value(&m).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        assert_eq!(
            names,
            vec![
                "resync",
                "ping",
                "error",
                "needs_human",
                "transcript_delta",
                "participants",
                "model_progress"
            ]
        );
    }
}
