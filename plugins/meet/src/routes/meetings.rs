//! Роуты управления записью и чтения результата.
//!
//! Таблица-контракт — в шапке [`crate::routes`]. Здесь: старт, `stop`,
//! `cancel`, показ окна, скриншот, карточка встречи, расшифровка страницами,
//! саммари и повтор этапа.
//!
//! # Повтор этапа и «пересчитать саммари» — одна операция под двумя именами
//!
//! `POST /:id/summary` — это в точности `POST /:id/retry?from=summarizing`, и
//! оба идут через [`start_stage`]. Второе имя существует потому, что «пересчёт
//! саммари» — самостоятельное действие в глазах пользователя (кнопка в
//! карточке), а не частный случай повтора; но реализация обязана быть ОДНА,
//! иначе два пути смены статуса разъедутся в первом же исправлении.
//!
//! # Порядок проверок — не стиль, а лечение неверных диагнозов
//!
//! ```text
//! готовность бинарей (409 *_unavailable)
//!   → слот (409 busy)
//!     → резолв встречи (404)
//!       → допустимость перехода (409 invalid_state)
//!         → разбор параметров (400)
//! ```
//!
//! Тело и query разбираются **вручную внутри хендлера**. Экстрактор `Json`
//! отработал бы ДО тела функции и отдал 400 «плохой JSON» раньше, чем мы
//! успели сказать «Chrome не установлен» — и человек чинил бы не то.
//!
//! # `attached: true` — не ошибка
//!
//! Одну ссылку кидают в двух чатах, и обе кнопки обязаны показывать ОДНУ
//! запись. Поэтому повторный старт по тому же `url_key` возвращает
//! существующую встречу с `attached: true`, а не 409. Дедуп структурный: его
//! держит частичный уникальный индекс `ux_meetings_active_url`, а не мьютекс в
//! памяти — мьютекс защищает один процесс и забывает всё при перезапуске.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::bot::JoinMode;
use crate::db::repo::{meetings, MeetingRow, SourceKey};
use crate::error::MeetError;
use crate::hub::event::StopReason;
use crate::pipeline::run::Stage;
use crate::providers::parse_meeting_url;
use crate::recording;
use crate::state::MeetState;
use crate::status::{on_command, MeetCommand, MeetStatus};

use super::{check_ladder, Preconditions};

/// Тело `POST /api/meet/meetings`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StartRequest {
    /// Ссылка на встречу как есть из сообщения.
    pub url: String,
    /// Откуда пришла ссылка. Непрозрачная тройка: плагин про мессенджеры
    /// ничего не знает.
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<i64>,
    /// `normal` (по умолчанию) либо `assisted`.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Ответ `POST /api/meet/meetings`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartResponse {
    pub meeting_id: String,
    /// `true` — встреча уже писалась, к ней привязан новый источник.
    pub attached: bool,
    pub status: MeetStatus,
    pub mode: String,
}

/// Разбирает тело старта ВРУЧНУЮ.
///
/// Отдельной чистой функцией, чтобы порядок проверок можно было проверить
/// тестом, а не рассуждением: сначала зависимости и слот, и только потом это.
pub fn parse_start_request(body: &[u8]) -> Result<(StartRequest, JoinMode, Option<SourceKey>), MeetError> {
    let req: StartRequest = serde_json::from_slice(body)
        .map_err(|e| MeetError::BadRequest(format!("тело не разбирается: {e}")))?;
    if req.url.trim().is_empty() {
        return Err(MeetError::BadRequest("нужна ссылка на встречу".to_string()));
    }
    let mode = match req.mode.as_deref() {
        None | Some("") => JoinMode::Normal,
        Some(m) => JoinMode::parse(m)
            .ok_or_else(|| MeetError::BadRequest(format!("неизвестный режим {m}")))?,
    };
    // Источник — либо тройка целиком, либо ничего: половина связи хуже её
    // отсутствия, потому что бейдж повис бы на несуществующем сообщении.
    let source = match (&req.plugin, &req.chat_id, req.message_id) {
        (Some(p), Some(c), Some(m)) if !p.is_empty() && !c.is_empty() => {
            Some(SourceKey::new(p.clone(), c.clone(), m))
        }
        (None, None, None) => None,
        _ => {
            return Err(MeetError::BadRequest(
                "источник задаётся тройкой plugin + chat_id + message_id".to_string(),
            ))
        }
    };
    Ok((req, mode, source))
}

