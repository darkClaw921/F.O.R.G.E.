//! Единый контракт ошибок REST-поверхности плагина.
//!
//! Тело ответа всегда одной формы — фронту не нужно угадывать, что пришло:
//!
//! ```json
//! {"error":{"code":"not_authorized","message":"VK account is not linked yet"}}
//! {"error":{"code":"flood_wait","message":"too many requests","retry_after_secs":60}}
//! {"error":{"code":"captcha_required","message":"…","captcha_sid":"239","captcha_img":"https://…"}}
//! ```
//!
//! | HTTP | code | когда |
//! |------|------|-------|
//! | 400 | `invalid_input`       | VK отверг параметры (код 100/913/914/921) или их отверг наш разбор |
//! | 404 | `unknown_dialog`      | беседы нет либо она недоступна (18/30/113/917/936) |
//! | 404 | `unknown_message`     | сообщения нет: `getById` вернул пустой `items` |
//! | 404 | `no_media`            | у вложения нет запрошенного представления |
//! | 409 | `not_authorized`      | токен не привязан / отозван (VK 5, 27, 28) |
//! | 409 | `invalid_state`       | операция невозможна в текущем состоянии (VK 909, 925) |
//! | 409 | `captcha_required`    | VK 14 — тело несёт `captcha_sid` + `captcha_img` |
//! | 409 | `no_messages_scope`   | VK 15 — у токена нет права `messages` |
//! | 409 | `send_forbidden`      | VK 900/901/902 — адресат запретил сообщения |
//! | 409 | `validation_required` | VK 17 — тело несёт `redirect_uri` |
//! | 409 | `unsupported_button`  | кнопку этого типа нельзя нажать под user-токеном |
//! | 409 | `claude_unavailable`  | бинаря Claude CLI нет по `claude_cli_path` — анализ чата запустить нечем |
//! | 411 | `length_required`     | `POST /upload` без `Content-Length` |
//! | 413 | `too_large`           | файл больше `max_upload_bytes` |
//! | 429 | `flood_wait`          | VK 6/9/29 — просит подождать `retry_after_secs` |
//! | 500 | `storage_failed`      | не удалось записать локальный файл плагина (заметки к чатам) |
//! | 502 | `vk_error`            | VK ответил ошибкой, которую не исправит ни ввод, ни повтор |
//! | 503 | `not_connected`       | транспортный отказ, см. `ConnHealth` |
//!
//! ## Чем контракт отличается от телеграмного
//!
//! - **переносятся 1:1**: `invalid_input`, `not_authorized`, `invalid_state`,
//!   `unknown_dialog`, `unknown_message`, `no_media`, `length_required`,
//!   `too_large`, `flood_wait`, `not_connected`;
//! - `telegram_error` → **`vk_error`** (то же значение, другое имя);
//! - **исчезает `bot_timeout`**: у VK нечему таймаутиться. Клиентской
//!   половины `message_event` («нажать callback от имени пользователя») в
//!   публичном API нет вовсе, поэтому нажатие либо превращается в обычную
//!   отправку (`text`), либо отвергается сразу как `unsupported_button`;
//! - **добавляются** `captcha_required`, `no_messages_scope`,
//!   `send_forbidden`, `validation_required`, `unsupported_button`,
//!   `claude_unavailable` (АЧ-Ф3).
//!
//! Три необязательных поля (`captcha_sid`, `captcha_img`, `redirect_uri`) —
//! это НАДМНОЖЕСТВО телеграмного контракта: старый фронт-разбор
//! (`code` + `message` + `retry_after_secs`) продолжает работать без правок,
//! потому что отсутствующие поля не сериализуются вовсе.
//!
//! `429 flood_wait` НИКОГДА не ретраится автоматически на уровне REST:
//! коды 9 и 29 у VK — это часовые и суточные лимиты, и автоповторы только
//! продлевают блокировку. UI обязан показать обратный отсчёт. Единственное
//! исключение живёт ниже, в транспорте (Ф2): код 6 («слишком много запросов
//! в секунду») — это промах НАШЕГО троттлера, и он чинится одним повтором
//! через секунду, не доходя до пользователя.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Тело ошибки (внутренний объект).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    /// Машинный код (см. таблицу в шапке модуля).
    pub code: String,
    /// Человекочитаемое пояснение — можно показывать пользователю.
    pub message: String,
    /// Сколько секунд ждать перед повтором (только для `flood_wait`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// Идентификатор captcha (только для `captcha_required`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captcha_sid: Option<String>,
    /// URL картинки captcha у VK (только для `captcha_required`).
    ///
    /// Фронт ставит в `<img src>` НЕ его, а прокси бэкенда: браузер
    /// пользователя на серверы VK не ходит — то же правило, что для медиа.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captcha_img: Option<String>,
    /// Куда отправить пользователя для подтверждения (только для
    /// `validation_required`, VK код 17).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
}

