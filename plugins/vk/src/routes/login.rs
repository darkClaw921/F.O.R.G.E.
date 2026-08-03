//! Роуты логина и captcha.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | POST | `/api/vk/login/token`  | принять вставленную адресную строку, сохранить токен, проверить его |
//! | POST | `/api/vk/login/verify` | перепроверить текущий токен (в том числе с ответом на captcha) |
//! | POST | `/api/vk/logout`       | стереть токен ЛОКАЛЬНО — в VK не ходим |
//! | POST | `/api/vk/captcha`      | отдать `{sid, key}` и продолжить |
//! | GET  | `/api/vk/captcha/:sid` | ПРОКСИ картинки captcha |
//!
//! Все четыре POST'а отвечают одним и тем же [`LoginStateDto`] — фронт
//! применяет его той же `renderLoginState`, что и снапшот `GET /api/vk/status`
//! и WS-кадр `login_state`. Три источника одной формы означают одну функцию
//! отрисовки вместо трёх ветвлений.
//!
//! # Инвариант секрета в HTTP-слое
//!
//! Тело `POST /api/vk/login/token` СОДЕРЖИТ СЕКРЕТ целиком. `TraceLayer` хоста
//! тела запросов не пишет, но полагаться только на это нельзя, поэтому:
//!
//! - хендлер не логирует ни тело, ни его длину, ни отдельные поля;
//! - [`crate::login::parse_token_input`] чистая и молчит по построению;
//! - в лог уходит ТОЛЬКО отпечаток (`SecretToken::fingerprint`) и только из
//!   `config::save_token_to`.
//!
//! Проверяется тестом `login_route_keeps_the_token_out_of_the_logs`
//! в `plugins/vk/tests/vk_stub.rs`.
//!
//! # Почему у captcha нет авто-решателя
//!
//! Единственный корректный повтор после кода 14 — ТОТ ЖЕ запрос плюс
//! `captcha_sid` + `captcha_key`, а «тот же запрос» на уровне [`VkClient`]
//! (`crate::api`) не восстанавливается: параметры остались у вызывающего.
//! Поэтому схема сквозная и без магии: клиент отдаёт `VkError::Api{code:14}`
//! наверх → REST отвечает `409 captcha_required` с `captcha_sid`/`captcha_img`
//! → каждый мутирующий роут принимает необязательные `captcha_sid` +
//! `captcha_key` и подставляет их как есть → повтор с фронта = тот же запрос
//! плюс два поля.
//!
//! # Почему картинка идёт через прокси
//!
//! `captcha_img` — адрес на серверах VK. Поставить его в `<img src>` значит
//! отправить браузер пользователя на api.vk.com со всеми его cookie и
//! засветить факт использования плагина в чужих логах. То же правило, что для
//! медиа (Ф7): наружу ходит только бэкенд.
//!
//! Прокси НЕ принимает URL параметром — только `sid`, по которому адрес
//! ищется в [`crate::state::VkState::captcha_imgs`]. Карта наполняется
//! исключительно из ответов VK, поэтому открытым прокси (SSRF) роут стать не
//! может.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::error::to_api_error;
use crate::api::methods::Captcha;
use crate::config::SecretToken;
use crate::login::{
    parse_token_input, verify_token, HttpBackend, LoginError, LoginFlow, LoginStateDto,
};
use crate::routes::error::ApiError;
use crate::state::VkState;

/// Потолок картинки captcha, байт.
///
/// Живая картинка VK — 3-6 КБ. 512 КБ с огромным запасом покрывают любой
/// разумный ответ и не дают ответу неограниченного размера осесть в памяти
/// процесса: тело читается кусками и обрывается при превышении.
pub const MAX_CAPTCHA_BYTES: usize = 512 * 1024;