/// `POST /api/meet/meetings` — старт записи.
pub async fn start(State(state): State<Arc<MeetState>>, body: Bytes) -> Result<Response, MeetError> {
    // 1. Зависимости. Разбор тела — ПОСЛЕ: иначе человек с неустановленным
    //    Chrome получил бы 400 про JSON и чинил бы не то.
    let deps = state.deps_snapshot().await;
    let dep_blocker = deps.record_blocker();

    // 2. Слот. Занимаем ДО резолва: `Busy` обязан приходить за микросекунды.
    let slot = if dep_blocker.is_none() {
        match state.hub.try_acquire_recording() {
            Ok(slot) => Some(slot),
            Err(MeetError::Busy) => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    check_ladder(Preconditions {
        dep_blocker,
        busy: dep_blocker.is_none() && slot.is_none(),
        // Резолва встречи у старта нет: он её создаёт. Подставляем пустышку,
        // чтобы ступень 404 не сработала на пустом месте.
        meeting: Some(&PLACEHOLDER),
        transition: None,
        params: parse_start_request(&body).err(),
    })?;

    let slot = slot.expect("лестница обязана была отвергнуть отсутствие слота");
    let (req, mode, source) = parse_start_request(&body)?;

    let link = parse_meeting_url(&req.url)
        .ok_or_else(|| MeetError::BadRequest("ссылка не похожа на встречу".to_string()))?;

    // Идентификатор каталога появится только после вставки строки, поэтому
    // каталог адресуется тем же `id`: путь детерминирован, и пост-обработка
    // после перезапуска найдёт его, ничего не помня.
    let meeting_dir = state.config.cache_dir.join("pending");
    let outcome = meetings::create_or_attach(
        &state,
        meetings::NewMeeting {
            link,
            source,
            title: req.title.clone(),
            mode: mode.as_str().to_string(),
            bot_name: state.config.bot_name.clone(),
            meeting_dir: meeting_dir.to_string_lossy().to_string(),
        },
    )
    .await?;

    if outcome.attached {
        // Слот не нужен: запись уже идёт. Освобождаем немедленно, иначе
        // повторный клик по той же ссылке съедал бы слоты.
        drop(slot);
        return Ok(Json(StartResponse {
            meeting_id: outcome.meeting.id.clone(),
            attached: true,
            status: outcome.meeting.status,
            mode: outcome.meeting.mode.clone(),
        })
        .into_response());
    }

    // Каталог встречи знает свой идентификатор — правим сразу после вставки.
    let dir = state.config.meeting_dir(&outcome.meeting.id);
    meetings::set_meeting_dir(&state, &outcome.meeting.id, &dir.to_string_lossy()).await?;
    let mut meeting = outcome.meeting.clone();
    meeting.meeting_dir = dir.to_string_lossy().to_string();

    recording::start(&state, &meeting, mode, slot).await?;

    Ok(Json(StartResponse {
        meeting_id: meeting.id,
        attached: false,
        status: MeetStatus::Launching,
        mode: mode.as_str().to_string(),
    })
    .into_response())
}

/// Пустышка для ступени резолва у старта: встречи ещё нет, и 404 здесь
/// невозможен по смыслу.
static PLACEHOLDER: std::sync::LazyLock<MeetingRow> = std::sync::LazyLock::new(|| MeetingRow {
    id: String::new(),
    url_key: String::new(),
    canonical_url: String::new(),
    platform: crate::providers::Platform::Telemost,
    room_id: String::new(),
    title: None,
    status: MeetStatus::Queued,
    status_reason: None,
    summary_status: crate::status::SummaryStatus::None,
    mode: "normal".to_string(),
    bot_name: None,
    created_at: 0,
    updated_at: 0,
    started_at: None,
    ended_at: None,
    duration_ms: None,
    meeting_dir: String::new(),
    audio_path: None,
    transcript_path: None,
    summary_path: None,
    track_mode: None,
    capture_path: None,
});

/// Ответ команд `stop` / `cancel`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandResponse {
    pub meeting_id: String,
    pub status: MeetStatus,
}

async fn command(
    state: &Arc<MeetState>,
    meeting_id: &str,
    cmd: MeetCommand,
) -> Result<Json<CommandResponse>, MeetError> {
    let meeting = meetings::get(state, meeting_id).await.ok();
    let target = meeting.as_ref().and_then(|m| on_command(m.status, cmd));

    check_ladder(Preconditions {
        dep_blocker: None,
        busy: false,
        meeting: meeting.as_ref(),
        // Если команда бессмысленна в текущем статусе, `on_command` вернул
        // `None` — подставляем текущий статус, чтобы получить `invalid_state`
        // с внятными обеими сторонами перехода. У НЕСУЩЕСТВУЮЩЕЙ встречи
        // статуса нет вовсе, и подставлять нечего: ступень 404 сработает
        // раньше, а `None` здесь означает «переход не проверяем».
        transition: match (&meeting, target) {
            (Some(_), Some(to)) => Some(to),
            (Some(m), None) => Some(m.status),
            (None, _) => None,
        },
        params: None,
    })?;

    let meeting = meeting.expect("лестница обязана была отвергнуть отсутствие встречи");
    let target = target.ok_or(MeetError::InvalidState {
        from: meeting.status.as_str(),
        to: meeting.status.as_str(),
    })?;

    let reason = match cmd {
        MeetCommand::Stop => StopReason::UserStop,
        MeetCommand::Cancel => StopReason::UserCancel,
    };

    // Идёт запись — просим сеанс остановиться и даём воркеру довести дело до
    // конца: он закроет WAV, починит заголовки и переведёт статус.
    if let Some(session) = state.sessions.get(meeting_id).await {
        session.request_stop(reason).await?;
        return Ok(Json(CommandResponse {
            meeting_id: meeting.id,
            status: meeting.status,
        }));
    }

    // Сеанса нет (запись уже в пост-обработке либо ещё не началась) — меняем
    // статус сами.
    let row = meetings::set_status(state, meeting_id, target, Some(reason.as_str())).await?;
    Ok(Json(CommandResponse {
        meeting_id: row.id,
        status: row.status,
    }))
}

/// `POST /api/meet/meetings/:id/stop` — «встреча кончилась, обработай».
pub async fn stop(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Json<CommandResponse>, MeetError> {
    command(&state, &id, MeetCommand::Stop).await
}

/// `POST /api/meet/meetings/:id/cancel` — «прерви и выброси».
pub async fn cancel(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Json<CommandResponse>, MeetError> {
    command(&state, &id, MeetCommand::Cancel).await
}

/// Ответ показа окна.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowResponse {
    pub meeting_id: String,
    /// Фактические границы после переноса. Оконный сервер macOS вправе
    /// подрезать координаты, и разница между «попросили» и «получилось» — то,
    /// что ассистированному входу надо знать, а не предполагать.
    pub visible: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// `POST /api/meet/meetings/:id/show-window` — показать окно бота человеку.
///
/// **Единственный путь, которому позволено вывести окно на передний план**, и
/// он всегда начинается с действия человека.
pub async fn show_window(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<WindowResponse>, MeetError> {
    // Тело необязательно: `{"visible": false}` прячет окно обратно.
    let visible = if body.is_empty() {
        true
    } else {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("visible").and_then(serde_json::Value::as_bool))
            .unwrap_or(true)
    };

    let session = state
        .sessions
        .get(&id)
        .await
        .ok_or(MeetError::NotFound)?;
    let report = session.show_window(visible).await?;
    Ok(Json(WindowResponse {
        meeting_id: id,
        visible: report.visible,
        left: report.left,
        top: report.top,
        width: report.width,
        height: report.height,
    }))
}

/// `GET /api/meet/meetings/:id/screenshot` — «что видит бот».
///
/// Не имеет права падать на ещё не загруженной странице: именно в этот момент
/// он и нужен больше всего — человек смотрит, почему бот не вошёл.
pub async fn screenshot(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Response, MeetError> {
    let session = state
        .sessions
        .get(&id)
        .await
        .ok_or(MeetError::NotFound)?;
    let png = session.screenshot().await?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            // Кадр живёт секунды: «что видит бот» дёргается раз в пять секунд,
            // и закэшированная картинка была бы прямым враньём.
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    )
        .into_response())
}

/// `POST /api/meet/meetings/:id/confirm` — «я внутри, пиши».
pub async fn confirm(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Json<CommandResponse>, MeetError> {
    let session = state
        .sessions
        .get(&id)
        .await
        .ok_or(MeetError::NotFound)?;
    session.confirm_human_ready().await?;
    let row = meetings::get(&state, &id).await?;
    Ok(Json(CommandResponse {
        meeting_id: row.id,
        status: row.status,
    }))
}

// --- карточка, расшифровка, саммари -------------------------------------------

/// Тело `GET /api/meet/meetings/:id`.
///
/// Обёртка `{meeting, participants}`, а не голая строка: карточке нужны оба
/// списка, а второй запрос за участниками стоил бы лишнего похода в базу на
/// каждое открытие попапа.
#[derive(Debug, Clone, Serialize)]
pub struct MeetingDto {
    pub meeting: MeetingRow,
    pub participants: Vec<crate::db::repo::participants::Participant>,
    /// Сколько всего сегментов расшифровки. Нужен кнопке «ещё» во вкладке
    /// расшифровки до первого запроса страницы.
    pub segments_total: usize,
}

/// `GET /api/meet/meetings/:id` — карточка встречи.
pub async fn get(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Json<MeetingDto>, MeetError> {
    let meeting = meetings::get(&state, &id).await?;
    // Участники и счётчик — приятная, но не обязательная часть: ошибка на них
    // не имеет права превратить карточку в 500.
    let participants = crate::db::repo::participants::list(&state, &id)
        .await
        .unwrap_or_default();
    let segments_total = crate::db::repo::segments::count(&state, &id)
        .await
        .unwrap_or(0);
    Ok(Json(MeetingDto {
        meeting,
        participants,
        segments_total,
    }))
}

/// Тело `GET /api/meet/meetings/:id/transcript`.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptDto {
    pub meeting_id: String,
    pub offset: i64,
    pub limit: i64,
    pub total: usize,
    pub segments: Vec<crate::db::repo::segments::Segment>,
}

/// Сколько сегментов отдаётся за раз по умолчанию.
///
/// Столько же, сколько просит фронт (`TRANSCRIPT_PAGE` в `meet-popup.js`):
/// примерно полчаса встречи.
pub const TRANSCRIPT_PAGE: i64 = 500;

/// Разбирает `?offset&limit` ВРУЧНУЮ — см. «Порядок проверок» в шапке.
pub fn parse_page_query(raw: Option<&str>) -> Result<(i64, i64), MeetError> {
    let mut offset = 0i64;
    let mut limit = TRANSCRIPT_PAGE;
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        let parse = |name: &str| -> Result<i64, MeetError> {
            v.parse::<i64>()
                .map_err(|_| MeetError::BadRequest(format!("{name} должен быть целым числом")))
        };
        match k.as_ref() {
            "offset" => offset = parse("offset")?,
            "limit" => limit = parse("limit")?,
            // Незнакомые параметры игнорируются: новый фронт может дописать
            // версию клиента, а старый сервер не имеет права отвечать на это
            // ошибкой.
            _ => {}
        }
    }
    if offset < 0 {
        return Err(MeetError::BadRequest("offset отрицателен".to_string()));
    }
    if limit <= 0 {
        return Err(MeetError::BadRequest("limit должен быть положительным".to_string()));
    }
    Ok((offset, limit))
}

/// `GET /api/meet/meetings/:id/transcript?offset&limit` — расшифровка
/// страницами.
///
/// Пагинация, а не виртуализация: час встречи это 600–1200 реплик, а лента
/// рутинно держит 500+ строк — виртуализация здесь была бы решением
/// несуществующей задачи.
pub async fn transcript(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<TranscriptDto>, MeetError> {
    let meeting = meetings::get(&state, &id).await.ok();
    check_ladder(Preconditions {
        meeting: meeting.as_ref(),
        params: parse_page_query(raw.as_deref()).err(),
        ..Default::default()
    })?;
    let (offset, limit) = parse_page_query(raw.as_deref())?;
    let total = crate::db::repo::segments::count(&state, &id).await?;
    let segments = crate::db::repo::segments::page(&state, &id, offset, limit).await?;
    Ok(Json(TranscriptDto {
        meeting_id: id,
        offset,
        limit,
        total,
        segments,
    }))
}

/// Тело `GET /api/meet/meetings/:id/summary`.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryDto {
    pub meeting_id: String,
    /// Статус САММАРИ, а не встречи: у него своя колонка ровно затем, чтобы
    /// его неудача не была неудачей встречи.
    pub status: crate::status::SummaryStatus,
    /// Текст. `null` — саммари ещё нет либо оно не получилось.
    pub text: Option<String>,
}

/// `GET /api/meet/meetings/:id/summary` — саммари встречи.
///
/// **Отсутствие файла — не 404.** 404 здесь означал бы «нет такой встречи», а
/// саммари могло не считаться вовсе (выключено конфигом) или не получиться — и
/// это разные вещи, которые карточка показывает по-разному. Поэтому 200 с
/// `text: null` и честным `status`.
pub async fn summary(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Json<SummaryDto>, MeetError> {
    let meeting = meetings::get(&state, &id).await?;
    let text = crate::summary::read_summary(std::path::Path::new(&meeting.meeting_dir));
    Ok(Json(SummaryDto {
        meeting_id: id,
        status: meeting.summary_status,
        text,
    }))
}

// --- повтор этапа --------------------------------------------------------------

/// Ответ повтора этапа.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetryResponse {
    pub meeting_id: String,
    /// Этап, с которого пошёл повтор.
    pub from: &'static str,
    pub status: MeetStatus,
}

/// Разбирает `?from=` ВРУЧНУЮ.
///
/// Неизвестное значение — 400, а не 409: «этапа `banana` не существует» и
/// «этап есть, но сейчас в него нельзя» — разные диагнозы, и лечатся они
/// по-разному.
pub fn parse_retry_stage(raw: Option<&str>) -> Result<Stage, MeetError> {
    let mut from: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        if k == "from" {
            from = Some(v.into_owned());
        }
    }
    let Some(from) = from.filter(|s| !s.is_empty()) else {
        return Err(MeetError::BadRequest(
            "нужен параметр from: transcoding, transcribing или summarizing".to_string(),
        ));
    };
    Stage::parse(&from).ok_or_else(|| {
        MeetError::BadRequest(format!(
            "неизвестный этап {from}: бывают transcoding, transcribing и summarizing"
        ))
    })
}

/// Общий путь повтора: лестница проверок, перевод статуса и запуск в фоне.
///
/// `stage` приходит уже разобранным (либо ошибкой разбора) — потому что от
/// него зависит, ЧЕГО не хватает: расшифровке нужен whisper с моделью и Metal,
/// саммари — включённый саммаризатор. Битый параметр трактуется как самый
/// тяжёлый путь: человек с неустановленным whisper обязан узнать про whisper, а
/// не про опечатку в query.
async fn start_stage(
    state: &Arc<MeetState>,
    meeting_id: &str,
    stage: Result<Stage, MeetError>,
) -> Result<Json<RetryResponse>, MeetError> {
    let deps = state.deps_snapshot().await;
    let dep_blocker = match stage {
        Ok(Stage::Summarizing) => summary_blocker(state),
        _ => deps.transcribe_blocker(),
    };

    let meeting = meetings::get(state, meeting_id).await.ok();
    check_ladder(Preconditions {
        dep_blocker,
        busy: false,
        meeting: meeting.as_ref(),
        transition: stage.as_ref().ok().map(|s| s.status()),
        params: stage.as_ref().err().cloned(),
    })?;

    let stage = stage?;
    let row = meetings::set_status(state, meeting_id, stage.status(), Some("retry")).await?;
    crate::pipeline::run::spawn_from(state, meeting_id, stage);
    Ok(Json(RetryResponse {
        meeting_id: row.id,
        from: stage.as_str(),
        status: row.status,
    }))
}

/// Почему саммари сейчас невозможно. `None` — возможно.
///
/// Отдельной пробы Claude CLI нет намеренно: его отсутствие обнаруживается при
/// запуске и попадает в `summary_status = failed` вместе с хвостом stderr —
/// то есть человек всё равно узнаёт причину, но не ценой ещё одного
/// периодического процесса на старте devforge.
fn summary_blocker(state: &Arc<MeetState>) -> Option<&'static str> {
    (!state.config.summary_enabled).then_some("summary_disabled")
}

