//! Web-push доставка (Фаза 3): VAPID-JWT + RFC 8291/8188 шифрование + воркер.
//!
//! Модуль шлёт «требуется внимание»-пуши на телефон, когда сессия Claude
//! поднимает permission-prompt. Состоит из трёх слоёв:
//!
//!   1. **Крипто (без openssl)** — VAPID JWT (`ES256`) и шифрование payload по
//!      RFC 8291 (`aes128gcm`, поверх RFC 8188) на чистом RustCrypto. Крейт
//!      `web-push` НЕ используется: он безусловно тянет `ece` с
//!      `backend-openssl` (у `ece` нет rust-crypto-фичи) и фиксирует hyper 0.14,
//!      несовместимый с axum 0.7. Весь стек проекта — rustls.
//!   2. **Транспорт** — [`send_one`] / [`send_to_all`]: HTTP POST на
//!      `subscription.endpoint` через общий `reqwest::Client` (rustls), с
//!      `tokio::time::timeout(10s)`. [`send_to_all`] классифицирует ошибки,
//!      собирает мёртвые endpoint'ы (404/410) и батчем прунит их из стора.
//!   3. **Воркер** — [`attention_watcher`]: раз в 1.5с снапшотит
//!      [`crate::attention::AttentionState`], на edge-trigger `false→true`
//!      шлёт пуш «требуется внимание». Антиспам: пока сессия остаётся `true`,
//!      повторно не шлём.
//!
//! ## RFC 8291 (`aes128gcm`) — шаги шифрования
//!
//! Дано: подписка браузера (`ua_public` = `p256dh` 65-байт uncompressed,
//! `auth_secret` 16 байт) и plaintext.
//!
//!   1. Генерируем эфемерную серверную пару P-256 (`as_private`/`as_public`).
//!   2. ECDH: `shared = ECDH(as_private, ua_public)` → 32-байтный x-координат.
//!   3. PRK_key = HKDF-SHA256(salt=`auth_secret`, IKM=`shared`,
//!      info=`"WebPush: info\0" || ua_public || as_public`, L=32).
//!   4. Случайный 16-байтный `ece_salt`.
//!   5. CEK   = HKDF-SHA256(salt=`ece_salt`, IKM=PRK_key,
//!      info=`"Content-Encoding: aes128gcm\0"`, L=16).
//!   6. NONCE = HKDF-SHA256(salt=`ece_salt`, IKM=PRK_key,
//!      info=`"Content-Encoding: nonce\0"`, L=12).
//!   7. Запись: `plaintext || 0x02` (delimiter последней записи, RFC 8188 §2.1),
//!      зашифровать AES-128-GCM(CEK, NONCE) → ciphertext+16-байт tag.
//!   8. ECE-заголовок (RFC 8188 §2.1):
//!      `ece_salt(16) || rs(4, BE) || idlen(1)=65 || as_public(65) || запись`.
//!
//! ## VAPID (RFC 8292)
//!
//! JWT = `b64url(header) || "." || b64url(claims) || "." || b64url(sig)`, где
//! header `{"typ":"JWT","alg":"ES256"}`, claims `{"aud","exp","sub"}`, а
//! `sig` — ECDSA P-256 (SHA-256) подпись signing-input'а в raw P1363-формате
//! (64 байта, `r||s`). HTTP-заголовок:
//! `Authorization: vapid t=<jwt>, k=<b64url(as_pub_vapid)>`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::ecdh::diffie_hellman;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::Signature;
use p256::{PublicKey, SecretKey};
use rand_core::OsRng;
use sha2::Sha256;

use crate::attention::AttentionState;
use crate::push_store::{PushSubscriptionStore, StoredSubscription};
use crate::vapid::VapidStore;

/// base64url-движок БЕЗ паддинга (RFC 4648 §5). Тот же формат, что в `vapid.rs`
/// и который браузер использует для `p256dh`/`auth` в `PushSubscription`.
const B64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Интервал тика воркера (как у `attention::watcher_loop` — 1.5с).
const TICK: Duration = Duration::from_millis(1500);

/// Таймаут одного HTTP POST на push-сервис.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Record size (RFC 8188) — пишем в ECE-заголовок. Берём щедрый верхний предел
/// браузерных push-сервисов (4096); реальный payload у нас крошечный.
const RECORD_SIZE: u32 = 4096;

/// Время жизни VAPID-JWT. RFC 8292 рекомендует ≤ 24ч; берём 12ч с запасом.
const VAPID_JWT_TTL_SECS: i64 = 12 * 60 * 60;

/// Классификация результата доставки одного пуша — определяет, прунить ли
/// подписку.
#[derive(Debug)]
pub enum PushError {
    /// Подписка мертва (push-сервис вернул 404 Not Found или 410 Gone) —
    /// браузер отозвал подписку. Её НУЖНО удалить из стора.
    Gone,
    /// Проблема аутентификации VAPID (401/403) — НЕ прунить подписку (виноват
    /// сервер/ключ, а не подписка), только залогировать.
    Auth(String),
    /// Транзиентная ошибка (5xx, сетевой сбой, таймаут) — НЕ прунить, можно
    /// повторить в следующий раз.
    Transient(String),
    /// Ошибка шифрования/построения запроса (битый ключ подписки и т.п.) —
    /// НЕ прунить автоматически (это баг данных, а не отзыв), логируем.
    Crypto(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushError::Gone => write!(f, "subscription gone (404/410)"),
            PushError::Auth(m) => write!(f, "vapid auth error: {m}"),
            PushError::Transient(m) => write!(f, "transient error: {m}"),
            PushError::Crypto(m) => write!(f, "crypto/build error: {m}"),
        }
    }
}

/// Итог отправки тестового/attention-пуша всем подписчикам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendReport {
    /// Сколько подписок получили пуш с успехом (2xx).
    pub sent: usize,
    /// Сколько мёртвых подписок (404/410) удалено из стора.
    pub pruned: usize,
}

// =============================================================================
// Крипто: RFC 8291 (aes128gcm) шифрование payload
// =============================================================================

/// Зашифрованный ECE-блок (RFC 8188 §2.1) — готовое тело HTTP-запроса плюс
/// эфемерный серверный публичный ключ (он уже встроен в заголовок блока, но
/// возвращаем отдельно для тестов/диагностики).
#[derive(Debug, Clone)]
pub struct EncryptedMessage {
    /// Полный ECE-блок: `salt(16) || rs(4) || idlen(1) || as_public(65) ||
    /// ciphertext`. Кладётся в тело POST, заголовок `Content-Encoding: aes128gcm`.
    pub body: Vec<u8>,
}