/// Тело `POST /api/vk/login/token`.
///
/// **СОДЕРЖИТ СЕКРЕТ.** `Debug` не выводится намеренно: структура с голой
/// `String` внутри попала бы в отладочную печать целиком.
#[derive(Deserialize)]
pub struct TokenReq {
    /// То, что пользователь вставил в поле: адресная строка целиком, голый
    /// фрагмент либо сам токен. Разбор — [`parse_token_input`], и только на
    /// бэкенде.
    pub access_token: String,
}

/// Тело `POST /api/vk/login/verify` — необязательный ответ на captcha.
#[derive(Debug, Default, Deserialize)]
pub struct VerifyReq {
    /// `captcha_sid` из предыдущего отказа.
    #[serde(default)]
    pub captcha_sid: Option<String>,
    /// Символы, которые ввёл пользователь.
    #[serde(default)]
    pub captcha_key: Option<String>,
}

/// Тело `POST /api/vk/captcha`.
#[derive(Debug, Deserialize)]
pub struct CaptchaReq {
    /// `captcha_sid` из ошибки VK.
    pub sid: String,
    /// Символы с картинки.
    pub key: String,
}

/// Ответ `POST /api/vk/logout` — тот же `LoginStateDto`, что у остальных.
pub type LogoutResp = LoginStateDto;

/// Подсказка «где взять токен», уходящая в `GET /api/vk/status`.
///
/// Живёт НА БЭКЕНДЕ, а не в `index.html`, потому что правила выдачи scope
/// `messages` у VK меняются, а разметку пользователь получает из кэша PWA и
/// может не обновлять неделями. Один источник — один текст во всех вкладках.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginHint;

/// Текст подсказки для формы логина.
pub const TOKEN_HINT: &str = "Откройте страницу авторизации VK, разрешите доступ и СКОПИРУЙТЕ АДРЕСНУЮ СТРОКУ ЦЕЛИКОМ (она начинается с https://oauth.vk.ru/blank.html#access_token=…). Разбор сделает сервер: подойдёт и полный адрес, и его хвост, и сам токен. Токену нужны права messages и offline; обычным standalone-приложениям VK право messages не выдаёт, поэтому токен добывается под app_id клиента, который личные сообщения поддерживает. Значение хранится только локально — в vk.toml с правами 0600 — и уходит только на api.vk.com.";

/// Роуты логина и captcha.
pub fn router() -> Router<Arc<VkState>> {
    Router::new()
        .route("/api/vk/login/token", post(post_login_token))
        .route("/api/vk/login/verify", post(post_login_verify))
        .route("/api/vk/logout", post(post_logout))
        .route("/api/vk/captcha", post(post_captcha))
        .route("/api/vk/captcha/:sid", get(get_captcha_image))
}

/// `POST /api/vk/login/token` — принять вставленное значение.
///
/// Порядок действий важен и обратным быть не может:
///
/// 1. **разобрать** ([`parse_token_input`]) — мусор отвергается `400
///    invalid_input` ДО того, как что-то попадёт на диск;
/// 2. **сохранить** в `vk.toml` (атомарно, 0600, чужие ключи целы);
/// 3. **проверить** сетью ([`verify_token`]).
///
/// Сохранение ПЕРЕД проверкой — намеренно: `verify_token` строит клиент из
/// состояния, а пользователь, закрывший вкладку на середине проверки, всё
/// равно останется с введённым токеном и увидит результат после перезапуска.
/// Обратный порядок требовал бы второго пути «проверить, не сохраняя», то есть
/// второго места, где секрет живёт в памяти.
///
/// # Errors
///
/// `400 invalid_input` — строка не содержит токена; `500` — не удалось
/// записать конфиг (в этом случае токен НЕ применяется).
pub async fn post_login_token(
    State(state): State<Arc<VkState>>,
    // ↓ ТЕЛО СОДЕРЖИТ СЕКРЕТ: не логировать ни целиком, ни по полям.
    Json(req): Json<TokenReq>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let parsed = parse_token_input(&req.access_token).map_err(|e| match e {
        LoginError::BadInput { reason } => ApiError::bad_input(reason),
        other => ApiError::bad_input(other.to_string()),
    })?;

    if let Some(hint) = parsed.user_id {
        // `user_id` из адресной строки — ПОДСКАЗКА: источник истины ниже,
        // в `users.get`. Логируем его как признак «тот ли аккаунт», секретом
        // он не является.
        tracing::debug!(target: "forge_vk", user_id_hint = hint, "forge-vk: token pasted");
    }

    save_token(&state, &parsed.token).await?;
    let flow = verify_token(&state, &HttpBackend::new(client(&state).await?), None).await;
    Ok(Json(flow.to_dto()))
}

