//! REST-роуты статуса и логина: `/api/telegram/status`, `/login/*`, `/logout`.
//!
//! `GET /status` — единственный роут, который отвечает 200 в любом
//! состоянии: по нему фронтенд решает, показывать ли кнопку ✈ вообще
//! (роут существует ⇒ плагин включён флагом `--telegram`).
//!
//! ## Кто за что отвечает
//!
//! Хендлеры здесь — тонкий слой: они проверяют вход, обеспечивают наличие
//! MTProto-подключения и передают управление переходам автомата из
//! [`crate::login`]. Вся логика состояний (что можно из какого состояния,
//! куда девать токены) живёт там и покрыта unit-тестами без сети.
//!
//! ```text
//!   POST /login/start    {api_id?, api_hash?, phone}
//!        │  api_id+api_hash ⇒ save_credentials (atomic, 0600)
//!        │  нет рантайма    ⇒ build_runtime
//!        └─▶ login::request_code            → 200 {state:"code_sent", phone}
//!   POST /login/code     {code}
//!        └─▶ login::submit_code             → 200 authorized | password_required
//!   POST /login/password {password}
//!        └─▶ login::submit_password         → 200 authorized
//!   POST /logout
//!        └─▶ sign_out + drop runtime + rm session → 200 disconnected
//! ```
//!
//! ## FLOOD_WAIT
//!
//! `429 {error:{code:"flood_wait", retry_after_secs}}` НИКОГДА не
//! ретраится сервером (риск №2 плана: FLOOD_WAIT на логине измеряется
//! часами, автоповторы только продлевают блокировку). Ретрай инициирует
//! пользователь после countdown'а в UI.
//!
//! ## api_hash — секрет
//!
//! Он принимается только в теле `POST /login/start`, сразу уходит в
//! `telegram.toml` с правами `0600` и НИКОГДА не возвращается ни одним
//! GET-роутом и не пишется в логи (см. тест `status_never_leaks_api_hash`).

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::login::{self, LoginStateDto, MtBackend};
use crate::routes::error::ApiError;
use crate::runtime;
use crate::state::{AppCredentials, TelegramState};

/// Ответ `GET /api/telegram/status`.
///
/// `enabled` всегда `true`: роут существует только когда плагин включён,
/// поэтому сам факт ответа 200 уже означает «плагин активен». Поле
/// оставлено явным, чтобы фронт читал одну и ту же форму и не полагался
/// на HTTP-статус.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDto {
    /// Плагин зарегистрирован в этом процессе.
    pub enabled: bool,
    /// Состояние логина (плоско: `state`, `phone`, `hint`, `error`, `user`).
    #[serde(flatten)]
    pub login: LoginStateDto,
}

/// Тело `POST /api/telegram/login/start`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoginStartReq {
    /// Телефон в международном формате.
    #[serde(default)]
    pub phone: Option<String>,
    /// `api_id` приложения — если пользователь вводит креды прямо в wizard'е.
    #[serde(default)]
    pub api_id: Option<i32>,
    /// `api_hash` приложения.
    #[serde(default)]
    pub api_hash: Option<String>,
}

/// Тело `POST /api/telegram/login/code`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoginCodeReq {
    /// Код из Telegram.
    #[serde(default)]
    pub code: Option<String>,
}

/// Тело `POST /api/telegram/login/password`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoginPasswordReq {
    /// Пароль 2FA (cloud password).
    #[serde(default)]
    pub password: Option<String>,
}

/// Роутер статуса и логина.
pub fn router() -> Router<Arc<TelegramState>> {
    Router::new()
        .route("/api/telegram/status", get(get_status))
        .route("/api/telegram/login/start", post(post_login_start))
        .route("/api/telegram/login/code", post(post_login_code))
        .route("/api/telegram/login/password", post(post_login_password))
        .route("/api/telegram/logout", post(post_logout))
}

/// `GET /api/telegram/status` → 200 всегда.
pub async fn get_status(State(state): State<Arc<TelegramState>>) -> Json<StatusDto> {
    Json(StatusDto {
        enabled: true,
        login: state.login_state_dto().await,
    })
}

