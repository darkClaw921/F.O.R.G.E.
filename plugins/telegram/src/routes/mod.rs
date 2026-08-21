//! HTTP-поверхность плагина Telegram.
//!
//! Всё живёт под префиксом `/api/telegram/*` плюс WebSocket
//! `/ws/telegram` — коллизий с роутами хоста нет по построению.
//!
//! Сборка идёт в [`build_router`], который вызывается из
//! [`crate::register_routes`] ДО `.fallback(...)` и `.layer(auth)` хоста:
//! только так Bearer-auth и csrf_guard автоматически покрывают роуты
//! плагина в remote-mode.
//!
//! Суб-модули:
//! - [`error`] — единый контракт ошибок `{"error":{code,message,…}}`;
//! - [`login`] — `status`, `login/start|code|password`, `logout`;
//! - [`dialogs`] — диалоги (в т.ч. `?folder=archive`), папки, сообщения,
//!   нажатие inline-кнопок ботов, read, typing;
//! - [`media`] — отдача принятых медиа из файлового кэша;
//! - [`upload`] — приём файла от браузера (raw streaming body);
//! - [`analysis`] — «Анализ чата через Claude»: заметка к диалогу,
//!   сохранённый разбор, тред Q&A, запуск и отмена run'а.
//!
//! # ТАБЛИЦА-КОНТРАКТ ПУТЕЙ
//!
//! Единственный машиночитаемый список всей HTTP-поверхности плагина.
//! `telegram_contract.test.mjs` парсит его и сверяет с путями в
//! `tmux-web/static/js/telegram/api.js` В ОБЕ СТОРОНЫ: без этой сверки
//! расхождение фронта и роутов даёт молчаливый 404 — запрос уходит, ответ
//! приходит, а панель просто «ничего не делает».
//!
//! Формат строки заморожен: `| МЕТОД | путь | назначение | коды отказа |`.
//! Путь пишется РОВНО так, как объявлен в axum-роутере (с `:id` / `:mid`),
//! потому что тест сопоставляет его с шаблоном из `api.js` после подстановки
//! плейсхолдеров. Добавили роут — обязаны добавить строку: полнота таблицы и
//! есть то, что делает тест осмысленным.
//!
//! | Метод | Путь | Назначение | Коды отказа |
//! |-------|------|------------|-------------|
//! | GET | `/api/telegram/status` | состояние плагина и логина; 404 ⇒ плагин выключен | — |
//! | POST | `/api/telegram/login/start` | шаг логина: телефон | 400, 409, 429 |
//! | POST | `/api/telegram/login/code` | шаг логина: код из Telegram | 400, 409, 429 |
//! | POST | `/api/telegram/login/password` | шаг логина: пароль 2FA | 400, 409, 429 |
//! | POST | `/api/telegram/logout` | выход и удаление сессии | — |
//! | GET | `/api/telegram/dialogs` | список диалогов (`limit`, `folder`) | 400, 409, 503 |
//! | GET | `/api/telegram/folders` | папки (dialog filters) аккаунта | 409, 503 |
//! | GET | `/api/telegram/dialogs/:id/messages` | история (`limit`, `before_id`) | 400, 404, 409, 429, 503 |
//! | POST | `/api/telegram/dialogs/:id/messages` | отправка текста или медиа | 400, 404, 409, 413, 429, 503 |
//! | GET | `/api/telegram/dialogs/:id/messages/:mid` | одно сообщение по id | 404, 409, 429, 503 |
//! | POST | `/api/telegram/dialogs/:id/messages/:mid/callback` | нажатие inline-кнопки бота | 400, 404, 409, 429, 502, 503, 504 |
//! | POST | `/api/telegram/dialogs/:id/read` | отметить прочитанным до `max_id` | 400, 404, 409, 429, 503 |
//! | POST | `/api/telegram/dialogs/:id/typing` | «печатает…» / отмена | 400, 404, 409, 429, 503 |
//! | GET | `/api/telegram/media/:dialog_id/:message_id` | байты медиа из файлового кэша | 404, 409, 429, 503 |
//! | POST | `/api/telegram/upload` | приём файла от браузера | 409, 411, 413, 503 |
//! | GET | `/api/telegram/dialogs/:id/prompt` | заметка пользователя к диалогу | 404, 409, 503 |
//! | PUT | `/api/telegram/dialogs/:id/prompt` | сохранить заметку (пустая строка — стереть) | 400, 404, 409, 500, 503 |
//! | GET | `/api/telegram/dialogs/:id/analysis` | последний сохранённый разбор + тред | 404, 409, 503 |
//! | POST | `/api/telegram/dialogs/:id/analysis` | запустить разбор, обновление либо вопрос | 400, 404, 409, 429, 503 |
//! | DELETE | `/api/telegram/dialogs/:id/analysis` | стереть сохранённый разбор | 404, 409, 500, 503 |
//! | POST | `/api/telegram/dialogs/:id/analysis/cancel` | остановить идущий анализ | 404, 409, 503 |
//! | DELETE | `/api/telegram/dialogs/:id/analysis/thread` | стереть тред вопросов | 404, 409, 500, 503 |
//! | POST | `/api/telegram/voice/:dialog_id/:message_id/:index/transcribe` | запустить расшифровку голосового | 400, 404, 409, 500, 503 |
//! | GET | `/api/telegram/voice/:dialog_id/:message_id/:index/transcript` | готовая расшифровка голосового | 400, 409 |
//! | GET | `/api/telegram/voice/settings` | выбранный движок расшифровки | 500 |
//! | PUT | `/api/telegram/voice/settings` | переключить движок (`local`/`cloud`) | 400, 500 |
//!
//! WebSocket `/ws/telegram` в таблицу не входит намеренно: у него другой
//! контракт (кадры `TgServerMsg`), и его сверяет отдельная ось того же теста.

