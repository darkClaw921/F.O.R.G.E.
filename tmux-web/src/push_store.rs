//! Хранилище push-подписок (opt-in PWA, активируется флагом `--pwa`).
//!
//! Каждый браузер/устройство, согласившийся получать Web Push, присылает
//! свою [`StoredSubscription`] на `POST /api/push/subscribe` (см. `pwa.rs`).
//! Подписки переживают рестарт сервера — иначе после перезапуска некому было
//! бы слать пуши. Поэтому они живут в одном файле
//! `~/.forge/push_subscriptions.json` (рядом с `user_settings.json` и
//! `vapid.json`).
//!
//! ## Модель
//!
//! [`StoredSubscription`] — это сериализованный браузерный `PushSubscription`
//! плюс серверные метаданные:
//!   - `endpoint` — URL push-сервиса браузера (FCM/Mozilla/Apple). Уникальный
//!     идентификатор подписки: дедуп и удаление идут по нему.
//!   - `keys.p256dh` / `keys.auth` — публичный ECDH-ключ и auth-secret
//!     браузера (base64url). Нужны Фазе 3 для RFC8188-шифрования payload.
//!   - `device_label` — опциональная человекочитаемая метка устройства
//!     (необязательна; для UI «мои устройства»).
//!   - `created_at` — RFC3339-таймстемп момента подписки (ставит сервер).
//!
//! ## Persistence
//!
//! Паттерн копирует [`crate::user_settings::UserSettingsStore`]:
//! `Arc<RwLock<Inner>>` (cheap-clone, один экземпляр на процесс — кладётся в
//! `PwaCtx.subs`), atomic save (tmp + `fs::rename` поверх). На POSIX rename
//! атомарен в рамках одного mount-point: при `kill -9` в момент записи на
//! диске останется либо старый, либо новый файл, но не битый.
//!
//! ## Политика «битый файл не блокирует работу»
//!
//! [`PushSubscriptionStore::new`] при отсутствующем/нечитаемом/невалидном
//! файле НЕ паникует, а логирует `warn` и стартует с пустым списком (как
//! `UserSettingsStore`). Push — опциональная фича; повреждённый файл не должен
//! ронять сервер. Список пере-наполнится при следующих `subscribe`.
//!
//! ## Lazy file creation
//!
//! `new(path)` файл НЕ создаёт. Он появляется только при первой мутации
//! ([`upsert`](PushSubscriptionStore::upsert) /
//! [`remove`](PushSubscriptionStore::remove) с реальным изменением /
//! [`prune`](PushSubscriptionStore::prune)). Это сохраняет opt-in инвариант:
//! без `--pwa` стор не создаётся вовсе (см. `main.rs`), а с флагом, но без
//! единой подписки — файла на диске тоже нет.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Ключи браузерной push-подписки (`PushSubscription.getKey(...)`),
/// base64url-кодированные. Нужны Фазе 3 для шифрования payload по RFC8188
/// (`aes128gcm`): `p256dh` — публичный ECDH-ключ браузера (65-байт
/// uncompressed point), `auth` — 16-байт auth-secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionKeys {
    /// Публичный ECDH P-256 ключ браузера (base64url, uncompressed point).
    pub p256dh: String,
    /// Auth-secret подписки (base64url, 16 байт).
    pub auth: String,
}

/// Одна сохранённая push-подписка. Сериализуется как элемент массива в
/// `~/.forge/push_subscriptions.json`.
///
/// `endpoint` — первичный ключ: [`upsert`](PushSubscriptionStore::upsert)
/// дедуплицирует по нему, [`remove`](PushSubscriptionStore::remove) /
/// [`prune`](PushSubscriptionStore::prune) удаляют по нему.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSubscription {
    /// URL push-сервиса браузера — уникальный идентификатор подписки.
    pub endpoint: String,
    /// ECDH/auth-ключи для шифрования payload (Фаза 3).
    pub keys: SubscriptionKeys,
    /// Опциональная метка устройства (для UI; не участвует в дедупе).
    #[serde(default)]
    pub device_label: Option<String>,
    /// RFC3339-таймстемп подписки (ставит сервер при `subscribe`).
    pub created_at: String,
}

#[derive(Debug)]
struct Inner {
    subs: Vec<StoredSubscription>,
    path: PathBuf,
}

