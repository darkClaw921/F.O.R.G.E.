//! Сегменты расшифровки.
//!
//! Час встречи — 2000–4000 сегментов, и ровно они делают JSON-стор
//! непригодным: каждое сохранение переписывало бы файл целиком, а краш
//! оставлял бы «последнюю удачную полную версию».
//!
//! `words_json` хранит ПОТОКЕННЫЕ метки whisper (`-ojf`). Проба Ф0 уточнила:
//! метки именно потокенные (subword), а не пословные — склейка в слова
//! делается в `speakers.rs`. Нужны они затем, чтобы резать сегмент ровно на
//! границе смены спикера: посегментные метки по 5–30 секунд регулярно её
//! перекрывают.

use serde::{Deserialize, Serialize};

use crate::error::MeetError;
use crate::state::MeetState;

/// Сегмент расшифровки.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub id: i64,
    pub meeting_id: String,
    pub idx: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub participant_id: Option<i64>,
    pub speaker_name: Option<String>,
    pub track_idx: Option<i64>,
    /// `dominant / Σoverlap`. Ниже 0.6 при втором претенденте ≥25 % сегмент
    /// режется по пословным меткам и помечается [`Segment::overlapped`].
    pub confidence: Option<f64>,
    pub overlapped: bool,
}

/// Черновик сегмента.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSegment {
    pub idx: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub participant_id: Option<i64>,
    pub speaker_name: Option<String>,
    pub track_idx: Option<i64>,
    pub confidence: Option<f64>,
    pub overlapped: bool,
    pub words_json: Option<String>,
}

/// Заменяет ВСЕ сегменты встречи одним пакетом.
///
/// Замена, а не дозапись: повтор расшифровки (`retry?from=transcribing`)
/// обязан дать тот же результат, что и первый прогон, а не удвоенный. Обе
/// операции в одной транзакции, поэтому промежуточного состояния «старых уже
/// нет, новых ещё нет» не существует.
pub async fn replace_all(
    state: &MeetState,
    meeting_id: &str,
    segments: Vec<NewSegment>,
) -> Result<usize, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let n = db
        .conn()
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM meeting_segments WHERE meeting_id = ?1",
                rusqlite::params![id],
            )?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO meeting_segments \
                        (meeting_id, idx, start_ms, end_ms, text, participant_id, speaker_name, \
                         track_idx, confidence, overlapped, words_json) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                )?;
                for s in &segments {
                    stmt.execute(rusqlite::params![
                        id,
                        s.idx,
                        s.start_ms,
                        s.end_ms,
                        s.text,
                        s.participant_id,
                        s.speaker_name,
                        s.track_idx,
                        s.confidence,
                        i64::from(s.overlapped),
                        s.words_json,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(segments.len())
        })
        .await?;
    Ok(n)
}