/// Зашифровать `plaintext` для одной подписки по RFC 8291 (`aes128gcm`).
///
/// `ua_public_b64` — `subscription.keys.p256dh` (65-байт uncompressed point,
/// base64url), `auth_b64` — `subscription.keys.auth` (16 байт, base64url).
///
/// Эфемерная серверная P-256 пара генерируется на каждый вызов (требование
/// RFC 8291: server key уникален на сообщение). При невалидных входных ключах
/// возвращает `Err` (вызывающий трактует как [`PushError::Crypto`]).
pub fn encrypt_payload(
    ua_public_b64: &str,
    auth_b64: &str,
    plaintext: &[u8],
) -> Result<EncryptedMessage, String> {
    // --- 1. Распарсить ключи подписки ---
    let ua_public_bytes = B64URL
        .decode(ua_public_b64.trim())
        .map_err(|e| format!("invalid base64url p256dh: {e}"))?;
    if ua_public_bytes.len() != 65 || ua_public_bytes[0] != 0x04 {
        return Err(format!(
            "p256dh must be 65-byte uncompressed point, got {} bytes",
            ua_public_bytes.len()
        ));
    }
    let ua_public = PublicKey::from_sec1_bytes(&ua_public_bytes)
        .map_err(|e| format!("p256dh is not a valid P-256 point: {e}"))?;

    let auth_secret = B64URL
        .decode(auth_b64.trim())
        .map_err(|e| format!("invalid base64url auth: {e}"))?;
    if auth_secret.len() != 16 {
        return Err(format!(
            "auth secret must be 16 bytes, got {}",
            auth_secret.len()
        ));
    }

    // --- 2. Эфемерная серверная пара + ECDH ---
    let as_secret = SecretKey::random(&mut OsRng);
    let as_public_point = as_secret.public_key().to_encoded_point(false);
    let as_public_bytes = as_public_point.as_bytes(); // 65-байт uncompressed
    debug_assert_eq!(as_public_bytes.len(), 65);

    let shared = diffie_hellman(as_secret.to_nonzero_scalar(), ua_public.as_affine());
    let shared_secret = shared.raw_secret_bytes(); // 32-байт x-координат

    // --- 3. PRK_key = HKDF(salt=auth, IKM=shared, info="WebPush: info\0"||ua||as) ---
    let mut key_info = Vec::with_capacity(14 + 65 + 65);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(&ua_public_bytes);
    key_info.extend_from_slice(as_public_bytes);

    let mut prk_key = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(Some(&auth_secret), shared_secret.as_slice());
    hk.expand(&key_info, &mut prk_key)
        .map_err(|e| format!("HKDF expand prk_key failed: {e}"))?;

    // --- 4. ece_salt (16 случайных байт) ---
    let mut ece_salt = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut OsRng, &mut ece_salt);

    // --- 5. CEK = HKDF(salt=ece_salt, IKM=prk_key, info="Content-Encoding: aes128gcm\0") ---
    let mut cek = [0u8; 16];
    let hk_cek = Hkdf::<Sha256>::new(Some(&ece_salt), &prk_key);
    hk_cek
        .expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .map_err(|e| format!("HKDF expand cek failed: {e}"))?;

    // --- 6. NONCE = HKDF(salt=ece_salt, IKM=prk_key, info="Content-Encoding: nonce\0") ---
    let mut nonce_bytes = [0u8; 12];
    let hk_nonce = Hkdf::<Sha256>::new(Some(&ece_salt), &prk_key);
    hk_nonce
        .expand(b"Content-Encoding: nonce\0", &mut nonce_bytes)
        .map_err(|e| format!("HKDF expand nonce failed: {e}"))?;

    // --- 7. Запись: plaintext || 0x02 (delimiter последней записи) → AES-128-GCM ---
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(0x02); // RFC 8188 §2: padding delimiter последней (и единственной) записи

    let cipher = Aes128Gcm::new_from_slice(&cek)
        .map_err(|e| format!("AES key init failed: {e}"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: &record,
                aad: &[],
            },
        )
        .map_err(|e| format!("AES-128-GCM encrypt failed: {e}"))?;

    // --- 8. ECE-заголовок (RFC 8188 §2.1) + ciphertext ---
    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + ciphertext.len());
    body.extend_from_slice(&ece_salt); // salt (16)
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes()); // rs (4, big-endian)
    body.push(65u8); // idlen (1) — длина keyid = длина server pubkey
    body.extend_from_slice(as_public_bytes); // keyid = server public key (65)
    body.extend_from_slice(&ciphertext); // зашифрованная запись + tag

    Ok(EncryptedMessage { body })
}

// =============================================================================
// Крипто: VAPID JWT (ES256, RFC 8292)
// =============================================================================

/// Построить VAPID-`Authorization`-заголовок для запроса на `endpoint`.
///
/// `aud` извлекается из endpoint'а как `scheme://host[:port]` (origin
/// push-сервиса). Возвращает готовое значение заголовка:
/// `vapid t=<jwt>, k=<b64url(vapid_public_uncompressed)>`.
pub fn vapid_authorization_header(
    vapid: &VapidStore,
    endpoint: &str,
    now_unix: i64,
) -> Result<String, String> {
    let aud = origin_of(endpoint).ok_or_else(|| format!("cannot derive origin from endpoint: {endpoint}"))?;
    let jwt = build_vapid_jwt(vapid, &aud, now_unix)?;
    Ok(format!("vapid t={jwt}, k={}", vapid.public_key_b64()))
}

/// Извлечь `scheme://host[:port]` (origin без пути) из URL endpoint'а — это
/// `aud` VAPID-JWT (RFC 8292 §2). Без внешних URL-крейтов: простой парс.
fn origin_of(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let scheme = &url[..scheme_end];
    let rest = &url[scheme_end + 3..];
    let host = match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    };
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// Собрать и подписать VAPID-JWT (`ES256`).
///
/// claims: `{"aud":<origin>,"exp":<now+12h>,"sub":"mailto:..."}`. Подпись —
/// ECDSA P-256 (SHA-256), raw P1363 (64 байта, `r||s`), как требует JWS ES256.
fn build_vapid_jwt(vapid: &VapidStore, aud: &str, now_unix: i64) -> Result<String, String> {
    // header: {"typ":"JWT","alg":"ES256"}
    let header_b64 = B64URL.encode(br#"{"typ":"JWT","alg":"ES256"}"#);

    let exp = now_unix + VAPID_JWT_TTL_SECS;
    // sub: контактный URL/mailto администратора (RFC 8292 §2.1). Берём дефолт —
    // конкретный mailto не критичен для большинства push-сервисов, но поле
    // должно присутствовать.
    let claims = format!(r#"{{"aud":"{aud}","exp":{exp},"sub":"mailto:admin@forge.local"}}"#);
    let claims_b64 = B64URL.encode(claims.as_bytes());

    let signing_input = format!("{header_b64}.{claims_b64}");

    // ES256: ECDSA P-256 + SHA-256, подпись в fixed-size P1363 (r||s, 64 байта).
    let signature: Signature = vapid.signing_key().sign(signing_input.as_bytes());
    let sig_bytes = signature.to_bytes(); // 64-байт P1363
    let sig_b64 = B64URL.encode(sig_bytes);

    Ok(format!("{signing_input}.{sig_b64}"))
}

// =============================================================================
// Транспорт: send_one / send_to_all
// =============================================================================

/// Отправить один пуш на подписку. Шифрует `payload` по RFC 8291, строит
/// VAPID-заголовок и делает POST на `sub.endpoint` (с таймаутом 10с).
///
/// Классификация результата → [`PushError`]:
///   - 2xx → `Ok(())`;
///   - 404/410 → `Err(Gone)` (прунить);
///   - 401/403 → `Err(Auth)` (не прунить);
///   - 5xx / сетевой сбой / таймаут → `Err(Transient)`;
///   - ошибка шифрования/ключей → `Err(Crypto)`.
pub async fn send_one(
    client: &reqwest::Client,
    sub: &StoredSubscription,
    vapid: &VapidStore,
    payload: &[u8],
) -> Result<(), PushError> {
    let encrypted = encrypt_payload(&sub.keys.p256dh, &sub.keys.auth, payload)
        .map_err(PushError::Crypto)?;

    let now_unix = chrono::Utc::now().timestamp();
    let auth_header = vapid_authorization_header(vapid, &sub.endpoint, now_unix)
        .map_err(PushError::Crypto)?;

    let request = client
        .post(&sub.endpoint)
        .header("Authorization", auth_header)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", "60")
        .header("Urgency", "high")
        .body(encrypted.body)
        .send();

    let resp = match tokio::time::timeout(SEND_TIMEOUT, request).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(PushError::Transient(format!("request failed: {e}"))),
        Err(_) => return Err(PushError::Transient("send timed out (10s)".to_string())),
    };

    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }

    match status.as_u16() {
        404 | 410 => Err(PushError::Gone),
        401 | 403 => {
            let body = resp.text().await.unwrap_or_default();
            Err(PushError::Auth(format!("status {status}: {body}")))
        }
        code if code >= 500 => Err(PushError::Transient(format!("server error {status}"))),
        _ => {
            let body = resp.text().await.unwrap_or_default();
            Err(PushError::Transient(format!("status {status}: {body}")))
        }
    }
}

/// Отправить `payload` всем подписям из стора. Мёртвые endpoint'ы (404/410)
/// собираются и батчем прунятся из `subs`. Возвращает [`SendReport`]
/// `{ sent, pruned }`.
///
/// Транзиентные/auth/crypto-ошибки логируются, но подписку НЕ удаляют.
pub async fn send_to_all(
    client: &reqwest::Client,
    subs: &PushSubscriptionStore,
    vapid: &VapidStore,
    payload: &[u8],
) -> SendReport {
    let list = subs.list();
    let mut sent = 0usize;
    let mut dead: Vec<String> = Vec::new();

    for sub in &list {
        match send_one(client, sub, vapid, payload).await {
            Ok(()) => sent += 1,
            Err(PushError::Gone) => {
                tracing::info!(endpoint = %sub.endpoint, "push subscription gone; pruning");
                dead.push(sub.endpoint.clone());
            }
            Err(e) => {
                tracing::warn!(endpoint = %sub.endpoint, error = %e, "push delivery failed (not pruning)");
            }
        }
    }

    let pruned = if dead.is_empty() {
        0
    } else {
        match subs.prune(&dead) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(error = ?format!("{e:#}"), "failed to prune dead push subscriptions");
                0
            }
        }
    };

    SendReport { sent, pruned }
}

// =============================================================================
// JSON-payload для уведомления (контракт с sw.js)
// =============================================================================

