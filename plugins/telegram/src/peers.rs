//! Резолв `dialog_id` из URL в [`PeerRef`], пригодный для вызовов grammers.
//!
//! # Зачем отдельный слой
//!
//! Фронт знает диалог по строке Bot API-формата (`777000`, `-100…`), а
//! методам MTProto нужен [`PeerRef`] = идентичность + `access_hash`
//! (`PeerAuth`). Хеш выдаётся Telegram'ом вместе с объектом peer'а и
//! персистится в сессии; «просто взять из id» его нельзя. Поэтому каждый
//! запрос вида `POST /dialogs/:id/messages` обязан сначала пройти через
//! [`resolve_peer`].
//!
//! # Три эшелона резолва (от дешёвого к дорогому)
//!
//! ```text
//!   "-1001234567890"
//!        │ parse_dialog_id            ошибка ⇒ 404 unknown_dialog
//!        ▼
//!   state.peers (RwLock<HashMap<PeerId, PeerRef>>)   ← попадание: 0 I/O
//!        │ промах
//!        ▼
//!   FileSession::peer_ref (кэш сессии на диске)      ← попадание: 0 сети
//!        │ промах
//!        ▼
//!   client.iter_dialogs() (ограниченный проход)      ← сеть, дорого
//!        │ не нашли
//!        ▼
//!   404 unknown_dialog
//! ```
//!
//! Проход по диалогам ограничен [`DIALOG_SCAN_LIMIT`]: он существует только
//! для случая «сессия свежая/переехала, а пользователь уже жмёт на диалог».
//! Побочный эффект прохода — пополнение обоих кэшей, поэтому второй промах
//! по тому же peer'у сети уже не касается.
//!
//! # Locks и await
//!
//! Guard'ы `RwLock` НИКОГДА не живут через `.await`: значение забирается в
//! локальную переменную, guard дропается явным блоком. Иначе медленный RPC
//! (а `iter_dialogs` может отвечать секунды) держал бы кэш заблокированным
//! и все параллельные запросы встали бы в очередь — вплоть до дедлока с
//! writer'ом кэша из updates-воркера.

use std::sync::Arc;

use async_trait::async_trait;
use grammers_client::Client;
use grammers_session::types::{PeerId, PeerRef};
use grammers_session::Session;

use crate::dto;
use crate::routes::error::ApiError;
use crate::session::FileSession;
use crate::state::TelegramState;

/// Сколько диалогов максимум просматривать в дорогом эшелоне резолва.
///
/// 200 покрывает «активную» часть списка любого пользователя; больше —
/// бессмысленно тратить RPC на диалог, о котором фронт всё равно узнал из
/// того же `GET /dialogs` (а тот уже наполнил кэш).
pub const DIALOG_SCAN_LIMIT: usize = 200;

/// Источник peer'ов «мимо кэша в памяти».
///
/// Существует ради тестируемости: [`resolve_peer_with`] содержит всю логику
/// эшелонов, а сеть и сессия подменяются в тестах фейком, который ещё и
/// считает обращения (иначе «кэш экономит вызовы» пришлось бы проверять
/// глазами по логам).
#[async_trait]
pub trait PeerSource: Send + Sync {
    /// Дешёвый эшелон: персистентный кэш сессии на диске.
    async fn lookup_session(&self, id: PeerId) -> Option<PeerRef>;

    /// Дорогой эшелон: ограниченный проход по диалогам.
    ///
    /// Реализация ОБЯЗАНА попутно наполнять кэш `state.peers` — иначе
    /// следующий промах снова пойдёт в сеть.
    async fn scan_dialogs(&self, state: &Arc<TelegramState>, id: PeerId) -> Option<PeerRef>;
}

/// Продакшен-источник: живая сессия + живой клиент.
pub struct RuntimeSource {
    /// Клиент для `iter_dialogs`.
    pub client: Client,
    /// Хранилище сессии (в нём лежат `access_hash`'и известных peer'ов).
    pub session: Arc<FileSession>,
}

