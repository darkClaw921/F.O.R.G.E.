//! Репозиторий встреч. **Здесь и только здесь меняется `meetings.status`.**
//!
//! [`set_status`] — единственная точка мутации. Она в одной транзакции правит
//! строку и пишет журнал, затем обновляет тёплый индекс бейджей и вещает
//! событие. Любой второй путь смены статуса немедленно даёт расхождение базы и
//! индекса, поэтому на его отсутствие есть грепающий тест
//! (`no_status_update_bypasses_set_status`).

use rusqlite::OptionalExtension;

use crate::db::repo::{now, row_to_meeting, MeetingRow, SourceKey, MEETING_COLUMNS};
use crate::error::MeetError;
use crate::providers::MeetingLink;
use crate::state::MeetState;
use crate::status::{can_transition, MeetStatus, SummaryStatus};
use crate::ws::protocol::MeetServerMsg;

/// Заявка на запись встречи.
#[derive(Debug, Clone)]
pub struct NewMeeting {
    pub link: MeetingLink,
    /// Откуда пришла ссылка. `None` — старт без привязки к сообщению
    /// (например, из вкладки настроек).
    pub source: Option<SourceKey>,
    pub title: Option<String>,
    /// `normal` либо `assisted`.
    pub mode: String,
    pub bot_name: String,
    /// Каталог встречи на диске.
    pub meeting_dir: String,
}

/// Результат [`create_or_attach`].
#[derive(Debug, Clone)]
pub struct CreateOutcome {
    pub meeting: MeetingRow,
    /// `true` — встреча уже писалась, к ней ПРИВЯЗАН новый источник.
    ///
    /// Это НЕ ошибка и НЕ 409: одну ссылку кидают в двух чатах, и обе кнопки
    /// обязаны показывать одну запись.
    pub attached: bool,
}

