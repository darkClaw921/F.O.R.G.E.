//! **Конвейер в облачном режиме: от сырых WAV до транскрипта и саммари.**
//!
//! Тот же пайплайн, что и в `tests/pipeline.rs`, но обе стадии переключены на
//! polza.ai, а машина изображается ЧИСТОЙ: `whisper-cli` не найден, весов на
//! диске нет, Metal нет, Claude CLI нет. Именно это и проверяется — что ни
//! одно из четырёх не мешает довести встречу до конца.
//!
//! # Сеть не настоящая
//!
//! Стенд — локальный axum на `127.0.0.1:0`, точка подмены та же, что у Ф14:
//! `MeetConfig::cloud_base_url`. Наружу тест не ходит и денег не тратит.
//!
//! # Что здесь доказывается, кроме «работает»
//!
//! | свойство | почему сломается тихо |
//! |---|---|
//! | облачный путь не берёт семафор whisper | вторая встреча ждала бы первую БЕЗ ПРИЧИНЫ, и выглядело бы это как «медленно» |
//! | облачный путь не ждёт конца записей | расшифровка стояла бы до конца чужой встречи |
//! | провал облака не теряет запись | WAV на диске есть, а встреча `failed` — единственный честный исход |
//! | саммари идёт тем же кадром `Progress` | фронт не переделывается, и это надо доказать, а не заявить |
//! | облако идёт ПО ДОРОЖКАМ, когда дорожка = человек | имена спикеров тихо съехали бы к догадке по миксу (замер Ф17: 32-34 верных из 43) |

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use forge_meet::config::{MeetConfig, SttBackend, SummaryBackend};
use forge_meet::db::repo;
use forge_meet::ingest::{wav::WavWriter, Manifest};
use forge_meet::pipeline;
use forge_meet::state::MeetState;
use forge_meet::status::{MeetStatus, SummaryStatus};
use forge_meet::testing::mocks;
use forge_meet::ws::protocol::MeetServerMsg;
use serde_json::{json, Value};

const WAIT_CAP: Duration = Duration::from_secs(30);

// ===========================================================================
// Стенд облака
// ===========================================================================

/// Как стенд отвечает на расшифровку и на саммари.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mood {
    /// Нормальный ответ.
    Good,
    /// Отказ с телом — текст обязан доехать до человека.
    Refuse,
}

#[derive(Default)]
struct StubState {
    calls: Mutex<Vec<String>>,
    stt: Mutex<Option<Mood>>,
    chat: Mutex<Option<Mood>>,
    /// Заготовленные ответы расшифровки по порядку запросов. Пусто — общий
    /// ответ по умолчанию. Нужно пер-трековому прогону: там запросов столько
    /// же, сколько дорожек, и текст у них разный.
    stt_script: Mutex<std::collections::VecDeque<Value>>,
}

struct Cloud {
    base_url: String,
    state: Arc<StubState>,
}

impl Cloud {
    async fn start() -> Self {
        let state = Arc::new(StubState::default());
        let app = Router::new()
            .route("/api/v1/audio/transcriptions", post(transcriptions))
            .route("/api/v1/chat/completions", post(chat))
            .route("/api/v1/key", axum::routing::get(key))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://{addr}/api/v1"),
            state,
        }
    }

    fn stt(&self, m: Mood) {
        *self.state.stt.lock().unwrap() = Some(m);
    }

    fn chat(&self, m: Mood) {
        *self.state.chat.lock().unwrap() = Some(m);
    }

    /// Ответ на ОЧЕРЕДНОЙ запрос расшифровки: `(начало, конец, текст)` в
    /// координатах присланного файла.
    fn script_stt(&self, segments: &[(f64, f64, &str)]) {
        let segs: Vec<Value> = segments
            .iter()
            .map(|(s, e, t)| json!({"start": s, "end": e, "text": t}))
            .collect();
        self.state
            .stt_script
            .lock()
            .unwrap()
            .push_back(json!({"language": "russian", "segments": segs}));
    }

    fn calls(&self) -> Vec<String> {
        self.state.calls.lock().unwrap().clone()
    }
}

async fn transcriptions(State(st): State<Arc<StubState>>, _body: axum::body::Bytes) -> Response {
    st.calls.lock().unwrap().push("stt".to_string());
    let mood = st.stt.lock().unwrap().unwrap_or(Mood::Good);
    if mood == Mood::Refuse {
        return (
            StatusCode::PAYMENT_REQUIRED,
            axum::Json(json!({"error": {"code": "NO_FUNDS", "message": "на балансе пусто"}})),
        )
            .into_response();
    }
    if let Some(scripted) = st.stt_script.lock().unwrap().pop_front() {
        return axum::Json(scripted).into_response();
    }
    // Окно нарезки — 90 с, запись в стенде десятисекундная, значит окно одно и
    // таймкоды в нём уже совпадают с общей осью.
    axum::Json(json!({
        "language": "russian",
        "segments": [
            {"start": 0.0, "end": 4.0, "text": " Коллеги, начинаем планёрку."},
            {"start": 5.2, "end": 9.0, "text": " Релиз готов к пятнице."}
        ]
    }))
    .into_response()
}

