//! Сквозной прогон расшифровки голосового: роуты, кадры WS, стор.
//!
//! Флагманский тест фичи «голосовые сообщения» в этом плагине. Он проходит ВЕСЬ
//! путь — `POST …/transcribe` → подготовка аудио настоящим ffmpeg → облако →
//! кадры `voice_transcribe_*` в broadcast → `GET …/transcript` — и ни разу не
//! выходит в сеть наружу.
//!
//! # ЧТО ЗДЕСЬ НАСТОЯЩЕЕ, А ЧТО ПОДМЕНЕНО
//!
//! | часть | как |
//! |---|---|
//! | ffmpeg | НАСТОЯЩИЙ (тест пропускается, если его нет на машине) |
//! | polza.ai | локальный стенд на `127.0.0.1:0`, как `tests/vk_stub.rs` |
//! | whisper.cpp | НЕ ЗАПУСКАЕТСЯ никогда: движок local проверяется только лестницей отказов |
//! | api.vk.com | не трогается вовсе: вложение кладётся в кэш руками |
//!
//! Настоящий whisper не запускается по двум причинам сразу: он требует GPU и
//! полутора гигабайт весов (правило проекта — локальные модели только на GPU),
//! а прогон занял бы секунды на каждый тест. Настоящий ключ polza.ai в
//! исходниках тестов ЗАПРЕЩЁН (`forge-v0m9k`), поэтому в конфиг едет заведомая
//! пустышка, а стенд её и не проверяет.
//!
//! # ПОЧЕМУ СВОЙ СТЕНД, А НЕ `wiremock`
//!
//! Ровно та же причина, что в `tests/vk_stub.rs`: новых dev-зависимостей крейт
//! не заводит, а стенд из `axum` + `TcpListener::bind("127.0.0.1:0")` короче
//! конфигурации мок-сервера. Порт всегда нулевой — жёсткий однажды упёрся бы в
//! живой devforge на 7331.
//!
//! # ИЗОЛЯЦИЯ БЕЗ ПЕРЕМЕННЫХ ОКРУЖЕНИЯ
//!
//! `voice.toml` пишется РЯДОМ с `vk.toml` теста, то есть во временный каталог
//! (см. `forge_voice::config::paths_next_to`). Переменные окружения здесь не
//! используются вовсе: они процессные, и два теста, выставившие их
//! одновременно, читали бы чужой файл — известная беда `forge-oi1l`.

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

use forge_vk::config::{SecretToken, VkConfig};
use forge_vk::login::{LoginFlow, VkUserDto};
use forge_vk::media_cache::{self, CacheKey, MediaMeta};
use forge_vk::routes::build_router;
use forge_vk::state::VkState;
use forge_vk::ws::protocol::VkServerMsg;

// ===========================================================================
// Стенд облака.
// ===========================================================================

/// Состояние стенда polza.ai.
struct Cloud {
    /// Сколько расшифровок он обслужил.
    calls: AtomicUsize,
    /// Задержка ответа — ею изображается «долгая расшифровка».
    delay: Duration,
    /// Текст, который стенд «распознал».
    text: String,
}

/// Живой стенд: адрес и счётчик вызовов.
struct CloudStub {
    base_url: String,
    state: Arc<Cloud>,
}

impl CloudStub {
    fn calls(&self) -> usize {
        self.state.calls.load(Ordering::SeqCst)
    }
}

/// Поднимает стенд облака на свободном порту.
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

