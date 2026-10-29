//! `PATCH /api/meet/meetings/:id/speakers` — на НАСТОЯЩЕМ роутере, НАСТОЯЩЕЙ
//! базе и НАСТОЯЩИХ файлах расшифровки.
//!
//! Ручное именование — последняя ступень каскада привязки трека к имени, и для
//! Телемоста она штатная: живая проба Ф0 доказала режим `Slots`, где дорожка не
//! соответствует человеку. Поэтому здесь проверяется не «поле поменялось», а то,
//! что переименование ПЕРЕСОБИРАЕТ ВСЮ РАСШИФРОВКУ:
//!
//! * `meeting_participants.display_name` — список в карточке;
//! * `meeting_segments.speaker_name` — вкладка расшифровки;
//! * `transcript.json` и `transcript.md` — файлы, которые читают человек и
//!   промпт саммари.
//!
//! Пропусти мы файлы — саммари, пересчитанное после переименования, всё равно
//! говорило бы «Участник 2».

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forge_meet::db::repo;
use forge_meet::db::Db;
use forge_meet::pipeline::render::{self, TranscriptMeta};
use forge_meet::pipeline::speakers::{AttributedSegment, Word};
use forge_meet::state::MeetState;
use forge_meet::status::MeetStatus;
use forge_meet::ws::protocol::MeetServerMsg;
use serde_json::Value;
use tower::ServiceExt;

const URL: &str = "https://telemost.yandex.ru/j/60497752704486";

struct Stand {
    state: Arc<MeetState>,
    app: axum::Router,
    id: String,
    dir: PathBuf,
    /// `participant_id` безымянного участника («Участник 2»).
    unnamed: i64,
    /// `participant_id` того, кого каскад назвал по DOM.
    named: i64,
    _tmp: tempfile::TempDir,
}

fn seg(start: i64, speaker: &str, text: &str) -> AttributedSegment {
    AttributedSegment {
        start_ms: start,
        end_ms: start + 1_500,
        text: text.to_string(),
        speaker: Some(speaker.to_string()),
        confidence: 1.0,
        overlapped: false,
        words: vec![Word {
            text: text.split_whitespace().next().unwrap_or("").to_string(),
            start_ms: start,
            end_ms: start + 500,
        }],
    }
}

impl Stand {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = forge_meet::testing::hermetic_config(tmp.path());
        let state = MeetState::new(cfg);
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();

        let dir = tmp.path().join("cache/m1");
        std::fs::create_dir_all(&dir).unwrap();

        let link = forge_meet::parse_meeting_url(URL).unwrap();
        let out = repo::meetings::create_or_attach(
            &state,
            repo::meetings::NewMeeting {
                link,
                source: None,
                title: Some("Планёрка".into()),
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: dir.display().to_string(),
            },
        )
        .await
        .unwrap();
        let id = out.meeting.id.clone();

        // Двое участников: одного каскад назвал по DOM, второй остался
        // «Участником 2» — ровно тот случай, ради которого фича и делалась.
        let named = repo::participants::upsert(
            &state, &id, "tile-anya", Some("Аня"), Some(0), Some("dom"), Some(0.9),
        )
        .await
        .unwrap();
        let unnamed = repo::participants::upsert(
            &state, &id, "track-1", None, Some(1), Some("correlation"), Some(0.4),
        )
        .await
        .unwrap();

        // Расшифровка на диске и в базе — с теми же подписями, что ставит
        // пайплайн.
        let segments = vec![
            seg(0, "Аня", "Коллеги, начинаем."),
            seg(2_000, "Участник 2", "Готов к пятнице."),
            seg(4_000, "Аня", "Отлично."),
            seg(6_000, "Участник 2", "До связи."),
        ];
        let meta = TranscriptMeta {
            meeting_id: id.clone(),
            platform: "telemost".into(),
            url: URL.into(),
            title: Some("Планёрка".into()),
            started_at_unix_ms: Some(1_700_000_000_000),
            duration_ms: 8_000,
            engine: "whisper.cpp".into(),
            model: "large-v3-turbo-q5_0".into(),
            language: "ru".into(),
            drift_scale: None,
        };
        render::write_transcript(&dir, &meta, &segments).unwrap();
        repo::segments::replace_all(
            &state,
            &id,
            segments
                .iter()
                .enumerate()
                .map(|(i, s)| repo::segments::NewSegment {
                    idx: i as i64,
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    text: s.text.clone(),
                    participant_id: None,
                    speaker_name: s.speaker.clone(),
                    track_idx: None,
                    confidence: Some(1.0),
                    overlapped: false,
                    words_json: None,
                })
                .collect(),
        )
        .await
        .unwrap();

