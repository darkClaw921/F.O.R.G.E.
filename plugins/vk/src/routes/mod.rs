//! HTTP-поверхность плагина VK.
//!
//! Всё живёт под префиксом `/api/vk/*` плюс WebSocket `/ws/vk` — коллизий с
//! роутами хоста нет по построению.
//!
//! Сборка идёт в [`build_router`], который вызывается из
//! [`crate::register_routes`] ДО `.fallback(...)` и `.layer(auth)` хоста:
//! только так Bearer-auth и csrf_guard автоматически покрывают роуты плагина
//! в remote-mode.
//!
//! # ЗАМОРОЖЕННЫЙ КОНТРАКТ ИМЁН
//!
//! Таблица ниже — источник истины для `tmux-web/static/js/vk/api.js`. Обе
//! стороны обязаны совпадать дословно: расхождение даёт молчаливый 404, а не
//! ошибку сборки.
//!
//! | метод | путь | фаза | что делает |
//! |---|---|---|---|
//! | GET  | `/api/vk/status`                              | **Ф1** | состояние плагина и логина; 200 всегда |
//! | GET  | `/ws/vk`                                      | **Ф1** | поток событий (см. [`crate::ws::protocol`]) |
//! | POST | `/api/vk/login/token`                         | Ф3 | принять вставленный токен (URL / фрагмент / голый токен) |
//! | POST | `/api/vk/login/verify`                        | Ф3 | перепроверить текущий токен |
//! | POST | `/api/vk/logout`                              | Ф3 | стереть токен локально (в VK не ходим) |
//! | POST | `/api/vk/captcha`                             | Ф3 | отдать `{sid, key}` и продолжить |
//! | GET  | `/api/vk/captcha/:sid`                        | Ф3 | ПРОКСИ картинки captcha (браузер на серверы VK не ходит) |
//! | GET  | `/api/vk/filters`                             | Ф4 | 4 серверных фильтра (замена телеграмных папок) |
//! | GET  | `/api/vk/dialogs`                             | Ф4 | список бесед (`?limit&offset&filter`) |
//! | GET  | `/api/vk/dialogs/:id/messages`                | Ф4 | страница истории (`?limit&before_id`) |
//! | POST | `/api/vk/dialogs/:id/messages`                | Ф4 | отправить сообщение (+ 3-шаговый upload внутри, Ф8) |
//! | GET  | `/api/vk/dialogs/:id/messages/:mid`           | Ф4 | одно сообщение по id |
//! | POST | `/api/vk/dialogs/:id/read`                    | Ф4 | отметить прочитанным |
//! | POST | `/api/vk/dialogs/:id/typing`                  | Ф4 | «печатает…» |
//! | POST | `/api/vk/dialogs/:id/messages/:mid/button`    | Ф9 | нажать кнопку бота по координатам `{row, col}` |
//! | GET  | `/api/vk/media/:peer_id/:message_id/:index`   | Ф7 | байты вложения (`?kind=thumb\|full`) |
//! | POST | `/api/vk/upload`                              | Ф8 | приём файла (raw body + `X-Forge-File-Name`) |
//!
//! ## Три решения, зафиксированные здесь навсегда (задача Ф1.1)
//!
//! 1. **`dialogs`, а не `conversations`.** VK-лексика (`peer_id`,
//!    `conversation_message_id`, `attachments`) живёт только в ПОЛЯХ DTO, а
//!    не в путях. Причина — симметрия с `/api/telegram/dialogs`: главный
//!    инструмент сопровождения двух панелей это `diff js/telegram/chat.js
//!    js/vk/chat.js`, и переименование ради «правильной» терминологии
//!    превратило бы осмысленный дифф в шум.
//! 2. **`no_messages_scope`, а не `insufficient_scope`.** То же имя
//!    используется и как состояние логина ([`crate::login::LoginFlow`]), и как
//!    `code` в конверте ошибки для VK-15 ([`error::ApiError::no_messages_scope`]).
//!    Одно имя на три места — меньше поводов разъехаться.
//! 3. **`vk_visible_kinds` / `vk_visible_peers`** (ключи настроек, Ф10), а не
//!    `vk_visible_filters` / `vk_visible_dialogs`. Белый список строится по
//!    ТИПАМ peer'а и поимённым беседам, а НЕ по четырём серверным фильтрам:
//!    «показывать только непрочитанные» как постоянная настройка бессмысленно.
//!
//! ## Порядок проверок во всех data-роутах (Ф4+)
//!
//! Авторизация (409) → соединение (503) → резолв peer (404) → разбор
//! параметров. Дословно как у Telegram, и на это есть тест
//! `hostile_dialog_ids_hit_auth_before_any_parsing`: иной порядок позволил бы
//! неавторизованному клиенту прощупывать валидность id.
//!
//! Суб-модули:
//! - [`error`] — единый контракт ошибок `{"error":{code,message,…}}`;
//! - [`login`] — вставка токена, повторная проверка, logout и captcha;
//! - [`dialogs`] — беседы, история, отдельное сообщение, фильтры и отправка;
//! - [`media`] — байты вложений из файлового кэша (Ф7);
//! - [`upload`] — staging файла и трёхшаговый конвейер заливки в VK (Ф8).
//!
//! ## Почему `POST /api/vk/upload` не ходит в VK
//!
//! Upload-сервер VK выдаётся под КОНКРЕТНЫЙ `peer_id`, а на шаге `/upload`
//! адресат ещё неизвестен. Поэтому роут загрузки кладёт байты на наш диск, а
//! конвейер (`getMessagesUploadServer` → `POST upload_url` → `save` →
//! `messages.send`) целиком живёт внутри `POST /api/vk/dialogs/:id/messages`.
//! Подробности и следствия для фронта — в шапке [`upload`].