/// `POST /audio/transcriptions` — ответ `verbose_json`, как у настоящего шлюза.
async fn handle_stt(State(state): State<Arc<Cloud>>, body: axum::body::Bytes) -> axum::Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    // Тело — multipart с mp3 внутри. Разбирать его целиком незачем, но ДВЕ
    // вещи проверить обязательно, потому что обе уже ломались в жизни:
    //
    // 1. тело не пустое — иначе подготовка аудио сломалась молча;
    // 2. внутри ИМЕННО mp3, а не WAV: WAV роняет шлюз polza.ai (измерено на
    //    forge-meet), и на стенде такая ошибка прошла бы незамеченной.
    assert!(body.len() > 100, "в облако уехало пустое тело: {} байт", body.len());
    let mp3 = body
        .windows(3)
        .any(|w| w == b"ID3" || (w[0] == 0xFF && w[1] & 0xE0 == 0xE0));
    let wav = body.windows(4).any(|w| w == b"RIFF");
    assert!(mp3 && !wav, "в облако уехал не mp3 (WAV роняет шлюз polza.ai)");
    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }
    axum::Json(json!({
        "language": "ru",
        "segments": [{ "start": 0.0, "end": 1.5, "text": state.text }],
    }))
}

// ===========================================================================
// Обвязка плагина.
// ===========================================================================

/// Путь ffmpeg или `None` — тогда тест пропускается.
fn ffmpeg() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("ffmpeg"))
            .find(|p| p.is_file())
    })
}

/// Пишет `voice.toml` рядом с конфигом плагина.
///
/// Ключ заведомо ненастоящий: настоящий ключ polza.ai в исходниках тестов
/// запрещён (`forge-v0m9k`), а стенд его и не проверяет.
fn write_voice_config(dir: &Path, base_url: &str, extra: &str) {
    // Движок объявляется только если его не задал сам вызывающий: TOML не
    // допускает повторного ключа, и вторая строка `engine = …` дала бы
    // `voice_config` вместо той ветки отказа, которую проверяет тест.
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
    // Пустой `meet.toml`: иначе лестница дотянулась бы до настоящего конфига
    // пользователя, и тест зависел бы от его машины.
    std::fs::write(dir.join("meet.toml"), b"").unwrap();
}

/// Авторизованное состояние с изолированными каталогами и мёртвой сетью VK.
async fn authorized(dir: &Path) -> Arc<VkState> {
    let state = VkState::new(VkConfig {
        access_token: SecretToken::new("vk1.a.TEST"),
        // Заведомо закрытый порт: ни один тест не имеет права уйти в живой VK.
        base_url: "http://127.0.0.1:1/method".into(),
        cache_dir: dir.join("cache"),
        config_path: dir.join("vk.toml"),
        prompts_path: dir.join("prompts.json"),
        ..VkConfig::default()
    });
    state
        .set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1234".into(),
                ..Default::default()
            },
        })
        .await;
    state
}

/// Кладёт в медиа-кэш настоящий WAV — ровно так же, как это делает докачка.
async fn seed_voice(state: &Arc<VkState>, mid: i32, index: u8, secs: f32, mime: &str) -> CacheKey {
    let key = CacheKey::parse("2000000042", &mid.to_string(), &index.to_string(), Some("full"))
        .unwrap();
    let bytes = wav(secs);
    let mut meta = MediaMeta::now(Some(mime.to_string()), bytes.len() as u64, None);
    meta.duration_secs = Some(secs.ceil() as i32);
    media_cache::put_atomic(&state.config, &key, &bytes, &meta)
        .await
        .unwrap();
    key
}

/// WAV 16 кГц моно с синусом внутри.
///
/// Тишина прошла бы ffmpeg, но не отличила бы «звук доехал» от «в файле нули».
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