/// `POST /api/meet/meetings/:id/retry?from=` — повтор этапа.
pub async fn retry(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<RetryResponse>, MeetError> {
    let stage = parse_retry_stage(raw.as_deref());
    start_stage(&state, &id, stage).await
}

/// `POST /api/meet/meetings/:id/summary` — пересчитать саммари поверх готового
/// транскрипта.
///
/// Ровно `retry?from=summarizing`, и намеренно тем же кодом: см. шапку модуля.
pub async fn regenerate_summary(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
) -> Result<Json<RetryResponse>, MeetError> {
    start_stage(&state, &id, Ok(Stage::Summarizing)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_parsed_by_hand_with_named_errors() {
        let (req, mode, source) = parse_start_request(
            br#"{"url":"https://telemost.yandex.ru/j/1","plugin":"telegram","chat_id":"7","message_id":42}"#,
        )
        .unwrap();
        assert_eq!(req.url, "https://telemost.yandex.ru/j/1");
        assert_eq!(mode, JoinMode::Normal, "режим по умолчанию — normal");
        assert_eq!(source, Some(SourceKey::new("telegram", "7", 42)));

        let (_, mode, source) =
            parse_start_request(br#"{"url":"https://telemost.yandex.ru/j/1","mode":"assisted"}"#).unwrap();
        assert_eq!(mode, JoinMode::Assisted);
        assert_eq!(source, None, "старт без привязки к сообщению законен");
    }

    /// ПОЛОВИНА ИСТОЧНИКА хуже его отсутствия: бейдж повис бы на
    /// несуществующем сообщении.
    #[test]
    fn a_half_filled_source_is_rejected() {
        for body in [
            br#"{"url":"https://telemost.yandex.ru/j/1","plugin":"telegram"}"#.as_slice(),
            br#"{"url":"https://telemost.yandex.ru/j/1","chat_id":"7","message_id":1}"#.as_slice(),
            br#"{"url":"https://telemost.yandex.ru/j/1","plugin":"telegram","chat_id":"7"}"#.as_slice(),
        ] {
            let err = parse_start_request(body).unwrap_err();
            assert!(matches!(err, MeetError::BadRequest(_)), "{err}");
        }
    }

    #[test]
    fn broken_bodies_are_named() {
        assert!(matches!(
            parse_start_request("не json".as_bytes()).unwrap_err(),
            MeetError::BadRequest(_)
        ));
        assert!(matches!(
            parse_start_request(br#"{"url":""}"#).unwrap_err(),
            MeetError::BadRequest(_)
        ));
        assert!(matches!(
            parse_start_request(br#"{"url":"https://telemost.yandex.ru/j/1","mode":"headless"}"#)
                .unwrap_err(),
            MeetError::BadRequest(_)
        ));
    }
}
