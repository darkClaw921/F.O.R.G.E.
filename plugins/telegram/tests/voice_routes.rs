//! Сквозной прогон расшифровки голосового в Telegram: роуты, кадры WS, стор.
//!
//! Близнец `plugins/vk/tests/voice_routes.rs`. Общий у трёх плагинов фронт-модуль
//! (ГС-Ф5) читает одинаковые кадры и одинаковые ответы, поэтому и проверяться они
//! обязаны одинаково — расхождение здесь означало бы «в VK работает, в Telegram
//! нет», причём выяснилось бы это у пользователя.
//!
//! Настоящий ffmpeg (тест пропускается, если его нет), локальный стенд вместо
//! polza.ai, whisper.cpp не запускается никогда (GPU + полтора гигабайта весов),
//! в Telegram не ходим вовсе — вложение кладётся в кэш руками.
//!
//! Настоящий ключ polza.ai в исходниках тестов ЗАПРЕЩЁН (`forge-v0m9k`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_telegram::config::TelegramConfig;
use forge_telegram::login::{LoginFlow, TgUserDto};
use forge_telegram::media_cache::{self, CacheKey, MediaMeta};
use forge_telegram::routes::build_router;
use forge_telegram::state::TelegramState;
use forge_telegram::ws::protocol::TgServerMsg;

// --- стенд облака -------------------------------------------------------------

struct Cloud {
    calls: AtomicUsize,
    delay: Duration,
    text: String,
}

struct CloudStub {
    base_url: String,
    state: Arc<Cloud>,
}

impl CloudStub {
    fn calls(&self) -> usize {
        self.state.calls.load(Ordering::SeqCst)
    }
}

async fn cloud_stub(text: &str, delay: Duration) -> CloudStub {
    let state = Arc::new(Cloud {
        calls: AtomicUsize::new(0),
        delay,
        text: text.to_string(),
    });
    let app = Router::new()
        .route("/audio/transcriptions", post(handle_stt))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    CloudStub {
        base_url: format!("http://{addr}"),
        state,
    }
}

async fn handle_stt(State(state): State<Arc<Cloud>>, body: axum::body::Bytes) -> axum::Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    assert!(body.len() > 100, "в облако уехало пустое тело");
    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }
    axum::Json(json!({
        "language": "ru",
        "segments": [{ "start": 0.0, "end": 1.5, "text": state.text }],
    }))
}

// --- обвязка плагина ----------------------------------------------------------

fn ffmpeg() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("ffmpeg"))
            .find(|p| p.is_file())
    })
}