        for st in [
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
            MeetStatus::Transcribing,
            MeetStatus::Ready,
        ] {
            repo::meetings::set_status(&state, &id, st, Some("test"))
                .await
                .unwrap();
        }

        let app = forge_meet::register_routes(axum::Router::new(), state.clone());
        Stand {
            state,
            app,
            id,
            dir,
            unnamed,
            named,
            _tmp: tmp,
        }
    }

    async fn patch(&self, body: &str) -> (StatusCode, Value) {
        self.patch_at(&self.id, body).await
    }

    async fn patch_at(&self, id: &str, body: &str) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/meet/meetings/{id}/speakers"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn json(&self) -> String {
        std::fs::read_to_string(self.dir.join("transcript.json")).unwrap()
    }

    fn md(&self) -> String {
        std::fs::read_to_string(self.dir.join("transcript.md")).unwrap()
    }

    async fn speakers_in_db(&self) -> Vec<String> {
        repo::segments::page(&self.state, &self.id, 0, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.speaker_name.unwrap_or_default())
            .collect()
    }
}

/// ГЛАВНЫЙ ТЕСТ: переименование меняет участника, сегменты в базе И ОБА ФАЙЛА.
#[tokio::test]
async fn renaming_rewrites_the_participant_the_segments_and_both_files() {
    let s = Stand::new().await;
    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Боря"}}]}}"#,
        s.unnamed
    );
    let (code, resp) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(resp["segments_updated"], 2, "{resp}");
    assert_eq!(resp["transcript_rewritten"], true, "{resp}");

    // 1. Участник.
    let people = repo::participants::list(&s.state, &s.id).await.unwrap();
    let renamed = people.iter().find(|p| p.id == s.unnamed).unwrap();
    assert_eq!(renamed.display_name.as_deref(), Some("Боря"));
    assert_eq!(renamed.source.as_deref(), Some("manual"), "источник — ручной ввод");
    assert_eq!(renamed.confidence, Some(1.0), "у ручного имени сомнений нет");
    // Соседа не тронули.
    let other = people.iter().find(|p| p.id == s.named).unwrap();
    assert_eq!(other.display_name.as_deref(), Some("Аня"));

    // 2. Сегменты в базе — источник вкладки расшифровки.
    assert_eq!(
        s.speakers_in_db().await,
        vec!["Аня", "Боря", "Аня", "Боря"],
        "подписи в базе не пересобрались"
    );

    // 3. transcript.json.
    let json = s.json();
    assert!(json.contains("Боря"), "{json}");
    assert!(!json.contains("Участник 2"), "старая подпись осталась в JSON");
    let dto: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        dto["speakers"],
        serde_json::json!(["Аня", "Боря"]),
        "список спикеров обязан пересчитаться"
    );
    // Потокенные метки пережили пересборку — иначе резать сегменты по границе
    // смены спикера было бы уже нечем.
    assert!(
        dto["segments"][0]["words"].as_array().unwrap().len() > 0,
        "{dto}"
    );

    // 4. transcript.md — его читает промпт саммари.
    let md = s.md();
    assert!(md.contains("**[00:00:02] Боря**"), "{md}");
    assert!(!md.contains("Участник 2"), "{md}");
    assert!(
        md.contains("- Участники: Аня, Боря"),
        "состав встречи в шапке обязан пересчитаться: {md}"
    );
}

/// СОБЫТИЕ WS доходит до открытых карточек — имя меняется без перезагрузки.
#[tokio::test]
async fn the_change_is_broadcast_to_open_cards() {
    let s = Stand::new().await;
    let mut rx = s.state.subscribe();

    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Боря"}}]}}"#,
        s.unnamed
    );
    let (code, _) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK);

    let mut saw_participants = false;
    let mut saw_transcript = false;
    while let Ok(msg) = rx.try_recv() {
        match msg {
            MeetServerMsg::Participants {
                meeting_id,
                participants,
            } => {
                assert_eq!(meeting_id, s.id);
                assert!(
                    participants.iter().any(|p| p.display_name.as_deref() == Some("Боря")),
                    "в кадре старое имя: {participants:?}"
                );
                saw_participants = true;
            }
            MeetServerMsg::TranscriptDelta { segments, .. } => {
                assert!(
                    segments.iter().any(|x| x.speaker_name.as_deref() == Some("Боря")),
                    "расшифровка в кадре не обновилась"
                );
                saw_transcript = true;
            }
            _ => {}
        }
    }
    assert!(saw_participants, "кадр participants не ушёл");
    assert!(
        saw_transcript,
        "кадр transcript_delta не ушёл — вкладка расшифровки осталась бы со старым именем"
    );
}