/// Собирает сетевой бэкенд логина из активного подключения.
///
/// # Errors
///
/// `409 invalid_state`, если кредов нет; `503 not_connected`, если нет
/// живого MTProto-подключения (шаги логина без него бессмысленны).
async fn backend_for(state: &Arc<TelegramState>) -> Result<MtBackend, ApiError> {
    let creds = state.credentials().await.ok_or_else(|| {
        ApiError::invalid_state(
            "api_id/api_hash are not configured — save application credentials first",
        )
    })?;
    let client = state.client().await.ok_or_else(|| {
        ApiError::not_connected(
            "there is no active MTProto connection — start the login again to reconnect",
        )
    })?;
    Ok(MtBackend::new(client, creds.api_hash))
}

/// Поднимает MTProto-подключение, если его ещё нет.
///
/// При неудаче переводит автомат в `Disconnected{error}` — иначе UI
/// показывал бы «всё хорошо», хотя соединения нет.
///
/// # Errors
///
/// `503 not_connected` с текстом причины (занятый lock сессии, битый
/// файл сессии, отсутствие кредов).
async fn ensure_runtime(state: &Arc<TelegramState>) -> Result<(), ApiError> {
    if state.has_runtime().await {
        return Ok(());
    }
    let cfg = state.effective_config().await;
    match runtime::build_runtime(&cfg).await {
        Ok(rt) => {
            state.set_runtime(rt).await;
            Ok(())
        }
        Err(e) => {
            let message = e.to_string();
            login::on_auth_invalidated(&state.clone(), message.clone()).await;
            Err(ApiError::not_connected(message))
        }
    }
}

/// `POST /api/telegram/login/start`.
///
/// Три шага в одном роуте — так задумано: wizard отправляет креды и
/// телефон одной формой, и для пользователя это одно действие
/// «подключиться».
pub async fn post_login_start(
    State(state): State<Arc<TelegramState>>,
    payload: Option<Json<LoginStartReq>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let req = payload.map(|Json(r)| r).unwrap_or_default();

    let phone = req.phone.unwrap_or_default();
    let phone = phone.trim().to_string();
    if phone.is_empty() {
        return Err(ApiError::bad_input("phone is required"));
    }

    // 1. Креды из тела (первый проход wizard'а) — сохранить и применить.
    let api_hash = req.api_hash.map(|h| h.trim().to_string());
    let has_creds_now = state.has_credentials().await;
    match (req.api_id, api_hash) {
        (Some(api_id), Some(api_hash)) if !api_hash.is_empty() => {
            if api_id <= 0 {
                return Err(ApiError::bad_input(
                    "api_id must be a positive number from my.telegram.org",
                ));
            }
            let mut cfg = state.config.clone();
            cfg.save_credentials(api_id, &api_hash).map_err(|e| {
                ApiError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "config_write_failed",
                    format!("failed to save credentials: {e}"),
                )
            })?;
            state
                .set_credentials(AppCredentials { api_id, api_hash })
                .await;
            // Переход автомата NotConfigured → Disconnected: креды есть,
            // логин стал возможен. Без него `GET /status` продолжал бы
            // отдавать not_configured, и UI показывал бы первый шаг
            // wizard'а как единственно доступный.
            if !state.login.lock().await.is_configured() {
                state
                    .set_login(crate::login::LoginFlow::Disconnected { error: None })
                    .await;
            }
            // Старое подключение использует прежний api_id — оно больше не
            // годится, а его lock мешает поднять новое.
            state.clear_runtime().await;
        }
        // Заполнена только половина формы: молча игнорировать нельзя —
        // пользователь думал, что вводит креды.
        (Some(_), None) | (None, Some(_)) if !has_creds_now => {
            return Err(ApiError::bad_input(
                "both api_id and api_hash are required (get them at my.telegram.org)",
            ));
        }
        _ => {}
    }

    if !state.has_credentials().await {
        return Err(ApiError::invalid_state(
            "api_id/api_hash are not configured — save application credentials first",
        ));
    }

    // 2. Соединение и запрос кода.
    ensure_runtime(&state).await?;
    let backend = backend_for(&state).await?;
    let dto = login::request_code(&state, &backend, &phone).await?;
    Ok(Json(dto))
}

/// `POST /api/telegram/login/code`.
pub async fn post_login_code(
    State(state): State<Arc<TelegramState>>,
    payload: Option<Json<LoginCodeReq>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let code = payload
        .map(|Json(r)| r)
        .unwrap_or_default()
        .code
        .unwrap_or_default();
    if code.trim().is_empty() {
        return Err(ApiError::bad_input("code is required"));
    }
    let backend = backend_for(&state).await?;
    let dto = login::submit_code(&state, &backend, code.trim()).await?;
    Ok(Json(dto))
}