/// `POST /api/vk/login/verify` — перепроверить то, что уже сохранено.
///
/// Нужен трём сценариям: кнопка «повторить» на экране `disconnected`, ответ на
/// captcha и ручная проверка после того, как пользователь поправил права
/// токена на стороне VK.
///
/// # Errors
///
/// `409 not_authorized` — токена нет вовсе (проверять нечего).
pub async fn post_login_verify(
    State(state): State<Arc<VkState>>,
    body: Option<Json<VerifyReq>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let req = body.map(|Json(v)| v).unwrap_or_default();
    require_token(&state).await?;
    let captcha = captcha_from(req.captcha_sid, req.captcha_key);
    let backend = HttpBackend::new(client(&state).await?);
    let flow = verify_token(&state, &backend, captcha.as_ref()).await;
    Ok(Json(flow.to_dto()))
}

/// `POST /api/vk/logout` — отвязать аккаунт.
///
/// **В VK не ходим вовсе.** У пользовательского токена нет метода «отозвать
/// себя»: `auth.logout` работает по паре логин/пароль, а не по токену.
/// Единственное честное действие — стереть значение у себя, и делать вид, что
/// мы что-то отозвали на той стороне, было бы враньём в интерфейсе.
///
/// Идемпотентен: отсутствие токена и файла — не ошибка.
///
/// # Errors
///
/// `500` — не удалось переписать конфиг (например, каталог только для чтения).
pub async fn post_logout(
    State(state): State<Arc<VkState>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let mut cfg = state.effective_config().await;
    cfg.clear_token().map_err(|e| {
        tracing::warn!(target: "forge_vk", error = %e, "forge-vk: logout failed to rewrite the config");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            format!("could not remove the token from the config: {e}"),
        )
    })?;
    // Порядок обратный логину: сначала диск, потом память. Иначе неудачная
    // запись оставила бы пользователя «разлогиненным» до перезапуска, после
    // которого токен вернулся бы из файла.
    state.set_token(SecretToken::default()).await;
    state.forget_captchas().await;
    // Имена собеседников — тоже данные прошлого аккаунта: после отвязки
    // токена они не должны переживать её ни в памяти, ни в следующих ответах.
    state.peers.clear();
    state.set_login(LoginFlow::NotConfigured).await;
    tracing::info!(target: "forge_vk", "forge-vk: logged out, token removed locally");
    Ok(Json(state.login_state_dto().await))
}

/// `POST /api/vk/captcha` — отдать ответ на картинку.
///
/// Перепроверяет токен с подставленными `captcha_sid`/`captcha_key`. При
/// успехе состояние становится `authorized`, а `set_login` будит
/// LongPoll-воркера через `authorized_notify` — то есть realtime поднимается
/// без перезапуска процесса.
///
/// # Errors
///
/// `400 invalid_input` — пустые `sid`/`key`; `409 not_authorized` — токена
/// нет.
pub async fn post_captcha(
    State(state): State<Arc<VkState>>,
    Json(req): Json<CaptchaReq>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let Some(captcha) = captcha_from(Some(req.sid), Some(req.key)) else {
        return Err(ApiError::bad_input(
            "both `sid` and `key` are required to answer a captcha",
        ));
    };
    require_token(&state).await?;
    let backend = HttpBackend::new(client(&state).await?);
    let flow = verify_token(&state, &backend, Some(&captcha)).await;
    Ok(Json(flow.to_dto()))
}