/// In-memory + on-disk хранилище push-подписок.
///
/// Cheap-clonable: внутри `Arc<RwLock<Inner>>`. Один экземпляр на процесс —
/// кладётся в `PwaCtx.subs` (`AppState.pwa`). Все мутации проходят под
/// write-lock'ом и делают atomic save на диск.
#[derive(Debug, Clone)]
pub struct PushSubscriptionStore {
    inner: Arc<RwLock<Inner>>,
}

impl PushSubscriptionStore {
    /// Создаёт store, загружая существующие подписки из `path`.
    ///
    /// Поведение при чтении файла:
    ///   - файл есть и парсится как `Vec<StoredSubscription>` → используем;
    ///   - файл есть, но битый/невалидный → `warn` + пустой список (битый
    ///     файл не блокирует работу);
    ///   - файла нет (`NotFound`) → пустой список без warn (нормальный
    ///     первый запуск; файл создастся при первой подписке).
    ///
    /// Файл **не создаётся** на этом этапе — см. «Lazy file creation».
    pub fn new(path: PathBuf) -> Self {
        let subs = match std::fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<Vec<StoredSubscription>>(&body) {
                Ok(list) => {
                    tracing::info!(
                        path = %path.display(),
                        count = list.len(),
                        "loaded push_subscriptions.json"
                    );
                    list
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "failed to parse push_subscriptions.json; starting with empty list"
                    );
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %path.display(),
                    "push_subscriptions.json not found; starting with empty list"
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = ?e,
                    "failed to read push_subscriptions.json; starting with empty list"
                );
                Vec::new()
            }
        };

        Self {
            inner: Arc::new(RwLock::new(Inner { subs, path })),
        }
    }

    /// Возвращает снимок (клон) всех подписок под read-lock'ом. Используется
    /// push-доставкой (Фаза 3) и `/api/push/test`, чтобы итерировать без
    /// удержания лока во время сетевых запросов.
    pub fn list(&self) -> Vec<StoredSubscription> {
        let inner = self.inner.read().expect("PushSubscriptionStore lock poisoned");
        inner.subs.clone()
    }

    /// Вставляет новую подписку или заменяет существующую с тем же
    /// `endpoint` (идемпотентно по endpoint — повторный `subscribe` того же
    /// браузера НЕ плодит дубли). Заменяет запись целиком (включая
    /// обновлённые `keys` — браузер мог ротировать ключи). После мутации —
    /// atomic save. Возвращает `Err` только при ошибке записи на диск.
    pub fn upsert(&self, sub: StoredSubscription) -> Result<()> {
        let mut inner = self.inner.write().expect("PushSubscriptionStore lock poisoned");
        match inner.subs.iter_mut().find(|s| s.endpoint == sub.endpoint) {
            Some(existing) => *existing = sub,
            None => inner.subs.push(sub),
        }
        save_locked(&inner)
    }

    /// Удаляет подписку по `endpoint`. Идемпотентно: если endpoint не найден,
    /// возвращает `Ok(false)` без записи на диск (вызывающий `unsubscribe`
    /// отвечает 200 в любом случае). При фактическом удалении делает atomic
    /// save и возвращает `Ok(true)`.
    pub fn remove(&self, endpoint: &str) -> Result<bool> {
        let mut inner = self.inner.write().expect("PushSubscriptionStore lock poisoned");
        let before = inner.subs.len();
        inner.subs.retain(|s| s.endpoint != endpoint);
        if inner.subs.len() == before {
            // Ничего не удалили — на диск не пишем (идемпотентность без I/O).
            return Ok(false);
        }
        save_locked(&inner)?;
        Ok(true)
    }

    /// Батч-удаление подписок по списку `endpoints` (мёртвые endpoint'ы,
    /// которые push-сервис вернул 404/410 при доставке — Фаза 3). Делает
    /// **одну** запись на диск вместо N. Возвращает число фактически
    /// удалённых записей; если ни одна не совпала — диск не трогается
    /// (`Ok(0)`).
    pub fn prune(&self, endpoints: &[String]) -> Result<usize> {
        if endpoints.is_empty() {
            return Ok(0);
        }
        let mut inner = self.inner.write().expect("PushSubscriptionStore lock poisoned");
        let before = inner.subs.len();
        inner.subs.retain(|s| !endpoints.contains(&s.endpoint));
        let removed = before - inner.subs.len();
        if removed == 0 {
            return Ok(0);
        }
        save_locked(&inner)?;
        Ok(removed)
    }
}

