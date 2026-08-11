//! Роуты подключения и отвязки портала.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | POST | `/api/bitrix/login/webhook` | принять вставленный URL, сохранить его 0600 и проверить |
//! | POST | `/api/bitrix/logout`       | стереть вебхук ЛОКАЛЬНО — на портал не ходим |
//!
//! Оба отвечают одним и тем же [`LoginStateDto`] — той же формы, что снапшот
//! `GET /api/bitrix/status` и WS-кадр `login_state`. Три источника одной формы
//! означают ОДНУ функцию отрисовки во фронте вместо трёх ветвлений.
//!
//! # Инвариант секрета в HTTP-слое
//!
//! Тело `POST /api/bitrix/login/webhook` СОДЕРЖИТ СЕКРЕТ целиком: у Bitrix24
//! вебхук — это весь URL. `TraceLayer` хоста тела запросов не пишет, но
//! полагаться только на это нельзя, поэтому:
//!
//! - хендлер не логирует ни тело, ни его длину, ни отдельные поля;
//! - [`parse_webhook_input`] чистая и молчит по построению;
//! - в лог уходит ТОЛЬКО отпечаток ([`crate::config::WebhookUrl::fingerprint`])
//!   и только из `config::save_webhook_to`;
//! - ответ роута — обычный `LoginStateDto`, в котором нет ни кода, ни хоста.
//!
//! # Порядок действий при подключении обратным быть не может
//!
//! 1. **разобрать** — мусор отвергается `400 invalid_input` и состоянием
//!    `invalid_url` ДО того, как что-то попадёт на диск и в сеть;
//! 2. **сохранить** в `bitrix.toml` (атомарно, 0600, чужие ключи целы);
//! 3. **применить к состоянию** — [`crate::state::B24State::set_webhook`]
//!    заодно сбрасывает кэш клиента, то есть и его троттлер;
//! 4. **проверить** сетью — `profile` → `scope`.
//!
//! Сохранение ПЕРЕД проверкой намеренно: проверка строит клиент из состояния, а
//! пользователь, закрывший вкладку на середине, всё равно останется с
//! введённым вебхуком и увидит результат после перезапуска. Обратный порядок
//! потребовал бы второго пути «проверить, не сохраняя» — то есть второго места,
//! где секрет живёт в памяти.
//!
//! # Почему logout ничего не отзывает на портале
//!
//! Отозвать входящий вебхук через REST невозможно: он живёт, пока владелец не
//! удалит его в интерфейсе Битрикс24 (Разработчикам → Другое → Входящий
//! вебхук). Мы стираем его у себя и говорим об этом прямо в UI — делать вид,
//! что «отвязали», значит врать пользователю о состоянии его портала.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::config::{clear_webhook_to, save_webhook_to};
use crate::login::{
    apply_login_error, parse_webhook_input, verify_webhook, HttpBackend, LoginFlow, LoginStateDto,
};
use crate::routes::error::ApiError;
use crate::state::B24State;

/// Тело `POST /api/bitrix/login/webhook`.
///
/// **СОДЕРЖИТ СЕКРЕТ.** `Debug` не выводится намеренно: структура с голой
/// `String` внутри попала бы в отладочную печать целиком.
///
/// Имя поля `webhook` — часть замороженного контракта с
/// `tmux-web/static/js/bitrix/api.js`.
#[derive(Deserialize)]
pub struct WebhookReq {
    /// То, что пользователь вставил в поле: адрес с методом, без метода, со
    /// слэшем или без. Разбор — [`parse_webhook_input`], и только здесь, на
    /// бэкенде.
    pub webhook: String,
}

/// Ответ обоих роутов — тот же снимок, что у `/status` и WS-кадра.
pub type LoginResp = LoginStateDto;

/// Роуты подключения.
pub fn router() -> Router<Arc<B24State>> {
    Router::new()
        .route("/api/bitrix/login/webhook", post(post_login_webhook))
        .route("/api/bitrix/logout", post(post_logout))
}

/// `POST /api/bitrix/login/webhook` — принять вставленный адрес вебхука.
///
/// Порядок — см. шапку модуля. Невалидный ввод НЕ доходит до сети: счётчик
/// вызовов портала после мусорной строки обязан остаться нулевым (тест
/// `garbage_input_never_reaches_the_portal`).
///
/// # Errors
///
/// `400 invalid_input` — строка не похожа на адрес входящего вебхука
/// (состояние автомата при этом становится `invalid_url`); `500 io_error` —
/// не удалось записать конфиг, вебхук в этом случае НЕ применяется.
pub async fn post_login_webhook(
    State(state): State<Arc<B24State>>,
    // ↓ ТЕЛО СОДЕРЖИТ СЕКРЕТ: не логировать ни целиком, ни по полям.
    Json(req): Json<WebhookReq>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let webhook = match parse_webhook_input(&req.webhook) {
        Ok(w) => w,
        Err(e) => {
            // Состояние обязано измениться ВМЕСТЕ с ответом: экран подключения
            // открыт и в соседних вкладках, а они узнают об отказе только
            // кадром `login_state`.
            let reason = e.to_string();
            apply_login_error(&state, e).await;
            return Err(ApiError::bad_input(reason));
        }
    };

    save_webhook_to(&state.config.config_path, &webhook).map_err(|e| {
        // Текст ошибки ФС может цитировать путь, но не сам секрет.
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not persist the webhook"
        );
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            format!("could not save the webhook to the config file: {e}"),
        )
    })?;

    // Сброс кэша клиента (а с ним и троттлера) делает set_webhook — иначе
    // проверка пошла бы в портал со СТАРЫМ секретом.
    state.set_webhook(webhook).await;

    let client = state.api_client().await.map_err(|e| {
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not build the API client for the new webhook"
        );
        ApiError::bad_input("this webhook cannot be used to call the portal")
    })?;
    let flow = verify_webhook(&state, &HttpBackend::new(client)).await;
    Ok(Json(flow.to_dto()))
}