/// `GET /api/vk/captcha/:sid` — байты картинки.
///
/// Адрес берётся ТОЛЬКО из запомненного ответа VK (см. шапку модуля).
/// Ответ помечается `Cache-Control: no-store`: картинка одноразовая, и
/// закэшированная браузером она показала бы прошлую задачу к новому `sid`.
///
/// # Errors
///
/// `404 no_media` — `sid` неизвестен или уже вытеснен; `502 vk_error` — VK
/// ответил не картинкой; `503 not_connected` — до VK не доехали.
pub async fn get_captcha_image(
    State(state): State<Arc<VkState>>,
    Path(sid): Path<String>,
) -> Result<Response, ApiError> {
    let Some(url) = state.captcha_img(&sid).await else {
        return Err(ApiError::no_media(
            "this captcha is unknown or has already expired; ask VK for a new one",
        ));
    };

    let client = client(&state).await?;
    let resp = client.get_stream(&url).await.map_err(|e| {
        tracing::warn!(target: "forge_vk", sid = %sid, error = %e, "forge-vk: captcha image fetch failed");
        to_api_error(&e)
    })?;

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    if !content_type.starts_with("image/") {
        // Вместо картинки приехала HTML-заглушка (captive portal, страница
        // ошибки VK). Отдать её браузеру означало бы показать пустую рамку без
        // объяснений.
        return Err(ApiError::vk_error(format!(
            "VK returned `{content_type}` instead of an image"
        )));
    }

    let bytes = read_capped(resp, MAX_CAPTCHA_BYTES).await?;
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    Ok((StatusCode::OK, headers, bytes).into_response())
}

// ---------------------------------------------------------------------------
// Вспомогательное.
// ---------------------------------------------------------------------------