async fn chat(State(st): State<Arc<StubState>>, _body: axum::body::Bytes) -> Response {
    st.calls.lock().unwrap().push("chat".to_string());
    let mood = st.chat.lock().unwrap().unwrap_or(Mood::Good);
    if mood == Mood::Refuse {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": {"code": "UNAUTHORIZED", "message": "ключ не годится"}})),
        )
            .into_response();
    }
    // Кадры уезжают по одному: тот же SSE, что у живого сервиса.
    let frames: Vec<String> = ["## Итоги\n", "Решили выпустить релиз ", "в пятницу."]
        .iter()
        .map(|c| format!("data: {}\n\n", json!({"choices": [{"delta": {"content": c}}]})))
        .chain(std::iter::once("data: [DONE]\n\n".to_string()))
        .collect();
    let body = axum::body::Body::from(frames.concat());
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

/// Бесплатный `GET /key` — им `GET /api/meet/status` проверяет живость ключа.
///
/// `limit_reset` здесь `monthly`, как у ЖИВОГО ключа: в плане было записано
/// `daily`, и живая проба Ф14 это опровергла.
async fn key(State(st): State<Arc<StubState>>) -> Response {
    st.calls.lock().unwrap().push("key".to_string());
    axum::Json(json!({
        "label": "pza_STANDs••••••••_local",
        "limit": 1000,
        "limit_remaining": 997,
        "limit_reset": "monthly",
        "usage": 3
    }))
    .into_response()
}

// ===========================================================================
// Стенд встречи
// ===========================================================================

struct Stand {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    state: Arc<MeetState>,
    meeting_id: String,
    meeting_dir: PathBuf,
}

impl Stand {
    /// Встреча в `transcoding` на ЧИСТОЙ машине: whisper не найден, весов нет,
    /// Metal нет, Claude CLI нет.
    async fn new(cloud: &Cloud, stt: SttBackend, summary: SummaryBackend) -> Self {
        Self::build(cloud, stt, summary, None).await
    }

    /// Та же встреча, но в режиме «дорожка соответствует человеку»: с
    /// пер-трековым VAD в журнале и `track_mode = PerParticipant`.
    ///
    /// `shouting` — расписание речи, при котором ВСЕ говорят одновременно всю
    /// встречу. Ровно этот случай и обязан не влезать в облачный бюджет.
    async fn per_participant(
        cloud: &Cloud,
        stt: SttBackend,
        summary: SummaryBackend,
        shouting: bool,
    ) -> Self {
        Self::build(cloud, stt, summary, Some(shouting)).await
    }

    async fn build(
        cloud: &Cloud,
        stt: SttBackend,
        summary: SummaryBackend,
        per_track: Option<bool>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();

        let cfg = MeetConfig {
            config_path: root.join("meet.toml"),
            cache_dir: root.join("cache"),
            db_path: root.join("cache/meet.db"),
            models_dir: root.join("models"),
            // Ничего локального на машине НЕТ, и пути ведут в никуда.
            whisper_path: Some(root.join("no-whisper")),
            chrome_path: Some(root.join("no-chrome")),
            claude_cli_path: root.join("no-claude"),
            // Стенд подменяет CLI моком (`with_claude`) там, где проверяет
            // локальный путь: запуски `claude -p` по умолчанию выключены
            // глобально (см. forge_claude_runner), стенд включает их явно.
            claude_enabled: true,
            stt_backend: stt,
            summary_backend: summary,
            cloud_base_url: cloud.base_url.clone(),
            polza_api_key: forge_meet::cloud::SecretKey::new("pza_STANDstandSTANDstand_local"),
            whisper_gate_poll_ms: 20,
            ffmpeg_timeout_secs: 60,
            summary_timeout_secs: 30,
            ..MeetConfig::default()
        };
        let state = MeetState::new(cfg);
        let db = forge_meet::db::Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();

        let link =
            forge_meet::parse_meeting_url("https://telemost.yandex.ru/j/60497752704486").unwrap();
        let meeting_dir = root.join("cache/m1");
        let out = repo::meetings::create_or_attach(
            &state,
            repo::meetings::NewMeeting {
                link,
                source: None,
                title: Some("Планёрка".into()),
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: meeting_dir.display().to_string(),
            },
        )
        .await
        .unwrap();
        let meeting_id = out.meeting.id.clone();

        let stand = Self {
            _dir: dir,
            bin,
            state,
            meeting_id,
            meeting_dir,
        };
        match per_track {
            Some(shouting) => stand.write_per_track_recording(shouting),
            None => stand.write_recording(),
        }
        stand.advance_to_transcoding().await;
        // ffmpeg на месте: облаку он нужен, чтобы нарезать окна и сжать их.
        // Пробы объявлены пройденными, whisper — ненайденным: ровно та чистая
        // машина, ради которой облачный режим и существует.
        stand.set_ffmpeg();
        stand
    }