/// Создаёт встречу либо привязывает источник к уже идущей записи той же
/// комнаты.
///
/// Дедуп держится на частичном уникальном индексе `ux_meetings_active_url`, а
/// не на мьютексе: мьютекс защищает один процесс и забывает всё при
/// перезапуске, а индекс живёт в самой базе. Две «одновременные» попытки
/// сериализуются рабочим потоком соединения, и вторая видит первую строку
/// уже вставленной.
pub async fn create_or_attach(
    state: &MeetState,
    req: NewMeeting,
) -> Result<CreateOutcome, MeetError> {
    let db = state.require_db()?;
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();

    let (row, attached, keys) = db
        .conn()
        .call(move |c| {
            let tx = c.transaction()?;

            // Активная запись на ту же комнату — берём её.
            let active_placeholders = MeetStatus::ACTIVE
                .iter()
                .map(|s| format!("'{}'", s.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            let existing: Option<MeetingRow> = tx
                .query_row(
                    &format!(
                        "SELECT {MEETING_COLUMNS} FROM meetings \
                         WHERE url_key = ?1 AND status IN ({active_placeholders}) LIMIT 1"
                    ),
                    rusqlite::params![req.link.url_key],
                    row_to_meeting,
                )
                .optional()?;

            let (row, attached) = match existing {
                Some(row) => (row, true),
                None => {
                    tx.execute(
                        "INSERT INTO meetings(id, url_key, canonical_url, platform, room_id, title, \
                            status, status_reason, summary_status, mode, bot_name, created_at, \
                            updated_at, meeting_dir) \
                         VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,?8,?9,?10,?11,?11,?12)",
                        rusqlite::params![
                            id,
                            req.link.url_key,
                            req.link.canonical_url,
                            req.link.platform.as_str(),
                            req.link.room_id,
                            req.title,
                            MeetStatus::Queued.as_str(),
                            SummaryStatus::None.as_str(),
                            req.mode,
                            req.bot_name,
                            ts,
                            req.meeting_dir,
                        ],
                    )?;
                    // Первая строка журнала: `from_status` у неё NULL, и это
                    // единственный случай, когда он пуст.
                    tx.execute(
                        "INSERT INTO meeting_events(meeting_id, at, from_status, to_status, reason) \
                         VALUES(?1, ?2, NULL, ?3, 'created')",
                        rusqlite::params![id, ts, MeetStatus::Queued.as_str()],
                    )?;
                    let row = tx.query_row(
                        &format!("SELECT {MEETING_COLUMNS} FROM meetings WHERE id = ?1"),
                        rusqlite::params![id],
                        row_to_meeting,
                    )?;
                    (row, false)
                }
            };

            if let Some(key) = &req.source {
                // Повторный клик по той же кнопке не обязан быть ошибкой.
                tx.execute(
                    "INSERT OR REPLACE INTO meeting_sources(plugin, chat_id, message_id, meeting_id, created_at) \
                     VALUES(?1,?2,?3,?4,?5)",
                    rusqlite::params![key.plugin, key.chat_id, key.message_id, row.id, ts],
                )?;
            }

            let keys = super::sources::source_keys_tx(&tx, &row.id)?;
            tx.commit()?;
            Ok((row, attached, keys))
        })
        .await?;

    state.publish_badge(&row, &keys).await;
    Ok(CreateOutcome {
        meeting: row,
        attached,
    })
}

/// **ЕДИНСТВЕННАЯ ТОЧКА МУТАЦИИ СТАТУСА.**
///
/// Одной функцией: строка в базе, строка в `meeting_events` (та же
/// транзакция), тёплый индекс бейджей и broadcast. Пропуск любого из четырёх
/// шагов даёт свой класс багов — от «непонятно, почему встреча умерла» до
/// «лента показывает запись, которой уже нет».
///
/// # Побочные эффекты по времени
///
/// - переход в `recording` проставляет `started_at`, если тот пуст: таймер во
///   фронте считается от него, а не инкрементом;
/// - выход из записи (`leaving` / `transcoding` / терминальные) проставляет
///   `ended_at` и, если возможно, грубую `duration_ms`. Точную длительность
///   позже перезапишет `ffprobe` — она единственная считается по фактическому
///   аудио.
pub async fn set_status(
    state: &MeetState,
    meeting_id: &str,
    to: MeetStatus,
    reason: Option<&str>,
) -> Result<MeetingRow, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let reason = reason.map(|s| s.to_string());
    let ts = now();

    let outcome: Result<(MeetingRow, Vec<SourceKey>, MeetStatus), MeetError> = db
        .conn()
        .call(move |c| {
            let tx = c.transaction()?;

            let current: Option<(String, Option<i64>, Option<i64>)> = tx
                .query_row(
                    "SELECT status, started_at, ended_at FROM meetings WHERE id = ?1",
                    rusqlite::params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;

            let Some((from_raw, started_at, ended_at)) = current else {
                return Ok(Err(MeetError::NotFound));
            };
            let Some(from) = MeetStatus::parse(&from_raw) else {
                return Ok(Err(MeetError::Internal(format!(
                    "в базе неизвестный статус {from_raw:?}"
                ))));
            };
            if !can_transition(from, to) {
                return Ok(Err(MeetError::InvalidState {
                    from: from.as_str(),
                    to: to.as_str(),
                }));
            }

            let started_at = if to == MeetStatus::Recording && started_at.is_none() {
                Some(ts)
            } else {
                started_at
            };
            let leaves_recording = matches!(
                to,
                MeetStatus::Leaving | MeetStatus::Transcoding
            ) || to.is_terminal();
            let ended_at = if leaves_recording && ended_at.is_none() {
                Some(ts)
            } else {
                ended_at
            };
            let duration_ms = match (started_at, ended_at) {
                (Some(s), Some(e)) if e >= s => Some((e - s) * 1000),
                _ => None,
            };

            tx.execute(
                "UPDATE meetings SET status = ?2, status_reason = ?3, updated_at = ?4, \
                    started_at = ?5, ended_at = ?6, \
                    duration_ms = COALESCE(duration_ms, ?7) \
                 WHERE id = ?1",
                rusqlite::params![id, to.as_str(), reason, ts, started_at, ended_at, duration_ms],
            )?;
            tx.execute(
                "INSERT INTO meeting_events(meeting_id, at, from_status, to_status, reason) \
                 VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![id, ts, from.as_str(), to.as_str(), reason],
            )?;

            let row = tx.query_row(
                &format!("SELECT {MEETING_COLUMNS} FROM meetings WHERE id = ?1"),
                rusqlite::params![id],
                row_to_meeting,
            )?;
            let keys = super::sources::source_keys_tx(&tx, &row.id)?;
            tx.commit()?;
            Ok(Ok((row, keys, from)))
        })
        .await?;

    let (row, keys, from) = outcome?;
    state.publish_badge(&row, &keys).await;
    state.broadcast_event(MeetServerMsg::MeetingUpdate {
        meeting: Box::new(row.clone()),
    });
    tracing::info!(
        target: "forge_meet",
        meeting_id = %row.id,
        from = from.as_str(),
        to = to.as_str(),
        reason = row.status_reason.as_deref().unwrap_or(""),
        "forge-meet: переход статуса"
    );
    Ok(row)
}

/// Записывает исход саммаризации по правилу «провал саммари не роняет
/// встречу».
///
/// Отдельная функция, а не два вызова на месте: соблазн написать
/// `set_status(..., Failed)` при неуспехе Claude слишком велик, а цена ошибки —
/// час расшифровки, выброшенный из-за неответившего саммаризатора.
pub async fn finish_summary(
    state: &MeetState,
    meeting_id: &str,
    ok: bool,
) -> Result<MeetingRow, MeetError> {
    let (status, summary) = crate::status::on_summary_finished(ok);
    set_summary_status(state, meeting_id, summary).await?;
    let reason = if ok { "summary_ready" } else { "summary_failed" };
    set_status(state, meeting_id, status, Some(reason)).await
}

/// Меняет ТОЛЬКО `summary_status`. Статус встречи не трогает — у саммари своя
/// колонка ровно затем, чтобы его неудача не была неудачей встречи.
pub async fn set_summary_status(
    state: &MeetState,
    meeting_id: &str,
    summary: SummaryStatus,
) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let ts = now();
    let affected: usize = db
        .conn()
        .call(move |c| {
            Ok(c.execute(
                "UPDATE meetings SET summary_status = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, summary.as_str(), ts],
            )?)
        })
        .await?;
    if affected == 0 {
        return Err(MeetError::NotFound);
    }
    Ok(())
}

/// Проставляет пути артефактов на диске. Статуса не касается.
pub async fn set_paths(
    state: &MeetState,
    meeting_id: &str,
    audio: Option<&str>,
    transcript: Option<&str>,
    summary: Option<&str>,
) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let (audio, transcript, summary) = (
        audio.map(str::to_string),
        transcript.map(str::to_string),
        summary.map(str::to_string),
    );
    let ts = now();
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE meetings SET \
                    audio_path      = COALESCE(?2, audio_path), \
                    transcript_path = COALESCE(?3, transcript_path), \
                    summary_path    = COALESCE(?4, summary_path), \
                    updated_at      = ?5 \
                 WHERE id = ?1",
                rusqlite::params![id, audio, transcript, summary, ts],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Стирает пути артефактов. Зовётся ретенцией после удаления каталога.
///
/// Отдельная функция, а не `set_paths(None, None, None)`: у той все три
/// параметра идут через `COALESCE`, то есть `None` означает «не менять». Иначе
/// карточка вычищенной встречи показывала бы путь к файлу, которого нет, — и
/// плеер молча отвечал бы 404 вместо внятного «файлы вычищены».
pub async fn clear_paths(state: &MeetState, meeting_id: &str) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let ts = now();
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE meetings SET audio_path = NULL, transcript_path = NULL, \
                    summary_path = NULL, updated_at = ?2 WHERE id = ?1",
                rusqlite::params![id, ts],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Проставляет каталог встречи. Статуса не касается.
///
/// Нужна ровно потому, что путь каталога содержит `meeting_id`, а тот
/// появляется только при вставке строки. Городить генерацию идентификатора до
/// вставки было бы хуже: `id` — свойство базы, и второй его источник рано или
/// поздно разъедется с первым.
pub async fn set_meeting_dir(
    state: &MeetState,
    meeting_id: &str,
    dir: &str,
) -> Result<(), MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let dir = dir.to_string();
    let ts = now();
    let affected: usize = db
        .conn()
        .call(move |c| {
            Ok(c.execute(
                "UPDATE meetings SET meeting_dir = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, dir, ts],
            )?)
        })
        .await?;
    if affected == 0 {
        return Err(MeetError::NotFound);
    }
    Ok(())
}

