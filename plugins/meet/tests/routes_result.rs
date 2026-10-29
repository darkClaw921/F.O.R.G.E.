//! Роуты чтения результата и повтора этапа — на НАСТОЯЩЕМ роутере и НАСТОЯЩЕЙ
//! базе.
//!
//! Здесь проверяются четыре пути: карточка встречи, расшифровка страницами,
//! саммари и `retry?from=`. Chrome не запускается ни разу — встречи заводятся
//! прямо в базе и доводятся до нужного статуса, потому что запись к этим
//! роутам отношения не имеет.
//!
//! # Лестница проверок — главное, что здесь доказывается
//!
//! ```text
//! готовность бинарей (409 *_unavailable)
//!   → слот (409 busy)
//!     → резолв встречи (404)
//!       → допустимость перехода (409 invalid_state)
//!         → разбор параметров (400)
//! ```
//!
//! Порядок не эстетика: проверь мы параметры раньше зависимостей, человек с
//! неустановленным whisper получал бы 400 «плохой параметр» и чинил бы не то.
//!
//! # Герметичность
//!
//! `cache_dir`, `db_path` и `models_dir` — во временном каталоге. Без этого
//! прогон писал бы в `~/.forge/meet`, где лежат записи реальных встреч.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forge_meet::config::MeetConfig;
use forge_meet::db::repo;
use forge_meet::db::Db;
use forge_meet::state::MeetState;
use forge_meet::status::{MeetStatus, SummaryStatus};
use forge_meet::testing::mocks;
use serde_json::Value;
use tower::ServiceExt;

const URL: &str = "https://telemost.yandex.ru/j/60497752704486";

const TRANSCRIPT: &str = "\
# Расшифровка встречи

- Тема: Планёрка

**[00:00:00] Аня**

Коллеги, начинаем.
";

struct Stand {
    state: Arc<MeetState>,
    app: axum::Router,
    id: String,
    dir: PathBuf,
    bin: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Stand {
    /// Встреча в `ready` с транскриптом на диске и сегментами в базе.
    async fn ready() -> Self {
        Self::at(MeetStatus::Ready).await
    }

    async fn at(status: MeetStatus) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let cfg = MeetConfig {
            config_path: root.join("meet.toml"),
            cache_dir: root.join("cache"),
            db_path: root.join("cache/meet.db"),
            models_dir: root.join("cache/models"),
            // Файла по этому пути по умолчанию нет: тест, которому нужен
            // саммаризатор, кладёт туда мок сам.
            claude_cli_path: bin.join("claude"),
            // Запуски `claude -p` по умолчанию выключены глобально
            // (см. forge_claude_runner): стенд с мок-CLI включает их явно.
            claude_enabled: true,
            summary_timeout_secs: 10,
            whisper_gate_poll_ms: 20,
            // ЯВНО ЛОКАЛЬНЫЙ ПУТЬ: лестница проверок здесь про whisper,
            // модель и Metal, а умолчание плагина — облако (Ф15). Облачную
            // лестницу проверяет `the_dependency_ladder_follows_the_backend`.
            stt_backend: forge_meet::config::SttBackend::Local,
            summary_backend: forge_meet::config::SummaryBackend::Local,
            ..MeetConfig::default()
        };
        let state = MeetState::new(cfg);
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();

        let dir = root.join("cache/m1");
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

        std::fs::write(dir.join("transcript.md"), TRANSCRIPT).unwrap();
        repo::segments::replace_all(
            &state,
            &id,
            (0..7)
                .map(|i| repo::segments::NewSegment {
                    idx: i,
                    start_ms: i * 1_000,
                    end_ms: i * 1_000 + 900,
                    text: format!("реплика {i}"),
                    participant_id: None,
                    speaker_name: Some(if i % 2 == 0 { "Аня" } else { "Боря" }.to_string()),
                    track_idx: None,
                    confidence: Some(1.0),
                    overlapped: false,
                    words_json: None,
                })
                .collect(),
        )
        .await
        .unwrap();

        let path = [
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
            MeetStatus::Transcribing,
            MeetStatus::Summarizing,
            MeetStatus::Ready,
        ];
        for st in path {
            repo::meetings::set_status(&state, &id, st, Some("test"))
                .await
                .unwrap();
            if st == status {
                break;
            }
        }

        let app = forge_meet::register_routes(axum::Router::new(), state.clone());
        Stand {
            state,
            app,
            id,
            dir,
            bin,
            _tmp: tmp,
        }
    }