    fn write_recording(&self) {
        let raw = self.meeting_dir.join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        for (idx, level) in [(0usize, 1_000i16), (1, 2_000)] {
            let mut w = WavWriter::create(raw.join(format!("track-{idx}.wav")), 16_000).unwrap();
            w.write_frame(0, &vec![level; 16_000 * 10]).unwrap();
            w.finish().unwrap();
        }

        let mut m = Manifest::new(
            &self.meeting_id,
            "https://telemost.yandex.ru/j/60497752704486",
            "telemost",
            "normal",
            16_000,
        );
        m.t0_unix_ms = 1_700_000_000_000;
        m.duration_ms = 10_000;
        m.capture_path = Some("rtcpeerconnection".into());
        m.tracks = vec![track_state(0, "a"), track_state(1, "b")];
        m.finished = true;
        m.save(&self.meeting_dir).unwrap();

        let mut j = forge_meet::ingest::journal::EventJournal::open(&self.meeting_dir).unwrap();
        j.note(
            0,
            "recording_started",
            json!({"t0_unix_ms": 1_700_000_000_000u64}),
        )
        .unwrap();
        j.note(0, "active_speaker", json!({"at_ms": 0, "name": "Аня"}))
            .unwrap();
        j.note(
            5_000,
            "active_speaker",
            json!({"at_ms": 5_000, "name": "Боря"}),
        )
        .unwrap();
        j.note(
            10_000,
            "recording_stopped",
            json!({"reason": "user_stop", "at_ms": 10_000}),
        )
        .unwrap();
    }

    /// Запись, где ДОРОЖКА СООТВЕТСТВУЕТ ЧЕЛОВЕКУ.
    ///
    /// Отличий от [`Self::write_recording`] три: `track_mode =
    /// PerParticipant`, пер-трековый VAD в журнале (`speech_started` /
    /// `speech_ended`) и `msid`, совпадающий с ключом участника, — по нему
    /// каскад называет дорожки настоящими именами первой же ступенью.
    ///
    /// Речь дорожек ПЕРЕСЕКАЕТСЯ: Боря вступает, пока Аня ещё говорит. По
    /// миксу это место и даёт ошибку в имени, ради которой всё затевалось.
    ///
    /// Запись десятисекундная — и это не небрежность фикстуры. Локальный
    /// бюджет на такой записи пер-трековый путь отклонил бы (постоянная цена
    /// запуска whisper больше сэкономленного звука), а облачный принимает:
    /// процесса там нет, и цена линейна по звуку.
    fn write_per_track_recording(&self, shouting: bool) {
        use forge_meet::bot::mode::TrackMode;

        let raw = self.meeting_dir.join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        for (idx, level) in [(0usize, 1_000i16), (1, 2_000)] {
            let mut w = WavWriter::create(raw.join(format!("track-{idx}.wav")), 16_000).unwrap();
            w.write_frame(0, &vec![level; 16_000 * 10]).unwrap();
            w.finish().unwrap();
        }

        let mut m = Manifest::new(
            &self.meeting_id,
            "https://telemost.yandex.ru/j/60497752704486",
            "telemost",
            "normal",
            16_000,
        );
        m.t0_unix_ms = 1_700_000_000_000;
        m.duration_ms = 10_000;
        m.capture_path = Some("rtcpeerconnection".into());
        m.track_mode = Some(TrackMode::PerParticipant);
        m.tracks = vec![track_state(0, "a"), track_state(1, "b")];
        m.finished = true;
        m.save(&self.meeting_dir).unwrap();

        let mut j = forge_meet::ingest::journal::EventJournal::open(&self.meeting_dir).unwrap();
        j.note(0, "recording_started", json!({"t0_unix_ms": 1_700_000_000_000u64}))
            .unwrap();
        j.note(0, "track_added", json!({"track_id": "a", "idx": 0, "msid": "name:аня"}))
            .unwrap();
        j.note(0, "track_added", json!({"track_id": "b", "idx": 1, "msid": "name:боря"}))
            .unwrap();
        j.note(0, "active_speaker", json!({"at_ms": 0, "name": "Аня"}))
            .unwrap();
        j.note(5_000, "active_speaker", json!({"at_ms": 5_000, "name": "Боря"}))
            .unwrap();
        let speech: Vec<(&str, u64, u64)> = if shouting {
            // Говорят оба и всю встречу: речи вдвое больше длительности, и
            // облачный бюджет её не выдерживает.
            vec![("a", 0, 10_000), ("b", 0, 10_000)]
        } else {
            vec![("a", 0, 4_000), ("b", 3_000, 6_000)]
        };
        for (track, from, to) in speech {
            j.note(from, "speech_started", json!({"track_id": track, "at_ms": from}))
                .unwrap();
            j.note(to, "speech_ended", json!({"track_id": track, "at_ms": to}))
                .unwrap();
        }
        j.note(10_000, "recording_stopped", json!({"reason": "user_stop", "at_ms": 10_000}))
            .unwrap();
    }

