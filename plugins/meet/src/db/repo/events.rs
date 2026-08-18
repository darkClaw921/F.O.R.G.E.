//! Журнал переходов встречи.
//!
//! Единственный способ через сутки ответить, ПОЧЕМУ запись умерла. Строки
//! пишет [`super::meetings::set_status`] в той же транзакции, что и сам
//! переход, поэтому «статус сменился, а журнала нет» невозможно. Здесь только
//! чтение и одна ручная вставка для событий, не связанных с переходом статуса
//! (диагностика бота, потери кадров).

use serde::{Deserialize, Serialize};

use crate::db::repo::now;
use crate::error::MeetError;
use crate::state::MeetState;

/// Строка журнала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingEvent {
    pub id: i64,
    pub meeting_id: String,
    pub at: i64,
    /// `None` только у самой первой строки — создания встречи.
    pub from_status: Option<String>,
    pub to_status: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
}

/// Дописывает строку, не меняя статуса.
///
/// Для диагностики: «трек 3 закончился», «кадров потеряно 12», «DOM-проба не
/// нашла кнопку выхода». Статус при этом остаётся прежним, поэтому
/// `from_status` равен `to_status` — журнал читают люди, и пустая колонка
/// сбивала бы их сильнее повтора.
pub async fn note(
    state: &MeetState,
    meeting_id: &str,
    status: &str,
    reason: &str,
    detail: Option<&str>,
) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let (id, status, reason, detail, ts) = (
        meeting_id.to_string(),
        status.to_string(),
        reason.to_string(),
        detail.map(str::to_string),
        now(),
    );
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO meeting_events(meeting_id, at, from_status, to_status, reason, detail) \
                 VALUES(?1,?2,?3,?3,?4,?5)",
                rusqlite::params![id, ts, status, reason, detail],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Журнал одной встречи в хронологическом порядке.
pub async fn list(state: &MeetState, meeting_id: &str) -> Result<Vec<MeetingEvent>, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, meeting_id, at, from_status, to_status, reason, detail \
                 FROM meeting_events WHERE meeting_id = ?1 ORDER BY id",
            )?;
            let out = stmt
                .query_map(rusqlite::params![id], |r| {
                    Ok(MeetingEvent {
                        id: r.get(0)?,
                        meeting_id: r.get(1)?,
                        at: r.get(2)?,
                        from_status: r.get(3)?,
                        to_status: r.get(4)?,
                        reason: r.get(5)?,
                        detail: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// Сколько строк в журнале встречи. Дешевле, чем тянуть их все.
pub async fn count(state: &MeetState, meeting_id: &str) -> Result<usize, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let n: i64 = db
        .conn()
        .call(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM meeting_events WHERE meeting_id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )?)
        })
        .await?;
    Ok(n as usize)
}