#[async_trait]
impl PeerSource for RuntimeSource {
    async fn lookup_session(&self, id: PeerId) -> Option<PeerRef> {
        match self.session.peer_ref(id).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "forge_telegram",
                    error = %e,
                    "forge-telegram: session peer lookup failed"
                );
                None
            }
        }
    }

    async fn scan_dialogs(&self, state: &Arc<TelegramState>, id: PeerId) -> Option<PeerRef> {
        let mut iter = self.client.iter_dialogs().limit(DIALOG_SCAN_LIMIT);
        let mut found = None;
        let mut scanned = 0usize;
        loop {
            match iter.next().await {
                Ok(Some(dialog)) => {
                    scanned += 1;
                    // Через `Peer::to_ref()`, а НЕ `Dialog::peer_ref()`:
                    // последний делает `auth().unwrap()` и паникует на
                    // «min»-peer'ах, пришедших без access_hash.
                    let Ok(Some(peer)) = dialog.peer().to_ref().await else {
                        continue;
                    };
                    cache_peer(state, peer).await;
                    if peer.id == id {
                        found = Some(peer);
                        // Не выходим: остаток прохода уже оплачен тем же
                        // RPC-чанком, а пополненный кэш избавит от следующих
                        // проходов.
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(
                        target: "forge_telegram",
                        error = %e,
                        "forge-telegram: dialog scan for peer resolution failed"
                    );
                    break;
                }
            }
        }
        tracing::debug!(
            target: "forge_telegram",
            scanned,
            resolved = found.is_some(),
            "forge-telegram: peer resolution scanned dialogs"
        );
        found
    }
}

/// Кладёт peer'а в кэш в памяти.
///
/// Вызывается загрузчиком диалогов и updates-воркером: апдейты приносят
/// свежие `access_hash`'и, и запоминать их дешевле, чем потом ходить за
/// ними в сеть. Перезапись существующей записи — норма (хеш мог обновиться).
pub async fn cache_peer(state: &Arc<TelegramState>, peer: PeerRef) {
    state.peers.write().await.insert(peer.id, peer);
}

/// Заглядывает в кэш в памяти, не удерживая lock через await.
pub async fn cached_peer(state: &Arc<TelegramState>, id: PeerId) -> Option<PeerRef> {
    let guard = state.peers.read().await;
    guard.get(&id).copied()
}

/// Сколько peer'ов сейчас в кэше (для логов и тестов).
pub async fn cache_len(state: &Arc<TelegramState>) -> usize {
    state.peers.read().await.len()
}

/// Полный резолв с подменяемым источником — вся логика эшелонов здесь.
///
/// # Errors
///
/// `404 unknown_dialog` — id не парсится или peer недоступен ни в одном из
/// эшелонов. Других исходов у функции нет: она не ходит в сеть сама и не
/// умеет отличать «нет связи» от «нет диалога» — это задача вызывающего
/// хендлера, который до резолва проверил наличие соединения.
pub async fn resolve_peer_with<S: PeerSource + ?Sized>(
    state: &Arc<TelegramState>,
    dialog_id: &str,
    source: &S,
) -> Result<PeerRef, ApiError> {
    let id = dto::parse_dialog_id(dialog_id).ok_or_else(|| {
        ApiError::unknown_dialog(format!("'{dialog_id}' is not a valid Telegram dialog id"))
    })?;

    if let Some(peer) = cached_peer(state, id).await {
        return Ok(peer);
    }

    if let Some(peer) = source.lookup_session(id).await {
        cache_peer(state, peer).await;
        return Ok(peer);
    }

    if let Some(peer) = source.scan_dialogs(state, id).await {
        // scan_dialogs уже положил peer'а в кэш, но повторная запись
        // идемпотентна и защищает от реализаций, которые этого не делают.
        cache_peer(state, peer).await;
        return Ok(peer);
    }

    Err(ApiError::unknown_dialog(format!(
        "dialog '{dialog_id}' is unknown or not accessible for this account"
    )))
}