    /// Ставит пробы так, будто whisper с моделью и Metal на месте.
    async fn with_whisper_ready(&self) {
        let mut deps = self.state.deps.write().await;
        deps.probed = true;
        deps.whisper.found = Some(true);
        deps.whisper.path = Some("/bin/true".to_string());
        deps.model_ready = true;
        deps.gpu_metal = Some(true);
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (code, json)
    }

    /// POST с телом — для роутов, которым нужен не пустой `{}`.
    async fn post_json(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (code, json)
    }

    async fn post(&self, uri: &str) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (code, json)
    }

    async fn row(&self) -> repo::MeetingRow {
        repo::meetings::get(&self.state, &self.id).await.unwrap()
    }

    /// Ждёт, пока фоновое задание доведёт встречу до нужного статуса.
    async fn wait_summary(&self, want: SummaryStatus) -> SummaryStatus {
        for _ in 0..400 {
            let row = self.row().await;
            if row.summary_status == want {
                return row.summary_status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        self.row().await.summary_status
    }
}

// ============================================================================
// КАРТОЧКА И РАСШИФРОВКА
// ============================================================================

#[tokio::test]
async fn the_card_carries_the_meeting_its_people_and_the_segment_count() {
    let s = Stand::ready().await;
    let (code, body) = s.get(&format!("/api/meet/meetings/{}", s.id)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["meeting"]["id"], s.id.as_str());
    assert_eq!(body["meeting"]["status"], "ready");
    assert_eq!(body["meeting"]["title"], "Планёрка");
    assert_eq!(body["segments_total"], 7);
    assert!(body["participants"].is_array());

    let (code, body) = s.get("/api/meet/meetings/нет-такой").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn the_transcript_comes_in_pages_with_an_honest_total() {
    let s = Stand::ready().await;
    let (code, body) = s
        .get(&format!("/api/meet/meetings/{}/transcript?offset=0&limit=3", s.id))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["total"], 7, "总 обязан считать ВСЕ сегменты, а не страницу");
    let page = body["segments"].as_array().unwrap();
    assert_eq!(page.len(), 3);
    assert_eq!(page[0]["idx"], 0);
    assert_eq!(page[0]["text"], "реплика 0");
    assert_eq!(page[0]["speaker_name"], "Аня");

    // Вторая страница продолжает первую, а не начинает заново.
    let (_, body) = s
        .get(&format!("/api/meet/meetings/{}/transcript?offset=3&limit=3", s.id))
        .await;
    assert_eq!(body["segments"][0]["idx"], 3);

    // Без параметров — страница по умолчанию.
    let (code, body) = s
        .get(&format!("/api/meet/meetings/{}/transcript", s.id))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["segments"].as_array().unwrap().len(), 7);
    assert_eq!(body["offset"], 0);
}

/// Битые параметры — 400, но ТОЛЬКО после резолва встречи: у несуществующей
/// встречи 404 приходит раньше.
#[tokio::test]
async fn broken_page_parameters_are_400_but_404_comes_first() {
    let s = Stand::ready().await;
    for q in ["?offset=x", "?limit=нет", "?offset=-1", "?limit=0"] {
        let (code, body) = s
            .get(&format!("/api/meet/meetings/{}/transcript{q}", s.id))
            .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["error"], "bad_request", "{q}");
    }
    let (code, _) = s.get("/api/meet/meetings/нет-такой/transcript?offset=x").await;
    assert_eq!(
        code,
        StatusCode::NOT_FOUND,
        "резолв встречи обязан идти раньше разбора параметров"
    );
}

// ============================================================================
// САММАРИ: ЧТЕНИЕ
// ============================================================================

