//! VAPID-ключи для Web Push (opt-in PWA, активируется флагом `--pwa`).
//!
//! VAPID (Voluntary Application Server Identification, RFC 8292) — это
//! ECDSA-пара на кривой P-256 (NIST secp256r1), которой push-сервер
//! подписывает JWT (`ES256`), доказывая push-сервису браузера, что
//! уведомления шлёт легитимный сервер. Браузеру отдаётся **публичный**
//! ключ (`applicationServerKey` в `pushManager.subscribe`), приватный
//! хранится только на сервере.
//!
//! ## Хранилище
//!
//! Пара живёт в одном файле `~/.forge/vapid.json` (рядом с
//! `user_settings.json`). Формат:
//!
//! ```json
//! {
//!   "private_key_b64": "<base64url-no-pad, 32-байт скаляр>",
//!   "public_key_b64":  "<base64url-no-pad, 65-байт uncompressed point>"
//! }
//! ```
//!
//! `private_key_b64` — это raw 32-байтный скаляр приватного ключа (формат,
//! который ожидают VAPID-генераторы и web-push-библиотеки: «raw base64url
//! key»). `public_key_b64` — 65-байтная uncompressed-форма публичной точки
//! (`0x04 || X || Y`), как требует Web Push API на стороне браузера.
//!
//! ## Lifecycle
//!
//! [`VapidStore::load_or_generate`] вызывается из `main.rs` **только** при
//! `pwa_enabled` (см. opt-in инвариант): без флага `--pwa` файл
//! `~/.forge/vapid.json` НЕ создаётся.
//!   - файл есть и валиден → грузим, ключи НЕ перегенерируются;
//!   - файла нет → генерируем новую пару (`SigningKey::random(OsRng)`) и
//!     атомарно сохраняем (tmp + rename, как `user_settings::save_locked`);
//!   - файл битый/нечитаемый → возвращаем `Err` (вызывающий код решает, но
//!     `main.rs` логирует и продолжает без PWA, чтобы не падать на старте).
//!
//! ## Отклонение от исходного плана (crate web-push)
//!
//! План предполагал `web_push::VapidSignatureBuilder::from_base64_no_sub`.
//! Однако `web-push 0.11` БЕЗУСЛОВНО тянет крейт `ece` с `backend-openssl`
//! (плюс openssl/hyper-tls через любой HTTP-клиент), что нарушает жёсткое
//! требование проекта «без openssl, hyper совместим с axum 0.7 (rustls)».
//! Поскольку VAPID — это просто ECDSA P-256 + ES256-JWT, всё реализовано на
//! чистом Rust-крейте [`p256`] без openssl. Тип-аналог `SubscriptionInfo` и
//! фактическая ES256-подпись JWT для доставки появятся в Фазах 2/3
//! (`push_store.rs` / `push.rs`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use p256::ecdsa::SigningKey;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};

/// base64url-движок БЕЗ паддинга (RFC 4648 §5, `-_`, без `=`). Это формат,
/// в котором VAPID-ключи передаются браузеру и хранятся в файле.
const B64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// On-disk JSON-представление VAPID-пары (`~/.forge/vapid.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VapidKeyFile {
    /// Приватный ключ: raw 32-байтный скаляр, base64url-no-pad.
    private_key_b64: String,
    /// Публичный ключ: 65-байт uncompressed point (`0x04||X||Y`),
    /// base64url-no-pad. Дублируется на диск, чтобы при загрузке не
    /// пересчитывать (и как самопроверка целостности файла).
    public_key_b64: String,
}

/// In-memory VAPID-хранилище. Cheap-clonable: внутри только
/// [`SigningKey`] (содержит скаляр) и две короткие `String`. Один экземпляр
/// на процесс, кладётся в `AppState.pwa` (через `PwaCtx`).
#[derive(Clone)]
pub struct VapidStore {
    /// Приватный ECDSA P-256 ключ — основа ES256-подписи VAPID-JWT
    /// (используется в Фазе 3 для аутентификации push-запросов).
    signing_key: SigningKey,
    /// Публичный ключ в формате, который отдаётся фронту как
    /// `applicationServerKey` (base64url-no-pad, 65-байт uncompressed).
    public_key_b64: String,
}