/// Построить JSON-тело уведомления, ожидаемое `sw.js` push-обработчиком:
/// `{ "title", "body", "data": { "session", "url" } }`. Все строковые значения
/// JSON-экранируются.
pub fn attention_payload(session: &str, base_url: &str) -> Vec<u8> {
    let title = "Требуется внимание";
    let body = format!("Claude ждёт ответа в сессии {session}");
    let url = session_url(base_url, session);
    let json = format!(
        r#"{{"title":{title},"body":{body},"data":{{"session":{session},"url":{url}}}}}"#,
        title = json_string(title),
        body = json_string(&body),
        session = json_string(session),
        url = json_string(&url),
    );
    json.into_bytes()
}

/// Построить URL для перехода из уведомления: `base_url` + якорь на сессию.
/// `base_url` обычно `/`; URL-кодируем имя сессии в query, чтобы фронт мог
/// выбрать вкладку.
fn session_url(base_url: &str, session: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{base}/?session={}", urlencode_component(session))
}

/// Минимальный JSON-эскейп строки (RFC 8259): оборачивает в кавычки и
/// экранирует `"`, `\`, управляющие символы. Без внешних крейтов.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Percent-encode для query-компоненты (RFC 3986 unreserved остаётся as-is).
fn urlencode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// =============================================================================
// Воркер: attention_watcher
// =============================================================================