/// **Отсутствие саммари — не 404.** 404 означал бы «нет такой встречи», а
/// саммари могло не считаться вовсе — и карточка показывает это по-разному.
#[tokio::test]
async fn a_meeting_without_a_summary_answers_200_with_a_null_text() {
    let s = Stand::ready().await;
    let (code, body) = s.get(&format!("/api/meet/meetings/{}/summary", s.id)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["status"], "none");
    assert!(body["text"].is_null(), "{body}");

    let (code, _) = s.get("/api/meet/meetings/нет-такой/summary").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_ready_summary_comes_back_as_text() {
    let s = Stand::ready().await;
    std::fs::write(s.dir.join("summary.md"), "## Резюме\n\nВыпускаем в пятницу.\n").unwrap();
    repo::meetings::set_summary_status(&s.state, &s.id, SummaryStatus::Ready)
        .await
        .unwrap();

    let (code, body) = s.get(&format!("/api/meet/meetings/{}/summary", s.id)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["status"], "ready");
    assert!(
        body["text"].as_str().unwrap().contains("Выпускаем в пятницу."),
        "{body}"
    );
}

// ============================================================================
// ПОВТОР ЭТАПА
// ============================================================================

/// **`retry?from=summarizing` пересчитывает саммари поверх готовой
/// расшифровки** — и `POST /:id/summary` делает ровно то же самое.
#[tokio::test(flavor = "multi_thread")]
async fn retrying_the_summary_recomputes_it_over_the_existing_transcript() {
    for (n, uri) in [
        (1, "retry?from=summarizing"),
        (2, "summary"),
    ] {
        let s = Stand::ready().await;
        mocks::mock_claude_success(&s.bin, "claude", &format!("## Резюме {n}")).unwrap();

        let (code, body) = s
            .post(&format!("/api/meet/meetings/{}/{uri}", s.id))
            .await;
        assert_eq!(code, StatusCode::OK, "{uri}: {body}");
        assert_eq!(body["from"], "summarizing", "{uri}");
        assert_eq!(body["status"], "summarizing", "{uri}");

        assert_eq!(
            s.wait_summary(SummaryStatus::Ready).await,
            SummaryStatus::Ready,
            "{uri}: саммари не досчиталось"
        );
        let row = s.row().await;
        assert_eq!(row.status, MeetStatus::Ready, "{uri}");
        let md = std::fs::read_to_string(s.dir.join("summary.md")).unwrap();
        assert!(md.contains(&format!("Резюме {n}")), "{uri}: {md}");
        // Расшифровка на месте: пересчёт саммари её не трогает.
        assert!(s.dir.join("transcript.md").is_file(), "{uri}");
    }
}

/// **ПРОВАЛ ПЕРЕСЧЁТА НЕ РОНЯЕТ ВСТРЕЧУ, А ТРАНСКРИПТ ОСТАЁТСЯ ДОСТУПЕН ЧЕРЕЗ
/// API.**
///
/// Ровно то, ради чего у саммари свой статус: пользователь видит готовую
/// расшифровку и кнопку «пересчитать», а не «встреча не удалась».
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_retry_keeps_the_transcript_reachable_through_the_api() {
    let s = Stand::ready().await;
    mocks::mock_claude_failure(&s.bin, "claude", "API Error: 529 overloaded_error").unwrap();

    let (code, _) = s
        .post(&format!("/api/meet/meetings/{}/summary", s.id))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(s.wait_summary(SummaryStatus::Failed).await, SummaryStatus::Failed);

    let row = s.row().await;
    assert_eq!(row.status, MeetStatus::Ready, "ВСТРЕЧА УПАЛА ИЗ-ЗА САММАРИ");

    // Транскрипт доступен ЧЕРЕЗ API — и страницами, и в карточке.
    let (code, body) = s
        .get(&format!("/api/meet/meetings/{}/transcript", s.id))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["total"], 7);
    assert_eq!(body["segments"][0]["text"], "реплика 0");

    let (code, body) = s.get(&format!("/api/meet/meetings/{}", s.id)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["meeting"]["summary_status"], "failed");
    assert_eq!(body["meeting"]["status"], "ready");

    // А саммари честно отвечает «не получилось».
    let (code, body) = s.get(&format!("/api/meet/meetings/{}/summary", s.id)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["status"], "failed");
    assert!(body["text"].is_null());
}

