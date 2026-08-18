//! Интервалы речи: «кто говорил с какой по какую миллисекунду».
//!
//! Сюда пишут ДВА независимых источника, и поддержка обоих обязательна:
//!
//! - `vad` — пер-трековый детектор речи. Работает там, где трек соответствует
//!   человеку (режим `PerParticipant`).
//! - `dom` — индикатор активного спикера самой встречи. Для Телемоста это
//!   ЕДИНСТВЕННЫЙ источник: проба Ф0 показала два аудиотрека при трёх
//!   удалённых участниках и `track.muted`, не менявшийся ни разу при смене
//!   говорящего. Резать по треку там нечего.
//!
//! Поэтому `attribute_segments` (Ф5) обязана получать интервалы отсюда, а не
//! из пер-трекового VAD напрямую: иначе платформа с общим треком осталась бы
//! без спикеров вовсе.
//!
//! Ось времени одна — миллисекунды от `T0` = старта первой записи.

use serde::{Deserialize, Serialize};

use crate::error::MeetError;
use crate::state::MeetState;

/// Интервал речи.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerSpan {
    pub id: i64,
    pub meeting_id: String,
    pub participant_id: Option<i64>,
    pub track_idx: Option<i64>,
    pub start_ms: i64,
    pub end_ms: i64,
    /// `vad` либо `dom`.
    pub source: String,
}

/// Черновик интервала — то, что кладут источники.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSpan {
    pub participant_id: Option<i64>,
    pub track_idx: Option<i64>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub source: String,
}

/// Пакетная вставка в одной транзакции.
///
/// Именно пакетом: час встречи даёт сотни интервалов, и транзакция на каждый
/// означала бы сотни fsync'ов на диск во время активной записи.
pub async fn insert_many(
    state: &MeetState,
    meeting_id: &str,
    spans: Vec<NewSpan>,
) -> Result<usize, MeetError> {
    if spans.is_empty() {
        return Ok(0);
    }
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let n = db
        .conn()
        .call(move |c| {
            let tx = c.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO meeting_speaker_spans \
                        (meeting_id, participant_id, track_idx, start_ms, end_ms, source) \
                     VALUES(?1,?2,?3,?4,?5,?6)",
                )?;
                for s in &spans {
                    stmt.execute(rusqlite::params![
                        id,
                        s.participant_id,
                        s.track_idx,
                        s.start_ms,
                        s.end_ms,
                        s.source
                    ])?;
                }
            }
            tx.commit()?;
            Ok(spans.len())
        })
        .await?;
    Ok(n)
}

/// Интервалы встречи, отсортированные по началу.
///
/// Сортировка здесь, а не у вызывающего: `attribute_segments` идёт двумя
/// указателями и на несортированном входе дала бы тихо неверный результат, а
/// не ошибку.
pub async fn list(state: &MeetState, meeting_id: &str) -> Result<Vec<SpeakerSpan>, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, meeting_id, participant_id, track_idx, start_ms, end_ms, source \
                 FROM meeting_speaker_spans WHERE meeting_id = ?1 ORDER BY start_ms, id",
            )?;
            let out = stmt
                .query_map(rusqlite::params![id], |r| {
                    Ok(SpeakerSpan {
                        id: r.get(0)?,
                        meeting_id: r.get(1)?,
                        participant_id: r.get(2)?,
                        track_idx: r.get(3)?,
                        start_ms: r.get(4)?,
                        end_ms: r.get(5)?,
                        source: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// Стирает интервалы встречи. Нужно повтору этапа: этапы идемпотентны по
/// детерминированным путям, и повторная атрибуция не имеет права удвоить
/// интервалы.
pub async fn clear(state: &MeetState, meeting_id: &str) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    db.conn()
        .call(move |c| {
            c.execute(
                "DELETE FROM meeting_speaker_spans WHERE meeting_id = ?1",
                rusqlite::params![id],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}