/// ИДЕМПОТЕНТНОСТЬ: повторный PATCH тем же значением ничего не ломает.
///
/// И это не просто «не падает»: после первого переименования у сегментов
/// проставлен `participant_id`, поэтому второй заход обязан находить их уже по
/// нему, а не по совпадению подписи.
#[tokio::test]
async fn renaming_twice_with_the_same_value_changes_nothing() {
    let s = Stand::new().await;
    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Боря"}}]}}"#,
        s.unnamed
    );

    let (code, first) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(first["segments_updated"], 2);
    let json_after_first = s.json();
    let md_after_first = s.md();

    let (code, second) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK, "{second}");
    assert_eq!(
        second["segments_updated"], 2,
        "второй заход обязан найти те же строки по participant_id: {second}"
    );
    assert_eq!(s.json(), json_after_first, "файл изменился на повторе");
    assert_eq!(s.md(), md_after_first, "файл изменился на повторе");
    assert_eq!(s.speakers_in_db().await, vec!["Аня", "Боря", "Аня", "Боря"]);

    // И третье — уже другое имя: связь по participant_id продолжает работать,
    // хотя подпись в сегментах давно не совпадает ни с одной заглушкой.
    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Борис Петрович"}}]}}"#,
        s.unnamed
    );
    let (code, third) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK, "{third}");
    assert_eq!(third["segments_updated"], 2, "{third}");
    assert!(s.json().contains("Борис Петрович"));
    assert!(!s.json().contains("\"Боря\""));
}

/// Переименовать можно и того, кого каскад уже назвал: DOM ошибается тоже.
#[tokio::test]
async fn a_name_found_by_the_cascade_can_be_corrected_too() {
    let s = Stand::new().await;
    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Анна Сергеевна"}}]}}"#,
        s.named
    );
    let (code, resp) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(resp["segments_updated"], 2);
    assert_eq!(
        s.speakers_in_db().await,
        vec!["Анна Сергеевна", "Участник 2", "Анна Сергеевна", "Участник 2"]
    );
    assert!(s.md().contains("Анна Сергеевна"));
}

/// Пачка переименований применяется ЦЕЛИКОМ или не применяется вовсе.
#[tokio::test]
async fn a_batch_is_all_or_nothing() {
    let s = Stand::new().await;

    // Один из двух участников чужой — не должно примениться НИЧЕГО.
    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Боря"}},{{"id":99999,"display_name":"Чужой"}}]}}"#,
        s.unnamed
    );
    let (code, resp) = s.patch(&body).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{resp}");
    assert!(
        resp["message"].as_str().unwrap().contains("99999"),
        "ошибка обязана называть виновника: {resp}"
    );
    assert_eq!(
        s.speakers_in_db().await,
        vec!["Аня", "Участник 2", "Аня", "Участник 2"],
        "половина пачки применилась — это хуже, чем ничего"
    );

    // А законная пачка из двоих применяется вся.
    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Боря"}},{{"id":{},"display_name":"Анна"}}]}}"#,
        s.unnamed, s.named
    );
    let (code, resp) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(resp["segments_updated"], 4);
    assert_eq!(s.speakers_in_db().await, vec!["Анна", "Боря", "Анна", "Боря"]);
}

/// ЛЕСТНИЦА: несуществующая встреча даёт 404 РАНЬШЕ разбора тела.
#[tokio::test]
async fn the_error_ladder_resolves_the_meeting_before_the_body() {
    let s = Stand::new().await;

    let (code, _) = s.patch_at("нет-такой", "совсем не json").await;
    assert_eq!(code, StatusCode::NOT_FOUND, "404 обязан быть раньше 400");

    for body in [
        "не json",
        r#"{"participants":[]}"#,
        r#"{"participants":[{"id":1,"display_name":"  "}]}"#,
    ] {
        let (code, resp) = s.patch(body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(resp["error"], "bad_request", "{body}");
    }
}

/// Расшифровки на диске может не быть — это не повод отказывать в имени.
#[tokio::test]
async fn a_missing_transcript_does_not_block_the_rename() {
    let s = Stand::new().await;
    std::fs::remove_file(s.dir.join("transcript.json")).unwrap();

    let body = format!(
        r#"{{"participants":[{{"id":{},"display_name":"Боря"}}]}}"#,
        s.unnamed
    );
    let (code, resp) = s.patch(&body).await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["transcript_rewritten"], false,
        "переписывать было нечего, и об этом сказано честно"
    );
    // Но база всё равно обновилась: карточка покажет новое имя.
    assert_eq!(s.speakers_in_db().await, vec!["Аня", "Боря", "Аня", "Боря"]);
}