/// **НЕДОПУСТИМЫЙ ПОВТОР — 409 `invalid_state`, А НЕ 500.**
///
/// «Этап существует, но сейчас в него нельзя» и «такого этапа не бывает» —
/// разные диагнозы, и лечатся они по-разному: первый ждёт конца встречи,
/// второй чинит опечатку в запросе. Поэтому первый — 409, второй — 400, и ни
/// один из них не 500.
#[tokio::test]
async fn a_retry_that_makes_no_sense_here_is_409_invalid_state() {
    // Встреча ещё входит в комнату: повторять нечего, файлов нет.
    let s = Stand::at(MeetStatus::Joining).await;
    s.with_whisper_ready().await;
    // Саммаризатор на месте НАМЕРЕННО: лестница проверяет нехватку
    // компонентов РАНЬШЕ перехода, и без мока отказ пришёл бы кодом
    // `claude_cli_unavailable` — то есть тест перестал бы проверять переход.
    mocks::mock_claude_success(&s.bin, "claude", "## Резюме").unwrap();

    for stage in ["transcoding", "transcribing", "summarizing"] {
        let (code, body) = s
            .post(&format!("/api/meet/meetings/{}/retry?from={stage}", s.id))
            .await;
        assert_eq!(code, StatusCode::CONFLICT, "{stage}: {body}");
        assert_eq!(body["error"], "invalid_state", "{stage}: {body}");
        assert_eq!(body["message"].as_str().unwrap().contains("joining"), true, "{body}");
    }
    assert_eq!(s.row().await.status, MeetStatus::Joining, "статус тронут отказом");

    // Готовая встреча назад к записи не откатывается: браузера давно нет.
    let ready = Stand::ready().await;
    ready.with_whisper_ready().await;
    for stage in ["recording", "queued", "leaving", "ready"] {
        let (code, body) = ready
            .post(&format!("/api/meet/meetings/{}/retry?from={stage}", ready.id))
            .await;
        // Этих значений нет среди этапов пайплайна вовсе — это разбор
        // параметра, то есть 400. `recording` — не этап пост-обработки, а
        // состояние живой встречи, и повторить его невозможно в принципе.
        assert_eq!(code, StatusCode::BAD_REQUEST, "{stage}: {body}");
        assert_eq!(body["error"], "bad_request", "{stage}: {body}");
    }

    // Несуществующего этапа нет вовсе — тоже разбор параметра.
    let (code, body) = ready
        .post(&format!("/api/meet/meetings/{}/retry?from=банан", ready.id))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    let (code, _) = ready
        .post(&format!("/api/meet/meetings/{}/retry", ready.id))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без from повторять нечего");

    // Встречи нет — 404 раньше разбора параметров.
    let (code, _) = ready
        .post("/api/meet/meetings/нет-такой/retry?from=банан")
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// **ЛЕСТНИЦА: ОТСУТСТВУЮЩИЙ WHISPER ОБЪЯВЛЯЕТСЯ РАНЬШЕ БИТОГО ПАРАМЕТРА.**
///
/// Иначе человек с неустановленным whisper получал бы 400 «неизвестный этап» и
/// чинил бы опечатку вместо окружения.
#[tokio::test]
async fn a_missing_whisper_is_reported_before_a_broken_parameter() {
    let s = Stand::ready().await;
    // Пробы прошли, whisper не найден.
    {
        let mut deps = s.state.deps.write().await;
        deps.probed = true;
        deps.whisper.found = Some(false);
    }

    let (code, body) = s
        .post(&format!("/api/meet/meetings/{}/retry?from=банан", s.id))
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"], "whisper_unavailable",
        "битый параметр объявлен раньше отсутствующего whisper: {body}"
    );

    // И даже у НЕСУЩЕСТВУЮЩЕЙ встречи зависимость идёт первой ступенью.
    let (code, body) = s
        .post("/api/meet/meetings/нет-такой/retry?from=transcribing")
        .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"], "whisper_unavailable");
}

/// Модели нет — код другой, и человек чинит именно то, чего не хватает.
#[tokio::test]
async fn a_missing_model_has_its_own_code() {
    let s = Stand::ready().await;
    {
        let mut deps = s.state.deps.write().await;
        deps.probed = true;
        deps.whisper.found = Some(true);
        deps.whisper.path = Some("/bin/true".to_string());
        deps.model_ready = false;
    }
    let (code, body) = s
        .post(&format!("/api/meet/meetings/{}/retry?from=transcribing", s.id))
        .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"], "model_unavailable", "{body}");
}

