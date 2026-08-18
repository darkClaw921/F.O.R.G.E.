//! `GET /api/meet/meetings/:id/audio` на НАСТОЯЩЕМ роутере и НАСТОЯЩИХ файлах.
//!
//! Проверяется именно то, ради чего роут переписан в Ф7: честный Range.
//! Отдельно — пер-трековая выдача через `?track=N`, которой пользуется вкладка
//! участников («послушать Участника 2»), и лестница ошибок.
//!
//! # Файл заведомо больше одного куска
//!
//! Архив здесь — 200 000 байт, то есть три с лишним куска по 64 КиБ. Меньший
//! файл прошёл бы одним `read`, и ошибка в перемотке (`seek`) или в счётчике
//! остатка не проявилась бы вовсе: именно так выглядит зелёный тест, который
//! ничего не проверяет.
//!
//! # Герметичность
//!
//! Все пути — во временном каталоге. Без этого прогон читал бы `~/.forge/meet`,
//! где лежат записи реальных встреч.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forge_meet::db::repo;
use forge_meet::db::Db;
use forge_meet::state::MeetState;
use forge_meet::status::MeetStatus;
use tower::ServiceExt;

const URL: &str = "https://telemost.yandex.ru/j/60497752704486";

/// Размер архивного файла в тесте — заведомо больше куска чтения (64 КиБ).
const ARCHIVE_LEN: usize = 200_000;

struct Stand {
    state: Arc<MeetState>,
    app: axum::Router,
    id: String,
    dir: PathBuf,
    archive: Vec<u8>,
    track1: Vec<u8>,
    _tmp: tempfile::TempDir,
}

/// Псевдослучайные, но воспроизводимые байты: сравнение кусков должно ловить
/// сдвиг на байт, а нули этого не умеют.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 13) as u8 ^ seed)
        .collect()
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
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        let archive = pattern(ARCHIVE_LEN, 0x5a);
        let track1 = pattern(9_000, 0xa5);
        std::fs::write(dir.join("audio.m4a"), &archive).unwrap();
        std::fs::write(dir.join("raw/track-0.wav"), pattern(3_000, 0x11)).unwrap();
        std::fs::write(dir.join("raw/track-1.wav"), &track1).unwrap();

        let link = forge_meet::parse_meeting_url(URL).unwrap();
        let out = repo::meetings::create_or_attach(
            &state,
            repo::meetings::NewMeeting {
                link,
                source: None,
                title: None,
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: dir.display().to_string(),
            },
        )
        .await
        .unwrap();
        let id = out.meeting.id.clone();
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
            archive,
            track1,
            _tmp: tmp,
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        range: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(r) = range {
            req = req.header("range", r);
        }
        let resp = self
            .app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let code = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24).await.unwrap();
        (code, headers, bytes.to_vec())
    }

    fn audio_uri(&self) -> String {
        format!("/api/meet/meetings/{}/audio", self.id)
    }

    fn header(headers: &axum::http::HeaderMap, name: &str) -> String {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }
}

/// Путь, которым ходит фронт, — БЕЗ Range, целиком, через `fetch` + `blob:`.
/// Он обязан продолжать работать байт в байт.
#[tokio::test]
async fn the_whole_file_is_served_with_accept_ranges_announced() {
    let s = Stand::new().await;
    let (code, headers, body) = s.send("GET", &s.audio_uri(), None).await;

    assert_eq!(code, StatusCode::OK);
    assert_eq!(body, s.archive, "тело обязано совпадать с файлом побайтово");
    assert_eq!(Stand::header(&headers, "accept-ranges"), "bytes");
    assert_eq!(Stand::header(&headers, "content-type"), "audio/mp4");
    assert_eq!(
        Stand::header(&headers, "content-length"),
        ARCHIVE_LEN.to_string()
    );
    assert!(
        Stand::header(&headers, "content-range").is_empty(),
        "у полного ответа Content-Range быть не должно"
    );
}

