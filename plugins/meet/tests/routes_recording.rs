//! Роуты управления записью на НАСТОЯЩЕМ роутере и НАСТОЯЩЕЙ базе.
//!
//! Chrome при этом не запускается ни разу: за `trait MeetBot` стоит
//! `ScriptedBot`, и весь пайплайн — дедуп, слоты, статусы, файлы — проверяется
//! детерминированно за миллисекунды. Ровно ради этого бот и спрятан за
//! трейтом.
//!
//! # Герметичность
//!
//! `cache_dir`, `db_path` и `models_dir` — во временном каталоге. Без этого
//! прогон писал бы в `~/.forge/meet`, где лежат записи реальных встреч
//! пользователя: чужие голоса, расшифровки и саммари.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forge_meet::bot::BotScript;
use forge_meet::config::MeetConfig;
use forge_meet::db::repo::meetings;
use forge_meet::db::Db;
use forge_meet::state::MeetState;
use forge_meet::status::MeetStatus;
use forge_meet::ScriptedBotFactory;
use serde_json::{json, Value};
use tower::ServiceExt;

const URL: &str = "https://telemost.yandex.ru/j/60497752704486";

struct Stand {
    state: Arc<MeetState>,
    app: axum::Router,
    _dir: tempfile::TempDir,
}

async fn stand_with(script: BotScript) -> Stand {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MeetConfig {
        config_path: dir.path().join("meet.toml"),
        cache_dir: dir.path().join("cache"),
        db_path: dir.path().join("cache/meet.db"),
        models_dir: dir.path().join("cache/models"),
        claude_cli_path: dir.path().join("no-such-claude"),
        ..MeetConfig::default()
    };
    let state = MeetState::new(cfg);
    let db = Db::open_memory().await.unwrap();
    db.migrate().await.unwrap();
    state.attach_db(Arc::new(db)).unwrap();
    state.set_bot_factory(Arc::new(ScriptedBotFactory::new(script)));

    let app = forge_meet::register_routes(axum::Router::new(), state.clone());
    Stand {
        state,
        app,
        _dir: dir,
    }
}

async fn stand() -> Stand {
    // Пауза при проигрывании: тесты, которые вмешиваются в ход записи,
    // иначе не успевали бы — сценарий доигрывает восемь секунд встречи за
    // миллисекунды.
    stand_with(BotScript {
        playback_delay_ms: 10,
        ..BotScript::two_speakers_overlapping(16_000, 20)
    })
    .await
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let code = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (code, json)
}

async fn get_raw(app: &axum::Router, uri: &str) -> (StatusCode, Vec<u8>, Option<String>) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let code = resp.status();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    (code, bytes.to_vec(), ctype)
}