/// Фоновый воркер: следит за [`AttentionState`] и шлёт пуш на переходе
/// `false→true` (как загорается оранжевый индикатор).
///
/// Логика:
///   - раз в [`TICK`] (1.5с) снапшотит `attention.snapshot()`
///     (`HashMap<session, needs_attention>`);
///   - держит `prev: HashMap<session, bool>`; для каждой сессии с
///     `prev=false/отсутствует → now=true` шлёт пуш (edge-trigger);
///   - **антиспам**: пока сессия остаётся `true`, повторно не шлём;
///   - `prev = snap` целиком — если сессия исчезла из снапшота и потом
///     вернулась, это новый эпизод (повторный пуш разрешён).
///
/// Один `reqwest::Client` создаётся на воркер и переиспользуется (пул
/// соединений). `attention.rs` НЕ модифицируется — только читаем снапшот.
pub async fn attention_watcher(
    attention: Arc<AttentionState>,
    subs: PushSubscriptionStore,
    vapid: VapidStore,
    base_url: String,
) {
    tracing::info!("push attention_watcher started (interval 1.5s)");

    let client = match reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "failed to build reqwest client; push worker disabled");
            return;
        }
    };

    let mut prev: HashMap<String, bool> = HashMap::new();
    let mut ticker = tokio::time::interval(TICK);

    loop {
        ticker.tick().await;
        let snap = attention.snapshot().await;

        // Edge-trigger false→true (или отсутствие→true) — собираем сессии,
        // которым нужно отправить пуш на этом тике.
        let newly_attention: Vec<String> = snap
            .iter()
            .filter(|(session, &now)| now && !prev.get(*session).copied().unwrap_or(false))
            .map(|(session, _)| session.clone())
            .collect();

        for session in &newly_attention {
            let payload = attention_payload(session, &base_url);
            let report = send_to_all(&client, &subs, &vapid, &payload).await;
            tracing::info!(
                session = %session,
                sent = report.sent,
                pruned = report.pruned,
                "attention push dispatched"
            );
        }

        // prev = snap целиком: исчезнувшая сессия не остаётся «true» в prev,
        // поэтому повторное появление = новый эпизод.
        prev = snap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    /// Сгенерировать тестовую «браузерную» подписку: P-256 пара (как ключ
    /// устройства) + 16-байт auth. Возвращает (p256dh_b64, auth_b64,
    /// ua_secret) — секрет нужен, чтобы в тесте расшифровать ECE-блок и
    /// проверить roundtrip.
    fn make_subscription() -> (String, String, SecretKey) {
        let ua_secret = SecretKey::random(&mut OsRng);
        let ua_public = ua_secret.public_key().to_encoded_point(false);
        let p256dh = B64URL.encode(ua_public.as_bytes());
        let mut auth = [0u8; 16];
        rand_core::RngCore::fill_bytes(&mut OsRng, &mut auth);
        (p256dh, B64URL.encode(auth), ua_secret)
    }

    #[test]
    fn ece_header_structure_is_valid() {
        let (p256dh, auth, _ua_secret) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"hello").unwrap();

        // Заголовок: salt(16) + rs(4) + idlen(1) + keyid(65) + ciphertext(>=17).
        assert!(
            msg.body.len() >= 16 + 4 + 1 + 65 + 17,
            "ECE-блок слишком короткий: {}",
            msg.body.len()
        );
        // rs (байты 16..20) == RECORD_SIZE big-endian.
        let rs = u32::from_be_bytes([msg.body[16], msg.body[17], msg.body[18], msg.body[19]]);
        assert_eq!(rs, RECORD_SIZE, "record size в заголовке");
        // idlen (байт 20) == 65.
        assert_eq!(msg.body[20], 65, "idlen = длина server pubkey");
        // keyid (байты 21..86) — server public key, начинается с 0x04
        // (uncompressed point).
        assert_eq!(msg.body[21], 0x04, "server pubkey — uncompressed point");
    }

    #[test]
    fn ece_roundtrip_decrypts_to_plaintext() {
        // Полный roundtrip: зашифровали серверным кодом, расшифровали как UA.
        let (p256dh, auth_b64, ua_secret) = make_subscription();
        let plaintext = b"{\"title\":\"hi\"}";
        let msg = encrypt_payload(&p256dh, &auth_b64, plaintext).unwrap();

        // Разбираем ECE-заголовок.
        let body = &msg.body;
        let ece_salt = &body[0..16];
        let idlen = body[20] as usize;
        assert_eq!(idlen, 65);
        let as_public_bytes = &body[21..21 + idlen];
        let ciphertext = &body[21 + idlen..];

        // UA-сторона воспроизводит ту же key-derivation.
        let as_public = PublicKey::from_sec1_bytes(as_public_bytes).unwrap();
        let shared =
            diffie_hellman(ua_secret.to_nonzero_scalar(), as_public.as_affine());
        let shared_secret = shared.raw_secret_bytes();

        let ua_public_bytes = ua_secret.public_key().to_encoded_point(false);
        let auth_secret = B64URL.decode(&auth_b64).unwrap();

        let mut key_info = Vec::new();
        key_info.extend_from_slice(b"WebPush: info\0");
        key_info.extend_from_slice(ua_public_bytes.as_bytes());
        key_info.extend_from_slice(as_public_bytes);

        let mut prk_key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&auth_secret), shared_secret.as_slice())
            .expand(&key_info, &mut prk_key)
            .unwrap();

        let mut cek = [0u8; 16];
        Hkdf::<Sha256>::new(Some(ece_salt), &prk_key)
            .expand(b"Content-Encoding: aes128gcm\0", &mut cek)
            .unwrap();
        let mut nonce_bytes = [0u8; 12];
        Hkdf::<Sha256>::new(Some(ece_salt), &prk_key)
            .expand(b"Content-Encoding: nonce\0", &mut nonce_bytes)
            .unwrap();

        let cipher = Aes128Gcm::new_from_slice(&cek).unwrap();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let decrypted = cipher
            .decrypt(nonce, Payload { msg: ciphertext, aad: &[] })
            .expect("UA должен расшифровать серверный ECE-блок");

        // Последний байт — delimiter 0x02 (RFC 8188); остальное == plaintext.
        assert_eq!(*decrypted.last().unwrap(), 0x02, "padding delimiter");
        assert_eq!(&decrypted[..decrypted.len() - 1], plaintext);
    }

    #[test]
    fn encrypt_rejects_bad_p256dh() {
        // Неверная длина p256dh → Err, без паники.
        let bad = B64URL.encode([0u8; 10]);
        let auth = B64URL.encode([0u8; 16]);
        assert!(encrypt_payload(&bad, &auth, b"x").is_err());
    }

    #[test]
    fn encrypt_rejects_bad_auth_len() {
        let (p256dh, _auth, _s) = make_subscription();
        let bad_auth = B64URL.encode([0u8; 8]); // не 16 байт
        assert!(encrypt_payload(&p256dh, &bad_auth, b"x").is_err());
    }

    #[test]
    fn each_encryption_uses_fresh_server_key_and_salt() {
        // RFC 8291: server key и salt уникальны на сообщение.
        let (p256dh, auth, _s) = make_subscription();
        let a = encrypt_payload(&p256dh, &auth, b"same").unwrap();
        let b = encrypt_payload(&p256dh, &auth, b"same").unwrap();
        // salt (0..16) различается.
        assert_ne!(&a.body[0..16], &b.body[0..16], "salt должен быть свежим");
        // server pubkey (21..86) различается.
        assert_ne!(&a.body[21..86], &b.body[21..86], "server key должен быть свежим");
    }

    fn test_vapid() -> VapidStore {
        // Генерируем VapidStore через временный файл (load_or_generate).
        let dir = std::env::temp_dir().join(format!(
            "forge-push-vapid-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vapid.json");
        let store = VapidStore::load_or_generate(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        store
    }

    #[test]
    fn vapid_jwt_verifies_with_own_pubkey() {
        let vapid = test_vapid();
        let aud = "https://fcm.googleapis.com";
        let now = 1_700_000_000i64;
        let jwt = build_vapid_jwt(&vapid, aud, now).unwrap();

        // Структура: header.claims.sig
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT = header.claims.sig");

        // header декодируется и содержит alg ES256.
        let header = B64URL.decode(parts[0]).unwrap();
        let header_str = String::from_utf8(header).unwrap();
        assert!(header_str.contains("ES256"), "alg=ES256: {header_str}");

        // claims содержат aud/exp/sub.
        let claims = String::from_utf8(B64URL.decode(parts[1]).unwrap()).unwrap();
        assert!(claims.contains(aud), "aud в claims: {claims}");
        assert!(claims.contains("\"exp\""), "exp в claims: {claims}");
        assert!(claims.contains("\"sub\""), "sub в claims: {claims}");
        assert!(
            claims.contains(&(now + VAPID_JWT_TTL_SECS).to_string()),
            "exp = now + ttl: {claims}"
        );

        // Подпись верифицируется публичным ключом этого же VapidStore.
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = B64URL.decode(parts[2]).unwrap();
        assert_eq!(sig_bytes.len(), 64, "ES256 sig = 64 байта P1363");
        let sig = Signature::from_slice(&sig_bytes).unwrap();

        // Восстанавливаем VerifyingKey из публичного ключа store.
        let pub_bytes = B64URL.decode(vapid.public_key_b64()).unwrap();
        let vk = VerifyingKey::from_sec1_bytes(&pub_bytes).unwrap();
        assert!(
            vk.verify(signing_input.as_bytes(), &sig).is_ok(),
            "VAPID JWT должен верифицироваться своим же публичным ключом"
        );
    }

    #[test]
    fn vapid_authorization_header_format() {
        let vapid = test_vapid();
        let header = vapid_authorization_header(
            &vapid,
            "https://fcm.googleapis.com/fcm/send/abc123",
            1_700_000_000,
        )
        .unwrap();
        // Формат: "vapid t=<jwt>, k=<pubkey>"
        assert!(header.starts_with("vapid t="), "header: {header}");
        assert!(header.contains(", k="), "header: {header}");
        assert!(
            header.contains(vapid.public_key_b64()),
            "k= должен содержать публичный ключ: {header}"
        );
    }

    #[test]
    fn origin_extraction() {
        assert_eq!(
            origin_of("https://fcm.googleapis.com/fcm/send/abc"),
            Some("https://fcm.googleapis.com".to_string())
        );
        assert_eq!(
            origin_of("https://updates.push.services.mozilla.com/wpush/v2/xyz"),
            Some("https://updates.push.services.mozilla.com".to_string())
        );
        // С портом.
        assert_eq!(
            origin_of("http://localhost:8080/push/abc"),
            Some("http://localhost:8080".to_string())
        );
        // Без пути.
        assert_eq!(
            origin_of("https://host.example"),
            Some("https://host.example".to_string())
        );
        // Мусор.
        assert_eq!(origin_of("not a url"), None);
    }

    #[test]
    fn attention_payload_is_valid_json_with_contract_fields() {
        let payload = attention_payload("my-session", "/");
        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed["title"], "Требуется внимание");
        assert!(
            parsed["body"].as_str().unwrap().contains("my-session"),
            "body упоминает сессию"
        );
        assert_eq!(parsed["data"]["session"], "my-session");
        assert!(
            parsed["data"]["url"].as_str().unwrap().contains("session=my-session"),
            "url содержит session query"
        );
    }

    #[test]
    fn attention_payload_escapes_special_chars() {
        // Имя сессии с кавычкой/бэкслешем не должно ломать JSON.
        let payload = attention_payload("a\"b\\c", "/");
        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed["data"]["session"], "a\"b\\c");
    }

    #[test]
    fn session_url_encodes_session() {
        // base_url '/' и имя с пробелом/слешем → query-encoded.
        let url = session_url("/", "proj/sess 1");
        assert!(url.starts_with("/?session="), "url: {url}");
        assert!(url.contains("proj%2Fsess%201"), "url: {url}");
        // base_url с trailing slash не двоится.
        let url2 = session_url("https://x.test/", "s");
        assert_eq!(url2, "https://x.test/?session=s");
    }

    // ---- edge-trigger логика воркера (без сети) ----
    //
    // Воркер сам по себе крутит бесконечный loop + сеть, поэтому тестируем
    // чистую edge-trigger-логику отдельной функцией, идентичной той, что
    // внутри attention_watcher.

    /// Чистая edge-trigger функция: возвращает список сессий, которым нужно
    /// слать пуш (переход false→true относительно `prev`). Зеркалит логику
    /// внутри [`attention_watcher`].
    fn newly_attention(prev: &HashMap<String, bool>, snap: &HashMap<String, bool>) -> Vec<String> {
        let mut v: Vec<String> = snap
            .iter()
            .filter(|(s, &now)| now && !prev.get(*s).copied().unwrap_or(false))
            .map(|(s, _)| s.clone())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn edge_trigger_fires_once_on_false_to_true() {
        let mut prev: HashMap<String, bool> = HashMap::new();

        // Тик 1: сессия появляется как true (нет в prev) → один триггер.
        let snap1: HashMap<String, bool> = [("s".to_string(), true)].into_iter().collect();
        assert_eq!(newly_attention(&prev, &snap1), vec!["s".to_string()]);
        prev = snap1;

        // Тик 2: сессия всё ещё true → НЕ триггерит (антиспам).
        let snap2: HashMap<String, bool> = [("s".to_string(), true)].into_iter().collect();
        assert!(newly_attention(&prev, &snap2).is_empty(), "антиспам: пока true — не слать");
        prev = snap2;

        // Тик 3: сессия гаснет (false) → нет триггера.
        let snap3: HashMap<String, bool> = [("s".to_string(), false)].into_iter().collect();
        assert!(newly_attention(&prev, &snap3).is_empty());
        prev = snap3;

        // Тик 4: снова true (false→true) → новый триггер.
        let snap4: HashMap<String, bool> = [("s".to_string(), true)].into_iter().collect();
        assert_eq!(newly_attention(&prev, &snap4), vec!["s".to_string()]);
    }

    #[test]
    fn edge_trigger_reappearing_session_is_new_episode() {
        // prev = snap целиком: исчезнувшая сессия → при возврате новый эпизод.
        let mut prev: HashMap<String, bool> =
            [("s".to_string(), true)].into_iter().collect();

        // Сессия исчезла из снапшота (закрыли вкладку/tmux-сессию).
        let snap_gone: HashMap<String, bool> = HashMap::new();
        assert!(newly_attention(&prev, &snap_gone).is_empty());
        prev = snap_gone; // prev = snap целиком — "s" больше нет

        // Сессия вернулась как true → новый эпизод, триггерит.
        let snap_back: HashMap<String, bool> =
            [("s".to_string(), true)].into_iter().collect();
        assert_eq!(newly_attention(&prev, &snap_back), vec!["s".to_string()]);
    }

    #[test]
    fn edge_trigger_multiple_sessions_independent() {
        let prev: HashMap<String, bool> =
            [("a".to_string(), true), ("b".to_string(), false)]
                .into_iter()
                .collect();
        // a остаётся true (нет триггера), b переходит false→true (триггер),
        // c новый true (триггер).
        let snap: HashMap<String, bool> = [
            ("a".to_string(), true),
            ("b".to_string(), true),
            ("c".to_string(), true),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            newly_attention(&prev, &snap),
            vec!["b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn send_report_fields() {
        let r = SendReport { sent: 3, pruned: 1 };
        assert_eq!(r.sent, 3);
        assert_eq!(r.pruned, 1);
    }

    // =========================================================================
    // ECE (RFC 8291/8188) — дополнительные краевые случаи.
    // =========================================================================

    /// UA-сторона ECE: воспроизводит ключевую деривацию из заголовка `body`
    /// (опционально с подменённым salt / переставленным порядком ua||as /
    /// непустым AAD для негативных под-кейсов) и расшифровывает запись.
    /// Эталон стиля — ece_roundtrip_decrypts_to_plaintext.
    fn ua_decrypt(
        body: &[u8],
        ua_secret: &SecretKey,
        auth_b64: &str,
        salt_override: Option<&[u8]>,
        swap_ua_as: bool,
        aad: &[u8],
    ) -> Result<Vec<u8>, aes_gcm::aead::Error> {
        let ece_salt: &[u8] = salt_override.unwrap_or(&body[0..16]);
        let idlen = body[20] as usize;
        let as_public_bytes = &body[21..21 + idlen];
        let ciphertext = &body[21 + idlen..];

        let as_public = PublicKey::from_sec1_bytes(as_public_bytes).unwrap();
        let shared = diffie_hellman(ua_secret.to_nonzero_scalar(), as_public.as_affine());
        let shared_secret = shared.raw_secret_bytes();

        let ua_public_bytes = ua_secret.public_key().to_encoded_point(false);
        let auth_secret = B64URL.decode(auth_b64).unwrap();

        let mut key_info = Vec::new();
        key_info.extend_from_slice(b"WebPush: info\0");
        if swap_ua_as {
            // Намеренно неверный порядок (as || ua вместо ua || as).
            key_info.extend_from_slice(as_public_bytes);
            key_info.extend_from_slice(ua_public_bytes.as_bytes());
        } else {
            key_info.extend_from_slice(ua_public_bytes.as_bytes());
            key_info.extend_from_slice(as_public_bytes);
        }

        let mut prk_key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&auth_secret), shared_secret.as_slice())
            .expand(&key_info, &mut prk_key)
            .unwrap();

        let mut cek = [0u8; 16];
        Hkdf::<Sha256>::new(Some(ece_salt), &prk_key)
            .expand(b"Content-Encoding: aes128gcm\0", &mut cek)
            .unwrap();
        let mut nonce_bytes = [0u8; 12];
        Hkdf::<Sha256>::new(Some(ece_salt), &prk_key)
            .expand(b"Content-Encoding: nonce\0", &mut nonce_bytes)
            .unwrap();

        let cipher = Aes128Gcm::new_from_slice(&cek).unwrap();
        let nonce = Nonce::from_slice(&nonce_bytes);
        cipher.decrypt(nonce, Payload { msg: ciphertext, aad })
    }

    #[test]
    fn ece_roundtrip_empty_payload() {
        let (p256dh, auth, ua_secret) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"").unwrap();

        // ciphertext = record(0 + 1 delimiter) + 16 tag = 17 байт.
        let ciphertext_len = msg.body.len() - (16 + 4 + 1 + 65);
        assert_eq!(ciphertext_len, 17, "пустой payload: ciphertext = 0+1+16");
        // Полная длина body = 86 + 17 = 103.
        assert_eq!(msg.body.len(), 103);

        let decrypted = ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).unwrap();
        assert_eq!(decrypted, vec![0x02], "только delimiter после расшифровки");
        assert!(decrypted[..decrypted.len() - 1].is_empty(), "пустой plaintext");
    }

    #[test]
    fn ece_roundtrip_various_payloads_exact_bytes() {
        let cases: Vec<Vec<u8>> = vec![
            br#"{"title":"hi","n":1}"#.to_vec(),
            "Привет, мир! 🚀🔥".as_bytes().to_vec(), // UTF-8 кириллица+эмодзи
            vec![0x00, 0x01, 0x02, 0x00, 0xFF],      // бинарь с нулями
        ];
        for plaintext in &cases {
            let (p256dh, auth, ua_secret) = make_subscription();
            let msg = encrypt_payload(&p256dh, &auth, plaintext).unwrap();
            let decrypted = ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).unwrap();
            assert_eq!(*decrypted.last().unwrap(), 0x02, "delimiter в конце");
            assert_eq!(
                &decrypted[..decrypted.len() - 1],
                plaintext.as_slice(),
                "plaintext восстановлен побайтово"
            );
        }
    }

    #[test]
    fn ece_payload_at_record_boundary() {
        // record = plaintext + delimiter(1); +16 tag должно дать ровно RECORD_SIZE.
        let pt_len = (RECORD_SIZE as usize) - 16 - 1; // 4079
        let plaintext = vec![0xAB_u8; pt_len];
        let (p256dh, auth, ua_secret) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, &plaintext).unwrap();

        let ciphertext_len = msg.body.len() - (16 + 4 + 1 + 65);
        assert_eq!(
            ciphertext_len,
            RECORD_SIZE as usize,
            "зашифрованная запись на границе == RECORD_SIZE"
        );
        let decrypted = ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).unwrap();
        assert_eq!(&decrypted[..decrypted.len() - 1], plaintext.as_slice());
    }

    #[test]
    fn ece_oversize_payload_does_not_panic() {
        // Превышение rs: код пишет фиксированный RECORD_SIZE в заголовок, но
        // шифрует одну запись произвольной длины. Документируем ФАКТ: не паника,
        // body консистентен. (Ред-флаг: реальный push-сервис отверг бы такое.)
        for len in [8192usize, 100_000] {
            let plaintext = vec![0x5A_u8; len];
            let (p256dh, auth, ua_secret) = make_subscription();
            let msg = encrypt_payload(&p256dh, &auth, &plaintext)
                .expect("oversize payload не должен паниковать/ошибаться в текущей реализации");

            // Заголовок всё ещё заявляет RECORD_SIZE.
            let rs = u32::from_be_bytes([msg.body[16], msg.body[17], msg.body[18], msg.body[19]]);
            assert_eq!(rs, RECORD_SIZE, "rs в заголовке остаётся RECORD_SIZE");
            // body консистентен с формулой 103 + len.
            assert_eq!(msg.body.len(), 103 + len, "body = 103 + plaintext.len()");
            // И всё ещё корректно расшифровывается (одна большая запись).
            let decrypted = ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).unwrap();
            assert_eq!(&decrypted[..decrypted.len() - 1], plaintext.as_slice());
        }
    }

    #[test]
    fn ece_rejects_invalid_base64url_p256dh() {
        let (_p, auth, _s) = make_subscription();
        for bad in ["!!!not_base64!!!", "ab+/cd"] {
            let e = encrypt_payload(bad, &auth, b"x").unwrap_err();
            assert!(
                e.contains("invalid base64url p256dh"),
                "ожидали 'invalid base64url p256dh', got: {e}"
            );
        }
    }

    #[test]
    fn ece_rejects_p256dh_with_padding() {
        // Валидный 65-байт ключ, закодированный СТАНДАРТНЫМ base64 с '=' padding.
        let (good, auth, _s) = make_subscription();
        let bytes = B64URL.decode(&good).unwrap();
        let padded = base64::engine::general_purpose::URL_SAFE.encode(&bytes); // c '='
        assert!(padded.contains('='), "контроль: padded содержит '='");
        let e = encrypt_payload(&padded, &auth, b"x").unwrap_err();
        assert!(
            e.contains("invalid base64url p256dh"),
            "NO_PAD движок отвергает padding: {e}"
        );
    }

    #[test]
    fn ece_rejects_p256dh_wrong_length() {
        let (_p, auth, _s) = make_subscription();
        for len in [10usize, 64, 66] {
            let bad = B64URL.encode(vec![0x04u8; len]);
            let e = encrypt_payload(&bad, &auth, b"x").unwrap_err();
            assert!(
                e.contains("65-byte uncompressed point"),
                "длина {len} → Err про 65 байт: {e}"
            );
        }
    }

    #[test]
    fn ece_rejects_p256dh_wrong_prefix() {
        let (_p, auth, _s) = make_subscription();
        // 65 байт, но первый байт != 0x04.
        for prefix in [0x00u8, 0x02, 0x03] {
            let mut bytes = vec![0u8; 65];
            bytes[0] = prefix;
            let bad = B64URL.encode(&bytes);
            let e = encrypt_payload(&bad, &auth, b"x").unwrap_err();
            assert!(
                e.contains("65-byte uncompressed point"),
                "prefix {prefix:#x} → Err: {e}"
            );
        }
    }

    #[test]
    fn ece_rejects_p256dh_not_on_curve() {
        let (_p, auth, _s) = make_subscription();
        // 0x04 + 64 байта мусора, не лежащих на кривой.
        let mut bytes = vec![0xFFu8; 65];
        bytes[0] = 0x04;
        let bad = B64URL.encode(&bytes);
        let e = encrypt_payload(&bad, &auth, b"x").unwrap_err();
        assert!(
            e.contains("not a valid P-256 point"),
            "точка не на кривой → Err: {e}"
        );
    }

    #[test]
    fn ece_rejects_auth_wrong_length() {
        let (p256dh, _a, _s) = make_subscription();
        for len in [0usize, 8, 15, 17, 32] {
            let bad_auth = B64URL.encode(vec![0u8; len]);
            let e = encrypt_payload(&p256dh, &bad_auth, b"x").unwrap_err();
            assert!(
                e.contains("auth secret must be 16 bytes"),
                "auth len {len} → Err: {e}"
            );
        }
    }

    #[test]
    fn ece_rejects_invalid_base64url_auth() {
        let (p256dh, _a, _s) = make_subscription();
        let e = encrypt_payload(&p256dh, "***", b"x").unwrap_err();
        assert!(
            e.contains("invalid base64url auth"),
            "битый auth base64 → Err: {e}"
        );
    }

    #[test]
    fn ece_fresh_salt_and_key_over_many_calls() {
        use std::collections::HashSet;
        let (p256dh, auth, _s) = make_subscription();
        let mut salts: HashSet<Vec<u8>> = HashSet::new();
        let mut keys: HashSet<Vec<u8>> = HashSet::new();
        for _ in 0..10 {
            let m = encrypt_payload(&p256dh, &auth, b"same-input").unwrap();
            salts.insert(m.body[0..16].to_vec());
            keys.insert(m.body[21..86].to_vec());
        }
        assert_eq!(salts.len(), 10, "все 10 salt уникальны (RFC 8291)");
        assert_eq!(keys.len(), 10, "все 10 server pubkey уникальны");
    }

    #[test]
    fn ece_fresh_key_yields_different_ciphertext() {
        let (p256dh, auth, _s) = make_subscription();
        let a = encrypt_payload(&p256dh, &auth, b"identical").unwrap();
        let b = encrypt_payload(&p256dh, &auth, b"identical").unwrap();
        assert_ne!(
            &a.body[86..],
            &b.body[86..],
            "одинаковый plaintext → разный ciphertext (нет nonce-reuse)"
        );
    }

    #[test]
    fn ece_wrong_key_order_breaks_decrypt() {
        // Порядок ua||as в key_info load-bearing: перестановка → decrypt Err.
        let (p256dh, auth, ua_secret) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"order-matters").unwrap();
        // Правильный порядок — Ok.
        assert!(ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).is_ok());
        // Перепутанный порядок — Err.
        assert!(
            ua_decrypt(&msg.body, &ua_secret, &auth, None, true, &[]).is_err(),
            "перестановка ua/as в key_info ломает деривацию"
        );
    }

    #[test]
    fn ece_nonempty_aad_breaks_decrypt() {
        // encrypt использует aad=&[]; decrypt с непустым AAD → tag mismatch.
        let (p256dh, auth, ua_secret) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"aad-test").unwrap();
        assert!(ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).is_ok());
        assert!(
            ua_decrypt(&msg.body, &ua_secret, &auth, None, false, b"x").is_err(),
            "непустой AAD → ошибка аутентификации"
        );
    }

    #[test]
    fn ece_tampered_ciphertext_breaks_decrypt() {
        // Целостность GCM: XOR одного байта в ciphertext → Err.
        let (p256dh, auth, ua_secret) = make_subscription();
        let mut msg = encrypt_payload(&p256dh, &auth, b"integrity").unwrap();
        let last = msg.body.len() - 1;
        msg.body[last] ^= 0x01; // портим байт tag/ciphertext
        assert!(
            ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).is_err(),
            "подмена ciphertext → ошибка аутентификации"
        );
    }

    #[test]
    fn ece_tampered_salt_breaks_decrypt() {
        // salt из заголовка реально используется: подменённый salt → Err.
        let (p256dh, auth, ua_secret) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"salt-test").unwrap();
        let mut bad_salt = msg.body[0..16].to_vec();
        bad_salt[0] ^= 0x01;
        assert!(
            ua_decrypt(&msg.body, &ua_secret, &auth, Some(&bad_salt), false, &[]).is_err(),
            "подменённый salt → другой CEK/nonce → tag mismatch"
        );
    }

    #[test]
    fn ece_header_idlen_and_keyid_valid_point() {
        let (p256dh, auth, _s) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"keyid").unwrap();
        assert_eq!(msg.body[20], 65, "idlen == 65");
        let keyid = &msg.body[21..86];
        assert_eq!(keyid.len(), 65);
        assert_eq!(keyid[0], 0x04, "keyid uncompressed prefix");
        assert!(
            PublicKey::from_sec1_bytes(keyid).is_ok(),
            "keyid — валидная P-256 точка"
        );
    }

    #[test]
    fn ece_header_rs_is_big_endian_record_size() {
        let (p256dh, auth, _s) = make_subscription();
        let msg = encrypt_payload(&p256dh, &auth, b"rs").unwrap();
        let rs = u32::from_be_bytes([msg.body[16], msg.body[17], msg.body[18], msg.body[19]]);
        assert_eq!(rs, RECORD_SIZE);
        // Явная BE-проверка: 4096 = 0x00001000.
        assert_eq!(&msg.body[16..20], &[0x00, 0x00, 0x10, 0x00], "rs big-endian");
    }

    #[test]
    fn ece_body_length_matches_formula() {
        // body.len() == 16+4+1+65+(L+1+16) == 103 + L.
        for l in [0usize, 5, 4079] {
            let plaintext = vec![0x42u8; l];
            let (p256dh, auth, _s) = make_subscription();
            let msg = encrypt_payload(&p256dh, &auth, &plaintext).unwrap();
            assert_eq!(msg.body.len(), 103 + l, "body длина для L={l}");
        }
    }

    #[test]
    fn ece_isolation_between_subscriptions() {
        // Блок одной подписки НЕ дешифруется секретом другой.
        let (p1, a1, s1) = make_subscription();
        let (_p2, _a2, s2) = make_subscription();
        let msg = encrypt_payload(&p1, &a1, b"isolated").unwrap();
        // Своим секретом — Ok.
        assert!(ua_decrypt(&msg.body, &s1, &a1, None, false, &[]).is_ok());
        // Чужим секретом — Err (другой ECDH shared → другой ключ).
        assert!(
            ua_decrypt(&msg.body, &s2, &a1, None, false, &[]).is_err(),
            "ECDH per-subscription изолирует ключи"
        );
    }

    #[test]
    fn ece_tolerates_whitespace_in_keys() {
        // trim() снимает обрамляющие пробелы/\n.
        let (p256dh, auth, ua_secret) = make_subscription();
        let p_ws = format!("  {p256dh}\n");
        let a_ws = format!("\t{auth}  ");
        let msg = encrypt_payload(&p_ws, &a_ws, b"ws").unwrap();
        // Расшифровываем чистым auth (без пробелов).
        let decrypted = ua_decrypt(&msg.body, &ua_secret, &auth, None, false, &[]).unwrap();
        assert_eq!(&decrypted[..decrypted.len() - 1], b"ws");

        // Негатив: пробел ВНУТРИ строки → decode падает.
        let mid = format!("{}xx yy", &p256dh[..p256dh.len() - 5]);
        assert!(encrypt_payload(&mid, &auth, b"x").is_err());
    }

    #[test]
    fn ece_url_safe_alphabet_minus_underscore_ok_plus_slash_rejected() {
        let (_p, auth, _s) = make_subscription();
        // '+'/'/' (стандартный алфавит) → Err по алфавиту.
        let std_b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(vec![0xFBu8; 65]);
        if std_b64.contains('+') || std_b64.contains('/') {
            let e = encrypt_payload(&std_b64, &auth, b"x").unwrap_err();
            assert!(
                e.contains("invalid base64url"),
                "'+'/'/' → invalid base64url: {e}"
            );
        }
        // '-'/'_' (url-safe) принимаются алфавитом: 0xFF*65 содержит и '-' и '_'
        // в url-safe кодировке; это валидный алфавит, но не 65 валидных кривых
        // байт → отвергается по длине/точке, НЕ по алфавиту.
        let url_b64 = B64URL.encode(vec![0xFFu8; 65]);
        let e = encrypt_payload(&url_b64, &auth, b"x").unwrap_err();
        assert!(
            !e.contains("invalid base64url"),
            "'-'/'_' проходят алфавит (ошибка не про base64): {e}"
        );
    }

    // =========================================================================
    // VAPID JWT — дополнительные краевые случаи.
    // =========================================================================

    /// Распарсить claims JWT в serde_json::Value.
    fn jwt_claims(jwt: &str) -> serde_json::Value {
        let parts: Vec<&str> = jwt.split('.').collect();
        let claims = B64URL.decode(parts[1]).unwrap();
        serde_json::from_slice(&claims).unwrap()
    }

    #[test]
    fn vapid_jwt_segments_have_no_padding() {
        let vapid = test_vapid();
        let jwt = build_vapid_jwt(&vapid, "https://fcm.googleapis.com", 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        for (i, p) in parts.iter().enumerate() {
            assert!(!p.contains('='), "сегмент {i} без padding '=': {p}");
            assert!(!p.contains('+'), "сегмент {i} url-safe (без '+')");
            assert!(!p.contains('/'), "сегмент {i} url-safe (без '/')");
        }
        // header декодируется в ровно {"typ":"JWT","alg":"ES256"}.
        let header = String::from_utf8(B64URL.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, r#"{"typ":"JWT","alg":"ES256"}"#);
    }

    #[test]
    fn vapid_jwt_signature_is_raw_p1363_and_verifies() {
        let vapid = test_vapid();
        let now = 1_700_000_000i64;
        let jwt = build_vapid_jwt(&vapid, "https://fcm.googleapis.com", now).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let sig_bytes = B64URL.decode(parts[2]).unwrap();
        assert_eq!(sig_bytes.len(), 64, "P1363 raw r||s = 64 байта");
        // Signature::from_slice принимает именно P1363 (не DER).
        let sig = Signature::from_slice(&sig_bytes).unwrap();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let pub_bytes = B64URL.decode(vapid.public_key_b64()).unwrap();
        let vk = VerifyingKey::from_sec1_bytes(&pub_bytes).unwrap();
        assert!(vk.verify(signing_input.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn vapid_jwt_exp_in_future_within_24h() {
        let vapid = test_vapid();
        let now = 1_700_000_000i64;
        let claims = jwt_claims(&build_vapid_jwt(&vapid, "https://h.test", now).unwrap());
        let exp = claims["exp"].as_i64().expect("exp числовой");
        assert_eq!(exp, now + VAPID_JWT_TTL_SECS, "exp == now + TTL");
        assert!(exp > now, "exp строго в будущем");
        assert!(exp - now <= 86_400, "exp - now <= 24h (RFC 8292)");
        assert!(claims["exp"].is_number(), "exp числовой, не строка");
    }

    #[test]
    fn vapid_jwt_aud_equals_origin() {
        let vapid = test_vapid();
        let now = 1_700_000_000i64;
        // Через authorization_header → распарсить t=<jwt>.
        let cases = [
            (
                "https://fcm.googleapis.com/fcm/send/abc123",
                "https://fcm.googleapis.com",
            ),
            ("http://localhost:8080/push/x", "http://localhost:8080"),
            ("https://h.test", "https://h.test"),
        ];
        for (endpoint, expected_aud) in cases {
            let header = vapid_authorization_header(&vapid, endpoint, now).unwrap();
            // header = "vapid t=<jwt>, k=<pub>"
            let t = header
                .strip_prefix("vapid t=")
                .unwrap()
                .split(", k=")
                .next()
                .unwrap();
            let claims = jwt_claims(t);
            assert_eq!(
                claims["aud"], expected_aud,
                "aud == origin для {endpoint}"
            );
        }
    }

    #[test]
    fn vapid_jwt_sub_is_nonempty() {
        let vapid = test_vapid();
        let claims = jwt_claims(&build_vapid_jwt(&vapid, "https://h.test", 0).unwrap());
        let sub = claims["sub"].as_str().expect("sub присутствует");
        assert!(!sub.is_empty(), "sub непустой (RFC 8292 §2.1)");
        assert!(
            sub.starts_with("mailto:") || sub.starts_with("https"),
            "sub — mailto/https URL: {sub}"
        );
    }

    #[test]
    fn vapid_authorization_header_k_equals_public_key() {
        let vapid = test_vapid();
        let header =
            vapid_authorization_header(&vapid, "https://fcm.googleapis.com/x", 0).unwrap();
        assert!(header.starts_with("vapid t="));
        let k = header.split(", k=").nth(1).unwrap();
        assert_eq!(k, vapid.public_key_b64(), "k= == публичный ключ store");
        // t= — валидный 3-сегментный JWT.
        let t = header.strip_prefix("vapid t=").unwrap().split(", k=").next().unwrap();
        assert_eq!(t.split('.').count(), 3);
    }

    #[test]
    fn vapid_authorization_header_invalid_endpoint_errors() {
        let vapid = test_vapid();
        for bad in ["not a url", "", "https://"] {
            let r = vapid_authorization_header(&vapid, bad, 0);
            assert!(r.is_err(), "невалидный endpoint '{bad}' → Err");
            let e = r.unwrap_err();
            assert!(
                e.contains("cannot derive origin"),
                "ошибка про origin для '{bad}': {e}"
            );
        }
    }

    #[test]
    fn origin_of_boundary_cases() {
        assert_eq!(
            origin_of("https://host.example"),
            Some("https://host.example".to_string())
        );
        assert_eq!(
            origin_of("http://localhost:8080/push/abc"),
            Some("http://localhost:8080".to_string())
        );
        // Пустой host после схемы → None.
        assert_eq!(origin_of("https://"), None);
        // Нет '://' → None.
        assert_eq!(origin_of("not a url"), None);
        // '://nohost' — нет схемы перед '://' (scheme пуст), но host есть.
        // scheme_end==0 → scheme="" host="nohost" → "://nohost". Фиксируем факт.
        assert_eq!(origin_of("://nohost"), Some("://nohost".to_string()));
    }

    #[test]
    fn vapid_jwt_different_now_each_verifies() {
        // Подписывается актуальный signing_input (не кэш): для разных now
        // подпись валидна, exp отличается.
        let vapid = test_vapid();
        let pub_bytes = B64URL.decode(vapid.public_key_b64()).unwrap();
        let vk = VerifyingKey::from_sec1_bytes(&pub_bytes).unwrap();

        let mut exps = std::collections::HashSet::new();
        for now in [0i64, 2_000_000_000] {
            let jwt = build_vapid_jwt(&vapid, "https://h.test", now).unwrap();
            let parts: Vec<&str> = jwt.split('.').collect();
            let signing_input = format!("{}.{}", parts[0], parts[1]);
            let sig = Signature::from_slice(&B64URL.decode(parts[2]).unwrap()).unwrap();
            assert!(
                vk.verify(signing_input.as_bytes(), &sig).is_ok(),
                "подпись валидна для now={now}"
            );
            let claims = jwt_claims(&jwt);
            exps.insert(claims["exp"].as_i64().unwrap());
        }
        assert_eq!(exps.len(), 2, "exp различается для разных now");
    }

    // =========================================================================
    // attention_payload — расширенные краевые случаи.
    // =========================================================================

    #[test]
    fn attention_payload_handles_control_chars_and_unicode() {
        for session in ["a\nb\tc", "проект/сессия 🚀", "x\"y\\z\r"] {
            let payload = attention_payload(session, "/");
            let parsed: serde_json::Value = serde_json::from_slice(&payload)
                .unwrap_or_else(|e| panic!("payload для '{session:?}' должен быть валидным JSON: {e}"));
            assert_eq!(parsed["data"]["session"], session, "session сохранён точно");
            assert!(parsed["title"].is_string());
            assert!(parsed["body"].is_string());
            assert!(parsed["data"]["url"].is_string());
        }
    }

    // =========================================================================
    // Воркер: edge-trigger логика (через зеркальную newly_attention).
    // =========================================================================

    #[test]
    fn edge_trigger_stable_true_for_many_ticks_no_repeat() {
        // 1 эпизод = 1 пуш: после первого true несколько подряд стабильных
        // true-тиков НЕ триггерят.
        let mut prev: HashMap<String, bool> = HashMap::new();
        let snap: HashMap<String, bool> = [("s".to_string(), true)].into_iter().collect();

        assert_eq!(newly_attention(&prev, &snap), vec!["s".to_string()], "первый true");
        prev = snap.clone();
        for tick in 0..5 {
            assert!(
                newly_attention(&prev, &snap).is_empty(),
                "стабильный true тик {tick} не триггерит"
            );
            prev = snap.clone();
        }
    }

    #[test]
    fn edge_trigger_empty_inputs_no_trigger() {
        let empty: HashMap<String, bool> = HashMap::new();
        // Пустые prev и snap.
        assert!(newly_attention(&empty, &empty).is_empty());
        // prev={s:true}, snap пуст (сессия исчезла) → нет триггера.
        let prev_true: HashMap<String, bool> = [("s".to_string(), true)].into_iter().collect();
        assert!(newly_attention(&prev_true, &empty).is_empty());
        // prev пуст, snap={s:false} (видели, prompt закрыт) → нет триггера.
        let snap_false: HashMap<String, bool> = [("s".to_string(), false)].into_iter().collect();
        assert!(newly_attention(&empty, &snap_false).is_empty());
    }

    #[test]
    fn edge_trigger_mixed_phases_simultaneously() {
        // a гаснет (true→false), b загорается (false→true), c стабильна (true).
        let prev: HashMap<String, bool> = [
            ("a".to_string(), true),
            ("b".to_string(), false),
            ("c".to_string(), true),
        ]
        .into_iter()
        .collect();
        let snap: HashMap<String, bool> = [
            ("a".to_string(), false),
            ("b".to_string(), true),
            ("c".to_string(), true),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            newly_attention(&prev, &snap),
            vec!["b".to_string()],
            "только b (false→true) триггерит"
        );
    }

    // =========================================================================
    // Транспорт: send_one / send_to_all (wiremock).
    //
    // ВАЖНО: send_one СНАЧАЛА шифрует payload (encrypt_payload), поэтому
    // подписки ОБЯЗАНЫ иметь валидный 65-байт p256dh и 16-байт auth — берём из
    // make_subscription(). VapidStore — из test_vapid(). HTTP мокается wiremock;
    // тесты с сетью идут под multi_thread-реактором (таймер reqwest требует
    // рабочий реактор).
    // =========================================================================

    use crate::push_store::{PushSubscriptionStore, SubscriptionKeys};
    use wiremock::matchers::{header, header_exists, method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Уникальный путь стора во временной директории.
    fn tmp_store_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "devforge_push_send_{label}_{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    /// Валидная подписка (валидные ключи из make_subscription) на заданный endpoint.
    fn valid_sub(endpoint: &str) -> StoredSubscription {
        let (p256dh, auth, _s) = make_subscription();
        StoredSubscription {
            endpoint: endpoint.to_string(),
            keys: SubscriptionKeys { p256dh, auth },
            device_label: None,
            created_at: "2026-06-25T00:00:00Z".to_string(),
        }
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_empty_list_no_send_no_file() {
        let path = tmp_store_path("empty");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        // Mock есть, но не должен получить ни одного запроса.
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&mock)
            .await;

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert!(store.list().is_empty());
        // Lazy creation: файл подписок НЕ создан (prune не вызывался).
        assert!(!path.exists(), "файл не должен создаваться без подписок");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_404_prunes() {
        let path = tmp_store_path("404");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        store
            .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
            .unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 1 });
        assert!(store.list().is_empty(), "404 → подписка удалена");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_410_prunes() {
        let path = tmp_store_path("410");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&mock)
            .await;
        store
            .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
            .unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 1 });
        assert!(store.list().is_empty(), "410 классифицируется как 404");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_401_does_not_prune() {
        let path = tmp_store_path("401");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad vapid"))
            .mount(&mock)
            .await;
        store
            .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
            .unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert_eq!(store.list().len(), 1, "401 — не прунить валидную подписку");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_403_does_not_prune() {
        let path = tmp_store_path("403");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&mock)
            .await;
        store
            .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
            .unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert_eq!(store.list().len(), 1, "403 — не прунить (Auth)");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_500_transient_does_not_prune() {
        let path = tmp_store_path("500");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&mock)
            .await;
        store
            .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
            .unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert_eq!(store.list().len(), 1, "5xx — транзиентно, не прунить");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_one_connection_refused_is_transient() {
        let vapid = test_vapid();
        let client = test_client();
        // Закрытый порт — connection refused мгновенно.
        let sub = valid_sub("http://127.0.0.1:1/push");
        let err = send_one(&client, &sub, &vapid, b"payload").await.unwrap_err();
        assert!(
            matches!(err, PushError::Transient(_)),
            "connection refused → Transient, got: {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_connection_refused_does_not_prune() {
        let path = tmp_store_path("refused");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();
        store.upsert(valid_sub("http://127.0.0.1:1/push")).unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert_eq!(store.list().len(), 1, "сетевой сбой → не прунить");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_2xx_increments_sent() {
        for code in [200u16, 201, 204] {
            let path = tmp_store_path(&format!("ok{code}"));
            let store = PushSubscriptionStore::new(path.clone());
            let vapid = test_vapid();
            let client = test_client();

            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(code))
                .mount(&mock)
                .await;
            store
                .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
                .unwrap();

            let report = send_to_all(&client, &store, &vapid, b"payload").await;
            assert_eq!(
                report,
                SendReport { sent: 1, pruned: 0 },
                "{code} → успех"
            );
            assert_eq!(store.list().len(), 1, "успешная подписка сохранена");

            let _ = std::fs::remove_file(&path);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_mixed_live_and_dead() {
        let path = tmp_store_path("mixed");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        // Разные пути → разные ответы.
        Mock::given(method("POST"))
            .and(wm_path("/ok"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(wm_path("/gone"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(wm_path("/gone2"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(wm_path("/srv"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock)
            .await;

        let ep_ok = format!("{}/ok", mock.uri());
        let ep_gone = format!("{}/gone", mock.uri());
        let ep_gone2 = format!("{}/gone2", mock.uri());
        let ep_srv = format!("{}/srv", mock.uri());
        store.upsert(valid_sub(&ep_ok)).unwrap();
        store.upsert(valid_sub(&ep_gone)).unwrap();
        store.upsert(valid_sub(&ep_gone2)).unwrap();
        store.upsert(valid_sub(&ep_srv)).unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 1, pruned: 2 });

        // Остались ровно ep_ok и ep_srv.
        let mut remaining: Vec<String> = store.list().into_iter().map(|s| s.endpoint).collect();
        remaining.sort();
        let mut expected = vec![ep_ok, ep_srv];
        expected.sort();
        assert_eq!(remaining, expected, "живой ok и транзиентный 5xx сохранены");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_bad_key_is_crypto_no_http_no_prune() {
        let path = tmp_store_path("badkey");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        // Mock не должен получить запрос (crypto fail ДО HTTP).
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&mock)
            .await;

        // Подписка с невалидным p256dh (10 байт вместо 65), валидный auth.
        let bad_sub = StoredSubscription {
            endpoint: format!("{}/push/ep1", mock.uri()),
            keys: SubscriptionKeys {
                p256dh: B64URL.encode([0u8; 10]),
                auth: B64URL.encode([0u8; 16]),
            },
            device_label: None,
            created_at: "2026-06-25T00:00:00Z".to_string(),
        };
        store.upsert(bad_sub).unwrap();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert_eq!(store.list().len(), 1, "битый ключ → не прунить");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_one_sends_required_headers_and_body() {
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wm_path("/push/ep1"))
            .and(header("Content-Encoding", "aes128gcm"))
            .and(header("Content-Type", "application/octet-stream"))
            .and(header("TTL", "60"))
            .and(header("Urgency", "high"))
            .and(header_exists("Authorization"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&mock)
            .await;

        let sub = valid_sub(&format!("{}/push/ep1", mock.uri()));
        send_one(&client, &sub, &vapid, b"payload").await.unwrap();

        // Проверяем тело и Authorization из полученного запроса.
        let received = mock.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let req = &received[0];
        // Тело — непустой ECE-блок (>= 86 байт заголовка + ciphertext).
        assert!(req.body.len() >= 86, "тело — ECE-блок: {} байт", req.body.len());
        let auth = req
            .headers
            .get("Authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(auth.starts_with("vapid t="), "Authorization: {auth}");
        assert!(auth.contains(", k="), "Authorization содержит k=");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_no_subscriptions_does_not_create_file() {
        // Opt-in инвариант: с --pwa, но без единой подписки — файла нет.
        let path = tmp_store_path("optin");
        let store = PushSubscriptionStore::new(path.clone());
        assert!(!path.exists());
        let vapid = test_vapid();
        let client = test_client();

        let report = send_to_all(&client, &store, &vapid, b"payload").await;
        assert_eq!(report, SendReport { sent: 0, pruned: 0 });
        assert!(
            !path.exists(),
            "push_subscriptions.json не создаётся без подписок (lazy)"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_all_concurrent_with_mutation_no_panic() {
        // Доставка по снимку list() безопасно конкурентна с upsert/remove.
        let path = tmp_store_path("concurrent");
        let store = PushSubscriptionStore::new(path.clone());
        let vapid = test_vapid();
        let client = test_client();

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&mock)
            .await;
        store
            .upsert(valid_sub(&format!("{}/push/ep1", mock.uri())))
            .unwrap();

        let store2 = store.clone();
        let mock_uri = mock.uri();
        let (report, _) = tokio::join!(
            send_to_all(&client, &store, &vapid, b"payload"),
            async move {
                // Параллельная мутация: добавляем и удаляем подписки.
                store2
                    .upsert(valid_sub(&format!("{mock_uri}/push/ep2")))
                    .unwrap();
                let _ = store2.remove(&format!("{mock_uri}/push/ep2"));
            }
        );
        // Снимок на момент входа содержал 1 живую подписку.
        assert_eq!(report.sent, 1, "send_to_all работает по снимку list()");

        let _ = std::fs::remove_file(&path);
    }
}
