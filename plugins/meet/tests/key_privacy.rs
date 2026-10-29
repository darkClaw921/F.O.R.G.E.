//! **КЛЮЧ POLZA.AI НЕ ВЫХОДИТ НАРУЖУ.** Ни в argv, ни в логи, ни в ответы
//! роутов, ни в файл с чужими правами.
//!
//! # Почему это отдельный файл, а не строчка в тесте роутов
//!
//! Потому что утечка секрета — не поведение одного модуля, а свойство ВСЕЙ
//! поверхности: конфиг печатается в лог, `GET /api/meet/config` и
//! `GET /api/meet/status` рисуют настройки в интерфейсе, пайплайн запускает
//! ffmpeg и whisper с argv, видимым в `ps aux` любому пользователю машины, а
//! ошибки облака уезжают в журнал встречи. Дырой становится любая из этих
//! точек по отдельности, и проверять их порознь означает пропустить ту, про
//! которую забыли.
//!
//! Образец — тест приватности в `plugins/claude-runner/src/lib.rs`
//! (`build_args_omits_prompt_when_via_stdin`): берётся заведомо узнаваемая
//! строка и доказывается, что её нет ТАМ, ГДЕ ЕЁ ВИДНО ЧУЖОМУ.
//!
//! # Тест обязан краснеть при порче маскирования
//!
//! Проверка «в ответе есть маска» без проверки «в ответе нет ключа» прошла бы
//! и на коде, который печатает и то, и другое. Поэтому у каждой точки два
//! утверждения: секрета НЕТ и маска ЕСТЬ.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use forge_meet::cloud::{CloudClient, CloudError, SecretKey};
use forge_meet::config::{self, MeetConfig};
use forge_meet::state::MeetState;
use forge_meet::testing::hermetic_config;
use serde_json::Value;
use tower::ServiceExt;

/// Фиктивный ключ формы живого. **Настоящий ключ пользователя в репозиторий не
/// коммитится**: живые тесты берут его из `FORGE_MEET_POLZA_KEY`.
const SECRET: &str = "pza_TESTKEYtestkeyTESTKEY_neverreal";

/// Стенд: конфиг с ключом, настоящий роутер, конфиг-файл во временном каталоге.
struct Stand {
    state: Arc<MeetState>,
    app: axum::Router,
    config_path: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

impl Stand {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = hermetic_config(tmp.path());
        let config_path = base.config_path.clone();
        let cfg = MeetConfig {
            polza_api_key: SecretKey::new(SECRET),
            ..base
        };
        let state = MeetState::new(cfg);
        let app = forge_meet::register_routes(axum::Router::new(), state.clone());
        Self {
            state,
            app,
            config_path,
            _tmp: tmp,
        }
    }