async fn wait_status(state: &Arc<MeetState>, id: &str, want: MeetStatus) -> MeetStatus {
    for _ in 0..400 {
        let row = meetings::get(state, id).await.unwrap();
        if row.status == want || row.status.is_terminal() {
            return row.status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    meetings::get(state, id).await.unwrap().status
}

/// ДЕДУП ПО URL: два старта по одной ссылке дают РОВНО ОДНУ строку в базе, и
/// второй ответ несёт `attached: true`. Это не ошибка и не 409 — одну ссылку
/// кидают в двух чатах, и обе кнопки обязаны показывать одну запись.
#[tokio::test]
async fn two_starts_of_one_link_give_one_meeting_and_attached_true() {
    let s = stand().await;

    let (code, first) = post(
        &s.app,
        "/api/meet/meetings",
        json!({"url": URL, "plugin": "telegram", "chat_id": "7", "message_id": 100}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{first}");
    assert_eq!(first["attached"], false);
    let id = first["meeting_id"].as_str().unwrap().to_string();

    // Тот же URL из ДРУГОГО чата.
    let (code, second) = post(
        &s.app,
        "/api/meet/meetings",
        json!({"url": URL, "plugin": "vk", "chat_id": "42", "message_id": 900}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{second}");
    assert_eq!(second["attached"], true, "второй старт обязан присоединиться: {second}");
    assert_eq!(second["meeting_id"], id, "встреча обязана быть той же");

    // В базе ровно одна активная запись.
    let active = meetings::list_by_status(&s.state, &MeetStatus::ACTIVE).await.unwrap();
    assert_eq!(active.len(), 1, "дедуп не сработал: {active:?}");

    // И бейдж горит на ОБОИХ сообщениях.
    for (plugin, chat, msg) in [("telegram", "7", 100), ("vk", "42", 900)] {
        let key = forge_meet::SourceKey::new(plugin, chat, msg);
        let badge = s.state.badge_of(&key).await;
        assert!(badge.is_some(), "нет бейджа на {plugin}/{chat}/{msg}");
        assert_eq!(badge.unwrap().meeting_id, id);
    }
}

/// ТРЕТИЙ одновременный старт получает `409 busy` БЫСТРЕЕ 50 мс.
///
/// Быстро — это половина требования. Пока запрос висел бы в очереди, встреча
/// шла бы без бота, и «дождался слота» означало бы «вошёл, когда всё
/// кончилось».
#[tokio::test]
async fn the_third_simultaneous_start_is_refused_immediately() {
    let s = stand().await;
    assert_eq!(s.state.limits().recordings, 2, "лимит записи по умолчанию — два");

    for (n, room) in ["111", "222"].iter().enumerate() {
        let (code, body) = post(
            &s.app,
            "/api/meet/meetings",
            json!({"url": format!("https://telemost.yandex.ru/j/{room}")}),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "старт {n}: {body}");
    }

    let started = std::time::Instant::now();
    let (code, body) = post(
        &s.app,
        "/api/meet/meetings",
        json!({"url": "https://telemost.yandex.ru/j/333"}),
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "busy");
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "Busy пришёл за {elapsed:?} — запрос где-то ждал очереди"
    );
}

/// ЛЕСТНИЦА ОШИБОК: при неготовом Chrome битое тело даёт
/// `409 chrome_unavailable`, а НЕ `400`.
///
/// Иначе человек с неустановленным браузером чинил бы JSON.
#[tokio::test]
async fn a_broken_body_with_no_chrome_answers_chrome_unavailable_not_400() {
    let s = stand().await;
    // Пробы прошли, Chrome не найден — ровно то состояние, в котором стоит
    // машина без браузера.
    {
        let mut deps = s.state.deps.write().await;
        deps.probed = true;
        deps.chrome = forge_meet::DepBinary {
            name: "chrome".into(),
            found: Some(false),
            path: None,
            version: None,
            error: Some("не найден".into()),
        };
    }

    let (code, body) = post(&s.app, "/api/meet/meetings", json!({"нет": "ссылки"})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"], "chrome_unavailable",
        "лестница пропустила ступень зависимостей: {body}"
    );
}

/// Разбор параметров — ПОСЛЕДНЯЯ ступень: при готовом окружении битое тело
/// даёт честный 400.
#[tokio::test]
async fn with_everything_ready_a_broken_body_is_a_400() {
    let s = stand().await;
    for body in [
        json!({}),
        json!({"url": ""}),
        json!({"url": URL, "mode": "headless"}),
        json!({"url": URL, "plugin": "telegram"}),
    ] {
        let (code, resp) = post(&s.app, "/api/meet/meetings", body.clone()).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "тело {body}: {resp}");
        assert_eq!(resp["error"], "bad_request");
    }
}

/// Ссылка не на встречу — тоже 400, а не молчаливая запись «непонятно чего».
#[tokio::test]
async fn a_link_that_is_not_a_meeting_is_refused() {
    let s = stand().await;
    let (code, body) = post(
        &s.app,
        "/api/meet/meetings",
        json!({"url": "https://example.com/j/1"}),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
}

/// `stop` СОХРАНЯЕТ файлы и ведёт в пост-обработку.
#[tokio::test]
async fn stop_keeps_the_files_and_moves_to_transcoding() {
    let s = stand().await;
    let (_, body) = post(&s.app, "/api/meet/meetings", json!({"url": URL})).await;
    let id = body["meeting_id"].as_str().unwrap().to_string();
    wait_status(&s.state, &id, MeetStatus::Recording).await;
    // Ждём первый файл: статус `recording` появляется на событии старта
    // записи, а звук доезжает следующими кадрами — остановись мы раньше,
    // сохранять было бы нечего, и тест проверял бы пустоту.
    let row = meetings::get(&s.state, &id).await.unwrap();
    let first_wav = std::path::Path::new(&row.meeting_dir).join("raw/track-0.wav");
    // Ждём именно ЗВУК, а не файл: `WavWriter::create` кладёт сорокачетырёх-
    // байтовый заголовок сразу, и остановка по факту существования файла
    // ловила бы запись, в которой ещё нет ни одного сэмпла. Под нагрузкой
    // (полный прогон workspace) это выстреливает: тест падал на «записана
    // тишина», хотя тишины там не было — были одни заголовки.
    let mut have_sound = false;
    for _ in 0..400 {
        if first_wav.is_file() {
            if let Ok(samples) = forge_meet::ingest::wav::read_samples(&first_wav) {
                if samples.iter().any(|&x| x != 0) {
                    have_sound = true;
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(have_sound, "звук так и не пошёл: {}", first_wav.display());

    let (code, resp) = post(&s.app, &format!("/api/meet/meetings/{id}/stop"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{resp}");

    let status = wait_status(&s.state, &id, MeetStatus::Transcoding).await;
    assert_eq!(status, MeetStatus::Transcoding, "stop обязан вести в пост-обработку");

    let row = meetings::get(&s.state, &id).await.unwrap();
    let dir = std::path::Path::new(&row.meeting_dir);
    assert!(dir.join("manifest.json").is_file(), "манифест пропал");
    assert!(dir.join("raw").is_dir(), "stop выбросил звук");
    let wav = dir.join("raw/track-0.wav");
    assert!(wav.is_file(), "нет {}", wav.display());
    assert!(
        forge_meet::ingest::wav::read_samples(&wav).unwrap().iter().any(|&x| x != 0),
        "записана тишина"
    );
}

/// `cancel` ВЫБРАСЫВАЕТ данные. Это единственная пара команд, ради разницы
/// между которыми они и разделены.
#[tokio::test]
async fn cancel_throws_the_recording_away() {
    let s = stand().await;
    let (_, body) = post(&s.app, "/api/meet/meetings", json!({"url": URL})).await;
    let id = body["meeting_id"].as_str().unwrap().to_string();
    wait_status(&s.state, &id, MeetStatus::Recording).await;

    let row = meetings::get(&s.state, &id).await.unwrap();
    let dir = row.meeting_dir.clone();
    assert!(std::path::Path::new(&dir).is_dir());

    let (code, resp) = post(&s.app, &format!("/api/meet/meetings/{id}/cancel"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{resp}");

    let status = wait_status(&s.state, &id, MeetStatus::Cancelled).await;
    assert_eq!(status, MeetStatus::Cancelled);
    assert!(
        !std::path::Path::new(&dir).exists(),
        "cancel обязан выбросить каталог, а он на месте: {dir}"
    );
}

/// Команда несуществующей встрече — 404, а не 500 и не молчание.
#[tokio::test]
async fn commands_on_an_unknown_meeting_are_404() {
    let s = stand().await;
    for path in ["stop", "cancel", "confirm", "show-window"] {
        let (code, body) = post(&s.app, &format!("/api/meet/meetings/нет-такой/{path}"), json!({})).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{path}: {body}");
    }
    let (code, _, _) = get_raw(&s.app, "/api/meet/meetings/нет-такой/screenshot").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// Повторный `stop` уже обработанной встречи — `409 invalid_state`, а не
/// молчаливое ничего.
#[tokio::test]
async fn stopping_a_finished_meeting_is_an_invalid_state() {
    let s = stand().await;
    let (_, body) = post(&s.app, "/api/meet/meetings", json!({"url": URL})).await;
    let id = body["meeting_id"].as_str().unwrap().to_string();
    wait_status(&s.state, &id, MeetStatus::Transcoding).await;

    let (code, resp) = post(&s.app, &format!("/api/meet/meetings/{id}/stop"), json!({})).await;
    assert_eq!(code, StatusCode::CONFLICT, "{resp}");
    assert_eq!(resp["error"], "invalid_state");
}

/// АССИСТИРОВАННЫЙ ВХОД целиком через HTTP: старт, скриншот, показ окна,
/// подтверждение — и только потом запись.
#[tokio::test]
async fn the_assisted_flow_works_end_to_end_over_http() {
    let s = stand().await;
    let (code, body) = post(
        &s.app,
        "/api/meet/meetings",
        json!({"url": URL, "mode": "assisted"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["mode"], "assisted");
    let id = body["meeting_id"].as_str().unwrap().to_string();

    // Запись НЕ идёт: ждём человека.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let row = meetings::get(&s.state, &id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Launching, "запись пошла без человека");

    // «Что видит бот» — PNG, и он доступен именно сейчас.
    let (code, png, ctype) = get_raw(&s.app, &format!("/api/meet/meetings/{id}/screenshot")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(ctype.as_deref(), Some("image/png"));
    assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G'], "это не PNG");

    // Показ окна возвращает ФАКТИЧЕСКИЕ границы.
    let (code, shown) = post(
        &s.app,
        &format!("/api/meet/meetings/{id}/show-window"),
        json!({"visible": true}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{shown}");
    assert_eq!(shown["visible"], true);
    assert!(shown["left"].as_i64().unwrap() >= 0);

    let (code, hidden) = post(
        &s.app,
        &format!("/api/meet/meetings/{id}/show-window"),
        json!({"visible": false}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{hidden}");
    assert_eq!(
        hidden["left"], -930,
        "оконный сервер macOS подрезает координаты, и ответ обязан это показывать"
    );

    // «Я внутри — пиши».
    let (code, resp) = post(&s.app, &format!("/api/meet/meetings/{id}/confirm"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{resp}");

    let status = wait_status(&s.state, &id, MeetStatus::Transcoding).await;
    assert_eq!(status, MeetStatus::Transcoding, "после подтверждения запись обязана пойти");
}

/// Скриншот и показ окна ПОСЛЕ конца записи — 404: сеанса больше нет, и
/// врать про окно несуществующего браузера нельзя.
#[tokio::test]
async fn window_actions_stop_working_once_the_session_is_over() {
    let s = stand().await;
    let (_, body) = post(&s.app, "/api/meet/meetings", json!({"url": URL})).await;
    let id = body["meeting_id"].as_str().unwrap().to_string();
    wait_status(&s.state, &id, MeetStatus::Transcoding).await;

    let (code, _, _) = get_raw(&s.app, &format!("/api/meet/meetings/{id}/screenshot")).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = post(
        &s.app,
        &format!("/api/meet/meetings/{id}/show-window"),
        json!({}),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// Сеанс освобождает слот, и следующая запись начинается штатно. Без этого
/// после двух встреч плагин перестал бы принимать новые до перезапуска.
#[tokio::test]
async fn slots_come_back_after_the_recording_is_over() {
    let s = stand_with(BotScript::two_speakers_overlapping(16_000, 20)).await;
    for n in 0..4 {
        let (code, body) = post(
            &s.app,
            "/api/meet/meetings",
            json!({"url": format!("https://telemost.yandex.ru/j/{}", 1000 + n)}),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "старт {n}: {body}");
        let id = body["meeting_id"].as_str().unwrap().to_string();
        let status = wait_status(&s.state, &id, MeetStatus::Transcoding).await;
        assert_eq!(status, MeetStatus::Transcoding, "старт {n}");
        // Ждём, пока воркер отпустит сеанс (а с ним и слот).
        for _ in 0..200 {
            if s.state.sessions.get(&id).await.is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}