pub mod dialogs;
pub mod error;
pub mod login;
pub mod media;
pub mod upload;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::login::LoginStateDto;
use crate::state::VkState;

/// Ответ `GET /api/vk/status`.
///
/// `enabled` всегда `true`: роут существует только когда плагин включён
/// флагом `--vk`, поэтому сам факт ответа 200 уже означает «плагин активен»
/// (а 404 — «выключен», и именно так фронт гейтит кнопку Ⓥ). Поле оставлено
/// явным, чтобы фронт читал одну и ту же форму и не полагался на HTTP-статус.
///
/// Состояние логина лежит ВЛОЖЕННЫМ объектом `login`, а не плоско: у VK в
/// статусе есть собственные поля (`api_version`, `rate_limit_rps`), и
/// плоская форма Telegram смешала бы два разных набора в одном объекте.
///
/// ## Чего здесь нет
///
/// **Отпечатка токена.** Соблазн понятный (у пользователя три токена в
/// блокноте, и он не помнит, какой вставлен), но любая часть секрета в
/// HTTP-ответе — это часть секрета: `/api/vk/status` уходит в браузер, в
/// devtools, в HAR-файл поддержки. Отпечаток остаётся только в логах сервера
/// ([`crate::config::SecretToken::fingerprint`]).
///
/// Технически положить токен сюда невозможно и по типам: `SecretToken` не
/// реализует `Serialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDto {
    /// Плагин зарегистрирован в этом процессе.
    pub enabled: bool,
    /// Состояние логина (см. [`crate::login::LoginFlow::kind`]).
    pub login: LoginStateDto,
    /// Версия API VK, с которой работает плагин.
    pub api_version: String,
    /// Действующий лимит запросов в секунду — фронтовый планировщик
    /// (`js/vk/ratelimit.js`, Ф4) настраивается по нему, а не по своей копии
    /// константы.
    pub rate_limit_rps: u32,
    /// Подсказка «где взять токен» для формы логина.
    ///
    /// Приходит С БЭКЕНДА, а не зашита в `index.html`: правила выдачи scope
    /// `messages` у VK меняются, а разметку браузер держит в кэше PWA и может
    /// не обновлять неделями. В разметке остаётся только контейнер с
    /// безопасным дефолтом (`#vk-login-hint`), текст в него кладёт
    /// `js/vk/login.js` из этого поля.
    pub hint: String,
}