async fn send(state: Arc<VkState>, method: &str, uri: &str) -> axum::response::Response {
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

/// Ждёт, пока реестр расшифровок опустеет.
async fn drain(state: &Arc<VkState>) {
    for _ in 0..300 {
        if state.voice.active() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("расшифровка не закончилась за отведённое время");
}

// ===========================================================================
// Тесты.
// ===========================================================================

/// **СЧАСТЛИВЫЙ ПУТЬ ЦЕЛИКОМ: POST → кадры → GET.**
///
/// Проверяется всё сразу, потому что порознь эти куски уже покрыты, а вместе —
/// нет: именно на стыках живут ошибки вроде «done пришёл раньше, чем текст
/// записан на диск».
#[tokio::test]
async fn a_full_cycle_runs_the_engine_and_stores_the_text() {
    let Some(_ff) = ffmpeg() else {
        eprintln!("ffmpeg не установлен — сквозной прогон пропущен");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("привет, это голосовое", Duration::ZERO).await;
    write_voice_config(dir.path(), &cloud.base_url, "");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 0, 1.5, "audio/ogg").await;

    let mut frames = state.subscribe();
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], false, "{json}");
    assert_eq!(json["attached"], false);
    assert_eq!(json["engine"], "cloud");
    let run_id = json["run_id"].as_str().unwrap().to_string();

    drain(&state).await;

    // Кадры: ровно один `started`, ноль и больше `delta`, ровно один `done`.
    let mut started = 0;
    let mut done_text = None;
    let mut errors = Vec::new();
    while let Ok(frame) = frames.try_recv() {
        match frame {
            VkServerMsg::VoiceTranscribeStarted {
                run_id: id,
                dialog_id,
                message_id,
                index,
                engine,
            } => {
                started += 1;
                assert_eq!(id, run_id);
                assert_eq!(dialog_id, "2000000042");
                assert_eq!(message_id, 7001);
                assert_eq!(index, 0);
                assert_eq!(engine, "cloud", "движок обязан быть назван ДО отправки");
            }
            VkServerMsg::VoiceTranscribeDone { text, language, .. } => {
                assert_eq!(language, "ru", "язык из ответа облака обязан доехать");
                done_text = Some(text);
            }
            VkServerMsg::VoiceTranscribeError { code, message, .. } => {
                errors.push(format!("{code}: {message}"));
            }
            _ => {}
        }
    }
    assert!(errors.is_empty(), "прогон отказал: {errors:?}");
    assert_eq!(started, 1, "кадр started обязан быть ровно один");
    assert_eq!(done_text.as_deref(), Some("привет, это голосовое"));
    assert_eq!(cloud.calls(), 1, "в облако ушёл ровно один запрос");

    // Текст лежит в сторе И отдаётся роутом.
    let json = body_json(
        send(
            state.clone(),
            "GET",
            "/api/vk/voice/2000000042/7001/0/transcript",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], true, "{json}");
    assert_eq!(json["text"], "привет, это голосовое");
    assert_eq!(json["language"], "ru");
    assert_eq!(json["engine"], "cloud");
    assert!(json["duration_ms"].as_i64().unwrap() >= 1400, "{json}");
    assert!(json["created_at"].as_i64().unwrap() > 0);

    // И ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК: состояние собирается заново на том же каталоге.
    drop(state);
    let restarted = authorized(dir.path()).await;
    let json = body_json(
        send(restarted, "GET", "/api/vk/voice/2000000042/7001/0/transcript").await,
    )
    .await;
    assert_eq!(json["text"], "привет, это голосовое");

    // Ни одного временного файла не осталось.
    let scratch = dir.path().join("cache").join("voice-work");
    let leftovers: Vec<_> = std::fs::read_dir(&scratch)
        .map(|it| it.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "мусор после прогона: {leftovers:?}");
}

/// **ДВА ОДНОВРЕМЕННЫХ ЗАПРОСА НА ОДНО ВЛОЖЕНИЕ — ОДИН ПРОГОН.**
///
/// Голосовое лежит в общей беседе, и «расшифруй» на нём нажмут двое. Без дедупа
/// это два запроса в облако: вдвое дороже ради того же самого текста.
#[tokio::test]
async fn two_requests_for_one_attachment_share_a_single_run() {
    let Some(_ff) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    // Задержка стенда держит прогон живым, пока летит второй запрос.
    let cloud = cloud_stub("один текст", Duration::from_millis(600)).await;
    write_voice_config(dir.path(), &cloud.base_url, "");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 0, 1.0, "audio/ogg").await;

    let first = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(first["attached"], false);

    // Второй запрос — пока первый ещё идёт.
    let second = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(second["attached"], true, "второй запрос запустил свой прогон");
    assert_eq!(second["run_id"], first["run_id"]);

    drain(&state).await;
    assert_eq!(cloud.calls(), 1, "в облако ушло два запроса на один файл");
}