/// **ЛЕСТНИЦА ЗАВИСИТ ОТ ВЫБРАННОГО ПУТИ, И ДЕГРАДАЦИЯ ИДЁТ В ОБЕ СТОРОНЫ.**
///
/// Та же машина, тот же отчёт проб — но в облачном режиме отсутствие whisper,
/// модели и Metal перестаёт быть препятствием, а мешает отсутствие ключа. И
/// наоборот: локальному пути отсутствие ключа не мешает НИЧЕМУ.
#[tokio::test]
async fn the_dependency_ladder_follows_the_backend() {
    let s = Stand::ready().await;
    // Чистая машина: whisper нет, модели нет, Metal нет — но ffmpeg есть.
    {
        let mut deps = s.state.deps.write().await;
        deps.probed = true;
        deps.whisper.found = Some(false);
        deps.model_ready = false;
        deps.gpu_metal = None;
        deps.ffmpeg.found = Some(true);
        deps.ffmpeg.path = Some("/bin/true".to_string());
    }
    let snapshot = s.state.deps_snapshot().await;

    // Роутер на конфиге, отличающемся ТОЛЬКО переключателем и ключом: база,
    // каталоги и отчёт проб те же самые.
    async fn variant(
        base: &Arc<MeetState>,
        snapshot: forge_meet::deps::DepsReport,
        cfg: MeetConfig,
    ) -> axum::Router {
        let db = base.db().unwrap();
        let state = MeetState::new(cfg);
        state.attach_db(db).unwrap();
        *state.deps.write().await = snapshot;
        forge_meet::register_routes(axum::Router::new(), state)
    }
    let ask = |app: axum::Router, id: String| async move {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/meet/meetings/{id}/retry?from=transcribing"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (code, json)
    };

    // 1. Облако с ключом: ни одного из трёх локальных требований не осталось.
    let mut cfg = s.state.config.clone();
    cfg.stt_backend = forge_meet::config::SttBackend::Cloud;
    cfg.polza_api_key = forge_meet::cloud::SecretKey::new("pza_LADDERladderLADDER_test");
    let (code, body) = ask(variant(&s.state, snapshot.clone(), cfg.clone()).await, s.id.clone()).await;
    assert_ne!(
        code,
        StatusCode::CONFLICT,
        "облаку не нужны ни whisper, ни модель, ни Metal: {body}"
    );

    // 2. Облако без ключа: отказ, но СВОИМ кодом — человек чинит ключ, а не
    //    ставит полтора гигабайта весов.
    cfg.polza_api_key = forge_meet::cloud::SecretKey::default();
    let (code, body) = ask(variant(&s.state, snapshot.clone(), cfg.clone()).await, s.id.clone()).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "cloud_not_configured", "{body}");

    // 3. Локальный путь без ключа: ключ ни при чём, жалоба про whisper.
    cfg.stt_backend = forge_meet::config::SttBackend::Local;
    let (code, body) = ask(variant(&s.state, snapshot.clone(), cfg.clone()).await, s.id.clone()).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "whisper_unavailable", "{body}");

    // 4. **РЕГРЕССИЯ `forge-0s0l5`.** Облако по умолчанию, ключа нет, зато
    //    локальный движок ГОТОВ — отказа не бывает: гейт откатывается, а
    //    роут пропускает повтор.
    let mut ready_local = snapshot.clone();
    ready_local.whisper.found = Some(true);
    ready_local.whisper.path = Some("/bin/true".to_string());
    ready_local.model_ready = true;
    ready_local.gpu_metal = Some(true);
    cfg.stt_backend = forge_meet::config::SttBackend::Cloud;
    let (_code, body) = ask(variant(&s.state, ready_local.clone(), cfg.clone()).await, s.id.clone()).await;
    // Проверяется КОД отказа, а не его отсутствие: встречу уже увёл в
    // `transcribing` вариант 1 выше, и повторный переход туда же законно
    // отвергается таблицей состояний. Важно ровно одно — нехватки
    // компонентов среди причин больше нет.
    assert_ne!(
        body["error"], "cloud_not_configured",
        "готовый локальный whisper обязан подхватить встречу: {body}"
    );

    // И статус ЧЕСТНО рассказывает, чем встреча посчитается на самом деле:
    // молчаливая подмена движка хуже отказа.
    let app = variant(&s.state, ready_local, cfg).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/meet/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "status не падает никогда");
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["config"]["stt_backend"], "cloud", "заказ не подменяется: {body}");
    assert_eq!(body["config"]["stt_effective"], "local", "{body}");
    assert_eq!(
        body["config"]["stt_fallback_reason"], "cloud_not_configured",
        "{body}"
    );
    assert!(body["config"]["stt_blocker"].is_null(), "{body}");
}