    async fn advance_to_transcoding(&self) {
        for st in [
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
        ] {
            repo::meetings::set_status(&self.state, &self.meeting_id, st, Some("test"))
                .await
                .unwrap();
        }
    }

    fn set_ffmpeg(&self) {
        let path = mocks::mock_ffmpeg_success(&self.bin, "ffmpeg").unwrap();
        let state = self.state.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut deps = state.deps.write().await;
                deps.probed = true;
                deps.ffmpeg.found = Some(true);
                deps.ffmpeg.path = Some(path.display().to_string());
                // ЧИСТАЯ МАШИНА: локального пути нет ни в одной его части.
                deps.whisper.found = Some(false);
                deps.whisper.path = None;
                deps.model_ready = false;
                deps.gpu_metal = None;
            })
        });
    }

    async fn status(&self) -> MeetStatus {
        repo::meetings::get(&self.state, &self.meeting_id)
            .await
            .unwrap()
            .status
    }

    /// Коды, записанные в журнал встречи. Пайплайн отдаёт наружу
    /// `MeetError::Internal` (так устроен перевод `ProcError` в HTTP), а
    /// НАСТОЯЩАЯ причина живёт в журнале и в поле `reason` статуса — именно её
    /// читает человек в карточке встречи.
    async fn event_codes(&self) -> Vec<String> {
        repo::events::list(&self.state, &self.meeting_id)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|e| e.reason)
            .collect()
    }

    /// Ставит РАБОЧИЙ Claude CLI.
    ///
    /// Без него локальная сторона саммари нерабочая, и гейт законно уводит её
    /// в облако (таблица `deps::choose_route`). То есть проверить «локальное
    /// саммари остаётся локальным» можно только на машине, где оно вообще
    /// возможно.
    ///
    /// Состояние пересобирается целиком (`MeetConfig` внутри него неизменяем),
    /// поэтому отчёт проб переносится руками: иначе вместе со старым
    /// состоянием потерялся бы подставленный ffmpeg.
    async fn with_claude(&mut self, path: &PathBuf) {
        let mut cfg = self.state.config.clone();
        cfg.claude_cli_path = path.clone();
        let deps = self.state.deps_snapshot().await;
        let db = self.state.db().expect("база обязана быть открыта");
        let state = MeetState::new(cfg);
        state.attach_db(db).unwrap();
        *state.deps.write().await = deps;
        self.state = state;
    }

    async fn summary_status(&self) -> SummaryStatus {
        repo::meetings::get(&self.state, &self.meeting_id)
            .await
            .unwrap()
            .summary_status
    }
}

fn track_state(idx: usize, id: &str) -> forge_meet::ingest::tracks::TrackState {
    forge_meet::ingest::tracks::TrackState {
        track_id: id.to_string(),
        idx,
        start_offset_ms: 0,
        end_offset_ms: Some(10_000),
        frames: 500,
        samples: 160_000,
        end_samples: 160_000,
        capture_path: None,
    }
}

// ===========================================================================
// Тесты
// ===========================================================================

/// **ГЛАВНЫЙ ТЕСТ ЗАДАЧИ.** Чистая машина + облако = транскрипт и саммари.
///
/// Ни `whisper-cli`, ни весов, ни Metal, ни Claude CLI — и встреча всё равно
/// доходит до `ready` с обоими файлами на диске.
#[tokio::test(flavor = "multi_thread")]
async fn a_clean_machine_with_no_whisper_at_all_still_gets_a_transcript_and_a_summary() {
    let cloud = Cloud::start().await;
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready);
    assert_eq!(report.segments, 2);
    // Телемост работает в режиме `Slots` (измерено): дорожка там не
    // соответствует человеку, и путь по дорожкам законно не включается —
    // в облаке ровно так же, как локально.
    assert_eq!(report.per_track, 0, "дорожка ≠ человек, а резали по дорожкам");
    assert!(!report.queued, "облако не встаёт в очередь whisper");

    let md = std::fs::read_to_string(s.meeting_dir.join("transcript.md")).unwrap();
    assert!(md.contains("Коллеги, начинаем планёрку."), "{md}");
    // Спикеры взяты из DOM-таймлайна ровно как на локальном пути: облако
    // меняет источник ТЕКСТА, а не источник имён.
    assert!(md.contains("**[00:00:00] Аня**"), "{md}");
    assert!(md.contains("**[00:00:05] Боря**"), "{md}");

    let summary = std::fs::read_to_string(s.meeting_dir.join("summary.md")).unwrap();
    assert!(summary.contains("Решили выпустить релиз в пятницу."), "{summary}");
    assert_eq!(s.summary_status().await, SummaryStatus::Ready);

    let calls = cloud.calls();
    assert!(calls.contains(&"stt".to_string()), "{calls:?}");
    assert!(calls.contains(&"chat".to_string()), "{calls:?}");
}

