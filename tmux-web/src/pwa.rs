//! PWA HTTP-хендлеры + DTO (opt-in, активируется флагом `--pwa`).
//!
//! Фаза 1 даёт единственный публичный эндпоинт:
//!
//! `GET /api/pwa/config` → `{ "enabled": true, "vapidPublicKey": "<b64url>" }`
//!
//! Роут регистрируется в `main.rs` **только** при включённом PWA (по образцу
//! блока remotes), до auth-layer — поэтому в remote-mode он попадает под
//! Bearer-auth, как прочие `/api/*`. Без флага `--pwa` роут не существует и
//! `GET /api/pwa/config` отдаёт 404 (fallback на статику) — это и есть
//! сигнал фронтовому bootstrap'у «PWA выключено» (Фаза 4).
//!
//! `vapidPublicKey` — публичный VAPID-ключ (base64url-no-pad, 65-байт
//! uncompressed point), фронт скармливает его в
//! `pushManager.subscribe({ applicationServerKey })`.
//!
//! ## Фаза 2 — управление подписками
//!
//! Добавлены три мутирующих эндпоинта (регистрируются в `main.rs` тем же
//! `if state.pwa.is_some()`-блоком, что и `/api/pwa/config`, и так же ДО
//! auth-layer — значит в remote-mode попадают под Bearer-auth + `csrf_guard`,
//! который требует same-origin + `Content-Type: application/json`):
//!
//!   - `POST /api/push/subscribe` — тело = браузерный `PushSubscription`
//!     (`{ endpoint, keys:{p256dh,auth}, device_label? }`). Сервер ставит
//!     `created_at` (RFC3339) и делает `store.upsert` (дедуп по endpoint).
//!     → `200 { "ok": true }`.
//!   - `POST /api/push/unsubscribe` — тело `{ endpoint }`. `store.remove`,
//!     идемпотентно: повторная отписка тоже `200` (даже если endpoint уже
//!     удалён). → `200 { "ok": true }`.
//!   - `POST /api/push/test` — отправляет тестовый пуш всем подписчикам.
//!     **Фаза 2: заглушка** (реальная RFC8188-доставка — Фаза 3), сейчас
//!     всегда `200 { "sent": 0, "pruned": 0 }`.
//!
//! Без флага `--pwa` ни один из роутов не существует → 404.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::push_store::{StoredSubscription, SubscriptionKeys};
use crate::AppState;

/// Ответ `GET /api/pwa/config`. Сериализуется в camelCase: фронту удобнее
/// читать `vapidPublicKey` напрямую (это имя совпадает с тем, как ключ
/// используется в Web Push API на стороне браузера).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PwaConfig {
    /// Всегда `true` когда роут зарегистрирован: сам факт ответа 200 (а не
    /// 404) означает «PWA включено». Поле явное, чтобы фронт мог опираться на
    /// единый контракт `{ enabled, vapidPublicKey }`.
    pub enabled: bool,
    /// Публичный VAPID-ключ (base64url-no-pad, uncompressed P-256 point) —
    /// `applicationServerKey` для `pushManager.subscribe`.
    pub vapid_public_key: String,
}