/// **ПОТОЛОК ПАРАЛЛЕЛИЗМА ОТКАЗЫВАЕТ СРАЗУ, А НЕ ЖДЁТ.**
///
/// Молчаливое ожидание в очереди означало бы открытую плашку без единого кадра:
/// человек видит «идёт расшифровка», а на деле его запрос стои́т за чужим.
#[tokio::test]
async fn the_parallel_limit_refuses_at_once() {
    let Some(_ff) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("долгий текст", Duration::from_millis(800)).await;
    write_voice_config(dir.path(), &cloud.base_url, "max_parallel = 1\n");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 0, 1.0, "audio/ogg").await;
    seed_voice(&state, 7003, 0, 1.0, "audio/ogg").await;

    let busy = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(busy["attached"], false);

    let started = std::time::Instant::now();
    let resp = send(
        state.clone(),
        "POST",
        "/api/vk/voice/2000000042/7003/0/transcribe",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let json = body_json(resp).await;
    assert_eq!(json["error"]["code"], "voice_busy");
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "отказ ждал на семафоре: {:?}",
        started.elapsed()
    );

    drain(&state).await;
    // Слот освободился — следующий запрос проходит.
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7003/0/transcribe",
        )
        .await,
    )
    .await;
    assert!(json["run_id"].is_string(), "{json}");
    drain(&state).await;
}

/// **КАЖДАЯ ВЕТКА ЛЕСТНИЦЫ ОТКАЗОВ ДАЁТ СВОЙ КОД, А НЕ ОБЩИЙ `500`.**
///
/// По этим кодам фронт показывает разные подсказки: «поставьте ffmpeg»,
/// «скачайте модель», «введите ключ». Общий код отправил бы половину
/// пользователей чинить не то.
#[tokio::test]
async fn every_refusal_branch_has_its_own_code() {
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("текст", Duration::ZERO).await;

    // 1. Не голосовое: в кэше лежит картинка.
    {
        let d = dir.path().join("not-audio");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(&d, &cloud.base_url, "");
        let state = authorized(&d).await;
        seed_voice(&state, 7001, 0, 0.5, "image/jpeg").await;
        let resp = send(state, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"]["code"], "voice_not_audio");
    }

    // 2. Слишком длинное: потолок в конфиге — одна секунда.
    {
        let d = dir.path().join("too-long");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(&d, &cloud.base_url, "max_duration_secs = 1\n");
        let state = authorized(&d).await;
        seed_voice(&state, 7001, 0, 3.0, "audio/ogg").await;
        let resp = send(state, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "voice_too_long");
        assert!(
            json["error"]["message"].as_str().unwrap().contains('1'),
            "потолок обязан быть назван: {json}"
        );
    }

    // 3. Нет ffmpeg — и это видно ДО скачивания вложения.
    {
        let d = dir.path().join("no-ffmpeg");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(
            &d,
            &cloud.base_url,
            &format!("ffmpeg_path = {:?}\n", d.join("нет-такого").display().to_string()),
        );
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(resp).await["error"]["code"], "voice_no_ffmpeg");
    }

    // 4. Локальный движок без бинаря и без весов — ДВА разных кода.
    {
        let d = dir.path().join("no-whisper");
        std::fs::create_dir_all(&d).unwrap();
        write_voice_config(
            &d,
            &cloud.base_url,
            &format!(
                "engine = \"local\"\nwhisper_path = {:?}\n",
                d.join("нет-бинаря").display().to_string()
            ),
        );
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(body_json(resp).await["error"]["code"], "voice_no_whisper");
    }
    {
        let d = dir.path().join("no-model");
        std::fs::create_dir_all(&d).unwrap();
        let fake_whisper = d.join("whisper-cli");
        std::fs::write(&fake_whisper, b"#!/bin/sh\nexit 1\n").unwrap();
        write_voice_config(
            &d,
            &cloud.base_url,
            &format!(
                "engine = \"local\"\nwhisper_path = {:?}\nmodels_dir = {:?}\n",
                fake_whisper.display().to_string(),
                d.join("models").display().to_string()
            ),
        );
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(body_json(resp).await["error"]["code"], "voice_no_model");
    }

    // 5. Облако без ключа.
    {
        let d = dir.path().join("no-key");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("voice.toml"), b"engine = \"cloud\"\n").unwrap();
        std::fs::write(d.join("meet.toml"), b"").unwrap();
        let state = authorized(&d).await;
        let resp = send(state, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "voice_no_key");
        assert!(!json["error"]["message"].as_str().unwrap().contains("pza_"));
    }
}