/// Пишет `voice.toml` рядом с файлом сессии — там же, где его ищет плагин.
fn write_voice_config(dir: &Path, base_url: &str, extra: &str) {
    let engine = if extra.contains("engine =") {
        String::new()
    } else {
        "engine = \"cloud\"\n".to_string()
    };
    std::fs::write(
        dir.join("voice.toml"),
        format!(
            "{engine}\
             polza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n\
             cloud_base_url = {base_url:?}\n\
             {extra}"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("meet.toml"), b"").unwrap();
}

async fn authorized(dir: &Path) -> Arc<TelegramState> {
    let state = TelegramState::new(TelegramConfig {
        cache_dir: dir.join("cache"),
        session_path: dir.join("telegram.session"),
        prompts_path: dir.join("prompts.json"),
        ..TelegramConfig::default()
    });
    state
        .set_login(LoginFlow::Authorized {
            user: TgUserDto {
                id: "1".into(),
                username: None,
                first_name: None,
                phone: None,
            },
        })
        .await;
    state
}

/// Кладёт в медиа-кэш настоящий ogg… точнее WAV: контейнер здесь неважен,
/// потому что ffmpeg определяет формат по содержимому, а не по имени, и
/// голосовое Telegram точно так же приезжает «просто файлом».
async fn seed_voice(
    state: &Arc<TelegramState>,
    mid: i32,
    secs: f32,
    mime: &str,
) -> CacheKey {
    let key = CacheKey::parse("-1001234", &mid.to_string(), Some("full")).unwrap();
    let bytes = wav(secs);
    let mut meta = MediaMeta::now(Some(mime.to_string()), bytes.len() as u64, None);
    meta.duration_secs = Some(secs.ceil() as i32);
    media_cache::put_atomic(&state.config, &key, &bytes, &meta)
        .await
        .unwrap();
    key
}

fn wav(secs: f32) -> Vec<u8> {
    let rate = 16_000u32;
    let n = (rate as f32 * secs) as u32;
    let data_len = n * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let v = (t * 440.0 * std::f32::consts::TAU).sin() * 8000.0;
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

async fn send(state: Arc<TelegramState>, method: &str, uri: &str) -> axum::response::Response {
    build_router(state)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 512 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn drain(state: &Arc<TelegramState>) {
    for _ in 0..300 {
        if state.voice.active() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("расшифровка не закончилась за отведённое время");
}

// --- тесты --------------------------------------------------------------------

/// **СЧАСТЛИВЫЙ ПУТЬ ЦЕЛИКОМ: POST → кадры → GET.**
#[tokio::test]
async fn a_full_cycle_runs_the_engine_and_stores_the_text() {
    let Some(_ff) = ffmpeg() else {
        eprintln!("ffmpeg не установлен — сквозной прогон пропущен");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("голосовое из телеграма", Duration::ZERO).await;
    write_voice_config(dir.path(), &cloud.base_url, "");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 1.5, "audio/ogg").await;

    let mut frames = state.subscribe();
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/telegram/voice/-1001234/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], false, "{json}");
    assert_eq!(json["engine"], "cloud");
    let run_id = json["run_id"].as_str().unwrap().to_string();
    drain(&state).await;

    let mut started = 0;
    let mut done_text = None;
    let mut errors = Vec::new();
    while let Ok(frame) = frames.try_recv() {
        match frame {
            TgServerMsg::VoiceTranscribeStarted {
                run_id: id,
                dialog_id,
                message_id,
                index,
                engine,
            } => {
                started += 1;
                assert_eq!(id, run_id);
                assert_eq!(dialog_id, "-1001234");
                assert_eq!(message_id, 7001);
                assert_eq!(index, 0, "у Telegram вложение одно, индекс всегда ноль");
                assert_eq!(engine, "cloud");
            }
            TgServerMsg::VoiceTranscribeDone { text, language, .. } => {
                assert_eq!(language, "ru");
                done_text = Some(text);
            }
            TgServerMsg::VoiceTranscribeError { code, message, .. } => {
                errors.push(format!("{code}: {message}"));
            }
            _ => {}
        }
    }
    assert!(errors.is_empty(), "прогон отказал: {errors:?}");
    assert_eq!(started, 1);
    assert_eq!(done_text.as_deref(), Some("голосовое из телеграма"));
    assert_eq!(cloud.calls(), 1);

    let json = body_json(
        send(
            state.clone(),
            "GET",
            "/api/telegram/voice/-1001234/7001/0/transcript",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], true, "{json}");
    assert_eq!(json["text"], "голосовое из телеграма");
    assert!(json["duration_ms"].as_i64().unwrap() >= 1400);

    // Ни одного временного файла.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("cache").join("voice-work"))
        .map(|it| it.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "мусор после прогона: {leftovers:?}");
}

/// **ДВА ОДНОВРЕМЕННЫХ ЗАПРОСА НА ОДНО ВЛОЖЕНИЕ — ОДИН ПРОГОН.**
#[tokio::test]
async fn two_requests_for_one_attachment_share_a_single_run() {
    let Some(_ff) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("один текст", Duration::from_millis(600)).await;
    write_voice_config(dir.path(), &cloud.base_url, "");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 1.0, "audio/ogg").await;

    let first = body_json(
        send(
            state.clone(),
            "POST",
            "/api/telegram/voice/-1001234/7001/0/transcribe",
        )
        .await,
    )
    .await;
    let second = body_json(
        send(
            state.clone(),
            "POST",
            "/api/telegram/voice/-1001234/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(second["attached"], true);
    assert_eq!(second["run_id"], first["run_id"]);

    drain(&state).await;
    assert_eq!(cloud.calls(), 1, "в облако ушло два запроса на один файл");
}

/// **ВЕТКИ ОТКАЗОВ ДАЮТ СВОИ КОДЫ — ТЕ ЖЕ, ЧТО В VK.**
#[tokio::test]
async fn every_refusal_branch_has_its_own_code() {
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("текст", Duration::ZERO).await;

    // Не голосовое: в кэше видеосообщение-кружок.
    {
        let d = dir.path().join("not-audio");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(&d, &cloud.base_url, "");
        let state = authorized(&d).await;
        seed_voice(&state, 7001, 0.5, "video/mp4").await;
        let resp = send(state, "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"]["code"], "voice_not_audio");
    }

    // Слишком длинное.
    {
        let d = dir.path().join("too-long");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(&d, &cloud.base_url, "max_duration_secs = 1\n");
        let state = authorized(&d).await;
        seed_voice(&state, 7001, 3.0, "audio/ogg").await;
        let resp = send(state, "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await;
        assert_eq!(body_json(resp).await["error"]["code"], "voice_too_long");
    }

    // Нет ffmpeg — до скачивания.
    {
        let d = dir.path().join("no-ffmpeg");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(
            &d,
            &cloud.base_url,
            &format!("ffmpeg_path = {:?}\n", d.join("нет").display().to_string()),
        );
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await;
        assert_eq!(body_json(resp).await["error"]["code"], "voice_no_ffmpeg");
    }

    // Локальный движок без весов.
    {
        let d = dir.path().join("no-model");
        std::fs::create_dir_all(&d).unwrap();
        let whisper = d.join("whisper-cli");
        std::fs::write(&whisper, b"#!/bin/sh\nexit 1\n").unwrap();
        write_voice_config(
            &d,
            &cloud.base_url,
            &format!(
                "engine = \"local\"\nwhisper_path = {:?}\nmodels_dir = {:?}\n",
                whisper.display().to_string(),
                d.join("models").display().to_string()
            ),
        );
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await;
        assert_eq!(body_json(resp).await["error"]["code"], "voice_no_model");
    }

    // Облако без ключа.
    {
        let d = dir.path().join("no-key");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("voice.toml"), b"engine = \"cloud\"\n").unwrap();
        std::fs::write(d.join("meet.toml"), b"").unwrap();
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await;
        assert_eq!(body_json(resp).await["error"]["code"], "voice_no_key");
    }
}
