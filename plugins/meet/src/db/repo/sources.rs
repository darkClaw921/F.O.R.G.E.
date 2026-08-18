//! Связь «встреча ↔ сообщение мессенджера» и батч состояний для ленты.
//!
//! # Почему GET с ДИАПАЗОНОМ, а не POST со списком идентификаторов
//!
//! Три довода, и ни один не про стиль:
//!
//! 1. Это чистое чтение. POST на чтение ломает кэширование, идемпотентность и
//!    смысл метода.
//! 2. Идентификаторы сообщений МОНОТОННЫ внутри чата. Страница ленты — это
//!    всегда непрерывный отрезок, поэтому диапазон описывает её точно, а
//!    список из пятидесяти чисел описывает то же самое в пятьдесят раз
//!    длиннее.
//! 3. Диапазон — ОДИН скан по префиксу первичного ключа
//!    `(plugin, chat_id, message_id)`, а таблица объявлена `WITHOUT ROWID`,
//!    то есть ключ и есть таблица. Список идентификаторов дал бы пятьдесят
//!    точечных поисков.
//!
//! В ответе только те сообщения, у которых встреча ЕСТЬ. Отсутствие бейджа и
//! означает «можно записать»: статуса `detected` в базе нет, иначе таблица
//! росла бы от простого скролла ленты.

use crate::db::repo::{now, MeetingBadge, SourceKey};
use crate::error::MeetError;
use crate::providers::Platform;
use crate::state::MeetState;
use crate::status::{MeetStatus, SummaryStatus};

/// Ключи всех источников встречи — внутри уже открытой транзакции.
pub(crate) fn source_keys_tx(
    tx: &rusqlite::Transaction<'_>,
    meeting_id: &str,
) -> rusqlite::Result<Vec<SourceKey>> {
    let mut stmt = tx.prepare(
        "SELECT plugin, chat_id, message_id FROM meeting_sources WHERE meeting_id = ?1",
    )?;
    let out = stmt
        .query_map(rusqlite::params![meeting_id], |r| {
            Ok(SourceKey {
                plugin: r.get(0)?,
                chat_id: r.get(1)?,
                message_id: r.get(2)?,
            })
        })?
        .collect();
    out
}

/// То же самое, но по соединению (прогрев индекса на старте).
pub(crate) fn source_keys_conn(
    c: &rusqlite::Connection,
    meeting_id: &str,
) -> rusqlite::Result<Vec<SourceKey>> {
    let mut stmt = c.prepare(
        "SELECT plugin, chat_id, message_id FROM meeting_sources WHERE meeting_id = ?1",
    )?;
    let out = stmt
        .query_map(rusqlite::params![meeting_id], |r| {
            Ok(SourceKey {
                plugin: r.get(0)?,
                chat_id: r.get(1)?,
                message_id: r.get(2)?,
            })
        })?
        .collect();
    out
}

/// Привязывает ещё одно сообщение к существующей встрече.
///
/// Одну ссылку кидают в двух чатах, и обе кнопки обязаны показывать одну
/// запись — ради этого источники и вынесены отдельной таблицей.
pub async fn attach(
    state: &MeetState,
    key: &SourceKey,
    meeting_id: &str,
) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let (owned_key, owned_id, ts) = (key.clone(), meeting_id.to_string(), now());
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO meeting_sources(plugin, chat_id, message_id, meeting_id, created_at) \
                 VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![
                    owned_key.plugin,
                    owned_key.chat_id,
                    owned_key.message_id,
                    owned_id,
                    ts
                ],
            )?;
            Ok(())
        })
        .await?;

    // Новая кнопка обязана увидеть состояние немедленно, а не после
    // следующего перехода статуса.
    let row = super::meetings::get(state, meeting_id).await?;
    state.publish_badge(&row, std::slice::from_ref(key)).await;
    Ok(())
}

/// Батч состояний ИЗ БАЗЫ по диапазону идентификаторов сообщений.
///
/// Боевой путь `GET /api/meet/badges` ходит не сюда, а в тёплый индекс —
/// иначе каждый скролл ленты стоил бы запроса к SQLite. Эта функция нужна для
/// прогрева, для сверки индекса с базой в тестах и как честный ответ на
/// вопрос «что на самом деле в хранилище».
pub async fn range(
    state: &MeetState,
    plugin: &str,
    chat_id: &str,
    min_id: i64,
    max_id: i64,
) -> Result<Vec<(SourceKey, MeetingBadge)>, MeetError> {
    let db = state.require_db()?;
    let (plugin, chat_id) = (plugin.to_string(), chat_id.to_string());
    let rows = db
        .conn()
        .call(move |c| {
            // Один индексный скан по префиксу первичного ключа: равенство по
            // двум первым колонкам плюс диапазон по третьей.
            let mut stmt = c.prepare(
                "SELECT s.plugin, s.chat_id, s.message_id, m.id, m.status, m.summary_status, \
                        m.platform, m.started_at, m.ended_at, m.duration_ms \
                 FROM meeting_sources s JOIN meetings m ON m.id = s.meeting_id \
                 WHERE s.plugin = ?1 AND s.chat_id = ?2 AND s.message_id BETWEEN ?3 AND ?4 \
                 ORDER BY s.message_id",
            )?;
            let out = stmt
                .query_map(rusqlite::params![plugin, chat_id, min_id, max_id], |r| {
                    let key = SourceKey {
                        plugin: r.get(0)?,
                        chat_id: r.get(1)?,
                        message_id: r.get(2)?,
                    };
                    let status: String = r.get(4)?;
                    let summary: String = r.get(5)?;
                    let platform: String = r.get(6)?;
                    let badge = MeetingBadge {
                        meeting_id: r.get(3)?,
                        status: MeetStatus::parse(&status).unwrap_or(MeetStatus::Failed),
                        summary_status: SummaryStatus::parse(&summary)
                            .unwrap_or(SummaryStatus::None),
                        platform: Platform::parse(&platform).unwrap_or(Platform::Telemost),
                        started_at: r.get(7)?,
                        ended_at: r.get(8)?,
                        duration_ms: r.get(9)?,
                    };
                    Ok((key, badge))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// Все связи всех встреч — прогрев тёплого индекса.
pub async fn all(state: &MeetState) -> Result<Vec<(SourceKey, String)>, MeetError> {
    let db = state.require_db()?;
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt =
                c.prepare("SELECT plugin, chat_id, message_id, meeting_id FROM meeting_sources")?;
            let out = stmt
                .query_map([], |r| {
                    Ok((
                        SourceKey {
                            plugin: r.get(0)?,
                            chat_id: r.get(1)?,
                            message_id: r.get(2)?,
                        },
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}
