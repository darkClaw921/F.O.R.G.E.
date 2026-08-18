//! `GET` / `PUT /api/meet/config` на НАСТОЯЩЕМ роутере и НАСТОЯЩЕМ файле.
//!
//! Здесь доказываются четыре вещи, каждая из которых ломается тихо:
//!
//! 1. `PUT` сохраняет АТОМАРНО и не оставляет мусора в каталоге;
//! 2. значения из переменных окружения помечены и НЕ ЗАТИРАЮТСЯ;
//! 3. `PUT` не трогает действующий конфиг процесса — идущие записи продолжают
//!    работать на старых значениях;
//! 4. записать путь к исполняемому файлу через этот роут НЕЛЬЗЯ.
//!
//! # ⚠ ЗАМОК НА ОКРУЖЕНИЕ БЕРЁТ КАЖДЫЙ ТЕСТ ФАЙЛА, а не только тот, что
//! переменные ставит
//!
//! `FORGE_MEET_*` — состояние ВСЕГО ПРОЦЕССА, а тесты одного бинаря идут
//! параллельно. Первая редакция брала замок только в тесте про env, и
//! остальные восемь падали через раз: они видели `bot_name` уже `env_locked`
//! и получали «сохранено 0 ключей» вместо ожидаемой ошибки разбора. Замок
//! обязан охватывать И установку переменной, И весь запрос, который её
//! читает.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forge_meet::state::MeetState;
use forge_meet::MeetConfig;
use serde_json::Value;
use tower::ServiceExt;

struct Stand {
    state: Arc<MeetState>,
    app: axum::Router,
    config_path: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

impl Stand {
    fn new(file: Option<&str>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = forge_meet::testing::hermetic_config(tmp.path());
        let config_path = cfg.config_path.clone();
        if let Some(raw) = file {
            std::fs::write(&config_path, raw).unwrap();
        }
        // Состояние собирается ИЗ ФАЙЛА: иначе `describe` показывал бы
        // «источник — файл», а значение — умолчание.
        let from_file = MeetConfig {
            ..MeetConfig::from_file(&config_path)
        };
        let cfg = MeetConfig {
            cache_dir: cfg.cache_dir.clone(),
            db_path: cfg.db_path.clone(),
            models_dir: cfg.models_dir.clone(),
            config_path: config_path.clone(),
            ..from_file
        };
        let state = MeetState::new(cfg);
        let app = forge_meet::register_routes(axum::Router::new(), state.clone());
        Stand {
            state,
            app,
            config_path,
            _tmp: tmp,
        }
    }

    async fn get(&self) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/meet/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        (code, serde_json::from_slice(&bytes).unwrap())
    }

    async fn put(&self, body: &str) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/meet/config")
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

    async fn delete_profile(&self) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/meet/profile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn file(&self) -> String {
        std::fs::read_to_string(&self.config_path).unwrap_or_default()
    }

    /// Файлы в каталоге конфига — доказательство атомарности.
    fn dir_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.config_path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    fn field<'a>(dto: &'a Value, key: &str) -> &'a Value {
        dto["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == key)
            .unwrap_or_else(|| panic!("в ответе нет поля {key}"))
    }
}

#[tokio::test]
async fn get_shows_the_value_and_where_it_came_from() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(Some("bot_name = \"Секретарь\"\n"));
    let (code, dto) = s.get().await;
    assert_eq!(code, StatusCode::OK);

    assert_eq!(dto["file_exists"], true);
    assert_eq!(dto["file_parses"], true);
    assert_eq!(dto["restart_required"], true);

    let bot = Stand::field(&dto, "bot_name");
    assert_eq!(bot["value"], "Секретарь");
    assert_eq!(bot["source"], "file");
    assert_eq!(bot["env_locked"], false);
    assert_eq!(bot["kind"], "text");
    assert_eq!(bot["group"], "bot");

    // Не заданное в файле — умолчание.
    assert_eq!(Stand::field(&dto, "retention_days")["source"], "default");

    // Только для чтения: пути и жёсткие константы.
    assert_eq!(dto["readonly"]["max_concurrent_whisper"], 1);
    assert_eq!(dto["readonly"]["audio_sample_rate"], 16_000);
    assert!(dto["readonly"]["models_dir"].as_str().unwrap().contains("models"));

    // Границы уезжают в UI, чтобы человек узнавал о них ДО сохранения.
    let alone = Stand::field(&dto, "alone_timeout_secs");
    assert_eq!(alone["min"], 5.0);
    assert_eq!(alone["max"], 3600.0);
    // У перечисления — список значений.
    assert_eq!(
        Stand::field(&dto, "window_mode")["choices"],
        serde_json::json!(["offscreen", "minimized", "visible", "headless"])
    );
}