/// **ГЛАВНЫЙ ТЕСТ Ф18.** Облако идёт ПО ДОРОЖКАМ, и имя спикера — факт.
///
/// Раньше здесь стоял глухой отказ: «облачная расшифровка идёт по миксу».
/// Обоснование («N загрузок = N-кратные деньги») опровергнуто замером: в
/// облако уезжает не запись, а вырезанная речь. Проверяется вся цепочка —
/// сколько запросов ушло, что попало в манифест, чем подписан артефакт и
/// какие имена стоят в базе.
#[tokio::test(flavor = "multi_thread")]
async fn a_per_participant_meeting_goes_to_the_cloud_track_by_track() {
    let cloud = Cloud::start().await;
    // Ответы приходят в координатах ВЫРЕЗАННОГО файла каждой дорожки:
    // у Ани кусок 0–4 300 общей оси, у Бори — 2 700–6 300.
    cloud.script_stt(&[(0.0, 4.0, " Я думаю, релиз надо перенести.")]);
    cloud.script_stt(&[(0.0, 3.0, " Подожди, тесты уже прошли.")]);
    let s = Stand::per_participant(&cloud, SttBackend::Cloud, SummaryBackend::Cloud, false).await;

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready);
    assert_eq!(report.per_track, 2, "расшифровывали не по дорожкам");
    assert_eq!(report.segments, 2);
    // Запрос на дорожку, а не на запись: их ровно два.
    assert_eq!(
        cloud.calls().iter().filter(|c| *c == "stt").count(),
        2,
        "{:?}",
        cloud.calls()
    );
    // И В ОБЛАКО УЕХАЛА РЕЧЬ, А НЕ ЗАПИСЬ: 4,3 + 3,6 секунды против 10 секунд
    // микса. Пер-трек здесь ДЕШЕВЛЕ микса.
    assert_eq!(report.audio_sent_ms, 4_300 + 3_600);
    assert!(report.audio_sent_ms < 10_000, "по дорожкам уехало больше микса");

    // Манифест несёт число дорожек: по нему `run.rs` строит `track_of`, и им
    // же отличаются пути в отчётах.
    let m = Manifest::load(&s.meeting_dir).unwrap();
    assert_eq!(m.postprocess.unwrap().per_track, 2);

    // Шапка артефакта осталась честной: путь облачный, модель облачная.
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(s.meeting_dir.join("transcript.json")).unwrap())
            .unwrap();
    assert_eq!(meta["engine"], json!("polza.ai"));
    assert_eq!(meta["model"], json!(forge_meet::cloud::DEFAULT_STT_MODEL));

    // --- ИМЯ СПИКЕРА — ФАКТ, А НЕ ДОГАДКА -------------------------------
    let rows = repo::segments::page(&s.state, &s.meeting_id, 0, 100).await.unwrap();
    let anya = rows.iter().find(|r| r.text.contains("релиз")).unwrap();
    let borya = rows.iter().find(|r| r.text.contains("Подожди")).unwrap();
    assert_eq!(anya.speaker_name.as_deref(), Some("Аня"));
    assert_eq!(borya.speaker_name.as_deref(), Some("Боря"));
    for r in [anya, borya] {
        assert_eq!(r.confidence, Some(1.0), "подпись дорожки не догадка: {r:?}");
    }
    // Номер дорожки в базе заполнен — по нему слушают `?track=`.
    assert_eq!(anya.track_idx, Some(0));
    assert_eq!(borya.track_idx, Some(1));

    // --- ПЕРЕБИВАНИЕ РАЗОБРАНО ------------------------------------------
    //
    // По миксу два одновременно говорящих человека склеиваются в одну
    // дорожку, и одна из реплик пропадает. Здесь обе на месте и пересекаются
    // во времени.
    assert!(
        borya.start_ms < anya.end_ms && borya.end_ms > anya.start_ms,
        "реплики обязаны пересекаться: {anya:?} / {borya:?}"
    );
    assert!(anya.overlapped && borya.overlapped, "перебивание не отмечено");

    // --- ТАЙМКОДЫ ВЕРНУЛИСЬ НА ОБЩУЮ ОСЬ --------------------------------
    //
    // Реплика Бори начиналась в нуле ЕГО вырезанного файла, кусок которого
    // начинается на 2 700 мс общей оси. Не сработай обратный пересчёт — она
    // легла бы в начало встречи, поверх Ани.
    assert_eq!((anya.start_ms, anya.end_ms), (0, 4_000), "{anya:?}");
    assert_eq!((borya.start_ms, borya.end_ms), (2_700, 5_700), "{borya:?}");
}