/// `POST /api/bitrix/logout` — отвязать портал.
///
/// **На портал не ходим вовсе** (см. шапку модуля). Идемпотентен: отсутствие
/// вебхука и файла — не ошибка.
///
/// Порядок обратный подключению: сначала диск, потом память. Иначе неудачная
/// запись оставила бы пользователя «отключённым» до перезапуска, после
/// которого вебхук вернулся бы из файла.
///
/// # Errors
///
/// `500 io_error` — не удалось переписать конфиг (например, каталог только для
/// чтения).
pub async fn post_logout(
    State(state): State<Arc<B24State>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    clear_webhook_to(&state.config.config_path).map_err(|e| {
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: logout failed to rewrite the config"
        );
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            format!("could not remove the webhook from the config file: {e}"),
        )
    })?;

    // Сбрасывает и кэш клиента: оставленный экземпляр продолжил бы ходить в
    // портал с уже стёртым секретом.
    state.set_webhook(crate::config::WebhookUrl::default()).await;
    // Курсор потока событий (Ф7) — данные прошлого подключения: следующий
    // вебхук может принадлежать другому порталу, и чужой offset дал бы либо
    // пустоту, либо чужую очередь.
    state.set_event_cursor(None).await;
    // Имена сотрудников и счётчики (Ф4) — тоже данные аккаунта. Пережив
    // отвязку, они подписали бы чужими именами первый же список следующего
    // портала.
    state.peers.clear();
    *state.counters.write().await = None;
    // Принятые, но не отправленные файлы (Ф9) — тоже данные аккаунта. Пережив
    // отвязку, невыбранный файл уехал бы в чат СЛЕДУЮЩЕГО портала первой же
    // отправкой, а его байты лежали бы на диске до истечения TTL.
    let dropped_uploads = crate::routes::upload::clear_uploads(&state).await;
    state.set_login(LoginFlow::NotConfigured).await;

    tracing::info!(
        target: "forge_bitrix",
        dropped_uploads,
        "forge-bitrix: logged out; the webhook is removed locally and is still valid on the portal"
    );
    Ok(Json(state.login_state_dto().await))
}

/// Текст для UI: что именно делает выход.
///
/// Живёт на бэкенде по той же причине, что и [`crate::routes::WEBHOOK_HINT`]:
/// разметку браузер держит в кэше PWA неделями, а формулировка обязана быть
/// одинаковой во всех вкладках.
pub const LOGOUT_HINT: &str = "Вебхук будет удалён только из этого devforge. \
На портале он продолжит действовать — отзовите его вручную: Разработчикам → Другое → \
Входящий вебхук → удалить.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BitrixConfig, WebhookUrl};
    use crate::login::{LoginBackend, LoginError};

    /// Ошибка разбора остаётся ошибкой ввода, а не «портал отказал».
    ///
    /// Полный путь роута проверяется на стенде (`tests/b24_stub.rs`): здесь
    /// нужен только контракт «`BadInput` → 400 `invalid_input`».
    #[tokio::test]
    async fn bad_input_becomes_invalid_url_and_never_reaches_the_state_as_authorized() {
        let dir = tempfile::tempdir().unwrap();
        let state = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        });

        let flow = apply_login_error(
            &state,
            LoginError::BadInput {
                reason: "не адрес вебхука".into(),
            },
        )
        .await;
        assert_eq!(flow.kind(), "invalid_url");
        assert!(!state.has_webhook().await);
        assert!(
            !dir.path().join("bitrix.toml").exists(),
            "мусорный ввод не должен создавать конфиг"
        );
    }

    /// Logout стирает секрет и с диска, и из памяти, и гасит клиента.
    #[tokio::test]
    async fn logout_wipes_the_secret_from_the_disk_and_from_the_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
        save_webhook_to(&path, &webhook).unwrap();

        let state = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: path.clone(),
            webhook: webhook.clone(),
            ..BitrixConfig::default()
        });
        // Клиент собран — после выхода его быть не должно.
        let _ = state.api_client().await.unwrap();
        assert!(state.client.read().await.is_some());

        let resp = post_logout(State(state.clone())).await.unwrap();
        assert_eq!(resp.0.state, "not_configured");
        assert!(!state.has_webhook().await);
        assert!(state.client.read().await.is_none(), "кэш клиента не сброшен");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("abc123def456"), "{body}");

        // Повторный выход — не ошибка.
        assert_eq!(
            post_logout(State(state)).await.unwrap().0.state,
            "not_configured"
        );
    }

    /// HttpBackend существует ради боевого пути и обязан оставаться объектно
    /// безопасным: `verify_webhook` принимает `&dyn LoginBackend`.
    #[test]
    fn the_backend_trait_stays_object_safe() {
        fn assert_object_safe(_: &dyn LoginBackend) {}
        let _ = assert_object_safe;
    }
}