/// 206 с ПРАВИЛЬНЫМ `Content-Range` и правильными байтами — в том числе на
/// куске, который начинается за пределами первого чтения и занимает несколько
/// кусков.
#[tokio::test]
async fn a_range_request_gets_206_with_the_exact_bytes() {
    let s = Stand::new().await;

    for (range, start, end) in [
        ("bytes=0-9", 0usize, 9usize),
        ("bytes=10-19", 10, 19),
        // Через границу куска чтения (64 КиБ) и с перемоткой.
        ("bytes=70000-140000", 70_000, 140_000),
        // Открытый хвост.
        ("bytes=199990-", 199_990, ARCHIVE_LEN - 1),
        // Суффиксная форма.
        ("bytes=-100", ARCHIVE_LEN - 100, ARCHIVE_LEN - 1),
        // Хвост за концом файла подрезается, а не отвергается.
        ("bytes=199000-999999", 199_000, ARCHIVE_LEN - 1),
    ] {
        let (code, headers, body) = s.send("GET", &s.audio_uri(), Some(range)).await;
        assert_eq!(code, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            Stand::header(&headers, "content-range"),
            format!("bytes {start}-{end}/{ARCHIVE_LEN}"),
            "{range}"
        );
        assert_eq!(
            Stand::header(&headers, "content-length"),
            (end - start + 1).to_string(),
            "{range}"
        );
        assert_eq!(Stand::header(&headers, "accept-ranges"), "bytes", "{range}");
        assert_eq!(
            body,
            s.archive[start..=end],
            "{range}: отдан не тот кусок файла"
        );
    }
}

/// 416 на диапазон за пределами файла — С `Content-Range: bytes */LEN`.
///
/// Без этого заголовка клиент не узнает длину файла и повторит тот же
/// невыполнимый запрос.
#[tokio::test]
async fn a_range_past_the_end_gets_416_with_the_real_length() {
    let s = Stand::new().await;
    for range in ["bytes=200000-", "bytes=999999-1000000", "bytes=-0"] {
        let (code, headers, body) = s.send("GET", &s.audio_uri(), Some(range)).await;
        assert_eq!(code, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(
            Stand::header(&headers, "content-range"),
            format!("bytes */{ARCHIVE_LEN}"),
            "{range}"
        );
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "range_not_satisfiable", "{range}");
    }
}

/// НЕСКОЛЬКО ДИАПАЗОНОВ обслуживаются ЦЕЛИКОМ, а не первым куском под видом
/// всех. Второе было бы молчаливой выдачей неверных данных.
#[tokio::test]
async fn multiple_ranges_never_yield_the_first_one_disguised_as_all() {
    let s = Stand::new().await;
    let (code, headers, body) = s
        .send("GET", &s.audio_uri(), Some("bytes=0-10,20-30"))
        .await;
    assert_eq!(code, StatusCode::OK, "206 здесь означал бы неверные данные");
    assert_eq!(body.len(), ARCHIVE_LEN);
    assert_eq!(body, s.archive);
    assert!(Stand::header(&headers, "content-range").is_empty());
}

/// Непонятный `Range` игнорируется, а не превращается в ошибку.
#[tokio::test]
async fn a_broken_range_header_falls_back_to_the_whole_file() {
    let s = Stand::new().await;
    for range in ["items=0-5", "bytes=abc", "bytes=10-5", "bytes="] {
        let (code, _, body) = s.send("GET", &s.audio_uri(), Some(range)).await;
        assert_eq!(code, StatusCode::OK, "{range}");
        assert_eq!(body.len(), ARCHIVE_LEN, "{range}");
    }
}