/// Атомарно сохраняет подписки под write-lock'ом.
///
/// Стратегия идентична `user_settings::save_locked` / `vapid::save_atomic`:
/// сериализуем в `<file>.tmp`, затем `rename` поверх. Родительский каталог
/// создаётся при необходимости (но в норме `~/.forge` уже создан в `main.rs`
/// под `pwa_enabled`).
fn save_locked(inner: &Inner) -> Result<()> {
    let body = serde_json::to_vec_pretty(&inner.subs)
        .context("failed to serialize push subscriptions")?;

    if let Some(parent) = inner.path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent dir {}", parent.display())
            })?;
        }
    }

    let mut tmp = inner.path.clone();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, &body)
        .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &inner.path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp.display(),
            inner.path.display()
        )
    })?;
    Ok(())
}

/// `~/.forge/push_subscriptions.json` — путь к файлу подписок. Резолвится от
/// `HOME` (рядом с `user_settings.json` / `vapid.json`). Возвращает `Err`,
/// если `HOME` не задан.
///
/// Используется в `main.rs` под `pwa_enabled` для
/// [`PushSubscriptionStore::new`].
pub fn default_subscriptions_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home)
        .join(".forge")
        .join("push_subscriptions.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Уникальный путь во временной директории (UUID v4), как в
    /// `user_settings` тестах.
    fn tmp_path(label: &str) -> PathBuf {
        let id = uuid::Uuid::new_v4();
        std::env::temp_dir().join(format!("devforge_push_subs_{label}_{id}.json"))
    }

    fn sub(endpoint: &str) -> StoredSubscription {
        StoredSubscription {
            endpoint: endpoint.to_string(),
            keys: SubscriptionKeys {
                p256dh: "p256dh-key".to_string(),
                auth: "auth-secret".to_string(),
            },
            device_label: Some("test-device".to_string()),
            created_at: "2026-06-25T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn empty_when_no_file() {
        let path = tmp_path("empty_no_file");
        assert!(!path.exists(), "precondition: path must not exist");
        let store = PushSubscriptionStore::new(path.clone());
        assert!(store.list().is_empty());
        // Чтение не должно создавать файл (lazy creation).
        assert!(!path.exists(), "file must not be created on read-only access");
    }

    #[test]
    fn upsert_inserts_and_persists() {
        let path = tmp_path("upsert_insert");
        let store = PushSubscriptionStore::new(path.clone());

        store.upsert(sub("https://push.example/a")).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].endpoint, "https://push.example/a");

        // Reload с диска — подписка сохранена.
        let store2 = PushSubscriptionStore::new(path.clone());
        let list2 = store2.list();
        assert_eq!(list2.len(), 1);
        assert_eq!(list2[0].endpoint, "https://push.example/a");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upsert_dedups_by_endpoint() {
        let path = tmp_path("upsert_dedup");
        let store = PushSubscriptionStore::new(path.clone());

        // Один и тот же endpoint дважды — без дублей.
        store.upsert(sub("https://push.example/same")).unwrap();
        let mut updated = sub("https://push.example/same");
        updated.keys.p256dh = "rotated-key".to_string();
        updated.device_label = Some("renamed".to_string());
        store.upsert(updated).unwrap();

        let list = store.list();
        assert_eq!(list.len(), 1, "повторный subscribe не должен плодить дубли");
        // Запись заменена целиком (новые keys/label).
        assert_eq!(list[0].keys.p256dh, "rotated-key");
        assert_eq!(list[0].device_label.as_deref(), Some("renamed"));

        // И на диске тоже один элемент.
        let store2 = PushSubscriptionStore::new(path.clone());
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].keys.p256dh, "rotated-key");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remove_is_idempotent() {
        let path = tmp_path("remove_idem");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/x")).unwrap();

        // Первое удаление — true.
        assert!(store.remove("https://push.example/x").unwrap());
        assert!(store.list().is_empty());

        // Повторное удаление того же endpoint — false (идемпотентно, без паники).
        assert!(!store.remove("https://push.example/x").unwrap());
        // Удаление никогда-не-существовавшего — тоже false.
        assert!(!store.remove("https://push.example/never").unwrap());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prune_batch_removes_listed() {
        let path = tmp_path("prune_batch");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        store.upsert(sub("https://push.example/b")).unwrap();
        store.upsert(sub("https://push.example/c")).unwrap();
        assert_eq!(store.list().len(), 3);

        // Удаляем a и c одним батчем; b остаётся. d не существует — игнор.
        let removed = store
            .prune(&[
                "https://push.example/a".to_string(),
                "https://push.example/c".to_string(),
                "https://push.example/d".to_string(),
            ])
            .unwrap();
        assert_eq!(removed, 2, "должны удалиться ровно a и c");

        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].endpoint, "https://push.example/b");

        // Persistence.
        let store2 = PushSubscriptionStore::new(path.clone());
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].endpoint, "https://push.example/b");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn prune_empty_is_noop() {
        let path = tmp_path("prune_empty");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();

        // Пустой список endpoint'ов — ничего не удаляем, ничего не пишем.
        assert_eq!(store.prune(&[]).unwrap(), 0);
        // Несовпадающие endpoint'ы — тоже 0.
        assert_eq!(
            store.prune(&["https://push.example/zzz".to_string()]).unwrap(),
            0
        );
        assert_eq!(store.list().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn roundtrip_save_load_preserves_fields() {
        // Полный roundtrip: все поля переживают save/load без потерь.
        let path = tmp_path("roundtrip");
        let store = PushSubscriptionStore::new(path.clone());

        let original = StoredSubscription {
            endpoint: "https://push.example/full".to_string(),
            keys: SubscriptionKeys {
                p256dh: "BExamplePublicKey".to_string(),
                auth: "AuthSecret16Byte".to_string(),
            },
            device_label: None, // проверяем и None-вариант
            created_at: "2026-06-25T12:34:56Z".to_string(),
        };
        store.upsert(original.clone()).unwrap();

        let store2 = PushSubscriptionStore::new(path.clone());
        let loaded = store2.list();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], original, "все поля должны пережить roundtrip");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_yields_empty_list_no_panic() {
        // Битый JSON → пустой список + warn (не паника).
        let path = tmp_path("corrupt");
        std::fs::write(&path, b"{ this is not valid json :(").unwrap();

        let store = PushSubscriptionStore::new(path.clone());
        assert!(
            store.list().is_empty(),
            "битый файл → пустой список, без паники"
        );

        // После битого файла стор должен оставаться рабочим: upsert чинит
        // файл (перезаписывает валидным JSON).
        store.upsert(sub("https://push.example/recover")).unwrap();
        let store2 = PushSubscriptionStore::new(path.clone());
        assert_eq!(store2.list().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_is_atomic_no_tmp_left_behind() {
        let path = tmp_path("atomic");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();

        // tmp-файл не должен остаться после успешного rename.
        let mut tmp = path.clone();
        let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
        tmp_name.push(".tmp");
        tmp.set_file_name(tmp_name);
        assert!(!tmp.exists(), "tmp-файл должен быть удалён через rename");

        // И сам файл должен быть валидным Vec на диске (не битый/частичный).
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<StoredSubscription> = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    // =========================================================================
    // Edge cases — расширение (НЕ дублирует существующие 9 тестов выше).
    // =========================================================================

    /// Хелпер: прочитать файл с диска и распарсить как Vec (паника при битом
    /// JSON — что и нужно: тест должен упасть, если на диске невалидно).
    fn read_disk(path: &PathBuf) -> Vec<StoredSubscription> {
        let body = std::fs::read_to_string(path).expect("file readable");
        serde_json::from_str(&body).expect("disk JSON is a valid Vec")
    }

    /// Ожидаемый tmp-путь рядом с основным файлом (`<name>.tmp`).
    fn tmp_sibling(path: &PathBuf) -> PathBuf {
        let mut tmp = path.clone();
        let mut name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
        name.push(".tmp");
        tmp.set_file_name(name);
        tmp
    }

    /// upsert ротации ключей у существующего endpoint: единственная запись,
    /// и ротируются ВСЕ поля keys — включая `auth` (существующий
    /// `upsert_dedups_by_endpoint` проверяет только p256dh).
    #[test]
    fn upsert_rotates_auth_and_label_not_just_p256dh() {
        let path = tmp_path("rotate_auth");
        let store = PushSubscriptionStore::new(path.clone());

        let mut a = sub("https://push.example/E");
        a.keys.p256dh = "K1".into();
        a.keys.auth = "S1".into();
        a.device_label = Some("L1".into());
        store.upsert(a).unwrap();

        let mut b = sub("https://push.example/E");
        b.keys.p256dh = "K2".into();
        b.keys.auth = "S2".into();
        b.device_label = Some("L2".into());
        store.upsert(b).unwrap();

        let list = store.list();
        assert_eq!(list.len(), 1, "ротация ключей не плодит дубль");
        assert_eq!(list[0].keys.p256dh, "K2");
        assert_eq!(list[0].keys.auth, "S2", "auth тоже должен ротироваться");
        assert_eq!(list[0].device_label.as_deref(), Some("L2"));

        // На диске после reload — тоже ровно одна запись с новыми ключами.
        let disk = read_disk(&path);
        assert_eq!(disk.len(), 1);
        assert_eq!(disk[0].keys.auth, "S2");

        let _ = std::fs::remove_file(&path);
    }

    /// list() и on-disk сохраняют порядок вставки; обновление существующего
    /// endpoint обновляет запись in-place, НЕ переставляя её в конец.
    #[test]
    fn upsert_preserves_insertion_order_in_place_update() {
        let path = tmp_path("order");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        store.upsert(sub("https://push.example/b")).unwrap();
        store.upsert(sub("https://push.example/c")).unwrap();

        // Порядок вставки сохранён.
        let endpoints: Vec<String> = store.list().into_iter().map(|s| s.endpoint).collect();
        assert_eq!(
            endpoints,
            vec![
                "https://push.example/a".to_string(),
                "https://push.example/b".to_string(),
                "https://push.example/c".to_string(),
            ]
        );

        // Обновление B (ротация ключа) НЕ переставляет его в конец.
        let mut b2 = sub("https://push.example/b");
        b2.keys.p256dh = "rotated".into();
        store.upsert(b2).unwrap();

        let list = store.list();
        assert_eq!(list[1].endpoint, "https://push.example/b", "B остаётся 2-м");
        assert_eq!(list[1].keys.p256dh, "rotated");
        assert_eq!(list[2].endpoint, "https://push.example/c", "C остаётся 3-м");

        // Порядок на диске тоже [a, b, c].
        let disk: Vec<String> = read_disk(&path).into_iter().map(|s| s.endpoint).collect();
        assert_eq!(disk[0], "https://push.example/a");
        assert_eq!(disk[1], "https://push.example/b");
        assert_eq!(disk[2], "https://push.example/c");

        let _ = std::fs::remove_file(&path);
    }

    /// remove несуществующего endpoint на НОВОМ сторе (файла нет): Ok(false),
    /// файл НЕ создаётся (lazy-creation сохранён), без паники.
    #[test]
    fn remove_missing_on_fresh_store_does_not_create_file() {
        let path = tmp_path("remove_missing_fresh");
        assert!(!path.exists(), "precondition: файла нет");
        let store = PushSubscriptionStore::new(path.clone());

        assert!(!store.remove("https://push.example/never").unwrap());
        assert!(store.list().is_empty());
        assert!(
            !path.exists(),
            "remove без фактического удаления не должен создавать файл"
        );
    }

    /// remove одного из нескольких удаляет только целевой, сохраняя порядок
    /// оставшихся и на диске.
    #[test]
    fn remove_one_preserves_rest_and_order() {
        let path = tmp_path("remove_one");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        store.upsert(sub("https://push.example/b")).unwrap();
        store.upsert(sub("https://push.example/c")).unwrap();

        assert!(store.remove("https://push.example/b").unwrap());

        let endpoints: Vec<String> = store.list().into_iter().map(|s| s.endpoint).collect();
        assert_eq!(
            endpoints,
            vec![
                "https://push.example/a".to_string(),
                "https://push.example/c".to_string(),
            ]
        );

        let disk: Vec<String> = read_disk(&path).into_iter().map(|s| s.endpoint).collect();
        assert_eq!(disk, vec!["https://push.example/a", "https://push.example/c"]);

        let _ = std::fs::remove_file(&path);
    }

    /// prune пустым срезом и срезом только-несовпадений — Ok(0), файл на диске
    /// НЕ перезаписан (содержимое байт-в-байт идентично). На новом сторе
    /// prune(&[]) не создаёт файл.
    #[test]
    fn prune_empty_and_nonmatching_do_not_touch_disk() {
        let path = tmp_path("prune_no_touch");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();

        let before = std::fs::read_to_string(&path).unwrap();

        // Пустой срез — ранний return до write-lock.
        assert_eq!(store.prune(&[]).unwrap(), 0);
        // Только несовпадения — removed==0 → ранний return до save.
        assert_eq!(
            store
                .prune(&["zzz".to_string(), "yyy".to_string()])
                .unwrap(),
            0
        );

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "файл не должен быть перезаписан при Ok(0)");
        assert_eq!(store.list().len(), 1);

        // На НОВОМ сторе без файла prune(&[]) не создаёт файл.
        let fresh_path = tmp_path("prune_fresh_noop");
        let fresh = PushSubscriptionStore::new(fresh_path.clone());
        assert_eq!(fresh.prune(&[]).unwrap(), 0);
        assert!(!fresh_path.exists(), "prune(&[]) не создаёт файл");

        let _ = std::fs::remove_file(&path);
    }

    /// prune, удаляющий ВСЕ записи, оставляет пустой список и пишет валидный
    /// пустой массив на диск (не битый файл).
    #[test]
    fn prune_all_writes_valid_empty_array() {
        let path = tmp_path("prune_all");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        store.upsert(sub("https://push.example/b")).unwrap();

        let removed = store
            .prune(&[
                "https://push.example/a".to_string(),
                "https://push.example/b".to_string(),
            ])
            .unwrap();
        assert_eq!(removed, 2);
        assert!(store.list().is_empty());

        // На диске — валидный пустой Vec.
        let disk = read_disk(&path);
        assert!(disk.is_empty());
        // Reload через new() тоже даёт пустой список.
        let store2 = PushSubscriptionStore::new(path.clone());
        assert!(store2.list().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    /// prune с дублирующимися endpoint в срезе удаляет элемент один раз:
    /// removed == before-after, а не длина среза.
    #[test]
    fn prune_dedups_repeated_endpoints_in_slice() {
        let path = tmp_path("prune_dups");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        store.upsert(sub("https://push.example/b")).unwrap();

        let removed = store
            .prune(&[
                "https://push.example/a".to_string(),
                "https://push.example/a".to_string(),
                "https://push.example/a".to_string(),
            ])
            .unwrap();
        assert_eq!(removed, 1, "a удалён один раз, не 3 (removed = before-after)");
        let endpoints: Vec<String> = store.list().into_iter().map(|s| s.endpoint).collect();
        assert_eq!(endpoints, vec!["https://push.example/b".to_string()]);

        let _ = std::fs::remove_file(&path);
    }

    /// roundtrip с device_label=Some(...) — дополняет существующий
    /// roundtrip_save_load_preserves_fields (там None-вариант).
    #[test]
    fn roundtrip_preserves_device_label_some() {
        let path = tmp_path("roundtrip_some");
        let store = PushSubscriptionStore::new(path.clone());

        let original = StoredSubscription {
            endpoint: "https://push.example/labelled".to_string(),
            keys: SubscriptionKeys {
                p256dh: "BPub".to_string(),
                auth: "Auth".to_string(),
            },
            device_label: Some("iPhone Сергея 📱".to_string()),
            created_at: "2026-06-25T12:34:56Z".to_string(),
        };
        store.upsert(original.clone()).unwrap();

        let store2 = PushSubscriptionStore::new(path.clone());
        let loaded = store2.list();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], original, "Some(label) с unicode переживает roundtrip");

        let _ = std::fs::remove_file(&path);
    }

    /// Валидный JSON неверной формы (объект вместо массива) → пустой список,
    /// без паники. Отличается от «синтаксически битого».
    #[test]
    fn valid_json_wrong_shape_object_yields_empty() {
        let path = tmp_path("wrong_shape_object");
        std::fs::write(&path, br#"{"endpoint":"x"}"#).unwrap();
        let store = PushSubscriptionStore::new(path.clone());
        assert!(store.list().is_empty(), "объект вместо Vec → пустой список");
        let _ = std::fs::remove_file(&path);
    }

    /// Валидный массив с элементом без обязательного поля (нет created_at) →
    /// пустой список, без паники (десериализация Vec падает целиком).
    #[test]
    fn array_with_missing_required_field_yields_empty() {
        let path = tmp_path("missing_field");
        std::fs::write(
            &path,
            br#"[{"endpoint":"x","keys":{"p256dh":"a","auth":"b"}}]"#,
        )
        .unwrap();
        let store = PushSubscriptionStore::new(path.clone());
        assert!(
            store.list().is_empty(),
            "отсутствие created_at → Err → пустой список, без partial-load"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Пустой файл (ноль байт) → пустой список (serde Err на EOF), без паники,
    /// НЕ NotFound-ветка.
    #[test]
    fn empty_file_yields_empty_no_panic() {
        let path = tmp_path("empty_bytes");
        std::fs::write(&path, b"").unwrap();
        let store = PushSubscriptionStore::new(path.clone());
        assert!(store.list().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// Файл с валидным пустым массивом `[]` → пустой список (нормальный кейс).
    #[test]
    fn valid_empty_array_file_yields_empty() {
        let path = tmp_path("empty_array");
        std::fs::write(&path, b"[]").unwrap();
        let store = PushSubscriptionStore::new(path.clone());
        assert!(store.list().is_empty());
        // upsert после этого добавляет запись (стор рабочий).
        store.upsert(sub("https://push.example/x")).unwrap();
        assert_eq!(store.list().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// created_at хранится verbatim — стор не парсит/не нормализует RFC3339.
    #[test]
    fn created_at_stored_verbatim() {
        let path = tmp_path("created_verbatim");
        let store = PushSubscriptionStore::new(path.clone());
        let mut s = sub("https://push.example/ts");
        s.created_at = "2026-06-25T12:34:56Z".to_string();
        store.upsert(s).unwrap();

        let store2 = PushSubscriptionStore::new(path.clone());
        assert_eq!(store2.list()[0].created_at, "2026-06-25T12:34:56Z");
        let _ = std::fs::remove_file(&path);
    }

    /// Атомарность: после КАЖДОЙ мутации файл на диске парсится в валидный Vec
    /// (serde никогда не видит частичный файл; rename атомарен).
    #[test]
    fn disk_always_valid_across_mutations() {
        let path = tmp_path("always_valid");
        let store = PushSubscriptionStore::new(path.clone());

        store.upsert(sub("https://push.example/a")).unwrap();
        assert_eq!(read_disk(&path).len(), 1);
        assert!(!tmp_sibling(&path).exists());

        store.upsert(sub("https://push.example/b")).unwrap();
        assert_eq!(read_disk(&path).len(), 2);
        assert!(!tmp_sibling(&path).exists());

        assert!(store.remove("https://push.example/a").unwrap());
        let disk: Vec<String> = read_disk(&path).into_iter().map(|s| s.endpoint).collect();
        assert_eq!(disk, vec!["https://push.example/b".to_string()]);
        assert!(!tmp_sibling(&path).exists());

        let _ = std::fs::remove_file(&path);
    }

    /// save создаёт родительский каталог при необходимости (create_dir_all).
    #[test]
    fn save_creates_parent_dir() {
        let id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("devforge_push_nonexistent_{id}"));
        let path = dir.join("subs.json");
        assert!(!dir.exists(), "precondition: каталога нет");

        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        assert!(path.exists(), "файл создан несмотря на отсутствующий каталог");

        let store2 = PushSubscriptionStore::new(path.clone());
        assert_eq!(store2.list().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// on-disk формат — pretty-printed JSON массив (to_vec_pretty).
    #[test]
    fn on_disk_format_is_pretty_array() {
        let path = tmp_path("pretty");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();
        store.upsert(sub("https://push.example/b")).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let trimmed = body.trim();
        assert!(trimmed.starts_with('['), "массив: got {body}");
        assert!(trimmed.ends_with(']'), "массив: got {body}");
        assert!(body.contains('\n'), "pretty-print должен содержать переносы строк");
        assert_eq!(read_disk(&path).len(), 2);

        let _ = std::fs::remove_file(&path);
    }

    /// Конкурентные upsert разных endpoint под RwLock — все сохраняются без
    /// потерь/дублей/паники.
    #[test]
    fn concurrent_upsert_distinct_endpoints() {
        use std::collections::HashSet;
        let path = tmp_path("concurrent_distinct");
        let store = PushSubscriptionStore::new(path.clone());

        std::thread::scope(|scope| {
            for i in 0..8 {
                let s = store.clone();
                scope.spawn(move || {
                    s.upsert(sub(&format!("https://push.example/e{i}"))).unwrap();
                });
            }
        });

        let list = store.list();
        assert_eq!(list.len(), 8, "все 8 endpoint сохранены без потерь");
        let set: HashSet<String> = list.iter().map(|s| s.endpoint.clone()).collect();
        for i in 0..8 {
            assert!(set.contains(&format!("https://push.example/e{i}")));
        }
        // Последняя запись на диск содержит все 8.
        assert_eq!(read_disk(&path).len(), 8);

        let _ = std::fs::remove_file(&path);
    }

    /// Конкурентные upsert ОДНОГО endpoint из нескольких потоков → ровно одна
    /// запись (дедуп под локом, last-writer-wins).
    #[test]
    fn concurrent_upsert_same_endpoint_single_record() {
        let path = tmp_path("concurrent_same");
        let store = PushSubscriptionStore::new(path.clone());

        std::thread::scope(|scope| {
            for i in 0..8 {
                let s = store.clone();
                scope.spawn(move || {
                    let mut sub = sub("https://push.example/same");
                    sub.device_label = Some(format!("dev-{i}"));
                    s.upsert(sub).unwrap();
                });
            }
        });

        assert_eq!(store.list().len(), 1, "дедуп под write-lock → одна запись");
        assert_eq!(read_disk(&path).len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    /// Конкурентные upsert + remove одного endpoint не паникуют и оставляют
    /// консистентное состояние (RwLock не отравлен).
    #[test]
    fn concurrent_upsert_and_remove_no_panic() {
        let path = tmp_path("concurrent_churn");
        let store = PushSubscriptionStore::new(path.clone());
        let endpoint = "https://push.example/churn";

        std::thread::scope(|scope| {
            let a = store.clone();
            scope.spawn(move || {
                for _ in 0..50 {
                    a.upsert(sub(endpoint)).unwrap();
                }
            });
            let b = store.clone();
            scope.spawn(move || {
                for _ in 0..50 {
                    let _ = b.remove(endpoint).unwrap();
                }
            });
        });

        // Финальное состояние консистентно: 0 или 1 запись, файл валиден.
        let n = store.list().len();
        assert!(n <= 1, "не более одной записи одного endpoint");
        if path.exists() {
            let _ = read_disk(&path); // паника, если битый
        }

        let _ = std::fs::remove_file(&path);
    }

    /// list() возвращает независимый снимок — мутация после list не влияет на
    /// ранее полученный Vec.
    #[test]
    fn list_returns_independent_snapshot() {
        let path = tmp_path("snapshot");
        let store = PushSubscriptionStore::new(path.clone());
        store.upsert(sub("https://push.example/a")).unwrap();

        let snap = store.list();
        store.upsert(sub("https://push.example/b")).unwrap();

        assert_eq!(snap.len(), 1, "снимок не меняется после последующего upsert");
        assert_eq!(store.list().len(), 2);

        let _ = std::fs::remove_file(&path);
    }

    /// cheap-clone стора разделяет одно состояние (Arc): мутация через один
    /// клон видна через другой.
    #[test]
    fn clone_shares_state_via_arc() {
        let path = tmp_path("clone_share");
        let s1 = PushSubscriptionStore::new(path.clone());
        let s2 = s1.clone();

        s2.upsert(sub("https://push.example/a")).unwrap();
        assert_eq!(s1.list().len(), 1, "оба клона указывают на один Arc-state");

        let _ = std::fs::remove_file(&path);
    }

    /// upsert возвращает Err (а не панику) когда parent существующего файла —
    /// не каталог. in-memory состояние при этом уже изменено (save после
    /// мутации) — задокументированное поведение.
    #[test]
    fn upsert_returns_err_on_write_failure_no_panic() {
        // parent = обычный файл, а не каталог → create_dir_all/write упадёт.
        let id = uuid::Uuid::new_v4();
        let blocker = std::env::temp_dir().join(format!("devforge_push_blocker_{id}"));
        std::fs::write(&blocker, b"i am a file, not a dir").unwrap();
        let path = blocker.join("subs.json"); // parent (blocker) — файл

        let store = PushSubscriptionStore::new(path.clone());
        let res = store.upsert(sub("https://push.example/a"));
        assert!(res.is_err(), "ошибка записи → Err, без паники");
        // in-memory запись добавлена несмотря на ошибку save (save после мутации).
        assert_eq!(store.list().len(), 1);

        let _ = std::fs::remove_file(&blocker);
    }

    /// default_subscriptions_path: HOME задан → корректный путь; HOME снят →
    /// Err. Один тест, env восстанавливается в конце (env-мутации не
    /// thread-safe — держим в одном месте).
    #[test]
    fn default_subscriptions_path_home_behaviour() {
        let saved = std::env::var("HOME").ok();

        // HOME задан → путь оканчивается на .forge/push_subscriptions.json и
        // начинается с HOME.
        std::env::set_var("HOME", "/some/home");
        let p = default_subscriptions_path().expect("HOME set → Ok");
        assert!(
            p.ends_with("push_subscriptions.json"),
            "путь оканчивается на файл подписок: {}",
            p.display()
        );
        assert!(
            p.to_string_lossy().contains("/some/home/.forge/"),
            "путь под HOME/.forge: {}",
            p.display()
        );

        // HOME снят → Err (context "HOME env var not set").
        std::env::remove_var("HOME");
        assert!(
            default_subscriptions_path().is_err(),
            "без HOME → Err"
        );

        // Восстановить HOME.
        match saved {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}
