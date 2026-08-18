//! Участники встречи.
//!
//! Имя берётся каскадом (Ф4): `msid`/`mid` → SSRC → корреляция VAD-таймлайна с
//! DOM-активным спикером → DOM напрямую → «Участник N» плюс ручное
//! переименование. Колонка `source` хранит, какая ступень сработала: интерфейс
//! показывает «Участник 2» и имя из DOM по-разному, а без этой колонки
//! отличить их было бы нечем.
//!
//! Для Телемоста (режим `Slots`, измерено пробой Ф0) ступени, опирающиеся на
//! «трек = человек», не выполняются НИКОГДА. Это не ошибка и не деградация —
//! это свойство платформы, и код обязан молча идти дальше по каскаду.

use serde::{Deserialize, Serialize};

use crate::error::MeetError;
use crate::state::MeetState;

/// Участник встречи.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub id: i64,
    pub meeting_id: String,
    /// Ключ внутри встречи: DOM-идентификатор плитки, `msid` трека или
    /// синтетический `track-3`. Ключ, а не имя: имя может смениться по ходу
    /// встречи, а связь с треком — нет.
    pub participant_key: String,
    pub display_name: Option<String>,
    pub track_idx: Option<i64>,
    pub is_bot: bool,
    /// `dom` | `msid` | `ssrc` | `correlation` | `manual`.
    pub source: Option<String>,
    pub confidence: Option<f64>,
    pub first_seen_ms: Option<i64>,
    pub last_seen_ms: Option<i64>,
}

/// Заводит участника либо обновляет уже известного.
///
/// `display_name` перезаписывается только непустым значением: ступень каскада,
/// сработавшая позже и хуже, не имеет права стереть уже найденное имя.
pub async fn upsert(
    state: &MeetState,
    meeting_id: &str,
    participant_key: &str,
    display_name: Option<&str>,
    track_idx: Option<i64>,
    source: Option<&str>,
    confidence: Option<f64>,
) -> Result<i64, MeetError> {
    let db = state.require_db()?;
    let (mid, key, name, source) = (
        meeting_id.to_string(),
        participant_key.to_string(),
        display_name.filter(|s| !s.trim().is_empty()).map(str::to_string),
        source.map(str::to_string),
    );
    let id = db
        .conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO meeting_participants \
                    (meeting_id, participant_key, display_name, track_idx, source, confidence) \
                 VALUES(?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT(meeting_id, participant_key) DO UPDATE SET \
                    display_name = COALESCE(excluded.display_name, display_name), \
                    track_idx    = COALESCE(excluded.track_idx, track_idx), \
                    source       = COALESCE(excluded.source, source), \
                    confidence   = COALESCE(excluded.confidence, confidence)",
                rusqlite::params![mid, key, name, track_idx, source, confidence],
            )?;
            Ok(c.query_row(
                "SELECT id FROM meeting_participants WHERE meeting_id = ?1 AND participant_key = ?2",
                rusqlite::params![mid, key],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await?;
    Ok(id)
}

/// Ручное переименование пользователем.
///
/// Оно и есть последняя ступень каскада, и терпима она именно потому, что у
/// нас есть пер-трековое аудио: пользователь жмёт play на «Участнике 2», за
/// две секунды узнаёт голос и подписывает.
pub async fn rename(state: &MeetState, participant_id: i64, name: &str) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let name = name.trim().to_string();
    let affected = db
        .conn()
        .call(move |c| {
            Ok(c.execute(
                "UPDATE meeting_participants SET display_name = ?2, source = 'manual', \
                    confidence = 1.0 WHERE id = ?1",
                rusqlite::params![participant_id, name],
            )?)
        })
        .await?;
    if affected == 0 {
        return Err(MeetError::NotFound);
    }
    Ok(())
}

/// Отмечает промежуток, в котором участник наблюдался.
pub async fn touch_seen(
    state: &MeetState,
    participant_id: i64,
    at_ms: i64,
) -> Result<(), MeetError> {
    let db = state.require_db()?;
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE meeting_participants SET \
                    first_seen_ms = MIN(COALESCE(first_seen_ms, ?2), ?2), \
                    last_seen_ms  = MAX(COALESCE(last_seen_ms, ?2), ?2) \
                 WHERE id = ?1",
                rusqlite::params![participant_id, at_ms],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Участники встречи в порядке появления.
pub async fn list(state: &MeetState, meeting_id: &str) -> Result<Vec<Participant>, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, meeting_id, participant_key, display_name, track_idx, is_bot, \
                        source, confidence, first_seen_ms, last_seen_ms \
                 FROM meeting_participants WHERE meeting_id = ?1 ORDER BY id",
            )?;
            let out = stmt
                .query_map(rusqlite::params![id], |r| {
                    Ok(Participant {
                        id: r.get(0)?,
                        meeting_id: r.get(1)?,
                        participant_key: r.get(2)?,
                        display_name: r.get(3)?,
                        track_idx: r.get(4)?,
                        is_bot: r.get::<_, i64>(5)? != 0,
                        source: r.get(6)?,
                        confidence: r.get(7)?,
                        first_seen_ms: r.get(8)?,
                        last_seen_ms: r.get(9)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}