/// Одна встреча по идентификатору.
pub async fn get(state: &MeetState, meeting_id: &str) -> Result<MeetingRow, MeetError> {
    let db = state.require_db()?;
    let id = meeting_id.to_string();
    let row: Option<MeetingRow> = db
        .conn()
        .call(move |c| {
            Ok(c.query_row(
                &format!("SELECT {MEETING_COLUMNS} FROM meetings WHERE id = ?1"),
                rusqlite::params![id],
                row_to_meeting,
            )
            .optional()?)
        })
        .await?;
    row.ok_or(MeetError::NotFound)
}

/// Все встречи в перечисленных статусах, свежие первыми.
pub async fn list_by_status(
    state: &MeetState,
    statuses: &[MeetStatus],
) -> Result<Vec<MeetingRow>, MeetError> {
    if statuses.is_empty() {
        return Ok(Vec::new());
    }
    let db = state.require_db()?;
    let list = statuses
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {MEETING_COLUMNS} FROM meetings WHERE status IN ({list}) \
                 ORDER BY updated_at DESC"
            ))?;
            let out = stmt
                .query_map([], row_to_meeting)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// Все встречи, СВЕЖИЕ ПЕРВЫМИ. Обход ретенции.
///
/// Порядок по `created_at`, а не по `updated_at`: «последние N записей, которые
/// не удаляются никогда» — это последние ПРОВЕДЁННЫЕ встречи, и открытие
/// старой карточки не должно двигать её в этом списке.
pub async fn all(state: &MeetState) -> Result<Vec<MeetingRow>, MeetError> {
    let db = state.require_db()?;
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {MEETING_COLUMNS} FROM meetings ORDER BY created_at DESC, id DESC"
            ))?;
            let out = stmt
                .query_map([], row_to_meeting)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// Все встречи — прогрев тёплого индекса на старте.