pub mod analysis;
pub mod dialogs;
pub mod error;
pub mod login;
pub mod media;
pub mod upload;
pub mod voice;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;

use crate::state::TelegramState;

/// Собирает Router плагина со всем состоянием внутри.
///
/// Возвращает `Router` без параметра state — его можно сразу мерджить в
/// приложение хоста (`app.merge(build_router(state))`).
pub fn build_router(state: Arc<TelegramState>) -> Router {
    // Лимит тела для загрузки берётся из конфига ДО `with_state`: слой
    // навешивается на один роут, а не на приложение (см. `upload::router`).
    let max_upload_bytes = state.config.max_upload_bytes;
    Router::new()
        .merge(login::router())
        .merge(dialogs::router())
        .merge(media::router())
        .merge(upload::router(max_upload_bytes))
        .merge(analysis::router())
        .merge(voice::router())
        .route("/ws/telegram", get(crate::ws::telegram_ws))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::config::TelegramConfig;

    /// Роутер на ИЗОЛИРОВАННОМ конфиге.
    ///
    /// `TelegramConfig::default()` указывает на реальные
    /// `~/.config/forge/telegram.session` и `~/.forge/telegram-cache`, а
    /// `POST /logout` удаляет файл сессии — тесты обязаны работать в
    /// tempdir, иначе прогон `cargo test` разлогинивал бы пользователя.
    /// `TempDir` возвращается наружу, чтобы каталог жил до конца теста.
    fn app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        (dir, build_router(TelegramState::new(cfg)))
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn status_returns_not_configured() {
        let (_dir, app) = app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["enabled"], true);
        assert_eq!(json["state"], "not_configured");
        assert!(json["user"].is_null());
    }

    /// Контракт ошибок в состоянии `not_configured` (ни кредов, ни
    /// соединения) — по каждому роуту свой честный ответ:
    ///
    /// - шаги логина с пустым телом ⇒ `400 invalid_input` (нет
    ///   обязательного поля);
    /// - данные-роуты ⇒ `409 not_authorized` (аккаунт не привязан).
    ///
    /// `logout` в список не входит: он успешен из любого состояния (см.
    /// отдельный тест ниже).
    #[tokio::test]
    async fn routes_report_expected_error_codes_when_not_configured() {
        let cases: &[(&str, &str, StatusCode, &str)] = &[
            (
                "POST",
                "/api/telegram/login/start",
                StatusCode::BAD_REQUEST,
                "invalid_input",
            ),
            (
                "POST",
                "/api/telegram/login/code",
                StatusCode::BAD_REQUEST,
                "invalid_input",
            ),
            (
                "POST",
                "/api/telegram/login/password",
                StatusCode::BAD_REQUEST,
                "invalid_input",
            ),
            (
                "GET",
                "/api/telegram/dialogs",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            (
                "GET",
                "/api/telegram/folders",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            (
                "GET",
                "/api/telegram/dialogs/123/messages",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            (
                "POST",
                "/api/telegram/dialogs/123/messages",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            // Одно сообщение по id (оригинал цитаты вне загруженной
            // истории): `:mid` разбирается ПОСЛЕ авторизации, поэтому 409.
            (
                "GET",
                "/api/telegram/dialogs/123/messages/7",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            // ...и мусорный `:mid` даёт тот же ответ: иначе неавторизованный
            // клиент по коду отличал бы валидный id от невалидного.
            (
                "GET",
                "/api/telegram/dialogs/123/messages/not-a-number",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            // Нажатие inline-кнопки бота: та же лестница. Авторизация
            // проверяется до разбора `:mid` и тела, поэтому 409, а не 400.
            (
                "POST",
                "/api/telegram/dialogs/123/messages/7/callback",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            (
                "POST",
                "/api/telegram/dialogs/123/read",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            (
                "POST",
                "/api/telegram/dialogs/123/typing",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            // Загрузка файла: авторизация проверяется ПЕРВОЙ, до
            // Content-Length и до чтения тела, поэтому здесь тоже 409, а не
            // 411 (тело в этом кейсе как раз есть).
            (
                "POST",
                "/api/telegram/upload",
                StatusCode::CONFLICT,
                "not_authorized",
            ),
        ];
        for (method, uri, status, code) in cases {
            let req = Request::builder()
                .method(*method)
                .uri(*uri)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let (_dir, app) = app();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), *status, "{method} {uri}");
            let json = body_json(resp).await;
            assert_eq!(
                json["error"]["code"].as_str().unwrap_or_default(),
                *code,
                "{method} {uri}"
            );
            assert!(
                !json["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "{method} {uri}: message обязателен"
            );
        }
    }

    /// Шаг логина с заполненным полем, но без кредов ⇒ `409 invalid_state`:
    /// сначала нужно сохранить api_id/api_hash.
    #[tokio::test]
    async fn filled_login_steps_without_credentials_are_invalid_state() {
        let cases: &[(&str, &str)] = &[
            ("/api/telegram/login/start", r#"{"phone":"+79990000000"}"#),
            ("/api/telegram/login/code", r#"{"code":"12345"}"#),
            ("/api/telegram/login/password", r#"{"password":"pw"}"#),
        ];
        for (uri, body) in cases {
            let req = Request::builder()
                .method("POST")
                .uri(*uri)
                .header("content-type", "application/json")
                .body(Body::from(*body))
                .unwrap();
            let (_dir, app) = app();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
            let json = body_json(resp).await;
            assert_eq!(
                json["error"]["code"].as_str().unwrap_or_default(),
                "invalid_state",
                "{uri}"
            );
        }
    }

    /// `logout` успешен всегда: пользователь нажал «выйти», и состояние
    /// обязано это отразить, даже если выходить было не из чего.
    #[tokio::test]
    async fn logout_succeeds_from_any_state() {
        let (_dir, app) = app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/telegram/logout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["state"], "not_configured");
    }

    /// Порядок проверок из контракта dialogs: авторизация — ПЕРВОЙ, до
    /// какого-либо разбора `:id`. Враждебные и вырожденные id (path
    /// traversal, знак `+`, переполнение i64, emoji, XSS-строка) обязаны
    /// получать тот же `409 not_authorized`, что и честный id: иной ответ
    /// позволил бы неавторизованному клиенту прощупывать валидность id и
    /// означал бы, что разбор ввода делается до аутентификации.
    #[tokio::test]
    async fn hostile_dialog_ids_hit_auth_before_any_parsing() {
        let ids = [
            "..%2F..%2Fetc%2Fpasswd",                  // path traversal
            "%2B79990000000",                          // «+» percent-encoded
            "+123",                                    // «+» сырым символом
            "99999999999999999999999999",              // переполнение i64
            "-0",
            "%F0%9F%98%80",                            // emoji
            "%3Cscript%3Ealert(1)%3C%2Fscript%3E",     // XSS-строка
            "null",
        ];
        for id in ids {
            for (method, tail, body) in [
                ("GET", "messages", Body::empty()),
                ("POST", "read", Body::from("{}")),
                // Двухпараметрические пути: `:id` враждебный, `:mid`
                // честный — auth обязана сработать раньше обоих разборов.
                ("GET", "messages/7", Body::empty()),
                ("POST", "messages/7/callback", Body::from(r#"{"data":"YQ=="}"#)),
            ] {
                let req = Request::builder()
                    .method(method)
                    .uri(format!("/api/telegram/dialogs/{id}/{tail}"))
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap();
                let (_dir, app) = app();
                let resp = app.oneshot(req).await.unwrap();
                assert_eq!(
                    resp.status(),
                    StatusCode::CONFLICT,
                    "{method} {id}/{tail}: auth обязана идти первой"
                );
                let json = body_json(resp).await;
                assert_eq!(json["error"]["code"], "not_authorized", "{method} {id}/{tail}");
            }
        }
    }

    /// Мусор в query (`limit=abc`, отрицательный/переполненный limit,
    /// `before_id` за пределами i32) — ошибка клиента, а не сервера: 400,
    /// никаких 500 и паник.
    #[tokio::test]
    async fn malformed_query_params_are_client_errors() {
        let uris = [
            "/api/telegram/dialogs?limit=abc",
            "/api/telegram/dialogs?limit=-1",
            "/api/telegram/dialogs?limit=4294967296",          // > u32::MAX
            "/api/telegram/dialogs/123/messages?before_id=2147483648", // > i32::MAX
            "/api/telegram/dialogs/123/messages?before_id=abc",
            "/api/telegram/dialogs/123/messages?before_id=1e3",
        ];
        for uri in uris {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let (_dir, app) = app();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    /// Валидные, но вырожденные значения query (`before_id=0`, отрицательный
    /// курсор, `limit=0`) парсятся успешно и доходят до обычного порядка
    /// проверок (здесь — 409 not_authorized), а не отвергаются на входе:
    /// клампинг и трактовку нулей делает сервер, а не парсер query.
    #[tokio::test]
    async fn degenerate_but_valid_query_values_reach_the_handler() {
        let uris = [
            "/api/telegram/dialogs?limit=0",
            // `folder` разбирается ПОСЛЕ авторизации (порядок проверок из
            // контракта dialogs) — до валидации значения дело не доходит.
            "/api/telegram/dialogs?folder=archive",
            "/api/telegram/dialogs?folder=bogus",
            "/api/telegram/dialogs?folder=main&limit=5",
            "/api/telegram/dialogs/123/messages?before_id=0",
            "/api/telegram/dialogs/123/messages?before_id=-2147483648",
            "/api/telegram/dialogs/123/messages?limit=0&before_id=1",
        ];
        for uri in uris {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let (_dir, app) = app();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
            let json = body_json(resp).await;
            assert_eq!(json["error"]["code"], "not_authorized", "{uri}");
        }
    }

    /// Битые тела (обрыв на полпути, не-JSON, не тот тип поля, массив вместо
    /// объекта) деградируют до «поля нет» ⇒ стабильный `400 invalid_input`
    /// нашего контракта — а не 415/422/500 в чужом формате, который фронт
    /// не умеет разбирать.
    #[tokio::test]
    async fn corrupt_json_bodies_degrade_to_invalid_input() {
        let cases: &[(&str, &str)] = &[
            ("/api/telegram/login/start", "{\"phone\": \"+7"), // обрыв посреди строки
            ("/api/telegram/login/start", "{not json"),
            ("/api/telegram/login/start", "[]"),
            ("/api/telegram/login/start", r#"{"phone":123}"#), // число вместо строки
            ("/api/telegram/login/code", r#"{"code":12345}"#),
            ("/api/telegram/login/code", r#"{"code":null}"#),
            ("/api/telegram/login/password", r#"{"password":null}"#),
            ("/api/telegram/login/password", "\"pw\""), // строка вместо объекта
        ];
        for (uri, body) in cases {
            let req = Request::builder()
                .method("POST")
                .uri(*uri)
                .header("content-type", "application/json")
                .body(Body::from(*body))
                .unwrap();
            let (_dir, app) = app();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri} {body}");
            let json = body_json(resp).await;
            assert_eq!(json["error"]["code"], "invalid_input", "{uri} {body}");
        }
    }

    /// Валидный JSON, но без `Content-Type: application/json` (curl без
    /// заголовка) трактуется как отсутствие тела ⇒ тот же `400
    /// invalid_input`, а не 415 вне контракта.
    #[tokio::test]
    async fn json_without_content_type_is_treated_as_missing_body() {
        let req = Request::builder()
            .method("POST")
            .uri("/api/telegram/login/start")
            .header("content-type", "text/plain")
            .body(Body::from(r#"{"phone":"+79990000000"}"#))
            .unwrap();
        let (_dir, app) = app();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "invalid_input");
    }

    /// Ни один роут не отдаёт 500 и не паникует, даже когда тела запроса
    /// нет вовсе.
    #[tokio::test]
    async fn routes_tolerate_missing_body() {
        for uri in [
            "/api/telegram/login/start",
            "/api/telegram/login/code",
            "/api/telegram/login/password",
            "/api/telegram/logout",
            "/api/telegram/dialogs/123/read",
            "/api/telegram/dialogs/123/typing",
            "/api/telegram/dialogs/123/messages/7/callback",
        ] {
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let (_dir, app) = app();
            let resp = app.oneshot(req).await.unwrap();
            assert!(
                !resp.status().is_server_error(),
                "{uri} отдал {}",
                resp.status()
            );
        }
    }
}