/// Читает тело ответа кусками, обрываясь на превышении потолка.
///
/// Именно потоком, а не `resp.bytes()`: заголовок `Content-Length` подделывается
/// тривиально, и единственная надёжная граница — та, которую мы считаем сами.
async fn read_capped(resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, ApiError> {
    let mut out: Vec<u8> = Vec::with_capacity(4096);
    let mut resp = resp;
    loop {
        let chunk = resp.chunk().await.map_err(|e| {
            ApiError::not_connected(format!("VK connection dropped while reading the captcha: {e}"))
        })?;
        let Some(chunk) = chunk else { break };
        if out.len() + chunk.len() > cap {
            return Err(ApiError::vk_error(
                "the captcha image is implausibly large; refusing to buffer it",
            ));
        }
        out.extend_from_slice(&chunk);
    }
    if out.is_empty() {
        return Err(ApiError::vk_error("VK returned an empty captcha image"));
    }
    Ok(out)
}

/// Собирает [`Captcha`], если обе половины непустые.
///
/// Половина ответа бесполезна: VK отвергнет запрос кодом 100, а пользователь
/// увидит «неверные параметры» вместо «введите символы ещё раз».
fn captcha_from(sid: Option<String>, key: Option<String>) -> Option<Captcha> {
    let sid = sid?.trim().to_string();
    let key = key?.trim().to_string();
    if sid.is_empty() || key.is_empty() {
        return None;
    }
    Some(Captcha { sid, key })
}

/// Клиент API текущего токена.
///
/// Берётся из состояния, а не собирается на каждый запрос: вместе с клиентом
/// создаётся троттлер, и второй экземпляр означал бы второй бюджет 3 rps.
///
/// # Errors
///
/// `503 not_connected` — не удалось собрать `reqwest::Client`. Это отказ
/// среды (TLS-бэкенд, дескрипторы), а не пользователя, и повтор — разумный
/// совет.
async fn client(state: &Arc<VkState>) -> Result<Arc<crate::api::VkClient>, ApiError> {
    state.api_client().await.map_err(|e| {
        tracing::warn!(target: "forge_vk", error = %e, "forge-vk: could not build the API client");
        ApiError::not_connected(format!("could not build the VK client: {e}"))
    })
}

/// Отвергает запрос, если токен не настроен.
async fn require_token(state: &Arc<VkState>) -> Result<(), ApiError> {
    if state.has_token().await {
        return Ok(());
    }
    Err(ApiError::not_authorized(
        "no VK token is configured yet: paste the address bar contents first",
    ))
}

/// Записывает токен в конфиг и применяет его к состоянию.
///
/// В лог уходит только ОТПЕЧАТОК — это делает `config::save_token_to`, здесь
/// секрет не логируется вовсе.
async fn save_token(state: &Arc<VkState>, token: &SecretToken) -> Result<(), ApiError> {
    let mut cfg = state.config.clone();
    cfg.save_token(token).map_err(|e| {
        tracing::warn!(target: "forge_vk", error = %e, "forge-vk: could not persist the token");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            format!("could not save the token: {e}"),
        )
    })?;
    // В память — ТОЛЬКО после успешной записи: иначе неудачная запись дала бы
    // работающий до перезапуска токен, который после рестарта исчезает.
    // `set_token` заодно сбрасывает кэш клиента, поэтому следующая проверка
    // пойдёт уже с новым секретом.
    state.set_token(token.clone()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::config::VkConfig;

    const TOKEN: &str = "vk1.a.SECRET-CANARY_0123456789abcdef";

    /// Состояние на ИЗОЛИРОВАННОМ конфиге: и `vk.toml`, и кэш живут в
    /// tempdir'е, иначе прогон `cargo test` затирал бы токен пользователя.
    fn state_in(dir: &tempfile::TempDir, token: &str) -> Arc<VkState> {
        VkState::new(VkConfig {
            access_token: SecretToken::new(token),
            // Порт 1 закрыт всегда: сетевые ветки падают мгновенно и
            // детерминированно, без ожидания таймаута.
            base_url: "http://127.0.0.1:1/method".into(),
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            ..VkConfig::default()
        })
    }

    fn app(state: Arc<VkState>) -> Router {
        router().with_state(state)
    }

    async fn post(app: Router, uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let resp = app
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
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// Мусор отвергается `400 invalid_input` и НЕ доезжает до диска.
    #[tokio::test]
    async fn garbage_is_rejected_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir, "");
        let path = dir.path().join("vk.toml");

        for garbage in [
            "",
            "   ",
            "https://oauth.vk.ru/blank.html#error=access_denied",
            "https://oauth.vk.ru/blank.html",
        ] {
            let (status, json) = post(
                app(state.clone()),
                "/api/vk/login/token",
                serde_json::json!({ "access_token": garbage }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "вход {garbage:?}");
            assert_eq!(json["error"]["code"], "invalid_input");
            assert!(!path.exists(), "файл конфига не должен появиться: {garbage:?}");
            assert!(!state.has_token().await);
        }
    }

    /// Валидная адресная строка: токен уходит на диск с правами 0600, email в
    /// файл не попадает, состояние доходит до сетевой проверки (здесь она
    /// падает по транспорту — порт 1 закрыт — и даёт `disconnected`).
    #[tokio::test]
    async fn pasted_address_bar_is_parsed_saved_and_verified() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir, "");
        let path = dir.path().join("vk.toml");

        let (status, json) = post(
            app(state.clone()),
            "/api/vk/login/token",
            serde_json::json!({
                "access_token": format!(
                    "https://oauth.vk.ru/blank.html#access_token={TOKEN}&expires_in=0&user_id=105431859&email=victim%40example.com"
                )
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["state"], "disconnected",
            "сеть недоступна ⇒ экран повтора, а не «токен плохой»: {json}"
        );

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(TOKEN), "токен обязан лечь в конфиг");
        assert!(!body.contains("example.com"), "email в файле: {body}");
        assert!(!body.contains("expires_in ="), "лишние поля не пишем: {body}");
        assert_eq!(state.token().await.expose(), TOKEN);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "получили {mode:o}");
        }
    }

    /// Logout стирает токен и с диска, и из памяти; чужие ключи выживают,
    /// права остаются 0600, повтор — не ошибка.
    #[tokio::test]
    async fn logout_removes_only_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(
            &path,
            format!("access_token = \"{TOKEN}\"\nrate_limit_rps = 2\nsome_future_key = \"keep\"\n"),
        )
        .unwrap();
        let state = state_in(&dir, TOKEN);
        state.remember_captcha("239", "https://vk/c.php").await;

        let (status, json) = post(app(state.clone()), "/api/vk/logout", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["state"], "not_configured");
        assert!(!state.has_token().await);
        assert!(
            state.captcha_img("239").await.is_none(),
            "captcha прошлой сессии больше не наша"
        );

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains(TOKEN), "токен остался в файле: {body}");
        assert!(body.contains("some_future_key"), "чужой ключ пропал: {body}");
        assert!(body.contains("rate_limit_rps"), "{body}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "получили {mode:o}");
        }

        // Повторный logout идемпотентен.
        let (status, _) = post(app(state.clone()), "/api/vk/logout", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// Проверка и captcha без токена — `409 not_authorized`, а не попытка
    /// сходить в сеть с пустым секретом.
    #[tokio::test]
    async fn verify_and_captcha_require_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir, "");

        let (status, json) = post(
            app(state.clone()),
            "/api/vk/login/verify",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "not_authorized");

        let (status, json) = post(
            app(state.clone()),
            "/api/vk/captcha",
            serde_json::json!({"sid": "239", "key": "abc"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "not_authorized");
    }

    /// Половина ответа на captcha бесполезна: отвергаем ДО сетевого вызова,
    /// иначе пользователь увидел бы «неверные параметры» от VK.
    #[tokio::test]
    async fn half_filled_captcha_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir, TOKEN);
        for body in [
            serde_json::json!({"sid": "239", "key": "   "}),
            serde_json::json!({"sid": "", "key": "abc"}),
        ] {
            let (status, json) = post(app(state.clone()), "/api/vk/captcha", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(json["error"]["code"], "invalid_input");
        }
    }

    /// Прокси картинки не знает адресов, кроме запомненных: неизвестный `sid`
    /// это 404, а не поход в интернет.
    #[tokio::test]
    async fn captcha_proxy_refuses_unknown_sid() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(&dir, TOKEN);

        let resp = app(state)
            .oneshot(
                Request::builder()
                    .uri("/api/vk/captcha/239123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "no_media");
    }

    /// Пара `sid`/`key` собирается только целиком.
    #[test]
    fn captcha_pair_requires_both_halves() {
        assert!(captcha_from(None, Some("k".into())).is_none());
        assert!(captcha_from(Some("s".into()), None).is_none());
        assert!(captcha_from(Some(" ".into()), Some("k".into())).is_none());
        assert_eq!(
            captcha_from(Some(" 239 ".into()), Some(" abc ".into())),
            Some(Captcha {
                sid: "239".into(),
                key: "abc".into()
            }),
            "пробелы по краям срезаются — они из буфера обмена, а не из VK"
        );
    }

    /// Подсказка приходит с бэкенда и говорит главное: вставлять надо
    /// АДРЕСНУЮ СТРОКУ, а про право `messages` — что VK его standalone-
    /// приложениям не выдаёт.
    #[test]
    fn hint_explains_the_only_two_things_that_confuse_users() {
        assert!(TOKEN_HINT.contains("АДРЕСНУЮ СТРОКУ"), "{TOKEN_HINT}");
        assert!(TOKEN_HINT.contains("blank.html"), "{TOKEN_HINT}");
        assert!(TOKEN_HINT.contains("messages"), "{TOKEN_HINT}");
        assert!(TOKEN_HINT.contains("0600"), "{TOKEN_HINT}");
    }
}