/// **ОТКАЗ ПО БЮДЖЕТУ НАЗЫВАЕТ СЕБЯ ЧЕЛОВЕКУ.**
///
/// Единственный случай, когда пер-трек в облаке дороже микса, — все говорят
/// одновременно всю встречу. Тогда путь идёт по миксу, имена становятся
/// догадкой, и человек обязан узнать почему: без события `per_track_skipped`
/// он увидел бы только, что имена вдруг стали хуже.
#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_where_everyone_talks_at_once_falls_back_to_the_mix_and_says_so() {
    let cloud = Cloud::start().await;
    let s = Stand::per_participant(&cloud, SttBackend::Cloud, SummaryBackend::Cloud, true).await;

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready);
    assert_eq!(report.per_track, 0, "речи вдвое больше микса, а резали по дорожкам");
    // Один запрос на всю запись — это микс.
    assert_eq!(cloud.calls().iter().filter(|c| *c == "stt").count(), 1);
    assert_eq!(report.audio_sent_ms, 10_000);

    // Событие ТО ЖЕ, что и на локальном пути, и причина названа поимённо.
    let codes = s.event_codes().await;
    assert!(
        codes.iter().any(|c| c == "per_track_skipped"),
        "отказ облака обязан писаться тем же событием, что и локальный: {codes:?}"
    );
    let events = repo::events::list(&s.state, &s.meeting_id).await.unwrap();
    let skipped = events
        .iter()
        .find(|e| e.reason.as_deref() == Some("per_track_skipped"))
        .unwrap();
    assert_eq!(skipped.detail.as_deref(), Some("budget_exceeded"));
}

/// **СЕМАФОР WHISPER ОБЛАКУ НЕ МЕШАЕТ.**
///
/// Слот `whisper` берётся ЗДЕСЬ, в тесте, и не отпускается до конца. Локальный
/// путь на этом месте встал бы навсегда; облачный обязан пройти насквозь — GPU
/// он не занимает, и ждать ему нечего.
///
/// Проверка сильнее, чем «два задания шли параллельно»: та зеленела бы и на
/// семафоре с двумя разрешениями.
#[tokio::test(flavor = "multi_thread")]
async fn the_whisper_semaphore_does_not_hold_the_cloud_back() {
    let cloud = Cloud::start().await;
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;

    let held = s
        .state
        .hub
        .acquire_whisper()
        .await
        .expect("слот whisper обязан браться");

    let run = tokio::time::timeout(WAIT_CAP, pipeline::run::run(&s.state, &s.meeting_id))
        .await
        .expect("облачная расшифровка ЖДЁТ СЕМАФОР whisper — это дефект, а не медленно");
    run.unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    drop(held);
}

/// **ГЕЙТ «НЕ РАСШИФРОВЫВАТЬ ВО ВРЕМЯ ЗАПИСИ» ОБЛАКУ ТОЖЕ НЕ МЕШАЕТ.**
///
/// Гейт существует потому, что Chrome и whisper делят Metal. Облако с Chrome
/// не конкурирует, и ожидание чужой встречи означало бы простой на ровном
/// месте.
#[tokio::test(flavor = "multi_thread")]
async fn a_recording_in_progress_does_not_block_the_cloud() {
    let cloud = Cloud::start().await;
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;

    // Чужая встреча занимает слот записи и числится активной.
    let busy = s
        .state
        .hub
        .try_acquire_recording()
        .expect("слот записи обязан браться");
    s.state.hub.mark_active("другая-встреча").await;
    assert!(!s.state.hub.active_meetings().await.is_empty());

    tokio::time::timeout(WAIT_CAP, pipeline::run::run(&s.state, &s.meeting_id))
        .await
        .expect("облачная расшифровка ЖДЁТ конца чужой записи — это дефект")
        .unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    s.state.hub.mark_inactive("другая-встреча").await;
    drop(busy);
}