/// Обёртка `{"error": {...}}` — ровно то, что уходит на провод.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorEnvelope {
    /// Сама ошибка.
    pub error: ApiErrorBody,
}

/// Ошибка REST-хендлера. Реализует `IntoResponse`, поэтому хендлеры
/// возвращают `Result<Json<T>, ApiError>` и не собирают ответы руками.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP-статус.
    pub status: StatusCode,
    /// Тело ошибки.
    pub body: ApiErrorBody,
}

impl ApiError {
    /// Произвольная ошибка с явными статусом и кодом.
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorBody {
                code: code.to_string(),
                message: message.into(),
                ..ApiErrorBody::default()
            },
        }
    }

    /// 400 — параметры отвергнуты (нами или VK).
    ///
    /// Отличается от `invalid_state` тем, что состояние сохраняется:
    /// пользователь исправляет ввод и повторяет ту же операцию.
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_input", message)
    }

    /// 409 — токен не привязан или перестал быть валидным (VK 5/27/28).
    pub fn not_authorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "not_authorized", message)
    }

    /// 409 — операция невозможна в текущем состоянии.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "invalid_state", message)
    }

    /// 404 — беседа не найдена.
    pub fn unknown_dialog(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_dialog", message)
    }

    /// 404 — сообщения нет.
    ///
    /// У VK этот код порождаем МЫ: `messages.getById` на чужой или удалённый
    /// id возвращает не ошибку, а пустой `items: []`. Отдельный код от
    /// [`ApiError::unknown_dialog`] нужен нажатию кнопки бота: «беседы нет»
    /// заставляет фронт перерисовать список, а «сообщения нет» — всего лишь
    /// погасить устаревшую клавиатуру в одной строке ленты.
    pub fn unknown_message(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_message", message)
    }

    /// 404 — у вложения нет запрошенного представления (например, `full` у
    /// видео: воспроизводимого файла в объекте VK нет вовсе).
    pub fn no_media(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "no_media", message)
    }

    /// 502 — VK ответил ошибкой, которую клиент исправить не может.
    ///
    /// Это НЕ `not_connected`: сеть жива, и повтор того же запроса ничего не
    /// изменит. И не `invalid_input`: ввод пользователя ни при чём. Фронт по
    /// этому коду показывает ошибку и НЕ предлагает ни «переподключиться»,
    /// ни «исправить данные».
    pub fn vk_error(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "vk_error", message)
    }

    /// 409 — VK требует ввести символы с картинки (код 14).
    ///
    /// Приходит на ЛЮБОЙ метод, а не только на логин, поэтому во фронте её
    /// оверлей вынесен из оверлея логина. Повтор — тот же запрос плюс
    /// `captcha_sid` + `captcha_key`, поэтому оба поля обязаны доехать до
    /// клиента в теле ошибки.
    pub fn captcha_required(
        sid: impl Into<String>,
        img: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut e = Self::new(StatusCode::CONFLICT, "captcha_required", message);
        e.body.captcha_sid = Some(sid.into());
        e.body.captcha_img = Some(img.into());
        e
    }

    /// 409 — у токена нет права `messages` (VK код 15).
    ///
    /// Отдельный код, а не `not_authorized`: токен исправен, и предлагать
    /// «вставьте новый» бессмысленно — нужен токен с другим scope. Экран под
    /// этот случай объясняет, что стандартные standalone-приложения VK право
    /// `messages` не получают.
    pub fn no_messages_scope(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "no_messages_scope", message)
    }

    /// 409 — адресат не принимает сообщения (VK 900/901/902: чёрный список,
    /// запрет сообщений от сообщества, настройки приватности).
    ///
    /// Отдельный код от `vk_error`, потому что реакция UI другая: писать
    /// именно этому собеседнику нельзя в принципе, и «повторить» — не выход.
    pub fn send_forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "send_forbidden", message)
    }

    /// 409 — VK требует подтверждения пользователя (код 17).
    ///
    /// `redirect_uri` открывается пользователем в браузере вручную; сами мы
    /// по нему не ходим — это страница VK с проверкой, а не API.
    pub fn validation_required(uri: impl Into<String>, message: impl Into<String>) -> Self {
        let mut e = Self::new(StatusCode::CONFLICT, "validation_required", message);
        e.body.redirect_uri = Some(uri.into());
        e
    }

    /// 409 — кнопку этого типа нельзя нажать под пользовательским токеном.
    ///
    /// Ограничение ПЛАТФОРМЫ: `callback`, `vkpay`, `open_app` и `location`
    /// требуют официального клиента или ключа сообщества. Честный отдельный
    /// код нужен, чтобы UI объяснил это словами, а не показал «ошибка
    /// сервера» и не выглядел поломкой бота.
    pub fn unsupported_button(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "unsupported_button", message)
    }

    /// 409 — запускать анализ нечем: бинаря Claude CLI нет по настроенному
    /// пути (АЧ-Ф3).
    ///
    /// Отдельный код, а не `invalid_state`, потому что действие пользователя
    /// конкретное и НЕ угадывается по общему «операция невозможна»: поставить
    /// CLI или прописать `claude_cli_path` в `~/.config/forge/vk.toml`. Ровно
    /// это и говорит сообщение, вместе с путём, по которому мы искали.
    ///
    /// Проверка живёт в РОУТЕ, до любой работы, потому что
    /// `ClaudeRunner::new` отсутствие бинаря только warn'ит (иначе плагин без
    /// установленного CLI не поднимался бы вовсе), а `spawn` упал бы уже после
    /// полутора секунд выгрузки истории — и пользователь увидел бы `spawn
    /// failed: No such file` кадром `analysis_error` вместо инструкции.
    ///
    /// В сообщение уезжает ТОЛЬКО проверенный путь: ни `$PATH`, ни `$HOME`, ни
    /// чего-либо ещё из окружения процесса.
    pub fn claude_unavailable(path: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "claude_unavailable",
            format!(
                "Claude CLI не найден: {path}; установите CLI или укажите claude_cli_path в ~/.config/forge/vk.toml"
            ),
        )
    }

    /// 411 — загрузка файла пришла без `Content-Length`.
    ///
    /// Отдельный код (а не `invalid_input`), потому что клиент по нему
    /// понимает конкретную поправку: тело ещё не читалось, размер надо
    /// прислать заголовком и повторить запрос.
    pub fn length_required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::LENGTH_REQUIRED, "length_required", message)
    }

    /// 413 — файл больше разрешённого `max_upload_bytes`.
    ///
    /// Тело при этом НЕ читается: отказ выносится по `Content-Length`, иначе
    /// клиент зря залил бы гигабайт ради `413`.
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message)
    }

    /// 429 — VK просит подождать (коды 6/9/29).
    pub fn flood_wait(secs: u64, message: impl Into<String>) -> Self {
        let mut e = Self::new(StatusCode::TOO_MANY_REQUESTS, "flood_wait", message);
        e.body.retry_after_secs = Some(secs);
        e
    }

    /// 503 — транспортный отказ (см. `crate::state::ConnHealth`).
    pub fn not_connected(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "not_connected", message)
    }

    /// 500 — не удалось записать ЛОКАЛЬНЫЙ файл плагина.
    ///
    /// Единственный код контракта, не связанный ни с вводом, ни с VK: диск
    /// заполнен, каталог недоступен, права сняты. Отдельно от `vk_error`,
    /// потому что действия пользователя противоположны — повтор здесь
    /// бессмыслен, пока не починен диск.
    pub fn storage_failed(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_failed",
            message,
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = (
            self.status,
            Json(ApiErrorEnvelope {
                error: self.body.clone(),
            }),
        )
            .into_response();
        // Retry-After — стандартный заголовок; клиенты и прокси понимают его
        // без знания нашего JSON-контракта.
        if let Some(secs) = self.body.retry_after_secs {
            if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ПОЛНАЯ таблица контракта: конструктор → HTTP-статус → код в теле.
    ///
    /// Это единственное место, где зафиксирован весь список кодов. Фронт
    /// (`js/vk/api.js`, `upload.js`, `captcha.js`) принимает решения ИМЕННО по
    /// `code`, а не по статусу: 409 бывает шести разных смыслов, и реакция на
    /// них разная. Поэтому переименование кода обязано валить тест, а не тихо
    /// ломать вкладку.
    ///
    /// Проверяется настоящий `IntoResponse`: заголовок `Content-Type` и
    /// точный состав полей тела — часть контракта.
    #[tokio::test]
    async fn full_error_contract_table() {
        // (ошибка, статус, code, сколько полей в теле сверх code+message)
        let table: Vec<(ApiError, StatusCode, &str, usize)> = vec![
            (ApiError::bad_input("x"), StatusCode::BAD_REQUEST, "invalid_input", 0),
            (
                ApiError::unknown_dialog("x"),
                StatusCode::NOT_FOUND,
                "unknown_dialog",
                0,
            ),
            (
                ApiError::unknown_message("x"),
                StatusCode::NOT_FOUND,
                "unknown_message",
                0,
            ),
            (ApiError::no_media("x"), StatusCode::NOT_FOUND, "no_media", 0),
            (
                ApiError::not_authorized("x"),
                StatusCode::CONFLICT,
                "not_authorized",
                0,
            ),
            (
                ApiError::invalid_state("x"),
                StatusCode::CONFLICT,
                "invalid_state",
                0,
            ),
            (
                ApiError::captcha_required("239", "https://vk/c.php", "x"),
                StatusCode::CONFLICT,
                "captcha_required",
                2,
            ),
            (
                ApiError::no_messages_scope("x"),
                StatusCode::CONFLICT,
                "no_messages_scope",
                0,
            ),
            (
                ApiError::send_forbidden("x"),
                StatusCode::CONFLICT,
                "send_forbidden",
                0,
            ),
            (
                ApiError::validation_required("https://vk/valid", "x"),
                StatusCode::CONFLICT,
                "validation_required",
                1,
            ),
            (
                ApiError::unsupported_button("x"),
                StatusCode::CONFLICT,
                "unsupported_button",
                0,
            ),
            (
                ApiError::claude_unavailable("/nope/claude"),
                StatusCode::CONFLICT,
                "claude_unavailable",
                0,
            ),
            (
                ApiError::length_required("x"),
                StatusCode::LENGTH_REQUIRED,
                "length_required",
                0,
            ),
            (
                ApiError::too_large("x"),
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                0,
            ),
            (
                ApiError::flood_wait(7, "x"),
                StatusCode::TOO_MANY_REQUESTS,
                "flood_wait",
                1,
            ),
            (
                ApiError::storage_failed("x"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_failed",
                0,
            ),
            (ApiError::vk_error("x"), StatusCode::BAD_GATEWAY, "vk_error", 0),
            (
                ApiError::not_connected("x"),
                StatusCode::SERVICE_UNAVAILABLE,
                "not_connected",
                0,
            ),
        ];

        let mut seen: Vec<String> = Vec::new();
        for (err, status, code, extra_fields) in table {
            let resp = err.into_response();
            assert_eq!(resp.status(), status, "статус для {code}");
            let ct = resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(ct.starts_with("application/json"), "{code}: {ct}");

            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(json["error"]["code"], code);
            assert!(
                json["error"]["message"]
                    .as_str()
                    .is_some_and(|m| !m.is_empty()),
                "{code}: сообщение обязано быть непустым"
            );
            // Необязательные поля появляются ТОЛЬКО там, где обещаны: лишний
            // `retry_after_secs` заставил бы фронт показать обратный отсчёт
            // на ровном месте.
            let obj = json["error"].as_object().unwrap();
            assert_eq!(
                obj.len(),
                2 + extra_fields,
                "{code}: неожиданный состав тела {json}"
            );
            seen.push(code.to_string());
        }

        seen.sort();
        assert_eq!(
            seen,
            vec![
                "captcha_required",
                "claude_unavailable",
                "flood_wait",
                "invalid_input",
                "invalid_state",
                "length_required",
                "no_media",
                "no_messages_scope",
                "not_authorized",
                "not_connected",
                "send_forbidden",
                "storage_failed",
                "too_large",
                "unknown_dialog",
                "unknown_message",
                "unsupported_button",
                "validation_required",
                "vk_error",
            ],
            "список кодов разошёлся с таблицей в шапке модуля"
        );
    }

    /// Коды, которых в контракте VK быть НЕ должно.
    ///
    /// `bot_timeout` ушёл вместе с callback-кнопками (нечему таймаутиться), а
    /// `telegram_error` переименован в `vk_error`. Тест держит это решение на
    /// виду: конструкторов с такими именами в типе нет, и попытка вернуть их
    /// строкой через `ApiError::new` тоже видна в diff'е.
    #[test]
    fn retired_telegram_codes_are_not_produced_by_constructors() {
        let produced = [
            ApiError::vk_error("x").body.code,
            ApiError::unsupported_button("x").body.code,
        ];
        for code in produced {
            assert_ne!(code, "telegram_error");
            assert_ne!(code, "bot_timeout");
        }
    }

    /// Captcha без обоих полей бесполезна: фронту нечего показать и нечем
    /// повторить запрос.
    #[tokio::test]
    async fn captcha_error_carries_sid_and_image() {
        let resp = ApiError::captcha_required("239123", "https://api.vk.com/captcha.php?sid=239123", "нужна captcha")
            .into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["captcha_sid"], "239123");
        assert_eq!(
            json["error"]["captcha_img"],
            "https://api.vk.com/captcha.php?sid=239123"
        );
        assert!(json["error"].get("retry_after_secs").is_none());
    }

    /// `Retry-After` — часть контракта наравне с JSON: стандартные клиенты и
    /// прокси читают именно заголовок. Он обязан появляться у `flood_wait`
    /// (и совпадать с телом) и НЕ появляться у остальных ошибок.
    #[tokio::test]
    async fn retry_after_header_mirrors_the_body_and_only_for_flood_wait() {
        let resp = ApiError::flood_wait(60, "wait").into_response();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("60")
        );

        for err in [
            ApiError::bad_input("x"),
            ApiError::not_authorized("x"),
            ApiError::not_connected("x"),
            ApiError::captcha_required("1", "u", "x"),
        ] {
            let code = err.body.code.clone();
            let resp = err.into_response();
            assert!(
                resp.headers().get(axum::http::header::RETRY_AFTER).is_none(),
                "{code}: Retry-After допустим только у flood_wait"
            );
        }
    }

    /// Границы `retry_after_secs`: `0` (повторяй немедленно) и `u64::MAX` не
    /// должны ни паниковать, ни терять точность в JSON.
    #[tokio::test]
    async fn flood_wait_boundary_values_survive_serialization() {
        for secs in [0_u64, u64::MAX] {
            let resp = ApiError::flood_wait(secs, "w").into_response();
            assert_eq!(
                resp.headers()
                    .get(axum::http::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
                Some(secs.to_string().as_str())
            );
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(json["error"]["retry_after_secs"].as_u64(), Some(secs));
        }
    }

    /// В `message` попадает пользовательский ввод и тексты ошибок VK
    /// (кавычки, переводы строк, emoji, RTL-override) — они обязаны доехать
    /// без порчи JSON. Ловит регрессию «собрали тело руками через format!».
    #[tokio::test]
    async fn message_with_hostile_characters_stays_valid_json() {
        let hostile = "код \"1\"\nстрока\t😀 \u{202E}RTL <script>alert(1)</script>";
        let resp = ApiError::bad_input(hostile).into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["message"].as_str(), Some(hostile));
    }

    /// Форма конверта — надмножество телеграмной: старый разбор
    /// (`code`+`message`) обязан продолжать работать, а пустые опциональные
    /// поля не должны появляться в JSON как `null`.
    #[test]
    fn envelope_round_trips_and_omits_empty_options() {
        let env = ApiErrorEnvelope {
            error: ApiErrorBody {
                code: "not_authorized".into(),
                message: "nope".into(),
                ..ApiErrorBody::default()
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        assert_eq!(json, r#"{"error":{"code":"not_authorized","message":"nope"}}"#);
        let back: ApiErrorEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }
}
