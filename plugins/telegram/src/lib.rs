//! # forge-telegram
//!
//! Плагин Telegram для F.O.R.G.E. — вкладка мессенджера, работающая с
//! **личным аккаунтом** пользователя (MTProto, не bot-API).
//!
//! Логин и транспорт — на `grammers-client 0.10` (MTProto поверх raw TCP,
//! чистое Rust-крипто: openssl в дереве нет, что проверяется гейтом
//! `cargo tree -i openssl-sys`). Сессия аккаунта хранится плагином
//! самостоятельно ([`session::FileSession`]) — `sqlite-storage` из
//! grammers несовместим с rusqlite плагина Echo, подробности в шапке
//! `src/session.rs`.
//!
//! Логика состояний намеренно отделена от сети: конфиг, автомат логина,
//! WS-протокол и DTO тестируются без единого сетевого вызова (см. trait
//! [`login::LoginBackend`]).
//!
//! ## Lifecycle (`tmux-web/src/main.rs`, opt-in флаг `--telegram`)
//!
//! ```ignore
//! let tg_state = forge_telegram::init(forge_telegram::TelegramConfig::load())?;
//! let app = forge_telegram::register_routes(app, tg_state.clone());
//! forge_telegram::spawn_workers(&tg_state).await;
//! // ... при получении сигнала остановки:
//! forge_telegram::shutdown(&tg_state).await;
//! ```
//!
//! [`register_routes`] ОБЯЗАТЕЛЬНО вызывать ДО `.fallback(...)` и
//! `.layer(auth_middleware)` хоста — тогда Bearer-auth и csrf_guard
//! покрывают `/api/telegram/*` и `/ws/telegram` автоматически.
//!
//! ## Host boundary
//!
//! Отдельного crate `telegram-host-api` НЕТ и не планируется: плагину от
//! хоста ничего не нужно (ни tmux, ни сессии, ни токен — auth решается
//! порядком регистрации). Поэтому [`register_routes`] принимает только
//! `Router` и собственное состояние, а зависимости на `AppState` нет
//! вообще.

pub mod analysis;
pub mod config;
pub mod dto;
pub mod login;
pub mod media_cache;
pub mod peers;
pub mod progress;
pub mod routes;
pub mod runtime;
pub mod session;
pub mod state;
pub mod voice;
pub mod voice_auto;
pub mod updates;
pub mod ws;

use std::sync::Arc;

use axum::Router;

pub use config::TelegramConfig;
pub use login::{LoginFlow, LoginStateDto, TgUserDto};
pub use runtime::TgRuntime;
pub use state::TelegramState;
pub use ws::protocol::{TgClientMsg, TgServerEvent, TgServerMsg};

/// Создаёт состояние плагина из конфигурации.
///
/// Синхронная и сетевонезависимая: начальное состояние логина выводится
/// исключительно из наличия `api_id`/`api_hash` (см.
/// [`LoginFlow::from_config`]). Восстановление сохранённой сессии — шаг
/// логина, он делается фоново после старта и не блокирует поднятие
/// сервера.
///
/// # Errors
///
/// Возвращает ошибку, если конфиг невалиден (см.
/// [`TelegramConfig::validate`]). Хост трактует это как «стартовать без
/// плагина», а не как фатальный сбой.
pub fn init(cfg: TelegramConfig) -> anyhow::Result<Arc<TelegramState>> {
    cfg.validate()?;
    if let Err(e) = cfg.ensure_dirs() {
        // Не фатально: каталоги понадобятся только при записи сессии/кэша,
        // и там ошибка всплывёт с более точным контекстом.
        tracing::warn!(target: "forge_telegram", error = %e, "forge-telegram: could not prepare directories");
    }
    let state = TelegramState::new(cfg);
    tracing::info!(
        target: "forge_telegram",
        login_state = if state.config.has_credentials() { "disconnected" } else { "not_configured" },
        session_path = %state.config.session_path.display(),
        session_exists = state.config.session_path.exists(),
        cache_dir = %state.config.cache_dir.display(),
        "forge-telegram: state initialized"
    );
    Ok(state)
}

/// Мерджит роуты плагина в приложение хоста.
///
/// Host-параметра нет намеренно (см. «Host boundary» в шапке модуля).
pub fn register_routes(app: Router, state: Arc<TelegramState>) -> Router {
    let plugin = routes::build_router(state);
    tracing::info!(
        target: "forge_telegram",
        "forge-telegram: registered routes (/api/telegram/status, /api/telegram/login/*, /api/telegram/logout, /api/telegram/dialogs*, /api/telegram/media/*, /api/telegram/upload, /ws/telegram)"
    );
    app.merge(plugin)
}