impl std::fmt::Debug for VapidStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Никогда не печатаем приватный ключ в логах/Debug.
        f.debug_struct("VapidStore")
            .field("public_key_b64", &self.public_key_b64)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

impl VapidStore {
    /// Загрузить пару из `path`, либо — если файла нет — сгенерировать новую
    /// и атомарно сохранить.
    ///
    /// Поведение:
    ///   - файла нет (`NotFound`) → генерируем `SigningKey::random(OsRng)`,
    ///     выводим публичный ключ, атомарно пишем `vapid.json`, возвращаем
    ///     [`VapidStore`];
    ///   - файл есть и парсится → реконструируем `SigningKey` из
    ///     `private_key_b64`, заново выводим публичный ключ из приватного
    ///     (источник истины — приватник; поле `public_key_b64` в файле
    ///     используется как кэш, но пере-выводится для согласованности);
    ///   - файл есть, но битый/нечитаемый/невалидный ключ → `Err` с
    ///     контекстом (вызывающий код в `main.rs` логирует и продолжает без
    ///     PWA — не fail-fast).
    ///
    /// Идемпотентность: повторный вызов с существующим валидным файлом НЕ
    /// перегенерирует ключи (критично — иначе все push-подписки браузеров
    /// инвалидировались бы при каждом рестарте).
    pub fn load_or_generate(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(body) => Self::from_file_body(&body, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let store = Self::generate();
                store.save_atomic(path).with_context(|| {
                    format!("failed to persist new VAPID key to {}", path.display())
                })?;
                Ok(store)
            }
            Err(e) => Err(e).with_context(|| {
                format!("failed to read VAPID key file {}", path.display())
            }),
        }
    }

    /// Сгенерировать новую случайную P-256 пару (без записи на диск).
    fn generate() -> Self {
        let signing_key = SigningKey::random(&mut OsRng);
        let public_key_b64 = Self::derive_public_b64(&signing_key);
        Self {
            signing_key,
            public_key_b64,
        }
    }

    /// Реконструировать `VapidStore` из тела файла `vapid.json`.
    fn from_file_body(body: &str, path: &Path) -> Result<Self> {
        let parsed: VapidKeyFile = serde_json::from_str(body)
            .with_context(|| format!("failed to parse VAPID key file {}", path.display()))?;

        let priv_bytes = B64URL
            .decode(parsed.private_key_b64.trim())
            .with_context(|| {
                format!("invalid base64url private key in {}", path.display())
            })?;

        // p256 SigningKey::from_slice ожидает ровно 32-байт скаляр.
        let signing_key = SigningKey::from_slice(&priv_bytes).with_context(|| {
            format!(
                "VAPID private key in {} is not a valid P-256 scalar ({} bytes)",
                path.display(),
                priv_bytes.len()
            )
        })?;

        // Источник истины — приватник: публичный ключ всегда выводим заново,
        // чтобы рассинхрон с кэшированным public_key_b64 в файле не приводил
        // к отдаче браузеру неверного applicationServerKey.
        let public_key_b64 = Self::derive_public_b64(&signing_key);
        Ok(Self {
            signing_key,
            public_key_b64,
        })
    }

    /// Вывести base64url-no-pad публичного ключа (65-байт uncompressed
    /// `0x04||X||Y`) из приватного ключа.
    fn derive_public_b64(signing_key: &SigningKey) -> String {
        let verifying_key = signing_key.verifying_key();
        let point = verifying_key.to_encoded_point(false); // false = uncompressed
        B64URL.encode(point.as_bytes())
    }

    /// Атомарная запись пары в файл (tmp + rename, как
    /// `user_settings`/`server_config::save_to`). На POSIX rename атомарен в
    /// рамках одного mount-point: при `kill -9` в момент записи получим либо
    /// старый, либо новый файл, но не битый.
    fn save_atomic(&self, path: &Path) -> Result<()> {
        let private_key_b64 = B64URL.encode(self.signing_key.to_bytes());
        let file = VapidKeyFile {
            private_key_b64,
            public_key_b64: self.public_key_b64.clone(),
        };
        let body =
            serde_json::to_vec_pretty(&file).context("failed to serialize VAPID key file")?;

        let mut tmp = path.to_path_buf();
        let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
        tmp_name.push(".tmp");
        tmp.set_file_name(tmp_name);

        std::fs::write(&tmp, &body)
            .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| {
            format!("failed to rename {} -> {}", tmp.display(), path.display())
        })?;
        Ok(())
    }

    /// Публичный ключ VAPID в формате `applicationServerKey` (base64url-no-pad,
    /// 65-байт uncompressed point). Отдаётся фронту через `GET /api/pwa/config`.
    pub fn public_key_b64(&self) -> &str {
        &self.public_key_b64
    }

    /// Приватный ECDSA P-256 ключ для подписи VAPID-JWT (`ES256`).
    /// Потребляется push-доставкой в Фазе 3 (`push.rs`) — там строится JWT
    /// `{aud, exp, sub}` и подписывается этим ключом. В Фазе 1 не
    /// используется, но экспонируется как часть стабильного API `VapidStore`.
    #[allow(dead_code)] // потребитель — Фаза 3 (push.rs)
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }
}