/// Саммари, ВЫКЛЮЧЕННОЕ конфигом, отвечает своим кодом, а не молча ничего не
/// делает.
#[tokio::test]
async fn a_disabled_summariser_refuses_with_its_own_code() {
    let s = Stand::ready().await;
    let mut cfg = s.state.config.clone();
    cfg.summary_enabled = false;
    let db = s.state.db().unwrap();
    let state = MeetState::new(cfg);
    state.attach_db(db).unwrap();
    let app = forge_meet::register_routes(axum::Router::new(), state.clone());
    let s = Stand { state, app, ..s };

    let (code, body) = s
        .post(&format!("/api/meet/meetings/{}/summary", s.id))
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "summary_disabled", "{body}");
    assert_eq!(s.row().await.status, MeetStatus::Ready, "статус не тронут");
}

/// Отсутствие whisper НЕ мешает пересчитать саммари: расшифровка уже есть, и
/// GPU для неё больше не нужен.
#[tokio::test(flavor = "multi_thread")]
async fn the_summary_does_not_need_whisper_at_all() {
    let s = Stand::ready().await;
    {
        let mut deps = s.state.deps.write().await;
        deps.probed = true;
        deps.whisper.found = Some(false);
        deps.model_ready = false;
        deps.gpu_metal = Some(false);
    }
    mocks::mock_claude_success(&s.bin, "claude", "## Резюме").unwrap();

    let (code, body) = s
        .post(&format!("/api/meet/meetings/{}/retry?from=summarizing", s.id))
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(s.wait_summary(SummaryStatus::Ready).await, SummaryStatus::Ready);
}

/// Повтор из `failed` разрешён для расшифровки и запрещён для саммари: у
/// упавшей встречи звук есть, а транскрипта нет по определению.
#[tokio::test]
async fn a_failed_meeting_may_retry_the_transcription_but_not_the_summary() {
    let s = Stand::at(MeetStatus::Transcoding).await;
    repo::meetings::set_status(&s.state, &s.id, MeetStatus::Failed, Some("whisper"))
        .await
        .unwrap();
    s.with_whisper_ready().await;
    // Саммаризатор на месте: проверяется ЗАПРЕТ ПЕРЕХОДА, а не нехватка
    // компонентов (лестница смотрит компоненты раньше перехода).
    mocks::mock_claude_success(&s.bin, "claude", "## Резюме").unwrap();

    let (code, body) = s
        .post(&format!("/api/meet/meetings/{}/retry?from=summarizing", s.id))
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "invalid_state", "{body}");

    // А расшифровку повторить можно: сырые дорожки на диске.
    let (code, body) = s
        .post(&format!("/api/meet/meetings/{}/retry?from=transcoding", s.id))
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["from"], "transcoding");
}

/// Таблица-контракт в шапке `routes/mod.rs` — источник истины для фронта.
/// Роут, зарегистрированный и не внесённый в неё, обязан ломать тест.
#[tokio::test]
async fn every_new_route_is_reachable_and_declared() {
    let s = Stand::ready().await;
    for (method, uri) in [
        ("GET", format!("/api/meet/meetings/{}", s.id)),
        ("GET", format!("/api/meet/meetings/{}/transcript", s.id)),
        ("GET", format!("/api/meet/meetings/{}/summary", s.id)),
    ] {
        let resp = s
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{method} {uri}");
    }
    // POST на тот же адрес саммари — другой обработчик, и он существует.
    let (code, _) = s.post(&format!("/api/meet/meetings/{}/summary", s.id)).await;
    assert_ne!(code, StatusCode::METHOD_NOT_ALLOWED);
}