/// Запускает фоновые воркеры плагина.
///
/// Их четыре:
///
/// 1. **updates-supervisor** ([`updates::spawn`]) — запускается ВСЕГДА, даже
///    без кредов и сессии: он сам ждёт перехода в `Authorized`, поэтому вход
///    через wizard в уже работающем процессе поднимает realtime без
///    рестарта devforge.
/// 2. **LRU-подчистка медиа-кэша** ([`media_cache::spawn_sweeper`]) — тоже
///    всегда: кэш на диске мог остаться с прошлого запуска и уже
///    превышать лимит, поэтому первый прогон делается на старте, не
///    дожидаясь ни авторизации, ни первого запроса медиа.
/// 3. **GC незавершённых загрузок** ([`routes::upload::spawn_uploads_gc`]) —
///    первым же делом сносит `.part`-сирот от прошлого процесса (реестр
///    загрузок живёт в памяти, поэтому после рестарта на них уже никто не
///    ссылается), дальше раз в 5 минут выбрасывает записи старше TTL.
/// 4. **восстановление сохранённой сессии** ([`restore_session`]) — только
///    если есть и креды, и файл сессии (иначе восстанавливать нечего).
///
/// Все ЗАПУСКАЮТСЯ, а не выполняются: `spawn_workers` возвращается
/// немедленно, поэтому медленная или отсутствующая сеть не задерживает
/// биндинг порта хостом.
pub async fn spawn_workers(state: &Arc<TelegramState>) {
    updates::spawn(state).await;
    media_cache::spawn_sweeper(state).await;
    routes::upload::spawn_uploads_gc(state).await;

    if !state.has_credentials().await {
        tracing::info!(
            target: "forge_telegram",
            "forge-telegram: spawn_workers — no credentials yet, waiting for the login wizard"
        );
        return;
    }
    if !state.config.session_path.exists() {
        tracing::info!(
            target: "forge_telegram",
            session_path = %state.config.session_path.display(),
            "forge-telegram: spawn_workers — no saved session, waiting for the login wizard"
        );
        return;
    }

    let st = state.clone();
    let handle = tokio::spawn(async move { restore_session(&st).await });
    state.register_worker(handle).await;
    tracing::info!(
        target: "forge_telegram",
        "forge-telegram: spawn_workers — session restore scheduled in background"
    );
}

/// Одна попытка поднять сохранённую сессию с диска.
///
/// ```text
///   build_runtime ──err──▶ Disconnected{error}
///        │
///   is_authorized_with_timeout(10с)
///        ├── Ok(true)  ──▶ get_me ──▶ Authorized{user}  (+ authorized_notify)
///        ├── Ok(false) ──▶ Disconnected{error: None}    (сессию отозвали)
///        └── Err/таймаут ─▶ Disconnected{error: текст}
/// ```
///
/// Сервер стартует в любом случае: ни одна ветка не возвращает ошибку
/// наружу, всё уходит в состояние логина и в лог. Повторные попытки —
/// задача updates-воркера (Фаза 3); здесь ровно одна, стартовая.
///
/// Рантайм при неудачной проверке НЕ выбрасывается: пользователь может
/// нажать «войти», и `ensure_runtime` переиспользует живое соединение.
pub async fn restore_session(state: &Arc<TelegramState>) {
    let cfg = state.effective_config().await;
    let rt = match runtime::build_runtime(&cfg).await {
        Ok(rt) => rt,
        Err(e) => {
            // Типичная причина — занятый lock сессии (второй процесс
            // devforge с тем же session_path). Сервер продолжает работать,
            // а UI видит внятную причину.
            login::on_auth_invalidated(state, e.to_string()).await;
            return;
        }
    };
    let client = rt.client.clone();
    state.set_runtime(rt).await;

    let timeout = std::time::Duration::from_secs(runtime::AUTH_CHECK_TIMEOUT_SECS);
    match runtime::is_authorized_with_timeout(&client, timeout).await {
        Ok(true) => match client.get_me().await {
            Ok(user) => {
                let user = login::TgUserDto::from_user(&user);
                tracing::info!(
                    target: "forge_telegram",
                    user_id = %user.id,
                    "forge-telegram: restored Telegram session from disk"
                );
                state.set_login(LoginFlow::Authorized { user }).await;
            }
            Err(e) => {
                // Авторизация есть, но профиль не забрали — честнее
                // показать «нет связи», чем «вошли неизвестно кем».
                login::on_auth_invalidated(
                    state,
                    format!("signed in, but could not fetch the account profile: {e}"),
                )
                .await;
            }
        },
        Ok(false) => {
            tracing::info!(
                target: "forge_telegram",
                "forge-telegram: saved session is not authorized (revoked or never completed)"
            );
            state.set_login(LoginFlow::Disconnected { error: None }).await;
        }
        Err(e) => login::on_auth_invalidated(state, e.to_string()).await,
    }
}