/// Страница расшифровки.
///
/// Виртуализации во фронте нет: час встречи это 600–1200 реплик, а лента
/// рутинно держит 500+ строк. Вместо неё пагинация — отсюда `limit`/`offset`.
pub async fn page(
    state: &MeetState,
    meeting_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<Segment>, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let limit = limit.clamp(1, 2000);
    let offset = offset.max(0);
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, meeting_id, idx, start_ms, end_ms, text, participant_id, \
                        speaker_name, track_idx, confidence, overlapped \
                 FROM meeting_segments WHERE meeting_id = ?1 \
                 ORDER BY idx LIMIT ?2 OFFSET ?3",
            )?;
            let out = stmt
                .query_map(rusqlite::params![id, limit, offset], |r| {
                    Ok(Segment {
                        id: r.get(0)?,
                        meeting_id: r.get(1)?,
                        idx: r.get(2)?,
                        start_ms: r.get(3)?,
                        end_ms: r.get(4)?,
                        text: r.get(5)?,
                        participant_id: r.get(6)?,
                        speaker_name: r.get(7)?,
                        track_idx: r.get(8)?,
                        confidence: r.get(9)?,
                        overlapped: r.get::<_, i64>(10)? != 0,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// ВСЕ сегменты встречи разом, по возрастанию `idx`.
///
/// Существует ради проверок «в базе не появилось ни одной реплики»: там нужен
/// именно ВЕСЬ список, а не страница, и просить [`page`] с выдуманным потолком
/// значило бы проверять «первые 2000 реплик пусты» вместо «реплик нет».
///
/// В боевом пути не зовётся: интерфейс листает расшифровку страницами
/// ([`page`]), потому что час встречи — это 600–1200 реплик.
pub async fn list(state: &MeetState, meeting_id: &str) -> Result<Vec<Segment>, MeetError> {
    let mut out = Vec::new();
    loop {
        let chunk = page(state, meeting_id, out.len() as i64, 2000).await?;
        let done = chunk.len() < 2000;
        out.extend(chunk);
        if done {
            return Ok(out);
        }
    }
}

/// Переписывает имя спикера в сегментах ОДНОГО участника.
///
/// # Как сегмент связывается с участником
///
/// Двумя способами, и оба нужны:
///
/// 1. `participant_id` — точная связь. Её проставляет эта же функция при
///    первом переименовании, поэтому ВТОРОЙ PATCH по тому же участнику уже
///    не зависит от совпадения имён.
/// 2. `speaker_name IN (…)` — связь по подписи, единственная доступная сразу
///    после пайплайна: `store_results` кладёт сегменты с `participant_id =
///    NULL` (атрибуция работает с именами, а не с идентификаторами строк
///    базы). Список подписей собирает
///    [`crate::routes::speakers::speaker_labels`].
///
/// Возвращает число изменённых строк.
pub async fn rename_speaker(
    state: &MeetState,
    meeting_id: &str,
    participant_id: i64,
    old_labels: &[String],
    new_name: &str,
) -> Result<usize, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let new_name = new_name.to_string();
    let labels: Vec<String> = old_labels.to_vec();
    let n = db
        .conn()
        .call(move |c| {
            let placeholders = (0..labels.len())
                .map(|i| format!("?{}", i + 4))
                .collect::<Vec<_>>()
                .join(",");
            let by_label = if labels.is_empty() {
                String::new()
            } else {
                format!(" OR (participant_id IS NULL AND speaker_name IN ({placeholders}))")
            };
            let sql = format!(
                "UPDATE meeting_segments SET speaker_name = ?2, participant_id = ?3 \
                 WHERE meeting_id = ?1 AND (participant_id = ?3{by_label})"
            );
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
                Box::new(id),
                Box::new(new_name),
                Box::new(participant_id),
            ];
            for l in labels {
                params.push(Box::new(l));
            }
            let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            Ok(c.execute(&sql, refs.as_slice())?)
        })
        .await?;
    Ok(n)
}

/// Имена спикеров в порядке ПЕРВОГО ПОЯВЛЕНИЯ.
///
/// Порядок появления, а не алфавит, и по той же причине, что в
/// `pipeline::render::speaker_order`: список читается как состав встречи, а
/// алфавитный ничего не говорит о том, кто её вёл. Уезжает в шапку промпта
/// саммари (`<participants>`).
///
/// `DISTINCT` в SQL не годится: он не сохраняет порядок. Поэтому выборка идёт
/// по `idx`, а повторы снимаются на месте.
pub async fn speaker_names(state: &MeetState, meeting_id: &str) -> Result<Vec<String>, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let rows: Vec<String> = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT speaker_name FROM meeting_segments \
                 WHERE meeting_id = ?1 AND speaker_name IS NOT NULL AND speaker_name <> '' \
                 ORDER BY idx",
            )?;
            let out = stmt
                .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;

    let mut seen = std::collections::HashSet::new();
    Ok(rows.into_iter().filter(|n| seen.insert(n.clone())).collect())
}

/// Сколько сегментов у встречи.
pub async fn count(state: &MeetState, meeting_id: &str) -> Result<usize, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let n: i64 = db
        .conn()
        .call(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM meeting_segments WHERE meeting_id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )?)
        })
        .await?;
    Ok(n as usize)
}