/// Хендлер `GET /api/pwa/config`.
///
/// Роут регистрируется только при включённом PWA, поэтому `state.pwa`
/// практически всегда `Some`. На случай рассинхрона (теоретически
/// невозможного) возвращаем 404 вместо паники/500 — это сохраняет инвариант
/// «нет PWA → 404» и для фронта эквивалентно выключенному PWA.
pub async fn get_pwa_config(State(state): State<AppState>) -> Response {
    match &state.pwa {
        Some(ctx) => Json(PwaConfig {
            enabled: true,
            vapid_public_key: ctx.vapid.public_key_b64().to_string(),
        })
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// =============================================================================
// Фаза 2 — управление push-подписками
// =============================================================================

/// Ключи браузерной подписки в теле `POST /api/push/subscribe`. Совпадают по
/// форме с тем, что отдаёт `PushSubscription.toJSON().keys` в браузере
/// (camelCase-нейтральные имена `p256dh`/`auth`).
#[derive(Debug, Deserialize)]
pub struct SubscribeKeys {
    /// Публичный ECDH P-256 ключ браузера (base64url, uncompressed point).
    pub p256dh: String,
    /// Auth-secret подписки (base64url, 16 байт).
    pub auth: String,
}

/// Тело `POST /api/push/subscribe` — сериализованный браузерный
/// `PushSubscription` (`subscription.toJSON()`), плюс необязательная метка
/// устройства. Серверные поля (`created_at`) сюда НЕ входят — их ставит
/// сервер, чтобы клиент не мог подделать таймстемп.
#[derive(Debug, Deserialize)]
pub struct SubscribeReq {
    /// URL push-сервиса браузера — первичный ключ подписки.
    pub endpoint: String,
    /// ECDH/auth-ключи браузера.
    pub keys: SubscribeKeys,
    /// Опциональная метка устройства (для UI «мои устройства»).
    #[serde(default)]
    pub device_label: Option<String>,
}

/// Тело `POST /api/push/unsubscribe` — только `endpoint` удаляемой подписки.
#[derive(Debug, Deserialize)]
pub struct UnsubscribeReq {
    /// Endpoint подписки, которую нужно удалить.
    pub endpoint: String,
}

/// Универсальный `{ "ok": true }`-ответ для мутаций subscribe/unsubscribe.
#[derive(Debug, Serialize)]
pub struct OkResp {
    /// Всегда `true` при успехе (200). Явное поле, чтобы фронт мог проверять
    /// тело, а не только статус.
    pub ok: bool,
}

/// Ответ `POST /api/push/test`: сколько пушей успешно отправлено и сколько
/// мёртвых подписок отпрунено при доставке. Контракт стабилен с Фазы 2;
/// в Фазе 3 значения реальные — заполняются результатом `push::send_to_all`.
#[derive(Debug, Serialize)]
pub struct TestResp {
    /// Число подписок, которым пуш успешно доставлен.
    pub sent: usize,
    /// Число мёртвых подписок (404/410), удалённых при доставке.
    pub pruned: usize,
}

/// Хендлер `POST /api/push/subscribe`.
///
/// Сохраняет (upsert, дедуп по endpoint) браузерную подписку в
/// `~/.forge/push_subscriptions.json`. `created_at` ставит сервер (RFC3339,
/// UTC, секундная точность). Роут зарегистрирован только при включённом PWA,
/// но на случай рассинхрона `state.pwa == None` отвечаем 404 (как
/// `get_pwa_config`). Ошибка записи на диск → 500.
pub async fn post_push_subscribe(
    State(state): State<AppState>,
    Json(req): Json<SubscribeReq>,
) -> Response {
    let Some(ctx) = &state.pwa else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let sub = StoredSubscription {
        endpoint: req.endpoint,
        keys: SubscriptionKeys {
            p256dh: req.keys.p256dh,
            auth: req.keys.auth,
        },
        device_label: req.device_label,
        // Серверный таймстемп: клиент не задаёт created_at.
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    };

    match ctx.subs.upsert(sub) {
        Ok(()) => {
            tracing::info!("push subscription upserted");
            Json(OkResp { ok: true }).into_response()
        }
        Err(e) => {
            tracing::error!(error = ?format!("{e:#}"), "failed to persist push subscription");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to persist push subscription: {e:#}"),
            )
                .into_response()
        }
    }
}

/// Хендлер `POST /api/push/unsubscribe`.
///
/// Удаляет подписку по `endpoint`. Идемпотентен: повторная отписка (или
/// отписка несуществующего endpoint) тоже возвращает `200 { ok: true }` —
/// `store.remove` в этом случае не пишет на диск и возвращает `Ok(false)`.
/// Ошибка записи на диск (при фактическом удалении) → 500.
pub async fn post_push_unsubscribe(
    State(state): State<AppState>,
    Json(req): Json<UnsubscribeReq>,
) -> Response {
    let Some(ctx) = &state.pwa else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match ctx.subs.remove(&req.endpoint) {
        Ok(removed) => {
            tracing::info!(removed, "push unsubscribe processed");
            Json(OkResp { ok: true }).into_response()
        }
        Err(e) => {
            tracing::error!(error = ?format!("{e:#}"), "failed to remove push subscription");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to remove push subscription: {e:#}"),
            )
                .into_response()
        }
    }
}

/// Хендлер `POST /api/push/test`.
///
/// Шлёт тестовое уведомление всем сохранённым подпискам через
/// [`crate::push::send_to_all`]: каждая payload шифруется по RFC 8291
/// (`aes128gcm`) на чистом RustCrypto, к запросу прикладывается ES256-VAPID-JWT,
/// POST уходит на `subscription.endpoint`. Мёртвые подписки (404/410)
/// прунятся батчем; транзиентные/сетевые ошибки логируются и НЕ удаляют
/// подписку. Возвращает реальные `{ sent, pruned }`.
///
/// HTTP-клиент создаётся на вызов — `/api/push/test` дёргается редко
/// (ручная проверка), пул соединений тут не нужен; постоянный воркер
/// доставки (`push::attention_watcher`) держит свой клиент.
pub async fn post_push_test(State(state): State<AppState>) -> Response {
    let Some(ctx) = &state.pwa else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "failed to build reqwest client for push test");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build http client: {e}"),
            )
                .into_response();
        }
    };

    // Тестовый payload в том же контракте, что ждёт sw.js push-обработчик:
    // { title, body, data:{ url } }. Кириллица → обычный str (byte-string
    // литералы допускают только ASCII), берём байты.
    let payload =
        r#"{"title":"FORGE тест","body":"Push-уведомления работают","data":{"url":"/"}}"#
            .as_bytes();

    let report = crate::push::send_to_all(&client, &ctx.subs, &ctx.vapid, payload).await;
    tracing::info!(sent = report.sent, pruned = report.pruned, "push test delivered");
    Json(TestResp {
        sent: report.sent,
        pruned: report.pruned,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwa_config_serializes_camel_case() {
        // Контракт с фронтом: { "enabled": true, "vapidPublicKey": "..." }.
        let cfg = PwaConfig {
            enabled: true,
            vapid_public_key: "BExampleKey123".to_string(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"enabled\":true"), "got: {json}");
        assert!(
            json.contains("\"vapidPublicKey\":\"BExampleKey123\""),
            "camelCase ключ обязателен для фронта, got: {json}"
        );
        // snake_case-формы быть не должно.
        assert!(!json.contains("vapid_public_key"), "got: {json}");
    }

    #[test]
    fn subscribe_req_deserializes_browser_shape() {
        // Тело ровно как `PushSubscription.toJSON()` в браузере + наша
        // опциональная device_label.
        let body = r#"{
            "endpoint": "https://fcm.googleapis.com/fcm/send/abc123",
            "keys": { "p256dh": "BPubKey", "auth": "AuthSecret" },
            "device_label": "iPhone Сергея"
        }"#;
        let req: SubscribeReq = serde_json::from_str(body).unwrap();
        assert_eq!(req.endpoint, "https://fcm.googleapis.com/fcm/send/abc123");
        assert_eq!(req.keys.p256dh, "BPubKey");
        assert_eq!(req.keys.auth, "AuthSecret");
        assert_eq!(req.device_label.as_deref(), Some("iPhone Сергея"));
    }

    #[test]
    fn subscribe_req_device_label_is_optional() {
        // Браузер обычно НЕ присылает device_label — поле должно быть
        // необязательным (#[serde(default)]).
        let body = r#"{
            "endpoint": "https://updates.push.services.mozilla.com/wpush/v2/xyz",
            "keys": { "p256dh": "BPubKey", "auth": "AuthSecret" }
        }"#;
        let req: SubscribeReq = serde_json::from_str(body).unwrap();
        assert_eq!(req.device_label, None);
    }

    #[test]
    fn unsubscribe_req_deserializes() {
        let req: UnsubscribeReq =
            serde_json::from_str(r#"{"endpoint":"https://push.example/x"}"#).unwrap();
        assert_eq!(req.endpoint, "https://push.example/x");
    }

    #[test]
    fn ok_resp_serializes() {
        let json = serde_json::to_string(&OkResp { ok: true }).unwrap();
        assert_eq!(json, r#"{"ok":true}"#);
    }

    #[test]
    fn test_resp_stub_contract() {
        // Контракт `POST /api/push/test` (Фаза 2 заглушка): { sent, pruned }.
        let json = serde_json::to_string(&TestResp { sent: 0, pruned: 0 }).unwrap();
        assert!(json.contains("\"sent\":0"), "got: {json}");
        assert!(json.contains("\"pruned\":0"), "got: {json}");
    }

    #[test]
    fn server_created_at_is_rfc3339_utc() {
        // Сервер ставит created_at в формате RFC3339/UTC секундной точности
        // (как в post_push_subscribe). Проверяем формат, не значение.
        let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        // Завершается на 'Z' (UTC), не на '+00:00'.
        assert!(ts.ends_with('Z'), "RFC3339 UTC должен оканчиваться на Z: {ts}");
        // Парсится обратно как валидный RFC3339.
        let parsed = chrono::DateTime::parse_from_rfc3339(&ts);
        assert!(parsed.is_ok(), "created_at должен быть валидным RFC3339: {ts}");
        // Секундная точность — без дробной части.
        assert!(!ts.contains('.'), "Secs-точность без миллисекунд: {ts}");
    }

    // =========================================================================
    // Edge cases — lenient serde, отклонение битых тел, контракты ответов.
    // Тестируем реальный serde-контракт DTO напрямую (без AppState) — это
    // максимально надёжно и воспроизводит то, что делает axum Json-экстрактор.
    //
    // ПРИМЕЧАНИЕ: opt-in 404-инвариант (state.pwa == None → 404) и доставка
    // через mock-сервер требуют AppState/wiremock и покрыты на уровне
    // push::send_to_all (15 тестов в push.rs) и router-блока main.rs. Здесь —
    // именно DTO-слой.
    // =========================================================================

    /// КЛЮЧЕВОЙ кейс: реальный браузерный PushSubscription.toJSON() в Chrome
    /// ВСЕГДА содержит `expirationTime` (обычно null). serde должен молча
    /// игнорировать это поле (нет #[serde(deny_unknown_fields)]). Без этого
    /// теста регрессия добавления deny_unknown_fields тихо сломала бы браузеры.
    #[test]
    fn subscribe_req_ignores_expiration_time_null() {
        let body = r#"{
            "endpoint": "https://fcm.googleapis.com/fcm/send/abc123",
            "expirationTime": null,
            "keys": { "p256dh": "BPubKey", "auth": "AuthSecret" }
        }"#;
        let req: SubscribeReq = serde_json::from_str(body).expect("expirationTime игнорируется");
        assert_eq!(req.endpoint, "https://fcm.googleapis.com/fcm/send/abc123");
        assert_eq!(req.keys.p256dh, "BPubKey");
        assert_eq!(req.keys.auth, "AuthSecret");
    }

    /// expirationTime с числовым значением + ещё одно неизвестное поле
    /// (contentEncoding) — всё молча отбрасывается, SubscribeReq валиден.
    #[test]
    fn subscribe_req_ignores_numeric_expiration_and_extra_fields() {
        let body = r#"{
            "endpoint": "https://fcm.googleapis.com/fcm/send/xyz",
            "expirationTime": 1718000000000,
            "contentEncoding": "aes128gcm",
            "keys": { "p256dh": "B", "auth": "A" }
        }"#;
        let req: SubscribeReq = serde_json::from_str(body).expect("лишние поля отброшены");
        assert_eq!(req.endpoint, "https://fcm.googleapis.com/fcm/send/xyz");
    }

    /// subscribe отклоняет тело без keys (keys обязателен, нет serde(default)).
    /// На уровне axum это даёт 422 ДО входа в хендлер.
    #[test]
    fn subscribe_req_rejects_missing_keys() {
        let body = r#"{"endpoint":"https://x/y"}"#;
        let r = serde_json::from_str::<SubscribeReq>(body);
        assert!(r.is_err(), "keys обязателен → десериализация падает");
    }

    /// subscribe отклоняет тело без endpoint и без keys.auth.
    #[test]
    fn subscribe_req_rejects_missing_endpoint_and_auth() {
        // нет endpoint, нет keys.auth
        let body = r#"{"keys":{"p256dh":"B"}}"#;
        let r = serde_json::from_str::<SubscribeReq>(body);
        assert!(r.is_err(), "endpoint и keys.auth обязательны → падает");
    }

    /// subscribe отклоняет битый JSON, пустое тело и null.
    #[test]
    fn subscribe_req_rejects_malformed_bodies() {
        assert!(serde_json::from_str::<SubscribeReq>("{ битый").is_err());
        assert!(serde_json::from_str::<SubscribeReq>("").is_err());
        assert!(serde_json::from_str::<SubscribeReq>("null").is_err());
    }

    /// subscribe принимает большие/юникодные device_label и длинный endpoint
    /// без обрезки (utf-8 roundtrip).
    #[test]
    fn subscribe_req_unicode_label_and_long_endpoint() {
        let long_token = "x".repeat(180);
        let endpoint = format!("https://fcm.googleapis.com/fcm/send/{long_token}");
        let body = format!(
            r#"{{"endpoint":"{endpoint}","keys":{{"p256dh":"B","auth":"A"}},"device_label":"iPhone Сергея 📱"}}"#
        );
        let req: SubscribeReq = serde_json::from_str(&body).unwrap();
        assert_eq!(req.endpoint, endpoint);
        assert_eq!(req.endpoint.len(), endpoint.len(), "endpoint не обрезан");
        assert_eq!(req.device_label.as_deref(), Some("iPhone Сергея 📱"));
    }

    /// unsubscribe отклоняет тело без endpoint (endpoint обязателен).
    #[test]
    fn unsubscribe_req_rejects_missing_endpoint() {
        assert!(serde_json::from_str::<UnsubscribeReq>("{}").is_err());
        assert!(serde_json::from_str::<UnsubscribeReq>(r#"{"foo":"bar"}"#).is_err());
    }

    /// unsubscribe игнорирует лишние поля (нет deny_unknown_fields), но требует
    /// endpoint.
    #[test]
    fn unsubscribe_req_ignores_extra_fields() {
        let req: UnsubscribeReq =
            serde_json::from_str(r#"{"endpoint":"https://x","extra":1}"#).unwrap();
        assert_eq!(req.endpoint, "https://x");
    }

    /// PwaConfig с непустым ключом сериализуется в строгий camelCase и не
    /// содержит snake_case (дополняет pwa_config_serializes_camel_case
    /// проверкой что значение ключа сохраняется как есть).
    #[test]
    fn pwa_config_camel_case_preserves_key_value() {
        let cfg = PwaConfig {
            enabled: true,
            vapid_public_key: "BKey".to_string(),
        };
        let v: serde_json::Value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(v.get("enabled").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(
            v.get("vapidPublicKey").and_then(|x| x.as_str()),
            Some("BKey")
        );
        assert!(v.get("vapid_public_key").is_none(), "не должно быть snake_case");
    }

    /// TestResp/OkResp контракт стабилен (имена и значения полей, на которые
    /// опирается фронт sw.js/push.js).
    #[test]
    fn test_resp_and_ok_resp_field_contract() {
        let tr: serde_json::Value =
            serde_json::to_value(&TestResp { sent: 3, pruned: 2 }).unwrap();
        assert_eq!(tr.get("sent").and_then(|x| x.as_u64()), Some(3));
        assert_eq!(tr.get("pruned").and_then(|x| x.as_u64()), Some(2));

        let ok: serde_json::Value = serde_json::to_value(&OkResp { ok: true }).unwrap();
        assert_eq!(ok.get("ok").and_then(|x| x.as_bool()), Some(true));
        // OkResp ровно {"ok":true}, без лишних ключей.
        assert_eq!(ok.as_object().unwrap().len(), 1);
    }
}