    async fn json(&self, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

/// Ключа нет в тексте, а маска есть. Обе половины обязательны — см. шапку.
#[track_caller]
fn hides_the_key(what: &str, text: &str) {
    assert!(
        !text.contains(SECRET),
        "{what}: КЛЮЧ УТЁК\n{text}"
    );
    // Хвост ключа так же опасен, как ключ целиком: по нему его узнают.
    assert!(
        !text.contains("neverreal"),
        "{what}: утёк хвост ключа\n{text}"
    );
}

// ===========================================================================
// 1. Ответы роутов
// ===========================================================================

/// `GET /api/meet/config` показывает МАСКУ и только её.
#[tokio::test]
async fn the_config_route_shows_a_mask_and_never_the_key() {
    let stand = Stand::new();
    let (code, dto) = stand.json("GET", "/api/meet/config", None).await;
    assert_eq!(code, StatusCode::OK);

    let raw = serde_json::to_string(&dto).unwrap();
    hides_the_key("GET /api/meet/config", &raw);

    let field = dto["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "polza_api_key")
        .expect("поля ключа нет в настройках вовсе");
    assert_eq!(field["kind"], "secret", "ключ обязан быть отдельным видом");
    let shown = field["value"].as_str().unwrap();
    assert!(
        shown.contains('•') && !shown.is_empty(),
        "вместо маски показано {shown:?}"
    );
    assert!(
        shown.starts_with("pza_TEST"),
        "маска обязана давать узнать свой ключ, показано {shown:?}"
    );
}

/// `GET /api/meet/status` — тот же запрет. Роут показывает настройки целиком, и
/// «настроено ли облако» обязано быть признаком, а не значением.
#[tokio::test]
async fn the_status_route_never_carries_the_key() {
    let stand = Stand::new();
    let (code, dto) = stand.json("GET", "/api/meet/status", None).await;
    assert_eq!(code, StatusCode::OK);
    hides_the_key("GET /api/meet/status", &serde_json::to_string(&dto).unwrap());
}

/// `PUT` принимает НОВЫЙ ключ, кладёт в файл именно его, а в ответе — маску.
#[tokio::test]
async fn a_put_stores_the_real_key_and_answers_with_a_mask() {
    let _guard = forge_meet::testing::env_guard();
    let stand = Stand::new();
    let fresh = "pza_FRESHfreshFRESHfresh_written";
    let (code, resp) = stand
        .json(
            "PUT",
            "/api/meet/config",
            Some(&format!(r#"{{"values":{{"polza_api_key":"{fresh}"}}}}"#)),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    assert_eq!(resp["saved"], serde_json::json!(["polza_api_key"]));

    // В ОТВЕТЕ ключа нет.
    let raw = serde_json::to_string(&resp).unwrap();
    assert!(!raw.contains(fresh), "PUT вернул сам ключ\n{raw}");

    // В ФАЙЛЕ — настоящий ключ, а не маска: иначе облако молча перестало бы
    // работать после первого же сохранения настроек.
    let file = std::fs::read_to_string(&stand.config_path).unwrap();
    assert!(
        file.contains(fresh),
        "в файл записан не ключ:\n{file}"
    );
    assert!(!file.contains('•'), "в файл записана маска:\n{file}");
    assert_eq!(
        MeetConfig::from_file(&stand.config_path)
            .polza_api_key
            .expose(),
        fresh
    );

    // И ПРАВА 0600: ключ в файле, который читает вся машина, — утёкший ключ.
    #[cfg(unix)]
    assert_eq!(
        config::file_mode(&stand.config_path),
        "600",
        "конфиг с ключом обязан быть только для владельца"
    );
    #[cfg(unix)]
    assert!(!config::file_is_world_readable(&stand.config_path));
}

/// **Маска, присланная обратно, не имеет права стать ключом.**
///
/// Форма настроек показывает маску; человек правит соседнее поле и жмёт
/// «сохранить». Без этой проверки ключ затирался бы строкой из точек, а узнал
/// бы об этом человек только на следующей встрече — по отказу облака.
#[tokio::test]
async fn a_mask_sent_back_is_refused_and_the_key_survives() {
    let _guard = forge_meet::testing::env_guard();
    let stand = Stand::new();
    let mask = SecretKey::new(SECRET).mask();
    let (code, resp) = stand
        .json(
            "PUT",
            "/api/meet/config",
            Some(&format!(r#"{{"values":{{"polza_api_key":"{mask}"}}}}"#)),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{resp}");
    assert!(
        !stand.config_path.exists()
            || !std::fs::read_to_string(&stand.config_path)
                .unwrap()
                .contains('•'),
        "маска всё-таки уехала в файл"
    );
    // Действующий ключ на месте.
    assert_eq!(stand.state.config.polza_api_key.expose(), SECRET);
}

/// Пустая строка — законное «убрать ключ», а не ошибка.
#[tokio::test]
async fn an_empty_value_clears_the_key() {
    let _guard = forge_meet::testing::env_guard();
    let stand = Stand::new();
    let (code, resp) = stand
        .json(
            "PUT",
            "/api/meet/config",
            Some(r#"{"values":{"polza_api_key":""}}"#),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{resp}");
    let cfg = MeetConfig::from_file(&stand.config_path);
    assert!(cfg.polza_api_key.is_empty());
    assert!(!cfg.cloud_configured());
}

// ===========================================================================
// 2. Печать и логи
// ===========================================================================

/// `Debug` конфига, клиента и ошибок не печатает ключ.
///
/// Именно `Debug`: `tracing::debug!("{cfg:?}")` пишется не в момент заведения
/// секрета, а через полгода, в чужой правке — и тогда ключ уезжает в лог
/// целиком.
#[test]
fn nothing_that_prints_itself_prints_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MeetConfig {
        polza_api_key: SecretKey::new(SECRET),
        ..hermetic_config(dir.path())
    };
    hides_the_key("Debug конфига", &format!("{cfg:?}"));
    hides_the_key("Debug облачных настроек", &format!("{:?}", cfg.cloud()));
    hides_the_key("Display ключа", &format!("{}", cfg.polza_api_key));
    hides_the_key("Debug ключа", &format!("{:?}", cfg.polza_api_key));

    let client = CloudClient::new(&cfg.cloud()).expect("клиент с ключом обязан собраться");
    hides_the_key("Debug клиента", &format!("{client:?}"));

    // Ошибки уезжают в журнал встречи и кадром на фронт.
    for e in [
        CloudError::NotConfigured,
        CloudError::Transport("dns error".into()),
        CloudError::Timeout("60s".into()),
        CloudError::Status {
            status: 401,
            api_code: Some("UNAUTHORIZED".into()),
            message: "bad key".into(),
        },
        CloudError::Decode("eof".into()),
    ] {
        hides_the_key("Debug ошибки", &format!("{e:?}"));
        hides_the_key("Display ошибки", &format!("{e}"));
        hides_the_key("message ошибки", &e.message());
    }

    // Починки конфига уходят в лог дословно.
    let mut broken = MeetConfig {
        cloud_base_url: String::new(),
        cloud_stt_window_secs: 10_000,
        ..cfg.clone()
    };
    hides_the_key("починки конфига", &broken.validate_and_fix().join("\n"));
}

// ===========================================================================
// 3. argv
// ===========================================================================

/// **КЛЮЧА НЕТ В ARGV.** `ps aux` читает любой пользователь машины.
///
/// Облачный путь запускает ровно один внешний процесс — ffmpeg, который режет
/// запись на окна. Ключ уходит заголовком `Authorization`, и argv обязан быть
/// от него свободен целиком.
#[test]
fn no_external_process_ever_sees_the_key_in_argv() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MeetConfig {
        polza_api_key: SecretKey::new(SECRET),
        ..hermetic_config(dir.path())
    };

    let args = forge_meet::cloud::stt::build_window_args(
        &dir.path().join("mix.wav"),
        forge_meet::cloud::stt::Window {
            index: 0,
            start_ms: 0,
            len_ms: 90_000,
        },
        &dir.path().join("cloud/000.mp3"),
    );
    let joined = args.join(" ");
    hides_the_key("argv ffmpeg", &joined);
    assert!(
        args.iter().all(|a| !a.contains("pza_")),
        "в argv есть что-то похожее на ключ: {args:?}"
    );

    // Поля формы `multipart` — тоже не место для ключа: они видны в теле
    // запроса, которое пишут в лог отладочные прокси.
    let fields = forge_meet::cloud::stt::build_form_fields(&cfg.cloud_stt_model, "ru");
    hides_the_key("поля формы", &format!("{fields:?}"));

    // И тело запроса саммари.
    let body = forge_meet::cloud::summary::build_chat_body(
        &cfg.cloud_summary_model,
        "system",
        "user",
    );
    hides_the_key("тело chat/completions", &body.to_string());
}

// ===========================================================================
// 4. Маскирование само по себе
// ===========================================================================

/// Маска не восстанавливается до ключа, а короткий ключ прячется целиком.
#[test]
fn the_mask_never_reveals_enough_to_reconstruct_the_key() {
    let key = SecretKey::new(SECRET);
    let mask = key.mask();
    hides_the_key("маска", &mask);
    assert!(mask.contains('•'));
    assert_eq!(
        mask.chars().filter(|c| *c != '•').count(),
        14,
        "открытыми обязаны остаться ровно 8 первых и 6 последних символов: {mask}"
    );

    // Короткий ключ (опечатка, обрезка при копировании) прячется ЦЕЛИКОМ:
    // показать половину восьмисимвольной строки — значит показать ключ.
    let short = SecretKey::new("pza_short");
    assert!(short.mask().chars().all(|c| c == '•'), "{}", short.mask());

    // Пустой ключ — пустая маска, а не строка точек: «не настроено» и
    // «настроено, но не показываем» человек обязан различать с одного взгляда.
    assert!(SecretKey::default().mask().is_empty());

    // И то, что читает интерфейс, — это маска, а не значение. Именно этот
    // assert краснеет, если маскирование однажды выключат.
    let dir = tempfile::tempdir().unwrap();
    let cfg = MeetConfig {
        polza_api_key: key,
        ..hermetic_config(dir.path())
    };
    let shown = config::editable_value(&cfg, "polza_api_key").unwrap();
    assert_ne!(shown, serde_json::json!(SECRET));
    assert_eq!(shown, serde_json::json!(mask));
}