/// `PUT` пишет АТОМАРНО и НЕ ТРОГАЕТ комментарии пользователя.
///
/// Пересборка файла из `toml::Table` стёрла бы объяснения, которые человек
/// писал руками, — и заметил бы он это в лучшем случае через месяц.
#[tokio::test]
async fn put_saves_atomically_and_keeps_the_users_comments() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(Some(
        "# у нас Teams зависает в лобби, поэтому две минуты\n\
         alone_timeout_secs = 120\n\
         bot_name = \"Старое\"\n",
    ));

    let (code, resp) = s
        .put(r#"{"values":{"bot_name":"Новое","retention_days":30}}"#)
        .await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["saved"],
        serde_json::json!(["bot_name", "retention_days"])
    );
    assert_eq!(resp["restart_required"], true);

    let raw = s.file();
    assert!(raw.contains("bot_name = \"Новое\""), "{raw}");
    assert!(raw.contains("retention_days = 30"), "{raw}");
    assert!(
        raw.contains("# у нас Teams зависает в лобби"),
        "комментарий пользователя стёрт: {raw}"
    );
    assert!(raw.contains("alone_timeout_secs = 120"), "{raw}");

    // Атомарность: временных остатков не осталось.
    assert_eq!(s.dir_entries(), vec!["meet.toml".to_string()]);

    // И файл читается обратно тем же разборщиком, что при старте.
    let back = MeetConfig::from_file(&s.config_path);
    assert_eq!(back.bot_name, "Новое");
    assert_eq!(back.retention_days, 30);
    assert_eq!(back.alone_timeout_secs, 120);
}

/// Файла может не быть вовсе — плагин ни разу не настраивали.
#[tokio::test]
async fn put_creates_the_file_when_there_is_none() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(None);
    let (_, dto) = s.get().await;
    assert_eq!(dto["file_exists"], false);

    let (code, _) = s.put(r#"{"values":{"summary_enabled":false}}"#).await;
    assert_eq!(code, StatusCode::OK);
    assert!(s.file().contains("summary_enabled = false"), "{}", s.file());
    assert!(!MeetConfig::from_file(&s.config_path).summary_enabled);
}

/// В ФАЙЛ УХОДИТ ПОЧИНЕННОЕ ЗНАЧЕНИЕ, а не присланное.
///
/// Иначе на диске осталось бы `alone_timeout_secs = 0`, при каждом старте
/// молча превращалось бы в 5, и человек читал бы в файле одно, а видел другое.
#[tokio::test]
async fn out_of_range_values_are_repaired_before_they_reach_the_file() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(None);
    let (code, resp) = s.put(r#"{"values":{"alone_timeout_secs":0}}"#).await;
    assert_eq!(code, StatusCode::OK, "{resp}");

    let fixes = resp["fixes"].as_array().unwrap();
    assert!(
        fixes.iter().any(|f| f.as_str().unwrap().contains("alone_timeout_secs")),
        "починка обязана называть себя: {resp}"
    );
    assert!(s.file().contains("alone_timeout_secs = 5"), "{}", s.file());
}