/// Резолв по живому подключению.
///
/// # Errors
///
/// - `503 not_connected` — MTProto-подключения нет (нечем резолвить);
/// - `404 unknown_dialog` — см. [`resolve_peer_with`].
pub async fn resolve_peer(
    state: &Arc<TelegramState>,
    dialog_id: &str,
) -> Result<PeerRef, ApiError> {
    // Порядок проверок — часть контракта. Сначала форма id: мусорный
    // `dialog_id` — ошибка клиента (404) в любом состоянии соединения, и
    // отвечать на него `503 not_connected` значило бы врать («переподключись
    // и повтори» тут не поможет). Затем кэш: попадание не требует ни
    // рантайма, ни сети. И только потом — проверка соединения.
    let id = dto::parse_dialog_id(dialog_id).ok_or_else(|| {
        ApiError::unknown_dialog(format!("'{dialog_id}' is not a valid Telegram dialog id"))
    })?;
    if let Some(peer) = cached_peer(state, id).await {
        return Ok(peer);
    }

    let source = {
        let guard = state.runtime.read().await;
        let rt = guard.as_ref().ok_or_else(|| {
            ApiError::not_connected("no active MTProto connection to resolve the dialog")
        })?;
        RuntimeSource {
            client: rt.client.clone(),
            session: Arc::clone(&rt.session),
        }
    };
    resolve_peer_with(state, dialog_id, &source).await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::TelegramConfig;
    use grammers_session::types::PeerAuth;

    fn state() -> Arc<TelegramState> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            ..TelegramConfig::default()
        };
        // TempDir нарочно «протекает»: конфиг только читается по путям, а
        // держать его до конца теста незачем — файлы не создаются.
        std::mem::forget(dir);
        TelegramState::new(cfg)
    }

    fn peer(raw: i64) -> PeerRef {
        PeerRef {
            id: PeerId::from_bot_api_dialog_id(raw).expect("валидный id"),
            auth: PeerAuth::from_hash(raw.wrapping_mul(31)),
        }
    }

    /// Считающий источник: знает про заранее заданный peer и фиксирует, сколько
    /// раз к нему обращались.
    struct CountingSource {
        known: Option<PeerRef>,
        dialog_peer: Option<PeerRef>,
        session_hits: AtomicUsize,
        dialog_hits: AtomicUsize,
    }

    impl CountingSource {
        fn with_session(p: PeerRef) -> Self {
            Self {
                known: Some(p),
                dialog_peer: None,
                session_hits: AtomicUsize::new(0),
                dialog_hits: AtomicUsize::new(0),
            }
        }
        fn with_dialogs(p: PeerRef) -> Self {
            Self {
                known: None,
                dialog_peer: Some(p),
                session_hits: AtomicUsize::new(0),
                dialog_hits: AtomicUsize::new(0),
            }
        }
        fn empty() -> Self {
            Self {
                known: None,
                dialog_peer: None,
                session_hits: AtomicUsize::new(0),
                dialog_hits: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl PeerSource for CountingSource {
        async fn lookup_session(&self, id: PeerId) -> Option<PeerRef> {
            self.session_hits.fetch_add(1, Ordering::SeqCst);
            self.known.filter(|p| p.id == id)
        }
        async fn scan_dialogs(&self, state: &Arc<TelegramState>, id: PeerId) -> Option<PeerRef> {
            self.dialog_hits.fetch_add(1, Ordering::SeqCst);
            let found = self.dialog_peer.filter(|p| p.id == id);
            if let Some(p) = found {
                cache_peer(state, p).await;
            }
            found
        }
    }

    #[tokio::test]
    async fn invalid_dialog_id_is_404_without_touching_the_source() {
        let st = state();
        let src = CountingSource::empty();
        for raw in ["", "abc", "0", "12.5", "-1001234567890123456"] {
            let err = resolve_peer_with(&st, raw, &src).await.unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND, "{raw:?}");
            assert_eq!(err.body.code, "unknown_dialog", "{raw:?}");
        }
        assert_eq!(
            src.session_hits.load(Ordering::SeqCst),
            0,
            "мусорный id не должен доходить до сессии"
        );
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 0);
    }

    /// Главный инвариант кэша: второй резолв того же диалога НЕ обращается
    /// ни к сессии, ни к сети.
    #[tokio::test]
    async fn cache_hit_skips_session_and_network() {
        let st = state();
        let p = peer(-1_001_234_567_890);
        let src = CountingSource::with_session(p);

        let first = resolve_peer_with(&st, "-1001234567890", &src).await.unwrap();
        assert_eq!(first.id, p.id);
        assert_eq!(first.auth, p.auth, "access_hash обязан сохраняться");
        assert_eq!(src.session_hits.load(Ordering::SeqCst), 1);

        for _ in 0..5 {
            let again = resolve_peer_with(&st, "-1001234567890", &src).await.unwrap();
            assert_eq!(again.id, p.id);
        }
        assert_eq!(
            src.session_hits.load(Ordering::SeqCst),
            1,
            "повторные резолвы обязаны обслуживаться кэшем"
        );
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 0);
        assert_eq!(cache_len(&st).await, 1);
    }

    #[tokio::test]
    async fn falls_back_to_dialog_scan_and_then_caches() {
        let st = state();
        let p = peer(777_000);
        let src = CountingSource::with_dialogs(p);

        let got = resolve_peer_with(&st, "777000", &src).await.unwrap();
        assert_eq!(got.id, p.id);
        assert_eq!(src.session_hits.load(Ordering::SeqCst), 1);
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 1);

        // Второй раз — уже из кэша, дорогой проход не повторяется.
        let _ = resolve_peer_with(&st, "777000", &src).await.unwrap();
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_peer_is_404_after_all_echelons() {
        let st = state();
        let src = CountingSource::empty();
        let err = resolve_peer_with(&st, "777000", &src).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(err.body.code, "unknown_dialog");
        assert!(err.body.message.contains("777000"), "{}", err.body.message);
        assert_eq!(src.session_hits.load(Ordering::SeqCst), 1);
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 1);
        assert_eq!(cache_len(&st).await, 0, "неудачный резолв не мусорит в кэше");
    }

    /// Без подключения резолв неизвестного диалога — `503 not_connected`
    /// (отличается от `404`: диалог, возможно, существует, просто спросить
    /// не у кого). Но уже закэшированный peer отдаётся и без соединения.
    #[tokio::test]
    async fn resolve_peer_without_runtime_reports_not_connected_but_serves_cache() {
        let st = state();
        let err = resolve_peer(&st, "777000").await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");

        // Мусорный id и без соединения остаётся 404 — так фронт не получает
        // ложную надежду «переподключись и повтори».
        let err = resolve_peer(&st, "nonsense").await.unwrap_err();
        assert_eq!(err.body.code, "unknown_dialog");

        let p = peer(777_000);
        cache_peer(&st, p).await;
        let got = resolve_peer(&st, "777000").await.unwrap();
        assert_eq!(got.auth, p.auth);
    }

    /// Источник-нарушитель контракта: `scan_dialogs` находит peer'а, но НЕ
    /// кладёт его в `state.peers` (в отличие от [`CountingSource`]).
    struct NonCachingSource {
        dialog_peer: PeerRef,
        dialog_hits: AtomicUsize,
    }

    #[async_trait]
    impl PeerSource for NonCachingSource {
        async fn lookup_session(&self, _id: PeerId) -> Option<PeerRef> {
            None
        }
        async fn scan_dialogs(&self, _state: &Arc<TelegramState>, id: PeerId) -> Option<PeerRef> {
            self.dialog_hits.fetch_add(1, Ordering::SeqCst);
            Some(self.dialog_peer).filter(|p| p.id == id)
        }
    }

    /// Гонка: несколько параллельных запросов одного диалога (двойной клик по
    /// чату, пока кэш пуст). Все обязаны завершиться одним и тем же peer'ом
    /// без дедлока, а кэш — сойтись к одной записи.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_resolves_of_the_same_dialog_converge() {
        let st = state();
        let p = peer(-1_001_234_567_890);
        let src = Arc::new(CountingSource::with_session(p));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let st = Arc::clone(&st);
            let src = Arc::clone(&src);
            handles.push(tokio::spawn(async move {
                resolve_peer_with(&st, "-1001234567890", src.as_ref()).await
            }));
        }
        for h in handles {
            let got = h.await.unwrap().unwrap();
            assert_eq!(got.id, p.id);
            assert_eq!(got.auth, p.auth);
        }
        assert_eq!(cache_len(&st).await, 1, "кэш обязан сойтись к одной записи");
        // До заполнения кэша к источнику могли успеть несколько задач — это
        // допустимо; важно, что не больше числа задач и без дорогого эшелона.
        let hits = src.session_hits.load(Ordering::SeqCst);
        assert!((1..=16).contains(&hits), "hits={hits}");
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 0);
    }

    /// Защитная повторная запись в `resolve_peer_with` обязана прикрывать
    /// реализации, которые нашли peer'а проходом по диалогам, но не положили
    /// его в кэш: второй резолв не должен снова идти в «сеть».
    #[tokio::test]
    async fn defensive_recache_covers_non_caching_scan_sources() {
        let st = state();
        let p = peer(777_000);
        let src = NonCachingSource {
            dialog_peer: p,
            dialog_hits: AtomicUsize::new(0),
        };

        let got = resolve_peer_with(&st, "777000", &src).await.unwrap();
        assert_eq!(got.auth, p.auth);
        assert_eq!(src.dialog_hits.load(Ordering::SeqCst), 1);
        assert_eq!(cache_len(&st).await, 1, "resolve обязан кэшировать сам");

        let _ = resolve_peer_with(&st, "777000", &src).await.unwrap();
        assert_eq!(
            src.dialog_hits.load(Ordering::SeqCst),
            1,
            "второй резолв обязан обслуживаться кэшем"
        );
    }

    /// Пробельное оформление id (curl/копипаста) не должно плодить записи:
    /// `" 777000 "` и `"777000"` — один peer и одна запись кэша.
    #[tokio::test]
    async fn padded_and_canonical_ids_share_one_cache_entry() {
        let st = state();
        let p = peer(777_000);
        let src = CountingSource::with_session(p);

        let a = resolve_peer_with(&st, " 777000 ", &src).await.unwrap();
        let b = resolve_peer_with(&st, "777000", &src).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(
            src.session_hits.load(Ordering::SeqCst),
            1,
            "канонический id обязан попадать в ту же запись кэша"
        );
        assert_eq!(cache_len(&st).await, 1);
    }

    /// Крайние валидные id Bot API (максимальный user, низ диапазона каналов,
    /// монофорум) проходят полный путь резолва и кэшируются без искажений.
    #[tokio::test]
    async fn boundary_ids_resolve_and_cache_without_distortion() {
        let st = state();
        for raw in [1_099_511_627_775_i64, -1_997_852_516_352, -2_002_147_483_649] {
            let p = peer(raw);
            let src = CountingSource::with_dialogs(p);
            let got = resolve_peer_with(&st, &raw.to_string(), &src).await.unwrap();
            assert_eq!(got.id.bot_api_dialog_id(), Some(raw), "{raw}");
            assert_eq!(got.auth, p.auth, "{raw}");
        }
        assert_eq!(cache_len(&st).await, 3);
    }

    #[tokio::test]
    async fn cache_peer_overwrites_stale_access_hash() {
        let st = state();
        let id = PeerId::from_bot_api_dialog_id(777_000).unwrap();
        cache_peer(
            &st,
            PeerRef {
                id,
                auth: PeerAuth::from_hash(1),
            },
        )
        .await;
        cache_peer(
            &st,
            PeerRef {
                id,
                auth: PeerAuth::from_hash(2),
            },
        )
        .await;
        assert_eq!(cache_len(&st).await, 1);
        assert_eq!(
            cached_peer(&st, id).await.unwrap().auth,
            PeerAuth::from_hash(2)
        );
    }
}