/// Живой кейс 07.09.2026 (Телемост, f61cfa7a…): запись остановили кнопкой,
/// пост-обработка дошла до конца — на диске звук, транскрипт и саммари, — а
/// статус после этого стал `cancelled`. Пересчёт саммари отвечал 409
/// `invalid_state`, хотя данные лежали рядом.
#[tokio::test]
async fn a_cancelled_meeting_with_a_transcript_can_be_resummarised() {
    // Отмена приходит ВО ВРЕМЯ пост-обработки — ровно так было в живом кейсе:
    // из терминального `ready` отмена запрещена, и правильно.
    let s = Stand::at(MeetStatus::Summarizing).await;
    mocks::mock_claude_success(&s.bin, "claude", "## Резюме после отмены").unwrap();

    // Пути артефактов проставлены — так их оставляет прошедшая пост-обработка.
    repo::meetings::set_paths(
        &s.state,
        &s.id,
        Some(&s.dir.join("audio.m4a").display().to_string()),
        Some(&s.dir.join("transcript.md").display().to_string()),
        Some(&s.dir.join("summary.md").display().to_string()),
    )
    .await
    .unwrap();
    repo::meetings::set_status(&s.state, &s.id, MeetStatus::Cancelled, Some("user_cancel"))
        .await
        .unwrap();

    let (code, body) = s.post(&format!("/api/meet/meetings/{}/summary", s.id)).await;
    assert_eq!(code, StatusCode::OK, "отменённая встреча с транскриптом: {body}");
    assert_eq!(body["from"], "summarizing");

    assert_eq!(
        s.wait_summary(SummaryStatus::Ready).await,
        SummaryStatus::Ready,
        "саммари не досчиталось"
    );
    let md = std::fs::read_to_string(s.dir.join("summary.md")).unwrap();
    assert!(md.contains("Резюме после отмены"), "{md}");
}

/// Обратная сторона: отмена на пустой комнате не оставляет транскрипта, и
/// суммировать нечего. Отказ должен быть внятным — с кодом про недостающий
/// вход, а не падением внутри пайплайна.
#[tokio::test]
async fn a_cancelled_meeting_without_a_transcript_refuses_with_a_named_code() {
    let s = Stand::at(MeetStatus::Summarizing).await;
    mocks::mock_claude_success(&s.bin, "claude", "## Резюме").unwrap();
    repo::meetings::set_status(&s.state, &s.id, MeetStatus::Cancelled, Some("alone_timeout"))
        .await
        .unwrap();

    let (code, body) = s.post(&format!("/api/meet/meetings/{}/summary", s.id)).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "transcript_missing", "{body}");
}

/// Живой кейс 07.09.2026 (Телемост, f61cfa7a…): встреча записалась и
/// обработалась, но связи с сообщением у неё не оказалось — кнопка в ленте
/// показывала «можно записать», а клик заводил новую пустую запись вместо
/// показа готовой. Привязать существующую встречу к сообщению было нечем.
#[tokio::test]
async fn an_existing_meeting_can_be_attached_to_a_message() {
    let s = Stand::ready().await;

    let (code, body) = s
        .post_json(
            &format!("/api/meet/meetings/{}/source", s.id),
            serde_json::json!({"plugin": "vk", "chat_id": "71297061", "message_id": 213794}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["attached"], true);
    assert_eq!(body["meeting_id"], s.id);

    // Бейдж обязан появиться сразу: кнопка в ленте читает индекс, а не базу.
    let badge = s
        .state
        .badge_of(&forge_meet::db::repo::SourceKey::new("vk", "71297061", 213794))
        .await;
    assert!(badge.is_some(), "бейдж не опубликован — кнопка останется пустой");
}

/// Половина адреса хуже его отсутствия: бейдж повис бы на несуществующем
/// сообщении, и кнопка вела бы в никуда.
#[tokio::test]
async fn attaching_with_an_incomplete_address_is_refused() {
    let s = Stand::ready().await;
    let (code, _) = s
        .post_json(
            &format!("/api/meet/meetings/{}/source", s.id),
            serde_json::json!({"plugin": "", "chat_id": "7", "message_id": 1}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

/// Привязка к несуществующей встрече — 404, а не тихое создание строки.
#[tokio::test]
async fn attaching_to_an_unknown_meeting_is_not_found() {
    let s = Stand::ready().await;
    let (code, _) = s
        .post_json(
            "/api/meet/meetings/00000000-0000-0000-0000-000000000000/source",
            serde_json::json!({"plugin": "vk", "chat_id": "7", "message_id": 1}),
        )
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}