/// `~/.forge/vapid.json` — путь к файлу VAPID-пары. Резолвится от `HOME`
/// (рядом с `user_settings.json`). Возвращает `Err`, если `HOME` не задан.
///
/// Используется в `main.rs` под `pwa_enabled` для [`VapidStore::load_or_generate`].
pub fn default_vapid_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(".forge").join("vapid.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-vapid-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn generate_creates_valid_pair_and_writes_file() {
        let dir = tempdir("gen");
        let path = dir.join("vapid.json");
        assert!(!path.exists());

        let store = VapidStore::load_or_generate(&path).unwrap();
        // Файл создан.
        assert!(path.exists(), "vapid.json должен быть создан");

        // public_key_b64 — валидный base64url, декодируется в 65 байт
        // uncompressed point (0x04 prefix).
        let pub_bytes = B64URL.decode(store.public_key_b64()).unwrap();
        assert_eq!(pub_bytes.len(), 65, "uncompressed P-256 point = 65 байт");
        assert_eq!(pub_bytes[0], 0x04, "uncompressed point начинается с 0x04");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_does_not_regenerate() {
        let dir = tempdir("reload");
        let path = dir.join("vapid.json");

        let first = VapidStore::load_or_generate(&path).unwrap();
        let first_pub = first.public_key_b64().to_string();
        let first_priv = B64URL.encode(first.signing_key().to_bytes());

        // Повторный вызов грузит тот же файл — ключи идентичны.
        let second = VapidStore::load_or_generate(&path).unwrap();
        assert_eq!(
            second.public_key_b64(),
            first_pub,
            "повторная загрузка не должна менять публичный ключ"
        );
        assert_eq!(
            B64URL.encode(second.signing_key().to_bytes()),
            first_priv,
            "повторная загрузка не должна менять приватный ключ"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_key_derived_from_private_is_consistent() {
        // Публичный ключ в файле (кэш) и выведенный из приватника совпадают.
        let dir = tempdir("consistent");
        let path = dir.join("vapid.json");
        let store = VapidStore::load_or_generate(&path).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let file: VapidKeyFile = serde_json::from_str(&body).unwrap();
        assert_eq!(
            file.public_key_b64,
            store.public_key_b64(),
            "кэш public_key_b64 в файле == выведенный из приватника"
        );

        // И при загрузке заново публичный ключ выводится из приватника
        // (а не слепо берётся из файла), и совпадает.
        let reloaded = VapidStore::load_or_generate(&path).unwrap();
        assert_eq!(reloaded.public_key_b64(), store.public_key_b64());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_returns_err_not_panic() {
        let dir = tempdir("corrupt");
        let path = dir.join("vapid.json");
        std::fs::write(&path, b"{ not json :(").unwrap();

        let r = VapidStore::load_or_generate(&path);
        assert!(r.is_err(), "битый JSON должен вернуть Err, а не панику");
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains(&path.display().to_string()),
            "ошибка должна содержать путь, got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_key_length_returns_err() {
        // Валидный JSON, но приватный ключ — не 32 байта.
        let dir = tempdir("badlen");
        let path = dir.join("vapid.json");
        let bad = VapidKeyFile {
            private_key_b64: B64URL.encode([0u8; 10]), // 10 байт вместо 32
            public_key_b64: "x".to_string(),
        };
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();

        let r = VapidStore::load_or_generate(&path);
        assert!(r.is_err(), "неверная длина ключа → Err");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_atomic_no_tmp_left_behind() {
        let dir = tempdir("atomic");
        let path = dir.join("vapid.json");
        let _ = VapidStore::load_or_generate(&path).unwrap();

        // tmp-файл не должен остаться после успешного rename.
        let tmp = dir.join("vapid.json.tmp");
        assert!(!tmp.exists(), "tmp-файл должен быть удалён через rename");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================================
    // Дополнительные краевые случаи (edge-cases map).
    // =========================================================================

    /// Генерация при отсутствии файла: приватник ровно 32 байта (валидный
    /// P-256 скаляр), публичный — 65 байт uncompressed. Дополняет
    /// generate_creates_valid_pair_and_writes_file проверкой длины приватника.
    #[test]
    fn generate_private_key_is_32_bytes() {
        let dir = tempdir("priv32");
        let path = dir.join("vapid.json");
        let store = VapidStore::load_or_generate(&path).unwrap();

        let priv_bytes = store.signing_key().to_bytes();
        assert_eq!(priv_bytes.len(), 32, "P-256 скаляр = 32 байта");

        let pub_bytes = B64URL.decode(store.public_key_b64()).unwrap();
        assert_eq!(pub_bytes.len(), 65);
        assert_eq!(pub_bytes[0], 0x04);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Рассинхрон кэша: public_key_b64 в файле подменён на чужой/мусорный, но
    /// при загрузке публичный ключ выводится ЗАНОВО из приватника и совпадает
    /// с реальным, а НЕ с подменённым кэшем.
    #[test]
    fn public_key_re_derived_when_cache_is_desynced() {
        let dir = tempdir("desync");
        let path = dir.join("vapid.json");

        // Сначала сгенерируем валидную пару, запомним настоящий публичный ключ.
        let genuine = VapidStore::load_or_generate(&path).unwrap();
        let genuine_pub = genuine.public_key_b64().to_string();
        let priv_b64 = B64URL.encode(genuine.signing_key().to_bytes());

        // Подменяем кэш public_key_b64 на заведомо чужой/мусорный.
        let tampered = VapidKeyFile {
            private_key_b64: priv_b64,
            public_key_b64: "ZZZZ-tampered-cache-not-the-real-pubkey".to_string(),
        };
        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();

        let reloaded = VapidStore::load_or_generate(&path).unwrap();
        assert_eq!(
            reloaded.public_key_b64(),
            genuine_pub,
            "публичный ключ выведен из приватника, а не взят из подменённого кэша"
        );
        assert_ne!(
            reloaded.public_key_b64(),
            "ZZZZ-tampered-cache-not-the-real-pubkey",
            "не должен совпасть с мусорным кэшем"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Пустой файл vapid.json → Err (serde_json::from_str("") падает), НЕ
    /// трактуется как NotFound и НЕ перегенерирует (файл существует).
    #[test]
    fn empty_file_returns_err() {
        let dir = tempdir("empty");
        let path = dir.join("vapid.json");
        std::fs::write(&path, b"").unwrap();

        let r = VapidStore::load_or_generate(&path);
        assert!(r.is_err(), "пустой файл → Err парсинга, не NotFound");
        let msg = format!("{:#}", r.unwrap_err());
        assert!(msg.contains("parse"), "контекст парсинга: {msg}");

        // Файл не перезаписан (содержимое осталось пустым — генерации не было).
        assert_eq!(std::fs::read(&path).unwrap().len(), 0, "файл не перегенерирован");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Валидный JSON, но без обязательных полей → Err на missing field, без
    /// паники. Проверяем отсутствие public_key_b64 и полностью пустой объект.
    #[test]
    fn json_missing_required_fields_returns_err() {
        let dir = tempdir("missingfields");

        // Нет public_key_b64.
        let p1 = dir.join("vapid1.json");
        std::fs::write(&p1, br#"{"private_key_b64":"abc"}"#).unwrap();
        assert!(
            VapidStore::load_or_generate(&p1).is_err(),
            "отсутствует public_key_b64 → Err"
        );

        // Пустой объект.
        let p2 = dir.join("vapid2.json");
        std::fs::write(&p2, br#"{}"#).unwrap();
        assert!(
            VapidStore::load_or_generate(&p2).is_err(),
            "{{}} → Err (нет ни одного поля)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Невалидный base64url в private_key_b64 → Err. Проверяем (а) мусорные
    /// символы вне алфавита и (б) строку с padding '=' (URL_SAFE_NO_PAD
    /// отклоняет '=') и (в) стандартный base64 с '+'/'/'.
    #[test]
    fn invalid_base64url_private_key_returns_err() {
        let dir = tempdir("badb64");

        for (i, bad_priv) in ["****", "AAAA=", "ab+/cd"].iter().enumerate() {
            let path = dir.join(format!("vapid{i}.json"));
            let bad = VapidKeyFile {
                private_key_b64: bad_priv.to_string(),
                public_key_b64: "x".to_string(),
            };
            std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();

            let r = VapidStore::load_or_generate(&path);
            assert!(r.is_err(), "невалидный base64url '{bad_priv}' → Err");
            let msg = format!("{:#}", r.unwrap_err());
            assert!(
                msg.contains("base64url") || msg.contains("scalar") || msg.contains("P-256"),
                "ошибка про base64/ключ для '{bad_priv}': {msg}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Корректный base64, но длиннее 32 байт (33/64) → Err (не валидный P-256
    /// скаляр по длине). Дополняет invalid_key_length_returns_err (там 10 байт).
    #[test]
    fn private_key_too_long_returns_err() {
        let dir = tempdir("toolong");

        for (i, len) in [33usize, 64].iter().enumerate() {
            let path = dir.join(format!("vapid{i}.json"));
            let bad = VapidKeyFile {
                private_key_b64: B64URL.encode(vec![0x11u8; *len]),
                public_key_b64: "x".to_string(),
            };
            std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(
                VapidStore::load_or_generate(&path).is_err(),
                "{len}-байтный приватник → Err"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 32-байтный, но криптографически невалидный скаляр: ноль и 0xFF*32
    /// (>= порядка кривой n) → Err, не паника. SigningKey::from_slice должен
    /// отклонить такие скаляры.
    #[test]
    fn invalid_32byte_scalar_returns_err() {
        let dir = tempdir("badscalar");

        // Скаляр 0 — невалиден.
        let p_zero = dir.join("zero.json");
        let zero = VapidKeyFile {
            private_key_b64: B64URL.encode([0u8; 32]),
            public_key_b64: "x".to_string(),
        };
        std::fs::write(&p_zero, serde_json::to_vec(&zero).unwrap()).unwrap();
        assert!(
            VapidStore::load_or_generate(&p_zero).is_err(),
            "нулевой скаляр → Err"
        );

        // Скаляр 0xFF*32 — больше порядка n кривой P-256, невалиден.
        let p_ff = dir.join("ff.json");
        let ff = VapidKeyFile {
            private_key_b64: B64URL.encode([0xFFu8; 32]),
            public_key_b64: "x".to_string(),
        };
        std::fs::write(&p_ff, serde_json::to_vec(&ff).unwrap()).unwrap();
        assert!(
            VapidStore::load_or_generate(&p_ff).is_err(),
            "out-of-range скаляр (0xFF*32) → Err"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Толерантность к whitespace: ведущие/замыкающие пробелы/\n/\t вокруг
    /// валидного base64url приватника — .trim() в from_file_body снимает их,
    /// ключ грузится, публичный ключ совпадает с эталоном.
    #[test]
    fn whitespace_around_private_key_is_tolerated() {
        let dir = tempdir("ws");
        let path = dir.join("vapid.json");

        // Эталонная пара.
        let genuine = VapidStore::load_or_generate(&path).unwrap();
        let genuine_pub = genuine.public_key_b64().to_string();
        let priv_b64 = B64URL.encode(genuine.signing_key().to_bytes());

        // Оборачиваем приватник в whitespace.
        let wrapped = VapidKeyFile {
            private_key_b64: format!("  \t{priv_b64}\n  "),
            public_key_b64: genuine_pub.clone(),
        };
        std::fs::write(&path, serde_json::to_vec(&wrapped).unwrap()).unwrap();

        let reloaded = VapidStore::load_or_generate(&path).unwrap();
        assert_eq!(
            reloaded.public_key_b64(),
            genuine_pub,
            "trim() снимает whitespace, ключ грузится корректно"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ошибка чтения НЕ NotFound (путь — существующий каталог): возвращается
    /// Err c контекстом чтения, БЕЗ попытки генерации. Подтверждает, что
    /// только NotFound триггерит генерацию.
    #[test]
    fn read_error_not_notfound_returns_err_no_generation() {
        let dir = tempdir("isdir");
        // path сам является каталогом — read_to_string вернёт ошибку (не NotFound).
        let path = dir.join("a_directory");
        std::fs::create_dir_all(&path).unwrap();

        let r = VapidStore::load_or_generate(&path);
        assert!(r.is_err(), "чтение каталога → Err, не генерация");
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains("read VAPID key file"),
            "контекст чтения: {msg}"
        );
        // Каталог остался каталогом — генерация/перезапись не выполнялась.
        assert!(path.is_dir(), "путь остался каталогом (генерации не было)");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Идемпотентность на уровне ФС: при существующем валидном файле повторный
    /// load_or_generate НЕ перезаписывает файл (контент байт-в-байт прежний).
    #[test]
    fn reload_does_not_rewrite_file_bytes() {
        let dir = tempdir("norewrite");
        let path = dir.join("vapid.json");

        let _ = VapidStore::load_or_generate(&path).unwrap();
        let before = std::fs::read(&path).unwrap();

        let _ = VapidStore::load_or_generate(&path).unwrap();
        let after = std::fs::read(&path).unwrap();

        assert_eq!(before, after, "повторная загрузка не переписывает файл");
        // И никакого tmp рядом.
        assert!(!dir.join("vapid.json.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Запись в отсутствующий родительский каталог (эмуляция отсутствия
    /// ~/.forge): NotFound на файле → генерация → save_atomic пишет tmp, но
    /// родителя нет → Err. Фиксирует fail-soft инвариант: каталог НЕ создаётся
    /// автоматически.
    #[test]
    fn missing_parent_dir_returns_err_no_autocreate() {
        let dir = tempdir("noparent");
        // Несуществующий промежуточный каталог.
        let path = dir.join("does-not-exist").join("vapid.json");
        assert!(!path.parent().unwrap().exists());

        let r = VapidStore::load_or_generate(&path);
        assert!(
            r.is_err(),
            "запись в отсутствующий каталог → Err (нет auto-create)"
        );
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains("write tmp") || msg.contains("persist new VAPID key"),
            "контекст записи: {msg}"
        );
        // Каталог так и не создан.
        assert!(
            !path.parent().unwrap().exists(),
            "родительский каталог не должен создаваться автоматически"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Детерминизм derive_public_b64: один и тот же приватник всегда даёт один
    /// и тот же публичный ключ (важно для стабильного applicationServerKey
    /// между рестартами).
    #[test]
    fn derive_public_b64_is_deterministic() {
        let dir = tempdir("determ");
        let path = dir.join("vapid.json");
        let store = VapidStore::load_or_generate(&path).unwrap();

        let a = VapidStore::derive_public_b64(store.signing_key());
        let b = VapidStore::derive_public_b64(store.signing_key());
        assert_eq!(a, b, "derive_public_b64 детерминирован");
        assert_eq!(a, store.public_key_b64());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// public_key_b64 — формат applicationServerKey: base64url-no-pad (без '=',
    /// без '+'/'/'), декодируется в 65 байт, [0]==0x04.
    #[test]
    fn public_key_is_base64url_no_pad() {
        let dir = tempdir("nopad");
        let path = dir.join("vapid.json");
        let store = VapidStore::load_or_generate(&path).unwrap();
        let pk = store.public_key_b64();

        assert!(!pk.contains('='), "no-pad: без '=' ({pk})");
        assert!(!pk.contains('+'), "url-safe: без '+' ({pk})");
        assert!(!pk.contains('/'), "url-safe: без '/' ({pk})");

        let bytes = B64URL.decode(pk).unwrap();
        assert_eq!(bytes.len(), 65);
        assert_eq!(bytes[0], 0x04);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Debug-вывод VapidStore НЕ печатает приватный ключ (защита от утечки
    /// секрета в логи): содержит public_key_b64 и '<redacted>', но НЕ
    /// base64(приватника).
    #[test]
    fn debug_does_not_leak_private_key() {
        let dir = tempdir("nodebugleak");
        let path = dir.join("vapid.json");
        let store = VapidStore::load_or_generate(&path).unwrap();
        let priv_b64 = B64URL.encode(store.signing_key().to_bytes());

        for rendered in [format!("{store:?}"), format!("{store:#?}")] {
            assert!(
                rendered.contains(store.public_key_b64()),
                "Debug содержит публичный ключ: {rendered}"
            );
            assert!(
                rendered.contains("<redacted>"),
                "Debug маскирует приватник как <redacted>: {rendered}"
            );
            assert!(
                !rendered.contains(&priv_b64),
                "Debug НЕ должен содержать base64 приватника"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// default_vapid_path: при заданном HOME → ~/.forge/vapid.json; при
    /// отсутствии HOME → Err. Оба под-кейса в ОДНОМ тесте, чтобы set/remove_var
    /// (процесс-глобальны) не гонялись параллельно с другими HOME-зависимыми.
    /// Старое значение HOME сохраняется и восстанавливается.
    #[test]
    fn default_vapid_path_respects_home() {
        let saved = std::env::var("HOME").ok();

        // (а) HOME задан → путь оканчивается на .forge/vapid.json и начинается с HOME.
        let fake_home = std::env::temp_dir().join(format!(
            "forge-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("HOME", &fake_home);
        let p = default_vapid_path().expect("HOME задан → Ok");
        assert!(
            p.ends_with("vapid.json"),
            "путь оканчивается на vapid.json: {}",
            p.display()
        );
        assert!(
            p.starts_with(&fake_home),
            "путь начинается с HOME: {}",
            p.display()
        );
        assert_eq!(p, fake_home.join(".forge").join("vapid.json"));

        // (б) HOME удалён → Err с контекстом 'HOME env var not set'.
        std::env::remove_var("HOME");
        let r = default_vapid_path();
        assert!(r.is_err(), "без HOME → Err");
        let msg = format!("{:#}", r.unwrap_err());
        assert!(msg.contains("HOME"), "контекст про HOME: {msg}");

        // Восстановить исходный HOME.
        match saved {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