/// ЗНАЧЕНИЕ ИЗ ОКРУЖЕНИЯ ПОМЕЧЕНО И НЕ ЗАТИРАЕТСЯ.
///
/// Человек, запустивший devforge с `FORGE_MEET_BOT_NAME=…`, ждёт именно это
/// имя. Запись файлом дала бы «сохранил в интерфейсе, а работает старое» —
/// худший вид молчаливого расхождения.
#[tokio::test]
async fn env_values_are_marked_and_never_overwritten() {
    let _env = forge_meet::testing::env_guard();
    let key = "FORGE_MEET_BOT_NAME";
    let saved = std::env::var_os(key);
    // Переменная ставится ДО стенда: `Stand::new` берёт замок и читает файл, а
    // `describe` спрашивает окружение на каждый запрос.
    std::env::set_var(key, "Из окружения");

    let s = Stand::new(Some("bot_name = \"Из файла\"\n"));

    let (_, dto) = s.get().await;
    let bot = Stand::field(&dto, "bot_name");
    assert_eq!(bot["source"], "env");
    assert_eq!(bot["env_locked"], true);
    assert_eq!(bot["env"], "FORGE_MEET_BOT_NAME");

    let (code, resp) = s
        .put(r#"{"values":{"bot_name":"Через интерфейс","retention_days":7}}"#)
        .await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(resp["env_locked"], serde_json::json!(["bot_name"]));
    assert_eq!(resp["saved"], serde_json::json!(["retention_days"]));

    let raw = s.file();
    assert!(
        raw.contains("bot_name = \"Из файла\""),
        "переменная окружения не защитила ключ от перезаписи: {raw}"
    );
    assert!(raw.contains("retention_days = 7"), "{raw}");

    match saved {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

/// **ПУТИ К ИСПОЛНЯЕМЫМ ФАЙЛАМ ЧЕРЕЗ ЭТОТ РОУТ НЕ ЗАПИСЫВАЮТСЯ.**
///
/// Не «неудобно», а «PUT запускает произвольный бинарь»: `claude_cli_path` и
/// `chrome_path` — это то, что плагин выполняет.
#[tokio::test]
async fn no_executable_path_can_be_written_through_the_route() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(None);
    for key in [
        "claude_cli_path",
        "chrome_path",
        "whisper_path",
        "ffmpeg_path",
        "ffprobe_path",
        "cache_dir",
        "db_path",
        "models_dir",
        "max_concurrent_whisper",
    ] {
        let (code, body) = s
            .put(&format!(r#"{{"values":{{"{key}":"/bin/sh"}}}}"#))
            .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{key}");
        assert_eq!(body["error"], "bad_request", "{key}");
        assert!(
            body["message"].as_str().unwrap().contains(key),
            "{key}: ошибка обязана называть поле — {body}"
        );
    }
    assert_eq!(s.file(), "", "файл не должен был появиться вовсе");
}

/// Битый TOML — единственный случай, когда `PUT` отказывается работать.
/// Перезаписать его молча значило бы выбросить все остальные настройки разом.
#[tokio::test]
async fn a_broken_file_is_refused_not_silently_overwritten() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(Some("bot_name = = = сломано\nretention_days = 30\n"));

    let (_, dto) = s.get().await;
    assert_eq!(dto["file_parses"], false, "битый файл обязан быть виден");

    let (code, body) = s.put(r#"{"values":{"bot_name":"Новое"}}"#).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("почините"),
        "{body}"
    );
    assert!(
        s.file().contains("сломано"),
        "файл пользователя перезаписан: {}",
        s.file()
    );
}

/// `PUT` НЕ ТРОГАЕТ ДЕЙСТВУЮЩИЙ КОНФИГ ПРОЦЕССА.
///
/// Идущая запись читает `state.config` — таймауты автовыхода, имя бота, модель
/// whisper. Смени мы их на лету, у записи посреди встречи поменялся бы таймаут,
/// а расшифровка началась бы одной моделью и продолжилась другой.
#[tokio::test]
async fn saving_does_not_change_the_running_configuration() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(Some("bot_name = \"Старое\"\nalone_timeout_secs = 90\n"));
    let before = s.state.config.clone();

    let (code, resp) = s
        .put(r#"{"values":{"bot_name":"Новое","alone_timeout_secs":600}}"#)
        .await;
    assert_eq!(code, StatusCode::OK);

    assert_eq!(s.state.config, before, "конфиг процесса изменился на лету");
    assert_eq!(s.state.config.bot_name, "Старое");
    assert_eq!(s.state.config.alone_timeout_secs, 90);

    // И ответ говорит об этом прямо, а не оставляет UI догадываться.
    assert_eq!(resp["restart_required"], true);
    assert_eq!(resp["config"]["fields"][0]["value"], "Старое");

    // А в файле уже новое.
    assert_eq!(MeetConfig::from_file(&s.config_path).bot_name, "Новое");
}

#[tokio::test]
async fn broken_bodies_are_named() {
    let _env = forge_meet::testing::env_guard();
    let s = Stand::new(None);
    for body in [
        "не json",
        r#"{"values":{}}"#,
        r#"{"values":{"bot_name":7}}"#,
        r#"{"values":{"window_mode":"невидимое"}}"#,
        r#"{"values":{"summary_enabled":"да"}}"#,
    ] {
        let (code, resp) = s.put(body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(resp["error"], "bad_request", "{body}");
    }
    // И ни одна из этих попыток не создала файла.
    assert_eq!(s.file(), "");
}

// ---------------------------------------------------------------------------
// Ф8: постоянный профиль браузера
// ---------------------------------------------------------------------------

/// Настройки честно рассказывают о профиле — даже когда он ВЫКЛЮЧЕН.
///
/// «Выключил галку, а сессия Google так и лежит на диске» — ровно то
/// молчаливое расхождение, которого плагин избегает везде. Поэтому блок
/// профиля есть в ответе всегда, вместе с путём, правами и прямым текстом о
/// том, что именно там хранится.
#[tokio::test]
async fn the_config_always_reports_the_browser_profile() {
    let stand = Stand::new(None);
    let (code, body) = stand.get().await;
    assert_eq!(code, StatusCode::OK);

    let profile = &body["profile"];
    assert_eq!(profile["persist"], serde_json::json!(false), "по умолчанию профиль одноразовый");
    assert_eq!(profile["exists"], serde_json::json!(false));
    assert!(
        profile["path"].as_str().unwrap_or_default().ends_with("browser-profile"),
        "{profile}"
    );
    // Предупреждение приходит С СЕРВЕРА: оно описывает свойство ХРАНЕНИЯ, и
    // свой текст во фронте разъехался бы с поведением в первый же день.
    let warning = profile["warning"].as_str().unwrap_or_default();
    assert!(warning.contains("сесси"), "предупреждение молчит о сессии: {warning}");

    // И сама настройка редактируется из интерфейса — иначе включить её можно
    // было бы только правкой файла руками.
    let fields = body["fields"].as_array().unwrap();
    assert!(
        fields.iter().any(|f| f["key"] == "persist_profile"),
        "persist_profile не редактируется из интерфейса"
    );
}

/// Сброс удаляет каталог целиком и говорит новое состояние.
#[tokio::test]
async fn resetting_the_profile_removes_the_directory() {
    let stand = Stand::new(None);
    let dir = forge_meet::bot::persistent_profile_dir(&stand.state.config.cache_dir);
    forge_meet::bot::ensure_private_dir(&dir).unwrap();
    std::fs::write(dir.join("Cookies"), b"session").unwrap();
    assert!(dir.join("Cookies").exists());

    let (code, body) = stand.delete_profile().await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["removed"], serde_json::json!(true));
    assert_eq!(body["profile"]["exists"], serde_json::json!(false));
    assert!(!dir.exists(), "каталог профиля пережил сброс");

    // Повторный сброс — не ошибка, просто удалять нечего.
    let (code, body) = stand.delete_profile().await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["removed"], serde_json::json!(false));
}

/// ПРАВА КАТАЛОГА ВИДНЫ В ИНТЕРФЕЙСЕ.
///
/// Не украшение: внутри живая сессия аккаунта, и человек обязан видеть, что
/// каталог не читается посторонними, не бегая за `ls -l`.
#[cfg(unix)]
#[tokio::test]
async fn the_profile_permissions_are_visible_in_the_settings() {
    let stand = Stand::new(None);
    let dir = forge_meet::bot::persistent_profile_dir(&stand.state.config.cache_dir);
    forge_meet::bot::ensure_private_dir(&dir).unwrap();

    let (_, body) = stand.get().await;
    assert_eq!(body["profile"]["exists"], serde_json::json!(true));
    assert_eq!(
        body["profile"]["mode"], serde_json::json!("700"),
        "интерфейс показывает не те права, с которыми каталог создан"
    );
}
