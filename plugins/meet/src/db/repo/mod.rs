//! Репозитории поверх [`crate::db::Db`].
//!
//! # Единственная точка мутации статуса
//!
//! [`meetings::set_status`] — и больше ничто в крейте не имеет права писать
//! `UPDATE meetings SET status`. Функция одним вызовом делает три вещи:
//!
//! 1. меняет строку в базе И пишет строку в `meeting_events` — в ОДНОЙ
//!    транзакции, поэтому «статус сменился, а журнала нет» невозможно;
//! 2. обновляет тёплый индекс бейджей по всем источникам этой встречи;
//! 3. вещает событие в broadcast.
//!
//! Разъезд базы и индекса возможен ровно в одном случае — если кто-то мутирует
//! статус мимо этой функции. Поэтому на отсутствие таких мест есть тест
//! `no_status_update_bypasses_set_status`, который грепает исходники крейта:
//! договорённость, которую нельзя нарушить незаметно, надёжнее договорённости,
//! записанной в комментарии.
//!
//! # Почему тёплый индекс вообще нужен
//!
//! `GET /api/meet/badges` дёргается на КАЖДОМ рендере ленты чата. Ходить за
//! этим в SQLite значит платить запросом за каждый скролл. 10 000 встреч в
//! памяти — около 800 КиБ, что дешевле одного кадра видео. База при этом
//! остаётся источником истины: индекс прогревается из неё на старте и
//! обновляется только через `set_status`.

pub mod events;
pub mod meetings;
pub mod participants;
pub mod segments;
pub mod sources;
pub mod spans;

use serde::{Deserialize, Serialize};

use crate::providers::Platform;
use crate::status::{MeetStatus, SummaryStatus};

/// Ключ связи «встреча ↔ сообщение мессенджера».
///
/// Непрозрачная тройка с фронта: плагин про мессенджеры ничего не знает и
/// знать не должен — ссылку приносит фронт, имена участников берутся из самой
/// встречи. Инвариант идентификаторов проекта соблюдён: `chat_id` — строка
/// (у Bitrix это `"chat7"`, у VK и Telegram числа), `message_id` — число.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceKey {
    pub plugin: String,
    pub chat_id: String,
    pub message_id: i64,
}

impl SourceKey {
    pub fn new(plugin: impl Into<String>, chat_id: impl Into<String>, message_id: i64) -> Self {
        Self {
            plugin: plugin.into(),
            chat_id: chat_id.into(),
            message_id,
        }
    }
}

/// То, что видит кнопка в ленте. Живёт в тёплом индексе и уезжает в
/// `GET /api/meet/badges`.
///
/// Намеренно узкая: путей на диске, названия комнаты и прочего здесь нет —
/// иначе индекс на 10 000 встреч перестал бы стоить 800 КиБ, а батч ленты
/// возил бы по сети данные, которые кнопке не нужны.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingBadge {
    pub meeting_id: String,
    pub status: MeetStatus,
    pub summary_status: SummaryStatus,
    pub platform: Platform,
    /// Момент старта ЗАПИСИ, unix-секунды. Таймер во фронте считается от него,
    /// а не инкрементом: инкремент обнуляется при перезагрузке вкладки.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// Полная строка встречи.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeetingRow {
    pub id: String,
    pub url_key: String,
    pub canonical_url: String,
    pub platform: Platform,
    pub room_id: String,
    pub title: Option<String>,
    pub status: MeetStatus,
    pub status_reason: Option<String>,
    pub summary_status: SummaryStatus,
    pub mode: String,
    pub bot_name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub meeting_dir: String,
    pub audio_path: Option<String>,
    pub transcript_path: Option<String>,
    pub summary_path: Option<String>,
    pub track_mode: Option<String>,
    pub capture_path: Option<String>,
}

impl MeetingRow {
    /// Проекция для ленты.
    pub fn to_badge(&self) -> MeetingBadge {
        MeetingBadge {
            meeting_id: self.id.clone(),
            status: self.status,
            summary_status: self.summary_status,
            platform: self.platform,
            started_at: self.started_at,
            ended_at: self.ended_at,
            duration_ms: self.duration_ms,
        }
    }
}

/// Разбор строки `meetings` в [`MeetingRow`].
///
/// Порядок колонок фиксирован [`MEETING_COLUMNS`]: у каждого запроса он один и
/// тот же, поэтому добавление колонки — правка в одном месте, а не в шести
/// разъезжающихся `SELECT`.
pub(crate) const MEETING_COLUMNS: &str = "id, url_key, canonical_url, platform, room_id, title, \
     status, status_reason, summary_status, mode, bot_name, created_at, updated_at, started_at, \
     ended_at, duration_ms, meeting_dir, audio_path, transcript_path, summary_path, track_mode, \
     capture_path";

pub(crate) fn row_to_meeting(row: &rusqlite::Row<'_>) -> rusqlite::Result<MeetingRow> {
    let platform_raw: String = row.get(3)?;
    let status_raw: String = row.get(6)?;
    let summary_raw: String = row.get(8)?;
    Ok(MeetingRow {
        id: row.get(0)?,
        url_key: row.get(1)?,
        canonical_url: row.get(2)?,
        // Неизвестная платформа/статус в базе — не повод падать: строку мог
        // записать более новый devforge. Берём безопасное значение и оставляем
        // сырое в логе вызывающего.
        platform: Platform::parse(&platform_raw).unwrap_or(Platform::Telemost),
        room_id: row.get(4)?,
        title: row.get(5)?,
        status: MeetStatus::parse(&status_raw).unwrap_or(MeetStatus::Failed),
        status_reason: row.get(7)?,
        summary_status: SummaryStatus::parse(&summary_raw).unwrap_or(SummaryStatus::None),
        mode: row.get(9)?,
        bot_name: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        started_at: row.get(13)?,
        ended_at: row.get(14)?,
        duration_ms: row.get(15)?,
        meeting_dir: row.get(16)?,
        audio_path: row.get(17)?,
        transcript_path: row.get(18)?,
        summary_path: row.get(19)?,
        track_mode: row.get(20)?,
        capture_path: row.get(21)?,
    })
}

/// Текущее время в unix-секундах.
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