/// **ПРОВАЛ ОБЛАКА НЕ ТЕРЯЕТ ЗАПИСЬ.**
///
/// Правило «`failed` только когда нет транскрипта» остаётся в силе: транскрипта
/// нет, статус `failed` — но дорожки, манифест и журнал на месте, и повтор
/// этапа возможен.
#[tokio::test(flavor = "multi_thread")]
async fn a_cloud_refusal_keeps_every_file_of_the_recording() {
    let cloud = Cloud::start().await;
    cloud.stt(Mood::Refuse);
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;

    let err = pipeline::run::run(&s.state, &s.meeting_id).await;
    assert!(err.is_err(), "отказ облака обязан быть отказом этапа");
    assert_eq!(s.status().await, MeetStatus::Failed);

    // ФАЙЛЫ ЗАПИСИ НА МЕСТЕ — это и есть «не потеряли встречу».
    assert!(s.meeting_dir.join("raw/track-0.wav").is_file());
    assert!(s.meeting_dir.join("raw/track-1.wav").is_file());
    assert!(s.meeting_dir.join("manifest.json").is_file());
    assert!(s.meeting_dir.join("events.ndjson").is_file());
    assert!(
        !s.meeting_dir.join("transcript.md").exists(),
        "транскрипта быть не должно: именно поэтому статус failed"
    );

    // И причина отказа доехала до журнала встречи дословно.
    let events = repo::events::list(&s.state, &s.meeting_id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| format!("{} {}", e.reason.unwrap_or_default(), e.detail.unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains("на балансе пусто"), "{events}");
}

/// **ПРОВАЛ ОБЛАЧНОГО САММАРИ НЕ РОНЯЕТ ВСТРЕЧУ.**
///
/// Тот же инвариант, что и у локального пути: `ready` плюс
/// `summary_status = failed`, транскрипт на диске.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_cloud_summary_still_leaves_a_ready_meeting() {
    let cloud = Cloud::start().await;
    cloud.chat(Mood::Refuse);
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready);
    assert_eq!(s.summary_status().await, SummaryStatus::Failed);
    assert!(s.meeting_dir.join("transcript.md").is_file());
    assert!(!s.meeting_dir.join("summary.md").exists());
}

/// **ДЕЛЬТЫ САММАРИ ИДУТ ТЕМ ЖЕ КАДРОМ.** Фронт не переделывается.
#[tokio::test(flavor = "multi_thread")]
async fn cloud_summary_deltas_arrive_as_the_same_progress_frame() {
    let cloud = Cloud::start().await;
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;
    let mut rx = s.state.subscribe();

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    let mut summarizing = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let MeetServerMsg::Progress { phase, done, .. } = msg {
            if phase == forge_meet::ws::protocol::Phase::Summarizing {
                summarizing.push(done);
            }
        }
    }
    assert!(
        summarizing.iter().any(|d| d.is_some_and(|n| n > 0)),
        "кадров с накопленными символами саммари нет вовсе: {summarizing:?}"
    );
}

/// **ВСЕ ЧЕТЫРЕ КОМБИНАЦИИ.** Переключатели независимы — ПОКА ЗАКАЗАННЫЙ
/// ПУТЬ РАБОТАЕТ.
///
/// Заказ соблюдается ровно до тех пор, пока заказанная сторона возможна: гейт
/// деградирует, а не блокирует (`forge-0s0l5`). Поэтому «локальное значит
/// локальное» проверяется на машине, где локальная сторона есть, а на ЧИСТОЙ
/// машине с ключом обе локальные половины честно уезжают в облако — с записью
/// отката в журнал.
#[tokio::test(flavor = "multi_thread")]
async fn all_four_combinations_take_the_path_they_are_told_to() {
    // 1. cloud + cloud — полный проход (проверен главным тестом), здесь только
    //    отметка, что комбинация допустима.
    // 2. cloud + local С РАБОЧИМ Claude CLI: саммари остаётся локальным.
    let cloud = Cloud::start().await;
    let mut s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Local).await;
    let claude = mocks::mock_claude_success(&s.bin, "claude", "## Резюме\n\nСчитали локально.")
        .unwrap();
    s.with_claude(&claude).await;
    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    assert!(s.meeting_dir.join("transcript.md").is_file());
    assert_eq!(s.summary_status().await, SummaryStatus::Ready);
    assert!(
        !cloud.calls().contains(&"chat".to_string()),
        "локальное саммари ушло в облако: {:?}",
        cloud.calls()
    );
    let codes = s.event_codes().await;
    assert!(
        !codes.iter().any(|c| c.contains("fallback")),
        "оба заказанных пути рабочие, откатываться было не от чего: {codes:?}"
    );

    // 3. local + cloud НА ЧИСТОЙ МАШИНЕ: локального whisper нет, ключ есть —
    //    расшифровка ОТКАТЫВАЕТСЯ В ОБЛАКО вместо того, чтобы похоронить
    //    запись. Обратная сторона `forge-0s0l5`.
    let cloud2 = Cloud::start().await;
    let s = Stand::new(&cloud2, SttBackend::Local, SummaryBackend::Cloud).await;
    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    let codes = s.event_codes().await;
    assert!(
        codes.iter().any(|c| c == "stt_fallback_cloud"),
        "откат обязан быть виден в журнале: {codes:?}"
    );
    assert!(
        cloud2.calls().contains(&"stt".to_string()),
        "откатились в облако, но так туда и не сходили: {:?}",
        cloud2.calls()
    );

    // 4. local + local на чистой машине: откатываются ОБЕ стадии, и оба
    //    отката записаны — молчаливой подмены не бывает.
    let cloud3 = Cloud::start().await;
    let s = Stand::new(&cloud3, SttBackend::Local, SummaryBackend::Local).await;
    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    let codes = s.event_codes().await;
    assert!(codes.iter().any(|c| c == "stt_fallback_cloud"), "{codes:?}");
    assert!(
        codes.iter().any(|c| c == "summary_fallback_cloud"),
        "{codes:?}"
    );
    assert_eq!(s.summary_status().await, SummaryStatus::Ready);
}