/// Graceful shutdown плагина.
///
/// 1. `shutdown.cancel()` — будит WS-циклы и воркеров;
/// 2. `shutdown_workers()` — abort'ит зарегистрированные задачи;
/// 3. `analysis.shutdown()` — убивает процессы `claude`, запущенные анализом
///    чата, и закрывает хаб для новых стартов;
/// 4. `clear_runtime()` — гасит MTProto-раннер и освобождает lock файла
///    сессии, иначе перезапущенный devforge не смог бы её открыть.
///
/// Шаг 3 обязателен и НЕ покрывается `shutdown_workers`: пампер run'а — это
/// таска ХАБА, а не зарегистрированный воркер плагина, а сам `claude` вообще
/// чужой процесс. Без него остановленный devforge оставлял бы за собой живые
/// `claude -p`, которые продолжали бы жечь токены и писать в файл заметок уже
/// после выгрузки плагина.
///
/// Идемпотентна: `CancellationToken` можно отменять сколько угодно раз,
/// список воркеров после первого прохода пуст, хаб после первого прохода
/// закрыт, а рантайма может не быть.
pub async fn shutdown(state: &Arc<TelegramState>) {
    tracing::info!(target: "forge_telegram", "forge-telegram: shutdown initiated");
    state.shutdown.cancel();
    state.shutdown_workers().await;
    state.analysis.shutdown().await;
    state.clear_runtime().await;
    tracing::info!(target: "forge_telegram", "forge-telegram: shutdown complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> (tempfile::TempDir, TelegramConfig) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        (dir, cfg)
    }

    #[tokio::test]
    async fn init_creates_state_without_credentials() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(!state.is_authorized().await);
    }

    #[tokio::test]
    async fn init_derives_disconnected_with_credentials() {
        let (_dir, mut cfg) = test_cfg();
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        let state = init(cfg).unwrap();
        assert_eq!(state.login_state_dto().await.state, "disconnected");
    }

    #[test]
    fn init_rejects_invalid_config() {
        let (_dir, mut cfg) = test_cfg();
        cfg.update_queue_limit = 0;
        assert!(init(cfg).is_err());
    }

    /// `init` обязан быть быстрым и НЕ ходить в сеть: восстановление
    /// сессии — работа `spawn_workers`.
    #[tokio::test]
    async fn init_never_touches_the_network() {
        let (dir, mut cfg) = test_cfg();
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        std::fs::write(dir.path().join("telegram.session"), b"SQLite format 3\0").unwrap();

        let started = std::time::Instant::now();
        let state = init(cfg).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "init занял {:?} — он не должен ждать сеть",
            started.elapsed()
        );
        assert_eq!(state.login_state_dto().await.state, "disconnected");
        assert!(!state.has_runtime().await, "рантайм поднимает spawn_workers");
    }

    /// Без кредов или без файла сессии ВОССТАНАВЛИВАТЬ нечего: запускаются
    /// ровно три воркера — updates-supervisor (ждёт входа через wizard),
    /// подчистка медиа-кэша и GC загрузок (обоим авторизация не нужна вовсе).
    /// Состояние логина при этом не меняется.
    #[tokio::test]
    async fn spawn_workers_starts_only_the_supervisor_when_nothing_to_restore() {
        // Нет кредов.
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        spawn_workers(&state).await;
        assert_eq!(
            state.workers.lock().await.len(),
            3,
            "supervisor + sweeper + uploads GC: восстанавливать нечего"
        );
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        shutdown(&state).await;

        // Креды есть, файла сессии нет.
        let (_dir2, mut cfg) = test_cfg();
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        let state = init(cfg).unwrap();
        spawn_workers(&state).await;
        assert_eq!(state.workers.lock().await.len(), 3);
        assert_eq!(state.login_state_dto().await.state, "disconnected");
        shutdown(&state).await;
    }

    /// Sweeper — полноценный участник graceful shutdown: он обязан выйти по
    /// токену сам, не дожидаясь `abort()`, иначе `shutdown_workers` тратил бы
    /// на него `WORKER_STOP_GRACE` при каждом завершении процесса.
    #[tokio::test]
    async fn media_sweeper_stops_on_shutdown() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        media_cache::spawn_sweeper(&state).await;
        assert_eq!(state.workers.lock().await.len(), 1);

        state.shutdown.cancel();
        let handle = state.workers.lock().await.pop().unwrap();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(
            stopped.is_ok(),
            "sweeper не вышел по shutdown-токену за 2с"
        );
    }

    /// Занятый lock сессии (второй процесс devforge) — не фатальная
    /// ошибка: состояние уходит в `Disconnected{error}` с внятной
    /// причиной, сервер продолжает работать.
    #[tokio::test]
    async fn restore_session_reports_busy_session_lock() {
        let (dir, mut cfg) = test_cfg();
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        std::fs::write(dir.path().join("telegram.session"), b"SQLite format 3\0").unwrap();
        let state = init(cfg.clone()).unwrap();

        // Имитируем «первый процесс», уже держащий сессию.
        let _held = runtime::SessionLock::acquire(&cfg.session_path).unwrap();

        restore_session(&state).await;

        let dto = state.login_state_dto().await;
        assert_eq!(dto.state, "disconnected");
        assert!(
            dto.error
                .as_deref()
                .unwrap_or_default()
                .contains("уже используется другим процессом"),
            "ожидалось объяснение про занятую сессию: {dto:?}"
        );
        assert!(!state.has_runtime().await);
    }

    /// Полный проход восстановления без валидной авторизации: сеть может
    /// быть или не быть, но результат всегда `disconnected`, а сервер жив.
    #[tokio::test]
    async fn restore_session_ends_disconnected_without_valid_auth() {
        let (dir, mut cfg) = test_cfg();
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        std::fs::write(dir.path().join("telegram.session"), b"SQLite format 3\0").unwrap();
        let state = init(cfg).unwrap();

        restore_session(&state).await;

        assert_eq!(state.login_state_dto().await.state, "disconnected");
        assert!(!state.is_authorized().await);
        shutdown(&state).await;
        // Lock освобождён вместе с рантаймом — сессию можно взять снова.
        let _again = runtime::SessionLock::acquire(&state.config.session_path).unwrap();
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();

        let token = state.shutdown.clone();
        state
            .register_worker(tokio::spawn(async move { token.cancelled().await }))
            .await;

        spawn_workers(&state).await;
        assert!(!state.shutdown.is_cancelled());

        shutdown(&state).await;
        assert!(state.shutdown.is_cancelled());
        assert!(state.workers.lock().await.is_empty());

        // Повторный вызов не паникует.
        shutdown(&state).await;
    }

    /// Пути из конфига могут быть несоздаваемы (родитель — обычный файл,
    /// нет прав): `init` обязан выжить — ошибка всплывёт позже, при
    /// реальной записи сессии/кэша, с более точным контекстом.
    #[tokio::test]
    async fn init_survives_uncreatable_directories() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file, not a dir").unwrap();

        let cfg = TelegramConfig {
            // parent обоих путей — обычный файл ⇒ create_dir_all падает.
            session_path: blocker.join("telegram.session"),
            cache_dir: blocker.join("cache"),
            prompts_path: blocker.join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        let state = init(cfg).expect("ошибка каталогов не фатальна для init");
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(blocker.is_file(), "чужой файл не перезаписан каталогом");
    }

    /// Креды + файл сессии ⇒ spawn_workers регистрирует ЧЕТВЁРТОГО воркера
    /// (restore_session). Лок сессии заранее занят «другим процессом»,
    /// поэтому restore детерминированно и БЕЗ сети завершается
    /// `disconnected{error}` — тест закрывает ветку, которую
    /// spawn_workers_starts_only_the_supervisor... не достигает.
    #[tokio::test]
    async fn spawn_workers_schedules_restore_when_creds_and_session_exist() {
        let (dir, mut cfg) = test_cfg();
        cfg.api_id = Some(42);
        cfg.api_hash = Some("hash".into());
        std::fs::write(dir.path().join("telegram.session"), b"SQLite format 3\0").unwrap();
        let state = init(cfg.clone()).unwrap();

        // «Первый процесс» держит сессию: restore упадёт мгновенно, офлайн.
        let _held = runtime::SessionLock::acquire(&cfg.session_path).unwrap();

        spawn_workers(&state).await;
        assert_eq!(
            state.workers.lock().await.len(),
            4,
            "supervisor + sweeper + uploads GC + restore"
        );

        // Ждём исхода фонового restore (busy-lock ⇒ disconnected с ошибкой).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let dto = state.login_state_dto().await;
            if dto.state == "disconnected" && dto.error.is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "restore не завершился за 5с: {dto:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!state.has_runtime().await, "лок занят — рантайм не поднялся");
        shutdown(&state).await;
    }

    #[tokio::test]
    async fn register_routes_merges_into_host_router() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        // Хост-роут, который обязан пережить merge.
        let host = Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
        let app = register_routes(host, state);

        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

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
    }
}