/// `POST /api/telegram/login/password`.
pub async fn post_login_password(
    State(state): State<Arc<TelegramState>>,
    payload: Option<Json<LoginPasswordReq>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let password = payload
        .map(|Json(r)| r)
        .unwrap_or_default()
        .password
        .unwrap_or_default();
    if password.is_empty() {
        return Err(ApiError::bad_input("password is required"));
    }
    let backend = backend_for(&state).await?;
    // Пароль НЕ триммится: пробелы могут быть его частью.
    let dto = login::submit_password(&state, &backend, &password).await?;
    Ok(Json(dto))
}

/// `POST /api/telegram/logout`.
///
/// Выход всегда успешен с точки зрения UI: пользователь нажал «выйти», и
/// локальное состояние обязано это отразить, даже если Telegram не
/// ответил. Порядок важен:
///
/// 1. `auth.logOut` (best-effort, пока соединение ещё живо);
/// 2. закрыть рантайм — освобождается lock файла сессии;
/// 3. удалить файл сессии (иначе следующий старт «восстановит» отозванную
///    авторизацию и будет получать `AUTH_KEY_UNREGISTERED`);
/// 4. перевести автомат в `Disconnected`/`NotConfigured`.
pub async fn post_logout(
    State(state): State<Arc<TelegramState>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let backend = backend_for(&state).await.ok();
    let dto = login::sign_out(
        &state,
        backend.as_ref().map(|b| b as &dyn login::LoginBackend),
    )
    .await;

    state.clear_runtime().await;
    remove_session_files(&state.config.session_path);

    Ok(Json(dto))
}