/// **НИ ОДНОГО РАБОЧЕГО ПУТИ — ТОЛЬКО ТОГДА ОТКАЗ.**
///
/// Чистая машина И ключа нет: откатываться некуда, и расшифровка обязана
/// отказать кодом ЗАКАЗАННОГО пути.
#[tokio::test(flavor = "multi_thread")]
async fn with_no_working_path_at_all_the_stage_refuses_by_the_configured_code() {
    for (backend, want) in [
        (SttBackend::Cloud, "cloud_not_configured"),
        (SttBackend::Local, "whisper_unavailable"),
    ] {
        let cloud = Cloud::start().await;
        let mut s = Stand::new(&cloud, backend, SummaryBackend::Cloud).await;
        let mut cfg = s.state.config.clone();
        cfg.polza_api_key = forge_meet::cloud::SecretKey::default();
        let deps = s.state.deps_snapshot().await;
        let db = s.state.db().unwrap();
        let state = MeetState::new(cfg);
        state.attach_db(db).unwrap();
        *state.deps.write().await = deps;
        s.state = state;

        assert!(pipeline::run::run(&s.state, &s.meeting_id).await.is_err());
        let codes = s.event_codes().await;
        assert!(
            codes.iter().any(|c| c == want),
            "{}: ждали {want}, получили {codes:?}",
            backend.as_str()
        );
        assert!(cloud.calls().is_empty());
    }
}

/// Ключа нет — облачная расшифровка отказывает СВОИМ кодом, а не «whisper не
/// установлен». Человек чинит то, чего действительно не хватает.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_key_the_cloud_path_says_so_by_its_own_code() {
    let cloud = Cloud::start().await;
    let mut s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;
    let mut cfg = s.state.config.clone();
    cfg.polza_api_key = forge_meet::cloud::SecretKey::default();
    let db = s.state.db().unwrap();
    let state = MeetState::new(cfg);
    state.attach_db(db).unwrap();
    {
        let mut deps = state.deps.write().await;
        *deps = s.state.deps_snapshot().await;
    }
    s.state = state;

    assert!(pipeline::run::run(&s.state, &s.meeting_id).await.is_err());
    let codes = s.event_codes().await;
    assert!(
        codes.iter().any(|c| c == "cloud_not_configured"),
        "облако без ключа обязано называть себя своим кодом: {codes:?}"
    );
    assert!(cloud.calls().is_empty());
}

// ===========================================================================
// GET /api/meet/status — облачный блок
// ===========================================================================

/// **СТАТУС ЧЕСТНО РАССКАЗЫВАЕТ ПРО ОБЛАКО** — и делает это одним бесплатным
/// вызовом, а повторный запрос берёт из кэша.
///
/// Кэш проверяется счётчиком вызовов стенда: без него открытая панель плагина
/// превратилась бы в поток запросов к чужому сервису.
#[tokio::test(flavor = "multi_thread")]
async fn the_status_route_tells_the_truth_about_the_cloud_and_caches_it() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let cloud = Cloud::start().await;
    let s = Stand::new(&cloud, SttBackend::Cloud, SummaryBackend::Cloud).await;
    let app = forge_meet::register_routes(axum::Router::new(), s.state.clone());

    let ask = |app: axum::Router| async move {
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/meet/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (code, json)
    };

    let (code, body) = ask(app.clone()).await;
    assert_eq!(code, 200);
    assert_eq!(body["cloud"]["configured"], true);
    assert!(body["cloud"]["error"].is_null(), "{}", body["cloud"]);
    assert_eq!(body["cloud"]["key"]["limit_remaining"].as_f64(), Some(997.0));
    assert_eq!(
        body["cloud"]["key"]["limit_reset"], "monthly",
        "у живого ключа месячный лимит, а не дневной"
    );
    assert_eq!(body["config"]["stt_backend"], "cloud");
    assert_eq!(body["config"]["summary_backend"], "cloud");
    // Ключа в ответе нет — только маска.
    let raw = serde_json::to_string(&body).unwrap();
    assert!(!raw.contains("pza_STANDstandSTANDstand_local"), "{raw}");

    // Второй запрос не идёт в сеть: кэш живёт минуту.
    let before = cloud.calls().iter().filter(|c| *c == "key").count();
    assert_eq!(before, 1, "первый запрос обязан быть ровно один: {:?}", cloud.calls());
    let _ = ask(app).await;
    let after = cloud.calls().iter().filter(|c| *c == "key").count();
    assert_eq!(after, 1, "статус ходит в облако на каждый запрос: {:?}", cloud.calls());
}