/// **ОТКАЗ ОБЛАКА ДОЕЗЖАЕТ ОДНИМ КАДРОМ `error` СО СВОИМ КОДОМ.**
///
/// И прогон при этом завершается: слот освобождается, а стор остаётся пустым —
/// «расшифровано в ничего» не кэшируется.
#[tokio::test]
async fn a_cloud_failure_ends_the_run_with_one_error_frame() {
    let Some(_ff) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    // Стенд, отвечающий 402: «нет средств» — код, ради которого коды вообще
    // существуют отдельно от текста.
    let app = Router::new().route(
        "/audio/transcriptions",
        post(|| async {
            (
                StatusCode::PAYMENT_REQUIRED,
                axum::Json(json!({"error": {"message": "insufficient funds"}})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    write_voice_config(dir.path(), &format!("http://{addr}"), "");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 0, 1.0, "audio/ogg").await;

    let mut frames = state.subscribe();
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert!(json["run_id"].is_string());
    drain(&state).await;

    let mut errors = Vec::new();
    let mut dones = 0;
    while let Ok(frame) = frames.try_recv() {
        match frame {
            VkServerMsg::VoiceTranscribeError { code, .. } => errors.push(code),
            VkServerMsg::VoiceTranscribeDone { .. } => dones += 1,
            _ => {}
        }
    }
    assert_eq!(errors.len(), 1, "ошибка обязана быть ровно одним кадром: {errors:?}");
    assert_eq!(errors[0], "cloud_no_funds", "код отказа облака обязан дожить");
    assert_eq!(dones, 0, "после ошибки не бывает done");

    // Стор пуст: неудачный прогон ничего не закэшировал.
    let json = body_json(
        send(
            state.clone(),
            "GET",
            "/api/vk/voice/2000000042/7001/0/transcript",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], false, "{json}");
    assert_eq!(state.voice.active(), 0, "слот не освобождён");
}

/// **`?force=1` ПЕРЕСЧИТЫВАЕТ, А БЕЗ НЕГО ГОТОВОЕ ОТДАЁТСЯ КАК ЕСТЬ.**
#[tokio::test]
async fn force_recomputes_and_the_default_reuses() {
    let Some(_ff) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let cloud = cloud_stub("первый разбор", Duration::ZERO).await;
    write_voice_config(dir.path(), &cloud.base_url, "");
    let state = authorized(dir.path()).await;
    seed_voice(&state, 7001, 0, 1.0, "audio/ogg").await;

    send(
        state.clone(),
        "POST",
        "/api/vk/voice/2000000042/7001/0/transcribe",
    )
    .await;
    drain(&state).await;
    assert_eq!(cloud.calls(), 1);

    // Без флага — движок не запускается вовсе.
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], true);
    assert_eq!(json["text"], "первый разбор");
    assert_eq!(cloud.calls(), 1, "готовое пересчиталось без просьбы");

    // С флагом — пересчёт.
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe?force=1",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], false, "{json}");
    drain(&state).await;
    assert_eq!(cloud.calls(), 2);
}

/// **ГЕЙТ ФАЗЫ: НАСТОЯЩИЙ ЛОКАЛЬНЫЙ ДВИЖОК НА GPU.**
///
/// Единственный тест крейта, который запускает whisper.cpp по-настоящему,
/// поэтому он `#[ignore]`: ему нужны бинарь, 574 МБ весов и Metal, а прогон
/// занимает секунды. Запуск руками:
///
/// ```text
/// cargo test -p forge-vk --test voice_routes -- --ignored --nocapture
/// FORGE_VOICE_GATE_WAV=/путь/к/речи.wav  # необязательно: без него берётся синус
/// ```
///
/// Что он доказывает и чего не докажет никакой мок:
///
/// 1. argv, собранный [`forge_voice::whisper::build_args`], НА САМОМ ДЕЛЕ
///    принимается установленной сборкой whisper.cpp;
/// 2. Metal подтверждён — иначе гард убил бы прогон, и мы увидели бы
///    `metal_unavailable` вместо текста (правило проекта: локальные модели
///    считаются только на GPU, молчаливого CPU не бывает);
/// 3. WAV 16 кГц моно, который готовит `audio::prepare`, действительно
///    читается движком;
/// 4. текст доезжает и до кадра `done`, и до стора.
#[tokio::test]
#[ignore = "требует whisper.cpp, веса модели и Metal — гейт фазы, запускается руками"]
async fn the_local_engine_really_runs_on_the_gpu() {
    let Some(_ff) = ffmpeg() else {
        panic!("гейт требует настоящий ffmpeg");
    };
    let whisper = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("whisper-cli"))
        .find(|p| p.is_file())
        .expect("гейт требует whisper-cli в PATH");
    let models = PathBuf::from(std::env::var("HOME").unwrap()).join(".forge/meet/models");
    assert!(
        models.join("ggml-large-v3-turbo-q5_0.bin").is_file(),
        "гейт требует веса в {}",
        models.display()
    );

    let dir = tempfile::tempdir().unwrap();
    write_voice_config(
        dir.path(),
        "http://127.0.0.1:1",
        &format!(
            "engine = \"local\"\nlanguage = \"ru\"\nwhisper_path = {:?}\nmodels_dir = {:?}\n",
            whisper.display().to_string(),
            models.display().to_string()
        ),
    );
    let state = authorized(dir.path()).await;

    // Речь, а не синус: на синусе движок честно вернул бы пустоту, и тест
    // проверял бы только то, что процесс не упал.
    let key = CacheKey::parse("2000000042", "7001", "0", Some("full")).unwrap();
    let bytes = match std::env::var("FORGE_VOICE_GATE_WAV") {
        Ok(p) if !p.trim().is_empty() => std::fs::read(p.trim()).expect("файл речи не читается"),
        _ => wav(2.0),
    };
    let mut meta = MediaMeta::now(Some("audio/wav".into()), bytes.len() as u64, None);
    meta.duration_secs = Some(3);
    media_cache::put_atomic(&state.config, &key, &bytes, &meta)
        .await
        .unwrap();

    let mut frames = state.subscribe();
    let json = body_json(
        send(
            state.clone(),
            "POST",
            "/api/vk/voice/2000000042/7001/0/transcribe",
        )
        .await,
    )
    .await;
    assert_eq!(json["engine"], "local", "{json}");
    drain(&state).await;

    let mut done = None;
    let mut errors = Vec::new();
    while let Ok(frame) = frames.try_recv() {
        match frame {
            VkServerMsg::VoiceTranscribeDone { text, engine, .. } => {
                assert_eq!(engine, "local");
                done = Some(text);
            }
            VkServerMsg::VoiceTranscribeError { code, message, .. } => {
                errors.push(format!("{code}: {message}"));
            }
            _ => {}
        }
    }
    assert!(
        errors.is_empty(),
        "локальный прогон отказал (metal_unavailable здесь означал бы, что GPU не подтверждён): {errors:?}"
    );
    let text = done.expect("кадр done не пришёл");
    eprintln!("ГЕЙТ: локальный движок распознал: {text:?}");
    assert!(!text.trim().is_empty(), "движок вернул пустой текст");

    let json = body_json(
        send(
            state.clone(),
            "GET",
            "/api/vk/voice/2000000042/7001/0/transcript",
        )
        .await,
    )
    .await;
    assert_eq!(json["ready"], true, "{json}");
    assert_eq!(json["engine"], "local");
    assert_eq!(json["text"], text);
}