/// Собирает Router плагина со всем состоянием внутри.
///
/// Возвращает `Router` без параметра state — его можно сразу мерджить в
/// приложение хоста (`app.merge(build_router(state))`).
///
/// Смонтированы `GET /api/vk/status`, `/ws/vk`, группа логина
/// ([`login::router`]), группа данных ([`dialogs::router`]), медиа
/// ([`media::router`]) и загрузка ([`upload::router`]).
///
/// Лимит тела загрузки читается из конфига ЗДЕСЬ, до `with_state`: слой
/// `DefaultBodyLimit` навешивается точечно на один роут, чтобы остальные
/// (JSON-роуты хоста в том числе) не начали принимать гигабайты.
pub fn build_router(state: Arc<VkState>) -> Router {
    let max_upload_bytes = state.config.max_upload_bytes;
    Router::new()
        .route("/api/vk/status", get(get_status))
        .route("/ws/vk", get(crate::ws::vk_ws))
        .merge(login::router())
        .merge(dialogs::router())
        .merge(media::router())
        .merge(upload::router(max_upload_bytes))
        .with_state(state)
}

/// `GET /api/vk/status` → 200 всегда.
///
/// Единственный роут, который отвечает 200 в любом состоянии: по нему
/// фронтенд решает, показывать ли кнопку Ⓥ вообще.
pub async fn get_status(State(state): State<Arc<VkState>>) -> Json<StatusDto> {
    Json(StatusDto {
        enabled: true,
        login: state.login_state_dto().await,
        api_version: state.config.api_version.clone(),
        rate_limit_rps: state.config.rate_limit_rps,
        hint: login::TOKEN_HINT.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{SecretToken, VkConfig};
    use crate::login::LoginFlow;

    /// Роутер на ИЗОЛИРОВАННОМ конфиге.
    ///
    /// `VkConfig::default()` указывает на реальный `~/.forge/vk-cache`, а
    /// logout (Ф3) удаляет токен из `~/.config/forge/vk.toml` — тесты обязаны
    /// работать в tempdir, иначе прогон `cargo test` разлогинивал бы
    /// пользователя. `TempDir` возвращается наружу, чтобы каталог жил до
    /// конца теста.
    fn app_with(cfg: impl FnOnce(&mut VkConfig)) -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = VkConfig {
            cache_dir: dir.path().join("cache"),
            ..VkConfig::default()
        };
        cfg(&mut c);
        (dir, build_router(VkState::new(c)))
    }

    fn app() -> (tempfile::TempDir, Router) {
        app_with(|_| {})
    }

    /// Состояние АВТОРИЗОВАННОГО плагина, чья сеть заведомо недоступна.
    ///
    /// `base_url` указывает на порт 1 (закрыт всегда): любой реальный вызов VK
    /// упадёт мгновенно и детерминированно, поэтому тесты лестницы проверок
    /// видят разницу между «дошло до сети» и «отвергнуто раньше».
    async fn authorized_state(dir: &tempfile::TempDir) -> Arc<VkState> {
        let state = VkState::new(VkConfig {
            access_token: SecretToken::new("vk1.a.TEST"),
            base_url: "http://127.0.0.1:1/method".into(),
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            ..VkConfig::default()
        });
        state
            .set_login(LoginFlow::Authorized {
                user: crate::login::VkUserDto {
                    id: "1234".into(),
                    first_name: Some("Тест".into()),
                    ..Default::default()
                },
            })
            .await;
        state
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(app: Router, uri: &str) -> axum::response::Response {
        app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn status_returns_not_configured_without_a_token() {
        let (_dir, app) = app();
        let resp = get(app, "/api/vk/status").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json["enabled"], true);
        assert_eq!(json["login"]["state"], "not_configured");
        assert!(json["login"]["user"].is_null());
        assert!(json["login"]["captcha_sid"].is_null());
        assert_eq!(json["api_version"], crate::config::DEFAULT_API_VERSION);
        assert_eq!(json["rate_limit_rps"], crate::config::DEFAULT_RATE_LIMIT_RPS);
    }

    /// С токеном плагин стартует в `disconnected`: `init` синхронный и сети не
    /// трогает, проверка токена — работа `spawn_workers` (Ф3).
    #[tokio::test]
    async fn status_reports_disconnected_when_a_token_is_present() {
        let (_dir, app) = app_with(|c| c.access_token = SecretToken::new("vk1.a.x"));
        let json = body_json(get(app, "/api/vk/status").await).await;
        assert_eq!(json["login"]["state"], "disconnected");
    }

    /// ИНВАРИАНТ УТЕЧКИ, ПОЛНЫЙ ПО СОСТОЯНИЯМ (задача Ф3.4).
    ///
    /// Предыдущий тест проверяет один живой ответ; этот — ВСЕ СЕМЬ достижимых
    /// состояний автомата, и оба DTO (`StatusDto` и вложенный
    /// `LoginStateDto`), потому что состояние логина уходит клиенту двумя
    /// путями: REST-снапшотом и WS-кадром.
    ///
    /// Технически положить токен в DTO невозможно по типам (`SecretToken` не
    /// реализует `Serialize`), но тест фиксирует НАМЕРЕНИЕ и краснеет на
    /// мутации «добавить token_fingerprint со всем токеном» — а такое поле
    /// добавляется одной строкой и выглядит безобидно.
    #[tokio::test]
    async fn status_never_leaks_token_in_any_state() {
        const TOKEN: &str = "vk1.a.SECRET-CANARY_0123456789abcdef";

        let flows = vec![
            LoginFlow::NotConfigured,
            LoginFlow::Checking,
            LoginFlow::Disconnected {
                error: Some("connection reset".into()),
            },
            LoginFlow::InvalidToken {
                error: "VK error 5".into(),
            },
            LoginFlow::NoMessagesScope {
                error: "VK error 15".into(),
            },
            LoginFlow::CaptchaRequired {
                sid: "239123".into(),
                img: "https://api.vk.com/captcha.php?sid=239123".into(),
            },
            LoginFlow::Authorized {
                user: crate::login::VkUserDto {
                    id: "105431859".into(),
                    first_name: Some("Игорь".into()),
                    ..Default::default()
                },
            },
        ];
        assert_eq!(flows.len(), 7, "состояний ровно семь — см. LoginFlow");

        for flow in flows {
            let kind = flow.kind();
            let (dir, app) = app_with(|c| c.access_token = SecretToken::new(TOKEN));
            let state = VkState::new(VkConfig {
                access_token: SecretToken::new(TOKEN),
                cache_dir: dir.path().join("cache"),
                ..VkConfig::default()
            });
            state.set_login(flow.clone()).await;
            drop(app);

            // 1. Живой ответ роута в этом состоянии.
            let live = build_router(state.clone());
            let bytes = axum::body::to_bytes(
                get(live, "/api/vk/status").await.into_body(),
                256 * 1024,
            )
            .await
            .unwrap();
            let rendered = String::from_utf8(bytes.to_vec()).unwrap();

            // 2. И оба DTO по отдельности — на случай, если роут когда-нибудь
            //    начнёт отдавать урезанную проекцию.
            let login_dto = serde_json::to_string(&flow.to_dto()).unwrap();
            let status_dto = serde_json::to_string(&StatusDto {
                enabled: true,
                login: flow.to_dto(),
                api_version: state.config.api_version.clone(),
                rate_limit_rps: state.config.rate_limit_rps,
                hint: login::TOKEN_HINT.to_string(),
            })
            .unwrap();

            for (what, json) in [
                ("ответ /status", &rendered),
                ("LoginStateDto", &login_dto),
                ("StatusDto", &status_dto),
            ] {
                assert!(!json.contains(TOKEN), "{kind}: токен целиком в {what}: {json}");
                assert!(
                    !json.contains("SECRET-CANARY"),
                    "{kind}: часть токена в {what}: {json}"
                );
                assert!(
                    !json.contains("0123456789abcdef"),
                    "{kind}: хвост токена (отпечаток) в {what}: {json}"
                );
                assert!(!json.contains("fingerprint"), "{kind}: {what}: {json}");
            }
            assert!(
                rendered.contains(kind),
                "{kind}: состояние обязано доехать до клиента: {rendered}"
            );
        }
    }

    /// ГЛАВНЫЙ инвариант поверхности: токен не появляется в ответе `/status`
    /// ни в каком виде — ни целиком, ни отпечатком.
    #[tokio::test]
    async fn status_never_leaks_the_token() {
        const TOKEN: &str = "vk1.a.SUPERSECRETVALUE0123456789";
        let (_dir, app) = app_with(|c| c.access_token = SecretToken::new(TOKEN));
        let bytes = axum::body::to_bytes(
            get(app, "/api/vk/status").await.into_body(),
            64 * 1024,
        )
        .await
        .unwrap();
        let raw = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(!raw.contains(TOKEN), "токен целиком в ответе: {raw}");
        assert!(!raw.contains("SUPERSECRET"), "часть токена в ответе: {raw}");
        assert!(
            !raw.contains("0123456789"),
            "хвост токена (отпечаток) в ответе: {raw}"
        );
        assert!(!raw.contains("fingerprint"), "{raw}");
    }

    /// Путь загрузки (Ф8) смонтирован: GET по нему — 405 («метод не тот»), а
    /// не 404, а POST без аккаунта — `409 not_authorized`, причём ДО чтения
    /// тела. 404 здесь означал бы, что фронт не отличит выключенный плагин от
    /// опечатки в пути.
    #[tokio::test]
    async fn the_upload_route_is_mounted_and_demands_authorization() {
        let (_dir, router) = app();
        let resp = get(router, "/api/vk/upload").await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

        let (_dir2, router) = app();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/vk/upload")
                    .header("content-type", "application/octet-stream")
                    .header("content-length", "5")
                    .header(super::upload::FILE_NAME_HEADER, "a.txt")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
    }

    /// Медиа-роут (Ф7) смонтирован и держит СВОЙ порядок проверок: форма
    /// параметров — раньше авторизации (мусорный индекс это ошибка клиента в
    /// любом состоянии), а корректный запрос без аккаунта — `409`, а не `404`.
    ///
    /// Это единственный роут плагина, где разбор идёт до авторизации, и
    /// расхождение с остальными намеренное: `:index` описывает ФОРМУ запроса,
    /// а не существование объекта, поэтому по 400 ничего о чужих беседах
    /// узнать нельзя.
    #[tokio::test]
    async fn the_media_route_is_mounted_and_checks_the_shape_first() {
        for uri in [
            "/api/vk/media/1/2/10",
            "/api/vk/media/1/2/0?kind=thmub",
            "/api/vk/media/абв/2/0",
        ] {
            let (_dir, app) = app();
            let resp = get(app, uri).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
        let (_dir, app) = app();
        let resp = get(app, "/api/vk/media/1/2/0").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
    }

    /// А роуты данных (Ф4) смонтированы: без аккаунта они обязаны отвечать
    /// `409 not_authorized`, а НЕ `404` — иначе фронт не отличит «плагин без
    /// логина» от «такого пути нет» и покажет не тот экран.
    #[tokio::test]
    async fn data_routes_are_mounted_and_demand_authorization() {
        for uri in [
            "/api/vk/filters",
            "/api/vk/dialogs",
            "/api/vk/dialogs/1234/messages",
            "/api/vk/dialogs/1234/messages/7001",
        ] {
            let (_dir, app) = app();
            let resp = get(app, uri).await;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "not_authorized", "{uri}");
        }
    }

    /// ПОРЯДОК ПРОВЕРОК ИЗ КОНТРАКТА: авторизация — ПЕРВОЙ, до какого-либо
    /// разбора `:id`, `:mid` и query.
    ///
    /// Враждебные и вырожденные идентификаторы (path traversal, знак `+`,
    /// переполнение i64, emoji, XSS-строка) обязаны получать тот же
    /// `409 not_authorized`, что и честный id. Иной ответ позволил бы
    /// неавторизованному клиенту прощупывать валидность идентификаторов по
    /// различию кодов — то есть означал бы, что разбор ввода делается до
    /// аутентификации.
    ///
    /// Мусор в query проверяется здесь же: экстрактор `Query<T>` axum'а
    /// отработал бы ДО тела хендлера и отдал бы 400 раньше авторизации —
    /// именно поэтому query в `routes::dialogs` разбирается вручную.
    #[tokio::test]
    async fn hostile_dialog_ids_hit_auth_before_any_parsing() {
        let ids = [
            "..%2F..%2Fetc%2Fpasswd",              // path traversal
            "%2B79990000000",                      // «+» percent-encoded
            "+123",                                // «+» сырым символом
            "99999999999999999999999999",          // переполнение i64
            "0",
            "-0",
            "%F0%9F%98%80",                        // emoji
            "%3Cscript%3Ealert(1)%3C%2Fscript%3E", // XSS-строка
            "null",
            "2000000000000000000000",
        ];
        for id in ids {
            for tail in [
                "messages",
                "messages?limit=abc&before_id=%F0%9F%98%80",
                // Двухпараметрический путь: `:id` враждебный, `:mid` честный —
                // auth обязана сработать раньше обоих разборов.
                "messages/7001",
                "messages/not-a-number",
            ] {
                let (_dir, app) = app();
                let uri = format!("/api/vk/dialogs/{id}/{tail}");
                let resp = get(app, &uri).await;
                assert_eq!(
                    resp.status(),
                    StatusCode::CONFLICT,
                    "{uri}: авторизация обязана идти первой"
                );
                assert_eq!(body_json(resp).await["error"]["code"], "not_authorized", "{uri}");
            }
        }

        // Мутирующие роуты — по тому же правилу, включая заведомо битое тело:
        // `Option<Json<T>>` не должен отвергать запрос раньше авторизации.
        for id in ids {
            for (tail, body) in [
                ("messages", r#"{"text":"привет"}"#),
                ("messages", "{ битый json"),
                ("read", "{}"),
                ("typing", r#"{"action":"typing"}"#),
            ] {
                let (_dir, app) = app();
                let uri = format!("/api/vk/dialogs/{id}/{tail}");
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(&uri)
                            .header("content-type", "application/json")
                            .body(Body::from(body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    resp.status(),
                    StatusCode::CONFLICT,
                    "POST {uri}: авторизация обязана идти первой"
                );
                assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
            }
        }

        // И то же для списка бесед с мусорными параметрами страницы.
        for uri in [
            "/api/vk/dialogs?limit=abc",
            "/api/vk/dialogs?limit=-1&offset=nope",
            "/api/vk/dialogs?filter=../../etc/passwd",
            "/api/vk/dialogs?limit=99999999999999999999",
        ] {
            let (_dir, app) = app();
            let resp = get(app, uri).await;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "not_authorized", "{uri}");
        }
    }

    /// Вторая ступень лестницы: авторизация пройдена, но связи нет ⇒ `503
    /// not_connected` ДО резолва peer'а. Мусорный `:id` при этом всё равно
    /// получает 503, а не 404: «переподключись» — честный совет, пока мы даже
    /// не знаем, есть ли такая беседа.
    #[tokio::test]
    async fn unreachable_vk_answers_503_before_resolving_the_peer() {
        for uri in [
            "/api/vk/dialogs",
            "/api/vk/dialogs/1234/messages",
            "/api/vk/dialogs/..%2F..%2Fetc/messages",
            "/api/vk/dialogs/1234/messages/7001",
        ] {
            let (dir, _app) = app();
            let state = authorized_state(&dir).await;
            // Здоровье «упало прямо сейчас» ⇒ мы внутри грейса.
            state.note_down(crate::dto::now_unix(), "connection reset").await;

            let resp = get(build_router(state), uri).await;
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            let json = body_json(resp).await;
            assert_eq!(json["error"]["code"], "not_connected", "{uri}");
            assert!(
                json["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("connection reset"),
                "причина отказа обязана доехать до пользователя: {json}"
            );
        }
    }

    /// Третья ступень: связь есть, а `:id` не разбирается ⇒ `404
    /// unknown_dialog`, и это происходит ДО единого обращения к VK (`base_url`
    /// указывает на закрытый порт — сетевой вызов дал бы 503).
    #[tokio::test]
    async fn garbage_dialog_id_is_404_after_auth_and_health() {
        for id in ["..%2F..%2Fetc%2Fpasswd", "abc", "0", "%F0%9F%98%80"] {
            let (dir, _app) = app();
            let state = authorized_state(&dir).await;
            let resp = get(
                build_router(state),
                &format!("/api/vk/dialogs/{id}/messages"),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{id}");
            assert_eq!(body_json(resp).await["error"]["code"], "unknown_dialog", "{id}");
        }
    }

    /// Четвёртая ступень: всё предыдущее пройдено — и только теперь
    /// разбираются параметры. `/filters` при этом остаётся статичным: он
    /// требует авторизации, но НЕ соединения (503 на константный ответ был бы
    /// враньём) и не делает ни одного вызова VK.
    #[tokio::test]
    async fn filters_are_static_and_survive_a_broken_connection() {
        let (dir, _app) = app();
        let state = authorized_state(&dir).await;
        state.note_down(crate::dto::now_unix(), "connection reset").await;

        let resp = get(build_router(state), "/api/vk/filters").await;
        assert_eq!(resp.status(), StatusCode::OK, "фильтры не зависят от сети");
        let json = body_json(resp).await;
        let filters = json["filters"].as_array().unwrap();
        assert_eq!(filters.len(), 4);
        assert_eq!(
            filters
                .iter()
                .map(|f| f["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["all", "unread", "important", "unanswered"]
        );
        assert!(
            filters.iter().all(|f| !f["title"].as_str().unwrap().is_empty()),
            "{json}"
        );
        // Счётчиков в ответе быть не должно: каждый стоил бы отдельного вызова
        // при бюджете 3 rps.
        assert!(filters.iter().all(|f| f.as_object().unwrap().len() == 2), "{json}");
    }

    /// Мусорные параметры страницы у АВТОРИЗОВАННОГО клиента — уже честный
    /// `400 invalid_input` в нашем конверте (а не текстовый 400 axum'а).
    #[tokio::test]
    async fn malformed_query_is_a_contract_error_once_authorized() {
        for uri in [
            "/api/vk/dialogs?limit=abc",
            "/api/vk/dialogs?offset=1e3",
            "/api/vk/dialogs/1234/messages?limit=2.5",
            "/api/vk/dialogs/1234/messages?before_id=99999999999999999999",
        ] {
            let (dir, _app) = app();
            let state = authorized_state(&dir).await;
            let resp = get(build_router(state), uri).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "invalid_input", "{uri}");
        }
    }

    /// А вот группа логина смонтирована: GET по её POST-путям обязан давать
    /// 405 («метод не тот»), а НЕ 404 («такого роута нет») — иначе фронт не
    /// отличит выключенный плагин от опечатки в пути.
    #[tokio::test]
    async fn login_routes_are_mounted() {
        for uri in [
            "/api/vk/login/token",
            "/api/vk/login/verify",
            "/api/vk/logout",
            "/api/vk/captcha",
        ] {
            let (_dir, app) = app();
            let resp = get(app, uri).await;
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{uri}");
        }
        // Прокси картинки — как раз GET; неизвестный sid даёт 404 с телом
        // контракта, а не пустой ответ роутера.
        let (_dir, app) = app();
        let resp = get(app, "/api/vk/captcha/239").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(resp).await["error"]["code"], "no_media");
    }

    /// `/ws/vk` без апгрейда — это ошибка клиента (426/400), но НЕ 404 и не
    /// 500: роут существует и отличается от несмонтированных.
    #[tokio::test]
    async fn ws_route_exists_and_rejects_plain_http() {
        let (_dir, app) = app();
        let resp = get(app, "/ws/vk").await;
        assert_ne!(resp.status(), StatusCode::NOT_FOUND, "роут обязан быть");
        assert!(!resp.status().is_server_error(), "{}", resp.status());
    }

    /// Мусор в query WS-апгрейда не должен ломать роутинг (фронт дописывает
    /// туда токен и версию клиента).
    #[tokio::test]
    async fn ws_route_tolerates_query_garbage() {
        let (_dir, app) = app();
        let resp = get(app, "/ws/vk?token=&v=2&tab=7").await;
        assert!(!resp.status().is_server_error(), "{}", resp.status());
    }

    /// Форма `StatusDto` — контракт с `js/vk/main.js::initVkGate`: он читает
    /// `enabled` и `login.state`. Round-trip ловит опечатку в именах полей.
    #[test]
    fn status_dto_round_trips() {
        let dto = StatusDto {
            enabled: true,
            login: LoginStateDto {
                state: "authorized".into(),
                ..LoginStateDto::default()
            },
            api_version: "5.199".into(),
            rate_limit_rps: 3,
            hint: "вставьте адресную строку".into(),
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains(r#""login":{"state":"authorized""#), "{json}");
        let back: StatusDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }

    /// Подсказка про токен приходит с сервера — на этом держится решение «в
    /// разметке только контейнер». Пустая строка означала бы форму без единого
    /// объяснения, что именно вставлять.
    #[tokio::test]
    async fn status_carries_the_login_hint() {
        let (_dir, app) = app();
        let json = body_json(get(app, "/api/vk/status").await).await;
        let hint = json["hint"].as_str().unwrap_or_default();
        assert!(!hint.is_empty(), "{json}");
        assert_eq!(hint, super::login::TOKEN_HINT);
    }
}
