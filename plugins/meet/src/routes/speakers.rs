//! `PATCH /api/meet/meetings/:id/speakers` — ручное именование участников.
//!
//! # Это не украшение, а штатная ступень каскада
//!
//! Живая проба Ф0 доказала: Телемост работает в режиме `Slots` — дорожка НЕ
//! соответствует человеку, и атрибуция строится корреляцией пер-трекового VAD
//! с DOM-индикатором активного спикера. Промахи в такой схеме неизбежны, а
//! значит ручное исправление — не «на всякий случай», а последняя ступень
//! каскада `msid → SSRC → корреляция → DOM → «Участник N» + руки`.
//!
//! Терпима эта ступень ровно потому, что у нас есть ПЕР-ТРЕКОВОЕ АУДИО:
//! человек жмёт «послушать» на «Участнике 2»
//! (`GET /:id/audio?track=1`, см. [`super::audio`]), за две секунды узнаёт
//! голос и подписывает.
//!
//! # Переименование ПЕРЕСОБИРАЕТ РАСШИФРОВКУ, а не меняет подпись
//!
//! Меняются три вещи разом, и меньшего набора не бывает:
//!
//! | что | зачем |
//! |---|---|
//! | `meeting_participants.display_name` | список участников в карточке |
//! | `meeting_segments.speaker_name` | вкладка расшифровки (её источник — база) |
//! | `transcript.json` + `transcript.md` | файлы, которые читают человек и промпт саммари |
//!
//! Пропусти мы файлы — саммари, пересчитанное после переименования, всё равно
//! говорило бы «Участник 2», потому что промпт читает `transcript.md`.
//!
//! Файлы пересобираются ИЗ `transcript.json`, а не из базы: в базе нет
//! потокенных меток (`words`), и рендер из неё стёр бы их при первой же правке
//! имени. Читает и разбирает — [`crate::pipeline::render::read_transcript`].
//!
//! # Связь «сегмент ↔ участник»
//!
//! После пайплайна у сегментов `participant_id IS NULL`: атрибуция работает с
//! ИМЕНАМИ, а не со строками базы. Поэтому первый PATCH ищет сегменты по
//! подписи ([`speaker_labels`]) и заодно проставляет `participant_id`, а
//! второй уже опирается на него. Отсюда же идемпотентность: повторный PATCH
//! тем же значением находит те же строки и записывает то же самое.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::db::repo::{meetings, participants, segments};
use crate::db::repo::participants::Participant;
use crate::error::MeetError;
use crate::state::MeetState;
use crate::ws::protocol::MeetServerMsg;

use super::{check_ladder, Preconditions};

/// Одно переименование.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpeakerPatch {
    /// `meeting_participants.id`.
    pub id: i64,
    pub display_name: String,
}

/// Тело `PATCH /api/meet/meetings/:id/speakers`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PatchRequest {
    pub participants: Vec<SpeakerPatch>,
}

/// Ответ.
#[derive(Debug, Clone, Serialize)]
pub struct PatchResponse {
    pub meeting_id: String,
    /// Участники встречи ПОСЛЕ правки — тот же список, что уходит в WS.
    pub participants: Vec<Participant>,
    /// Сколько сегментов расшифровки сменили подпись.
    pub segments_updated: usize,
    /// Переписан ли `transcript.json` / `transcript.md`. `false` — файлов не
    /// было (расшифровка ещё не доходила до рендера), и это не ошибка.
    pub transcript_rewritten: bool,
}

/// Сколько символов максимум в имени участника.
///
/// Столько же, сколько у имени бота: имя рисуется в строке расшифровки, и
/// «имя» на две тысячи символов сломало бы вёрстку карточки, а не сервер.
pub const MAX_NAME_CHARS: usize = 64;