///
/// Отдаётся вместе с ключами источников, потому что индекс адресуется именно
/// ими: одна встреча может висеть на нескольких сообщениях в разных чатах.
pub async fn all_with_sources(
    state: &MeetState,
) -> Result<Vec<(MeetingRow, Vec<SourceKey>)>, MeetError> {
    let db = state.require_db()?;
    let rows = db
        .conn()
        .call(move |c| {
            let mut stmt =
                c.prepare(&format!("SELECT {MEETING_COLUMNS} FROM meetings ORDER BY created_at"))?;
            let meetings = stmt
                .query_map([], row_to_meeting)?
                .collect::<Result<Vec<_>, _>>()?;
            let mut out = Vec::with_capacity(meetings.len());
            for m in meetings {
                let keys = super::sources::source_keys_conn(c, &m.id)?;
                out.push((m, keys));
            }
            Ok(out)
        })
        .await?;
    Ok(rows)
}

/// Сколько встреч сейчас занимают слот записи.
pub async fn count_active(state: &MeetState) -> Result<usize, MeetError> {
    let db = state.require_db()?;
    let list = MeetStatus::ACTIVE
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let n: i64 = db
        .conn()
        .call(move |c| {
            Ok(c.query_row(
                &format!("SELECT COUNT(*) FROM meetings WHERE status IN ({list})"),
                [],
                |r| r.get(0),
            )?)
        })
        .await?;
    Ok(n as usize)
}