/// HEAD отвечает ТЕМИ ЖЕ заголовками, но без тела — в том числе с Range.
#[tokio::test]
async fn head_answers_with_the_same_headers_and_no_body() {
    let s = Stand::new().await;

    let (code, headers, body) = s.send("HEAD", &s.audio_uri(), None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(body.is_empty(), "у HEAD не бывает тела");
    assert_eq!(
        Stand::header(&headers, "content-length"),
        ARCHIVE_LEN.to_string(),
        "именно за длиной HEAD и ходят"
    );
    assert_eq!(Stand::header(&headers, "accept-ranges"), "bytes");
    assert_eq!(Stand::header(&headers, "content-type"), "audio/mp4");

    let (code, headers, body) = s.send("HEAD", &s.audio_uri(), Some("bytes=5-14")).await;
    assert_eq!(code, StatusCode::PARTIAL_CONTENT);
    assert!(body.is_empty());
    assert_eq!(
        Stand::header(&headers, "content-range"),
        format!("bytes 5-14/{ARCHIVE_LEN}")
    );
    assert_eq!(Stand::header(&headers, "content-length"), "10");

    // И 416 у HEAD тоже 416.
    let (code, headers, _) = s.send("HEAD", &s.audio_uri(), Some("bytes=999999-")).await;
    assert_eq!(code, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        Stand::header(&headers, "content-range"),
        format!("bytes */{ARCHIVE_LEN}")
    );
}

/// ⚠ СТРАЖ, КОТОРЫЙ ПРИШЛОСЬ ПЕРЕПИСАТЬ.
///
/// Первая редакция проверяла пустоту тела HEAD ЧЕРЕЗ РОУТЕР — и не краснела,
/// когда ветку `if method == Method::HEAD` из хендлера убирали совсем. Причина:
/// `axum::routing::get` сам обслуживает HEAD и ВЫРЕЗАЕТ тело у готового
/// ответа. То есть проверка через роутер физически не может отличить «хендлер
/// не читал файл» от «хендлер прочитал 200 КБ, а axum их выбросил».
///
/// Разница не косметическая: у четырёхчасовой встречи пер-трековый WAV весит
/// полтора гигабайта, и HEAD без этой ветки прочитал бы их с диска целиком —
/// чтобы отдать ноль байт.
///
/// Поэтому хендлер зовётся НАПРЯМУЮ, минуя роутер.
#[tokio::test]
async fn head_is_answered_by_the_handler_itself_without_reading_the_file() {
    use axum::extract::{Path as AxPath, RawQuery, State};
    use axum::http::{HeaderMap, Method};

    let s = Stand::new().await;

    let resp = forge_meet::routes::audio::audio(
        State(s.state.clone()),
        AxPath(s.id.clone()),
        RawQuery(None),
        Method::HEAD,
        HeaderMap::new(),
    )
    .await
    .expect("HEAD обязан отвечать успехом");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 24).await.unwrap();
    assert!(
        body.is_empty(),
        "хендлер прочитал {} байт ради ответа без тела",
        body.len()
    );

    // А GET через тот же прямой вызов тело отдаёт — иначе проверка выше
    // зеленела бы просто потому, что хендлер сломан целиком.
    let resp = forge_meet::routes::audio::audio(
        State(s.state.clone()),
        AxPath(s.id.clone()),
        RawQuery(None),
        Method::GET,
        HeaderMap::new(),
    )
    .await
    .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 24).await.unwrap();
    assert_eq!(body.len(), ARCHIVE_LEN);
}

/// ПЕР-ТРЕКОВОЕ АУДИО — параметром запроса, тем же роутом, с тем же Range.
/// Ради него и существует ручное переименование спикеров.
#[tokio::test]
async fn a_single_track_is_served_by_query_parameter_with_ranges() {
    let s = Stand::new().await;
    let uri = format!("{}?track=1", s.audio_uri());

    let (code, headers, body) = s.send("GET", &uri, None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body, s.track1, "отдана не та дорожка");
    assert_eq!(Stand::header(&headers, "content-type"), "audio/wav");
    assert_ne!(body, s.archive, "дорожка обязана отличаться от архива");

    let (code, headers, body) = s.send("GET", &uri, Some("bytes=100-199")).await;
    assert_eq!(code, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, s.track1[100..=199]);
    assert_eq!(
        Stand::header(&headers, "content-range"),
        format!("bytes 100-199/{}", s.track1.len())
    );

    // Дорожка есть, а нулевая — другая: параметр действительно выбирает файл.
    let (_, _, zero) = s
        .send("GET", &format!("{}?track=0", s.audio_uri()), None)
        .await;
    assert_ne!(zero, s.track1);
    assert_eq!(zero.len(), 3_000);
}

/// Отсутствующий файл — 404, а не пустое тело со статусом 200.
#[tokio::test]
async fn a_missing_file_is_404_not_an_empty_success() {
    let s = Stand::new().await;

    // Дорожки с таким номером нет.
    let (code, _, body) = s
        .send("GET", &format!("{}?track=9", s.audio_uri()), None)
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "not_found");

    // Архива нет — сведение не выполнялось (не установлен ffmpeg).
    std::fs::remove_file(s.dir.join("audio.m4a")).unwrap();
    let (code, _, _) = s.send("GET", &s.audio_uri(), None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// ЛЕСТНИЦА: несуществующая встреча даёт 404 РАНЬШЕ, чем разбирается
/// параметр. Иначе клиент узнавал бы о чужих записях по коду ответа.
#[tokio::test]
async fn the_error_ladder_resolves_the_meeting_before_the_parameters() {
    let s = Stand::new().await;

    let (code, _, _) = s
        .send("GET", "/api/meet/meetings/нет-такой/audio?track=abc", None)
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "404 обязан быть раньше 400");

    // А у существующей встречи битый параметр — уже 400.
    let (code, _, body) = s
        .send("GET", &format!("{}?track=abc", s.audio_uri()), None)
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "bad_request");

    // Обход каталога не доходит до файловой системы.
    for bad in ["track=../../etc/passwd", "track=-1", "track=99999"] {
        let (code, _, _) = s
            .send("GET", &format!("{}?{bad}", s.audio_uri()), None)
            .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{bad}");
    }
}