/// Разбирает тело ВРУЧНУЮ — см. «Порядок проверок» в шапке [`super`].
pub fn parse_patch(body: &[u8]) -> Result<PatchRequest, MeetError> {
    let req: PatchRequest = serde_json::from_slice(body)
        .map_err(|e| MeetError::BadRequest(format!("тело не разбирается: {e}")))?;
    if req.participants.is_empty() {
        return Err(MeetError::BadRequest(
            "нужен непустой список participants".to_string(),
        ));
    }
    for p in &req.participants {
        let name = p.display_name.trim();
        if name.is_empty() {
            return Err(MeetError::BadRequest(format!(
                "участнику {} нужно непустое имя",
                p.id
            )));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(MeetError::BadRequest(format!(
                "имя участника {} длиннее {MAX_NAME_CHARS} символов",
                p.id
            )));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for p in &req.participants {
        if !seen.insert(p.id) {
            return Err(MeetError::BadRequest(format!(
                "участник {} указан в запросе дважды",
                p.id
            )));
        }
    }
    Ok(req)
}

/// Подписи, под которыми участник МОГ попасть в расшифровку.
///
/// Их несколько, потому что имя сегменту ставит атрибуция, а не база, и
/// источников подписи ровно три:
///
/// * найденное каскадом имя (`display_name`) — оно же стоит в сегментах;
/// * `Дорожка N` — заглушка пер-трекового VAD
///   (`pipeline::run::track_label`), когда имени ещё нет;
/// * `Участник N` — то, что показывает карточка у безымянного участника;
///   человек видит именно эту подпись и переименовывает именно её.
///
/// Порядок неважен, дубликаты убраны: список уходит в `IN (…)`.
pub fn speaker_labels(p: &Participant) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.trim().is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(name) = p.display_name.as_ref() {
        push(name.clone());
    }
    if let Some(idx) = p.track_idx {
        push(format!("Дорожка {idx}"));
        push(format!("Участник {}", idx + 1));
    }
    push(p.participant_key.clone());
    out
}

/// `PATCH /api/meet/meetings/:id/speakers`.
pub async fn patch(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PatchResponse>, MeetError> {
    // Лестница: резолв встречи (404) РАНЬШЕ разбора тела (400).
    let meeting = meetings::get(&state, &id).await.ok();
    check_ladder(Preconditions {
        meeting: meeting.as_ref(),
        params: parse_patch(&body).err(),
        ..Default::default()
    })?;
    let meeting = meeting.expect("лестница обязана была отвергнуть отсутствие встречи");
    let req = parse_patch(&body)?;

    let known = participants::list(&state, &id).await?;
    // Проверяем ВСЮ пачку до первой записи: половина применённых
    // переименований хуже нуля — её не видно ни в ответе, ни в файле.
    let mut targets = Vec::with_capacity(req.participants.len());
    for patch in &req.participants {
        let person = known
            .iter()
            .find(|p| p.id == patch.id)
            .ok_or_else(|| MeetError::BadRequest(format!(
                "участника {} нет во встрече {id}",
                patch.id
            )))?;
        targets.push((person.clone(), patch.display_name.trim().to_string()));
    }

    let mut renames: Vec<(Vec<String>, String)> = Vec::new();
    let mut segments_updated = 0usize;
    for (person, name) in &targets {
        let labels = speaker_labels(person);
        participants::rename(&state, person.id, name).await?;
        segments_updated +=
            segments::rename_speaker(&state, &id, person.id, &labels, name).await?;
        renames.push((labels, name.clone()));
    }

    let transcript_rewritten = rewrite_transcript(&meeting.meeting_dir, &renames);

    // Обе открытые карточки узнают о правке кадром, а не опросом: имя обязано
    // смениться без перезагрузки, в том числе в соседней вкладке.
    let participants_now = participants::list(&state, &id).await.unwrap_or_default();
    state.broadcast_event(MeetServerMsg::Participants {
        meeting_id: id.clone(),
        participants: participants_now.clone(),
    });
    if let Ok(page) = segments::page(&state, &id, 0, super::meetings::TRANSCRIPT_PAGE).await {
        state.broadcast_event(MeetServerMsg::TranscriptDelta {
            meeting_id: id.clone(),
            segments: page,
        });
    }

    tracing::info!(
        target: "forge_meet",
        meeting_id = %id,
        renamed = targets.len(),
        segments_updated,
        transcript_rewritten,
        "forge-meet: спикеры переименованы вручную"
    );

    Ok(Json(PatchResponse {
        meeting_id: id,
        participants: participants_now,
        segments_updated,
        transcript_rewritten,
    }))
}

/// Переписывает `transcript.json` и `transcript.md` с новыми именами.
///
/// `false` — файла нет или он не разбирается: расшифровка могла не дойти до
/// рендера, и это не повод отказывать в переименовании участника.
///
/// Чистая функция от файла и таблицы замен, поэтому проверяется отдельно от
/// HTTP.
pub fn rewrite_transcript(meeting_dir: &str, renames: &[(Vec<String>, String)]) -> bool {
    use crate::pipeline::render;

    let dir = PathBuf::from(meeting_dir);
    let Some(dto) = render::read_transcript(&dir) else {
        return false;
    };
    let mut segments = render::from_dto(&dto);
    for seg in &mut segments {
        let Some(current) = seg.speaker.as_ref() else {
            continue;
        };
        if let Some((_, new_name)) = renames
            .iter()
            .find(|(labels, _)| labels.iter().any(|l| l == current))
        {
            seg.speaker = Some(new_name.clone());
        }
    }
    // Оба файла — атомарно и заново: список спикеров в шапке `.md` и поле
    // `speakers` в `.json` пересчитываются из сегментов, поэтому «заменить
    // строку в файле» здесь не годится в принципе.
    render::write_transcript(&dir, &dto.meta, &segments).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: i64, name: Option<&str>, track: Option<i64>) -> Participant {
        Participant {
            id,
            meeting_id: "m1".into(),
            participant_key: format!("key-{id}"),
            display_name: name.map(str::to_string),
            track_idx: track,
            is_bot: false,
            source: None,
            confidence: None,
            first_seen_ms: None,
            last_seen_ms: None,
        }
    }

    #[test]
    fn the_body_is_parsed_by_hand_with_named_errors() {
        let req = parse_patch(r#"{"participants":[{"id":3,"display_name":" Аня "}]}"#.as_bytes()).unwrap();
        assert_eq!(req.participants[0].id, 3);

        for body in [
            br#"{"participants":[]}"#.as_slice(),
            br#"{"participants":[{"id":1,"display_name":"   "}]}"#.as_slice(),
            br#"{"participants":[{"id":1,"display_name":"a"},{"id":1,"display_name":"b"}]}"#
                .as_slice(),
            "не json".as_bytes(),
        ] {
            assert!(
                matches!(parse_patch(body).unwrap_err(), MeetError::BadRequest(_)),
                "{}",
                String::from_utf8_lossy(body)
            );
        }

        // Длинное имя сломало бы вёрстку карточки, а не сервер, — и всё равно
        // отвергается здесь, а не «как-нибудь потом».
        let long = format!(
            r#"{{"participants":[{{"id":1,"display_name":"{}"}}]}}"#,
            "я".repeat(MAX_NAME_CHARS + 1)
        );
        assert!(matches!(
            parse_patch(long.as_bytes()).unwrap_err(),
            MeetError::BadRequest(_)
        ));
    }

    /// Подписи безымянного участника — ОБЕ формы, и заглушка дорожки, и то,
    /// что человек видит в карточке. Оставь мы одну — переименование
    /// «Участника 2» не нашло бы ни одного сегмента и сделало бы вид, что
    /// сработало.
    #[test]
    fn an_unnamed_participant_is_addressable_by_both_labels() {
        let labels = speaker_labels(&person(1, None, Some(1)));
        assert!(labels.contains(&"Дорожка 1".to_string()), "{labels:?}");
        assert!(labels.contains(&"Участник 2".to_string()), "{labels:?}");
        assert!(labels.contains(&"key-1".to_string()), "{labels:?}");

        // У найденного каскадом имени первой стоит оно само.
        let labels = speaker_labels(&person(2, Some("Аня"), Some(0)));
        assert_eq!(labels[0], "Аня");
        assert!(labels.contains(&"Участник 1".to_string()));

        // Без дорожки остаётся только ключ — выдумывать «Участник ?» нечего.
        assert_eq!(speaker_labels(&person(3, None, None)), vec!["key-3"]);
    }

    /// Пересборка файлов — чистая операция над каталогом, и проверяется без
    /// HTTP: имя меняется В ОБОИХ форматах, а список участников в шапке `.md`
    /// пересчитывается.
    #[test]
    fn rewriting_the_transcript_changes_both_formats() {
        use crate::pipeline::render;
        use crate::pipeline::speakers::{AttributedSegment, Word};

        let dir = tempfile::tempdir().unwrap();
        let meta = render::TranscriptMeta {
            meeting_id: "m1".into(),
            platform: "telemost".into(),
            url: "https://telemost.yandex.ru/j/1".into(),
            title: None,
            started_at_unix_ms: None,
            duration_ms: 10_000,
            engine: "whisper.cpp".into(),
            model: "large-v3-turbo-q5_0".into(),
            language: "ru".into(),
            drift_scale: None,
        };
        let seg = |start: i64, speaker: &str, text: &str| AttributedSegment {
            start_ms: start,
            end_ms: start + 1000,
            text: text.into(),
            speaker: Some(speaker.to_string()),
            confidence: 1.0,
            overlapped: false,
            words: vec![Word {
                text: text.into(),
                start_ms: start,
                end_ms: start + 1000,
            }],
        };
        render::write_transcript(
            dir.path(),
            &meta,
            &[
                seg(0, "Участник 2", "Привет."),
                seg(2000, "Аня", "Начинаем."),
            ],
        )
        .unwrap();

        let ok = rewrite_transcript(
            &dir.path().display().to_string(),
            &[(vec!["Участник 2".to_string()], "Боря".to_string())],
        );
        assert!(ok);

        let json = std::fs::read_to_string(dir.path().join("transcript.json")).unwrap();
        assert!(json.contains("Боря"), "{json}");
        assert!(!json.contains("Участник 2"), "старая подпись осталась: {json}");
        let md = std::fs::read_to_string(dir.path().join("transcript.md")).unwrap();
        assert!(md.contains("**[00:00:00] Боря**"), "{md}");
        assert!(
            md.contains("- Участники: Боря, Аня"),
            "список участников в шапке обязан пересчитаться: {md}"
        );

        // Потокенные метки пережили пересборку.
        let back = render::read_transcript(dir.path()).unwrap();
        assert!(back.segments.iter().all(|s| !s.words.is_empty()));

        // ИДЕМПОТЕНТНОСТЬ: повтор той же правки не меняет ни байта.
        let before = std::fs::read(dir.path().join("transcript.json")).unwrap();
        assert!(rewrite_transcript(
            &dir.path().display().to_string(),
            &[(vec!["Боря".to_string()], "Боря".to_string())]
        ));
        assert_eq!(
            std::fs::read(dir.path().join("transcript.json")).unwrap(),
            before
        );
    }

    #[test]
    fn a_missing_transcript_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!rewrite_transcript(
            &dir.path().display().to_string(),
            &[(vec!["Участник 1".into()], "Аня".into())]
        ));
    }
}