/// Удаляет файл сессии и его SQLite-спутники (`-wal`, `-shm`, `-journal`).
///
/// Ошибки логируются, но не считаются провалом logout'а: пользователь уже
/// разлогинен локально, а без auth-key внутри файл ничего не даёт.
fn remove_session_files(session_path: &std::path::Path) {
    let mut removed = false;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = if suffix.is_empty() {
            session_path.to_path_buf()
        } else {
            let mut name = session_path.as_os_str().to_os_string();
            name.push(suffix);
            std::path::PathBuf::from(name)
        };
        match std::fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                target: "forge_telegram",
                path = %path.display(),
                error = %e,
                "forge-telegram: could not remove session file on logout"
            ),
        }
    }
    if removed {
        tracing::info!(
            target: "forge_telegram",
            path = %session_path.display(),
            "forge-telegram: session file removed on logout"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;
    use crate::login::{LoginFlow, TgUserDto};

    fn cfg_in(dir: &std::path::Path) -> TelegramConfig {
        TelegramConfig {
            session_path: dir.join("telegram.session"),
            cache_dir: dir.join("cache"),
            prompts_path: dir.join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        }
    }

    fn user() -> TgUserDto {
        TgUserDto {
            id: "1".into(),
            username: Some("forge".into()),
            first_name: Some("Forge".into()),
            phone: Some("+79990000000".into()),
        }
    }

    #[tokio::test]
    async fn status_serializes_flat_login_state() {
        let state = TelegramState::new(TelegramConfig::default());
        let Json(dto) = get_status(State(state)).await;
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["enabled"], true);
        assert_eq!(json["state"], "not_configured");
        assert!(
            json.get("login").is_none(),
            "login должен быть flatten: {json}"
        );
    }

    /// `api_hash` — секрет: он не должен появляться ни в одном ответе.
    #[tokio::test]
    async fn status_never_leaks_api_hash() {
        let cfg = TelegramConfig {
            api_id: Some(42),
            api_hash: Some("super-secret-hash".into()),
            ..TelegramConfig::default()
        };
        let state = TelegramState::new(cfg);
        state.set_login(LoginFlow::Authorized { user: user() }).await;

        let Json(dto) = get_status(State(state)).await;
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("super-secret-hash"), "{json}");
        assert!(!json.contains("api_hash"), "{json}");
        assert!(json.contains("\"state\":\"authorized\""), "{json}");
    }

    #[tokio::test]
    async fn login_start_requires_phone() {
        let state = TelegramState::new(TelegramConfig::default());
        let err = post_login_start(State(state.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");

        let err = post_login_start(
            State(state),
            Some(Json(LoginStartReq {
                phone: Some("   ".into()),
                ..Default::default()
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");
    }

    #[tokio::test]
    async fn login_start_without_credentials_reports_invalid_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_start(
            State(state),
            Some(Json(LoginStartReq {
                phone: Some("+79990000000".into()),
                ..Default::default()
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");
        assert!(err.body.message.contains("api_id/api_hash"));
    }

    #[tokio::test]
    async fn login_start_rejects_half_filled_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_start(
            State(state),
            Some(Json(LoginStartReq {
                phone: Some("+79990000000".into()),
                api_id: Some(42),
                api_hash: None,
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");
        assert!(err.body.message.contains("my.telegram.org"));
    }

    #[tokio::test]
    async fn login_start_rejects_non_positive_api_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_start(
            State(state),
            Some(Json(LoginStartReq {
                phone: Some("+79990000000".into()),
                api_id: Some(0),
                api_hash: Some("hash".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");
        assert!(err.body.message.contains("api_id"));
    }

    /// Права `0600` и read-modify-write проверяются на самом примитиве:
    /// хендлер пишет в дефолтный конфиг пользователя, его в тестах
    /// трогать нельзя.
    #[test]
    fn save_credentials_primitive_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("telegram.toml");
        let mut cfg = cfg_in(dir.path());
        cfg.save_credentials_to(&cfg_path, 42, "hash").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&cfg_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(cfg.api_id, Some(42));
    }

    /// С кредами `login/start` поднимает рантайм и доходит до Telegram.
    ///
    /// Тест намеренно терпим к окружению: без сети это `not_connected`,
    /// с сетью — `invalid_input` (`API_ID_INVALID`, ведь api_id=42
    /// выдуман), в худшем случае `flood_wait`. Проверяется главное: ни
    /// одного 500 и ни одной паники на пути «креды → рантайм → RPC →
    /// контракт ошибок».
    #[tokio::test]
    async fn login_start_with_credentials_reports_honest_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_in(dir.path());
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        let state = TelegramState::new(cfg);
        assert!(state.has_credentials().await);

        let err = post_login_start(
            State(state.clone()),
            Some(Json(LoginStartReq {
                phone: Some("+79990000000".into()),
                ..Default::default()
            })),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err.body.code.as_str(),
                "not_connected" | "flood_wait" | "invalid_input"
            ),
            "неожиданный код {}: {}",
            err.body.code,
            err.body.message
        );
        assert_ne!(
            err.status,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "путь логина не должен отдавать 500: {}",
            err.body.message
        );
        // Соединение ленивое, поэтому рантайм поднялся: убеждаемся, что он
        // корректно закрывается и отпускает lock сессии.
        state.clear_runtime().await;
    }

    #[tokio::test]
    async fn code_and_password_require_connection() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_in(dir.path());
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        let state = TelegramState::new(cfg);

        let err = post_login_code(
            State(state.clone()),
            Some(Json(LoginCodeReq {
                code: Some("12345".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");

        let err = post_login_password(
            State(state),
            Some(Json(LoginPasswordReq {
                password: Some("pw".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "not_connected");
    }

    #[tokio::test]
    async fn code_and_password_validate_input_before_touching_state() {
        let state = TelegramState::new(TelegramConfig::default());

        let err = post_login_code(State(state.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");
        let err = post_login_password(State(state.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");

        let err = post_login_code(
            State(state),
            Some(Json(LoginCodeReq {
                code: Some("  ".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");
    }

    /// Без кредов шаги логина отвечают `invalid_state` (а не 503):
    /// подключаться просто не к чему.
    #[tokio::test]
    async fn steps_without_credentials_report_invalid_state() {
        let state = TelegramState::new(TelegramConfig::default());
        let err = post_login_code(
            State(state),
            Some(Json(LoginCodeReq {
                code: Some("12345".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");
    }

    /// Порядок проверок в `login/start`: телефон валидируется РАНЬШЕ
    /// сохранения кредов. Отклонённый запрос не должен оставлять следов —
    /// иначе форма без телефона, но с кредами, молча записала бы их в
    /// telegram.toml, и пользователь не понял бы, откуда они взялись.
    #[tokio::test]
    async fn rejected_start_without_phone_does_not_persist_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_start(
            State(state.clone()),
            Some(Json(LoginStartReq {
                phone: None,
                api_id: Some(42),
                api_hash: Some("real-hash".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");
        assert!(
            !state.has_credentials().await,
            "креды из отклонённого запроса не должны сохраняться"
        );
    }

    /// `api_hash` из одних пробелов НИКОГДА не сохраняется как креды:
    /// пустой hash сделал бы все последующие логины загадочно битыми.
    /// Пользователь получает ошибку, а состояние остаётся ненастроенным.
    #[tokio::test]
    async fn blank_api_hash_is_never_saved_as_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_start(
            State(state.clone()),
            Some(Json(LoginStartReq {
                phone: Some("+79990000000".into()),
                api_id: Some(42),
                api_hash: Some("   ".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");
        assert!(
            !state.has_credentials().await,
            "пробельный api_hash не должен стать кредами"
        );
    }

    /// Код, скопированный из сообщения Telegram, часто приезжает с
    /// неразрывными пробелами (U+00A0, U+2007). `trim()` обязан считать их
    /// пустотой: это «пустой ввод», а не код из экзотических символов.
    #[tokio::test]
    async fn code_of_unicode_whitespace_is_rejected_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_code(
            State(state),
            Some(Json(LoginCodeReq {
                code: Some("\u{00A0}\u{2007}\u{2009}".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");
    }

    /// Пароль 2FA НЕ триммится (пробелы — законная часть пароля), поэтому
    /// пароль из одного пробела — это ПОПЫТКА входа, а не «пустой ввод»:
    /// запрос обязан пройти дальше валидации (здесь — упереться в отсутствие
    /// кредов, `invalid_state`, а не `invalid_input`).
    #[tokio::test]
    async fn whitespace_only_password_is_a_real_password_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg_in(dir.path()));
        let err = post_login_password(
            State(state),
            Some(Json(LoginPasswordReq {
                password: Some(" ".into()),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.body.code, "invalid_state",
            "пробельный пароль не должен отсекаться как пустой: {}",
            err.body.message
        );
    }

    /// SQLite в WAL-режиме оставляет рядом с сессией `-wal`/`-shm`
    /// (а старый journal-режим — `-journal`). Оставшийся `-wal` при
    /// следующем старте «воскресил» бы удалённую авторизацию, поэтому
    /// logout обязан убирать всех спутников, а не только основной файл.
    #[tokio::test]
    async fn logout_removes_sqlite_companion_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_in(dir.path());
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        let session = cfg.session_path.clone();

        let mut companions = Vec::new();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = session.as_os_str().to_os_string();
            name.push(suffix);
            let path = std::path::PathBuf::from(name);
            std::fs::write(&path, b"x").unwrap();
            companions.push(path);
        }

        let state = TelegramState::new(cfg);
        state.set_login(LoginFlow::Authorized { user: user() }).await;
        let Json(dto) = post_logout(State(state)).await.unwrap();
        assert_eq!(dto.state, "disconnected");
        for path in companions {
            assert!(!path.exists(), "{} должен быть удалён", path.display());
        }
    }

    /// Неудаляемый файл сессии (здесь — каталог на его месте, в жизни — нет
    /// прав) не превращает logout в ошибку: локально пользователь уже вышел,
    /// а проблема уходит в лог, не в UI.
    #[tokio::test]
    async fn logout_survives_undeletable_session_path() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_in(dir.path());
        let session = cfg.session_path.clone();
        std::fs::create_dir(&session).unwrap();

        let state = TelegramState::new(cfg);
        let Json(dto) = post_logout(State(state)).await.unwrap();
        assert_eq!(dto.state, "not_configured");
        assert!(session.exists(), "каталог не мог быть удалён remove_file'ом");
    }

    /// logout из любого состояния успешен, приводит состояние в
    /// «не авторизован» и удаляет файл сессии.
    #[tokio::test]
    async fn logout_is_idempotent_and_removes_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_in(dir.path());
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        let session = cfg.session_path.clone();
        std::fs::write(&session, b"SQLite format 3\0").unwrap();

        let state = TelegramState::new(cfg);
        state.set_login(LoginFlow::Authorized { user: user() }).await;

        let Json(dto) = post_logout(State(state.clone())).await.unwrap();
        assert_eq!(dto.state, "disconnected");
        assert!(!session.exists(), "файл сессии должен быть удалён");
        assert!(!state.is_authorized().await);

        // Повторный logout не падает и не меняет картину.
        let Json(dto) = post_logout(State(state)).await.unwrap();
        assert_eq!(dto.state, "disconnected");

        // Без кредов — not_configured.
        let unconfigured = TelegramState::new(cfg_in(dir.path()));
        let Json(dto) = post_logout(State(unconfigured)).await.unwrap();
        assert_eq!(dto.state, "not_configured");
    }
}
