//! ЖИВЫЕ вызовы polza.ai. По умолчанию НЕ ЗАПУСКАЮТСЯ.
//!
//! Тесты включаются переменной окружения `FORGE_MEET_POLZA_KEY` — ключ живёт
//! только там и в репозиторий не попадает никогда. Без переменной каждый тест
//! молча пропускается, поэтому `cargo test` на чужой машине и в CI не ходит в
//! сеть и не тратит деньги.
//!
//! Зачем они есть, если весь клиент проверен локальным стендом
//! (`tests/polza_stub.rs`): стенд доказывает НАШЕ поведение, а живой вызов —
//! что форма ответа у сервиса всё ещё та, под которую стенд написан. Это
//! разные утверждения, и второе протухает само по себе.
//!
//! Стоимость прогона: `/key` и `/balance` бесплатны, расшифровка шести секунд —
//! около 0,05 ₽, саммари на десяти строках — копейки.
//!
//! ```sh
//! FORGE_MEET_POLZA_KEY=pza_… cargo test -p forge-meet --test polza_live -- --ignored
//! ```

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use forge_meet::cloud::service::{filter_kind, KIND_CHAT, KIND_STT};
use forge_meet::cloud::summary::{stream_chat, ChatOptions};
use forge_meet::cloud::{CloudClient, CloudConfig, SecretKey, DEFAULT_STT_MODEL};

/// Клиент на живом API. `None` — ключа в окружении нет, тест пропускается.
fn live() -> Option<CloudClient> {
    let key = std::env::var("FORGE_MEET_POLZA_KEY").ok()?;
    if key.trim().is_empty() {
        return None;
    }
    CloudClient::new(&CloudConfig {
        api_key: SecretKey::new(key),
        ..CloudConfig::default()
    })
    .ok()
}

#[tokio::test]
#[ignore = "ходит в сеть и тратит деньги: FORGE_MEET_POLZA_KEY + --ignored"]
async fn the_service_endpoints_still_answer_in_the_shape_we_parse() {
    let Some(c) = live() else {
        eprintln!("FORGE_MEET_POLZA_KEY не задан — пропуск");
        return;
    };
    let t = Duration::from_secs(30);

    let key = c.key_info(t).await.expect("GET /key");
    assert!(key.limit_remaining.is_some(), "лимиты пропали: {key:?}");
    assert!(key.limit_reset.is_some(), "{key:?}");

    let b = c.balance(t).await.expect("GET /balance");
    assert!(b.amount > 0.0, "баланс кончился или форма ответа сменилась: {b:?}");

    let models = c.models(Duration::from_secs(60)).await.expect("GET /models");
    assert!(models.len() > 100, "каталог усох: {}", models.len());
    let stt = filter_kind(&models, KIND_STT);
    let whisper = stt
        .iter()
        .find(|m| m.id == DEFAULT_STT_MODEL)
        .expect("единственная модель с таймкодами исчезла из каталога");
    // ГЛАВНОЕ: цена по-прежнему лежит уровнем глубже.
    let p = whisper.pricing.as_ref().expect("цена STT пропала");
    assert!(p.stt_per_minute.unwrap_or_default() > 0.0, "{p:?}");
    let chat = filter_kind(&models, KIND_CHAT);
    assert!(chat.len() > 50);
    assert!(
        chat.iter().any(|m| m.id == "google/gemini-3.5-flash"),
        "модель саммари по умолчанию исчезла из каталога"
    );
}

/// Живая расшифровка одного окна: форма `verbose_json` всё ещё несёт
/// посегментные таймкоды.
///
/// Файл берётся из `FORGE_MEET_POLZA_MP3` — заранее сжатое короткое аудио.
#[tokio::test]
#[ignore = "ходит в сеть и тратит деньги: FORGE_MEET_POLZA_KEY + --ignored"]
async fn a_live_window_comes_back_with_segment_timestamps() {
    let Some(c) = live() else {
        eprintln!("FORGE_MEET_POLZA_KEY не задан — пропуск");
        return;
    };
    let Ok(path) = std::env::var("FORGE_MEET_POLZA_MP3") else {
        eprintln!("FORGE_MEET_POLZA_MP3 не задан — пропуск");
        return;
    };
    let bytes = std::fs::read(&path).expect("файл окна не читается");

    let (segments, language) = c
        .transcribe_bytes(
            bytes,
            "window-0000.mp3",
            DEFAULT_STT_MODEL,
            "ru",
            Duration::from_secs(120),
        )
        .await
        .expect("POST /audio/transcriptions");

    assert!(!segments.is_empty(), "таймкодов не пришло вовсе");
    assert!(
        segments.iter().any(|s| s.end_ms > s.start_ms),
        "у сегментов нет длительности: {segments:?}"
    );
    assert!(
        segments.iter().all(|s| s.tokens.is_empty()),
        "облако внезапно отдало пословные метки — ветку деградации можно пересмотреть"
    );
    eprintln!("язык: {language:?}, сегментов: {}", segments.len());
}

/// Живой стрим саммари: SSE обычного OpenAI-вида, дельты доезжают.
#[tokio::test]
#[ignore = "ходит в сеть и тратит деньги: FORGE_MEET_POLZA_KEY + --ignored"]
async fn a_live_chat_completion_streams_deltas() {
    let Some(c) = live() else {
        eprintln!("FORGE_MEET_POLZA_KEY не задан — пропуск");
        return;
    };
    let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let sink = frames.clone();
    let on_delta = move |n: usize| sink.lock().unwrap().push(n);

    let out = stream_chat(
        &c,
        "Ты отвечаешь одним коротким абзацем по-русски.",
        "Перескажи в двух предложениях: встреча про сроки релиза, решили перенести на неделю.",
        &ChatOptions {
            model: "google/gemini-3.5-flash".to_string(),
            idle_timeout: Duration::from_secs(60),
            cancel: CancellationToken::new(),
        },
        Some(&on_delta),
    )
    .await
    .expect("POST /chat/completions");

    assert!(!out.text.is_empty(), "пустой ответ");
    assert!(out.frames > 1, "стрима не было: кадров {}", out.frames);
    assert!(out.interrupted.is_none(), "{:?}", out.interrupted);
    eprintln!("исполнитель: {:?}, кадров: {}", out.provider, out.frames);
    eprintln!("{}", out.text);
}
