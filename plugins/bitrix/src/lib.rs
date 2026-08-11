//! # forge-bitrix
//!
//! Плагин Bitrix24 для F.O.R.G.E. — вкладка мессенджера портала: личные
//! диалоги, групповые чаты и **чаты задач** через ВХОДЯЩИЙ ВЕБХУК
//! пользователя (`https://<портал>/rest/<user_id>/<код>/`).
//!
//! Транспорт — `reqwest` на rustls: openssl в дереве нет, что проверяется
//! гейтом `cargo tree -i openssl-sys`. Единственный артефакт учётки — строка
//! вебхука в `~/.config/forge/bitrix.toml` (права `0600`), поэтому ни
//! сессионного файла, ни долгоживущего рантайма у плагина нет: нечего
//! хранить и нечего держать открытым.
//!
//! ## Два факта платформы, определяющие архитектуру
//!
//! 1. **Чат задачи — обычный IM-чат** с `entity_type="TASKS_TASK"`.
//!    Комментарии задачи и сообщения чата — ОДИН И ТОТ ЖЕ поток. Поэтому
//!    «Задачи» здесь не второй мессенджер, а директория одного списка
//!    диалогов плюс обогащённая шапка над лентой (Р2).
//! 2. **Realtime без публичного URL возможен**: `im.v2.Event.subscribe` +
//!    `im.v2.Event.get{offset}` работают через входящий вебхук. Всё
//!    остальное (`event.bind`, `pull.*`, исходящие вебхуки) вебхуку
//!    недоступно, поэтому событий ждём мы, а не портал стучится к нам.
//!
//! ## Ограничение, которое надо знать заранее (Р15)
//!
//! Вебхук выпускается от имени ОДНОГО сотрудника портала. Мультипортальности
//! и мультипользовательности в v1 нет: в remote-mode все, кто открыл этот
//! devforge, увидят переписку владельца вебхука. Это зафиксировано в тексте
//! экрана подключения ([`routes::WEBHOOK_HINT`]) и в документации `arhit`.
//!
//! ## Lifecycle (`tmux-web/src/main.rs`, opt-in флаг `--bitrix`)
//!
//! ```ignore
//! let state = forge_bitrix::init(forge_bitrix::BitrixConfig::load())?;
//! let app = forge_bitrix::register_routes(app, state.clone());
//! forge_bitrix::spawn_workers(&state).await;
//! // ... при получении сигнала остановки:
//! forge_bitrix::shutdown(&state).await;
//! ```
//!
//! [`register_routes`] ОБЯЗАТЕЛЬНО вызывать ДО `.fallback(...)` и
//! `.layer(auth_middleware)` хоста — тогда Bearer-auth и csrf_guard покрывают
//! `/api/bitrix/*` и `/ws/bitrix` автоматически. Регистрация после
//! `.fallback` дала бы 404 на роуты плагина, после `.layer` — дыру в
//! авторизации.
//!
//! Ошибка [`init`] для хоста НЕ фатальна: он логирует `warn` и стартует без
//! плагина. Сломанный `bitrix.toml` не должен мешать работать с терминалом.
//!
//! ## Host boundary
//!
//! Отдельного крейта `bitrix-host-api` НЕТ и не планируется — ровно по той же
//! причине, что у Telegram и VK: плагину от хоста не нужно ничего, кроме
//! `Router` (ни tmux, ни сессий, ни токена — auth решается порядком
//! регистрации). Пустой слой ради симметрии разделял бы нечего.
//!
//! ## Транспорт (Ф2)
//!
//! Весь выход в сеть идёт через [`api::B24Client`] — один экземпляр на секрет,
//! то есть один троттлер. Клиент соблюдает ОБА лимита портала (интенсивность и
//! `operating`-время метода), делает не более одного повтора и никогда не
//! повторяет отправку (Р9). Подробности — в шапках [`api`], [`api::throttle`],
//! [`api::error`] и [`api::batch`].
//!
//! ## Что появится в следующих фазах
//!
//! Автомат подключения (`login.rs`, Ф3), DTO и список диалогов (Ф4), лента
//! (Ф5), отправка (Ф6), realtime (`events.rs`, `recent_sync.rs`, Ф7), медиа
//! (Ф8), загрузка файлов (Ф9), задачи (`tasks.rs`, Ф10). Все они подключаются
//! через уже существующие четыре точки входа — сигнатуры ниже меняться не
//! будут.

pub mod actions;
pub mod analysis;
pub mod api;
pub mod attach;
pub mod bbcode;
pub mod config;
pub mod dto;
pub mod events;
pub mod login;
pub mod media_cache;
pub mod peers;
pub mod progress;
pub mod recent_sync;
pub mod routes;
pub mod state;
pub mod tasks;
pub mod ws;

/// Захват `tracing` для юнит-тестов: инвариант «вебхук не в логе» проверяется
/// чтением реальных строк, а не верой в дисциплину.
#[cfg(test)]
pub(crate) mod testlog;

use std::sync::Arc;

use axum::Router;

pub use api::{B24Client, B24Error};
pub use config::{BitrixConfig, WebhookUrl};
pub use login::{
    parse_webhook_input, verify_webhook, B24UserDto, LoginBackend, LoginError, LoginFlow,
    LoginStateDto,
};
pub use routes::StatusDto;
pub use state::{B24State, ConnHealth};
pub use tasks::{ActionMatrix, TaskDto, TaskIndex};
pub use ws::protocol::{B24ClientMsg, B24ServerEvent, B24ServerMsg};

/// Создаёт состояние плагина из конфигурации.
///
/// **Синхронная и сетевонезависимая.** Начальное состояние подключения
/// выводится исключительно из наличия вебхука (см.
/// [`LoginFlow::from_config`]); проверка вебхука (`profile` → `scope`) —
/// работа [`spawn_workers`], она делается фоново и не задерживает биндинг
/// порта хостом.
///
/// Инвариант закреплён тестом `init_never_touches_the_network`: конфиг с
/// `base_url_override` на заведомо закрытый порт 1 обязан отработать быстрее
/// 500 мс. Если кто-то добавит сюда «быстренько спросим профиль, чтобы сразу
/// показать имя» — тест упадёт.
///
/// # Errors
///
/// Возвращает ошибку, если конфиг невалиден (см. [`BitrixConfig::validate`]).
/// Хост трактует это как «стартовать без плагина», а не как фатальный сбой.
pub fn init(cfg: BitrixConfig) -> anyhow::Result<Arc<B24State>> {
    cfg.validate()?;
    if let Err(e) = cfg.ensure_dirs() {
        // Не фатально: каталог понадобится только при записи кэша, и там
        // ошибка всплывёт с более точным контекстом.
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not prepare the cache directory"
        );
    }
    let state = B24State::new(cfg);
    tracing::info!(
        target: "forge_bitrix",
        login_state = if state.config.has_webhook() { "disconnected" } else { "not_configured" },
        rate_limit_rps = state.config.rate_limit_rps,
        poll_active_secs = state.config.poll_active_secs,
        poll_idle_secs = state.config.poll_idle_secs,
        recent_sync_secs = state.config.recent_sync_secs,
        cache_dir = %state.config.cache_dir.display(),
        webhook = %state.config.webhook.fingerprint(),
        "forge-bitrix: state initialized"
    );
    Ok(state)
}

/// Мерджит роуты плагина в приложение хоста.
///
/// Host-параметра нет намеренно (см. «Host boundary» в шапке модуля).
pub fn register_routes(app: Router, state: Arc<B24State>) -> Router {
    let plugin = routes::build_router(state);
    tracing::info!(
        target: "forge_bitrix",
        "forge-bitrix: registered routes (/api/bitrix/status, /ws/bitrix)"
    );
    app.merge(plugin)
}

/// Запускает фоновые воркеры плагина.
///
/// Состав по фазам:
///
/// | воркер | когда запускается | фаза |
/// |---|---|---|
/// | поток событий `im.v2.Event.*` | ВСЕГДА, даже без вебхука | Ф7 |
/// | фолбэк-синхронизация списка и счётчиков | всегда | Ф7 |
/// | LRU-подчистка медиа-кэша | всегда | Ф8 |
/// | GC незавершённых загрузок (+ уборка сирот на старте) | всегда | Ф9 |
/// | стартовая проверка вебхука | только если вебхук есть | Ф3 |
///
/// Воркеры именно ЗАПУСКАЮТСЯ, а не выполняются: функция возвращается
/// немедленно, поэтому медленная или отсутствующая сеть не задерживает
/// биндинг порта хостом.
///
/// # Почему оба realtime-воркера стартуют ВСЕГДА
///
/// Они сами ждут подключения на [`B24State::authorized_notify`] и не делают до
/// него ни одного сетевого вызова. Ровно это и делает возможным подключение
/// вебхука в работающем процессе: пользователь вставляет URL в панель, автомат
/// логина переходит в `authorized`, `set_login` будит обоих — и realtime
/// поднимается БЕЗ рестарта devforge.
///
/// Обратная схема («стартовать воркеров после успешного логина») выглядит
/// экономнее, но требует помнить о запуске в каждой точке, где появляется
/// подключение (роут, стартовая проверка, повтор после сети), и первая же
/// забытая точка даёт панель без realtime, которая чинится только рестартом.
///
/// # Стартовая проверка вебхука (Ф3)
///
/// Запускается только если вебхук в конфиге есть. Без неё после перезапуска
/// процесса панель показывала бы `disconnected` до тех пор, пока пользователь
/// не нажмёт «подключить», хотя вебхук лежит в файле и совершенно исправен.
///
/// Проверка идёт ФОНОМ и ровно один раз: повторов нет намеренно — упавшая сеть
/// не должна превращаться в бесконечный опрос портала, а кнопка «повторить» на
/// экране подключения решает ту же задачу по требованию пользователя.
pub async fn spawn_workers(state: &Arc<B24State>) {
    let configured = state.has_webhook().await;
    if configured {
        let st = state.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                // Остановка сервера не должна ждать connect-таймаута портала.
                _ = st.shutdown.cancelled() => {}
                flow = login::verify_saved_webhook(&st) => {
                    tracing::info!(
                        target: "forge_bitrix",
                        state = flow.kind(),
                        "forge-bitrix: startup webhook check finished"
                    );
                }
            }
        });
        state.register_worker(handle).await;
    }
    events::spawn(state).await;
    recent_sync::spawn(state).await;
    // Подчистка кэша НЕ привязана к подключению: скачанные медиа переживают
    // logout, и без своего воркера подметать их было бы некому.
    media_cache::spawn_sweeper(state).await;
    // GC незавершённых загрузок (Ф9). Первый прогон — сразу на старте: реестр
    // живёт в памяти, поэтому после рестарта каждый `.part` в
    // `<cache_dir>/uploads/` — сирота, на которую уже никто не сошлётся, а
    // место она занимает вполне реальное.
    routes::upload::spawn_uploads_gc(state).await;
    tracing::info!(
        target: "forge_bitrix",
        configured,
        workers = usize::from(configured) + 4,
        "forge-bitrix: spawn_workers"
    );
}

/// Graceful shutdown плагина.
///
/// 1. `shutdown.cancel()` — будит WS-циклы и воркеров;
/// 2. `shutdown_workers()` — даёт каждому две секунды выйти самому и только
///    потом абортит. Пауза не косметическая: воркер событий на выходе
///    персистит курсор, а `abort()` в произвольной await-точке этого сделать
///    не даст.
/// 3. `analysis.shutdown()` — убивает процессы `claude`, запущенные анализом
///    чата, и закрывает хаб для новых стартов. Без него дочерний процесс
///    пережил бы devforge и продолжал бы жечь токены осиротевшим.
///
/// **Идемпотентна**: `CancellationToken` можно отменять сколько угодно раз, а
/// список воркеров после первого прохода пуст. Это не теория — хост зовёт
/// shutdown из обработчика сигнала, а тот срабатывает и на Ctrl-C, и на
/// SIGTERM от systemd.
pub async fn shutdown(state: &Arc<B24State>) {
    tracing::info!(target: "forge_bitrix", "forge-bitrix: shutdown initiated");
    state.shutdown.cancel();
    state.shutdown_workers().await;
    state.analysis.shutdown().await;
    tracing::info!(target: "forge_bitrix", "forge-bitrix: shutdown complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> (tempfile::TempDir, BitrixConfig) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        (dir, cfg)
    }

    #[tokio::test]
    async fn init_creates_state_without_a_webhook() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(!state.is_authorized().await);
        assert!(state.config.cache_dir.is_dir(), "cache_dir создан");
    }

    #[tokio::test]
    async fn init_derives_disconnected_with_a_webhook() {
        let (_dir, mut cfg) = test_cfg();
        cfg.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/");
        let state = init(cfg).unwrap();
        assert_eq!(state.login_state_dto().await.state, "disconnected");
        assert!(state.has_webhook().await);
    }

    #[test]
    fn init_rejects_invalid_config() {
        let (_dir, mut cfg) = test_cfg();
        cfg.rate_limit_rps = 0;
        assert!(init(cfg).is_err());

        let (_dir2, mut cfg) = test_cfg();
        cfg.max_upload_bytes = 0;
        assert!(init(cfg).is_err());
    }

    /// `init` обязан быть быстрым и НЕ ходить в сеть: проверка вебхука —
    /// работа `spawn_workers`. Тест сломается, если кто-то добавит в `init`
    /// вызов `profile` «чтобы сразу показать пользователя».
    #[tokio::test]
    async fn init_never_touches_the_network() {
        let (_dir, mut cfg) = test_cfg();
        cfg.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/");
        // Заведомо недостижимый адрес: если бы init ходил в сеть, тест
        // споткнулся бы о connect-таймаут.
        cfg.base_url_override = "https://127.0.0.1:1/rest/1/abc123".into();

        let started = std::time::Instant::now();
        let state = init(cfg).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "init занял {:?} — он не должен ждать сеть",
            started.elapsed()
        );
        assert_eq!(state.login_state_dto().await.state, "disconnected");
    }

    /// Каталог кэша может быть несоздаваем (родитель — обычный файл, нет
    /// прав): `init` обязан выжить — ошибка всплывёт позже, при реальной
    /// записи кэша, с более точным контекстом.
    #[tokio::test]
    async fn init_survives_an_uncreatable_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file, not a dir").unwrap();

        let cfg = BitrixConfig {
            cache_dir: blocker.join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: blocker.join("prompts.json"),
            claude_cli_path: blocker.join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        let state = init(cfg).expect("ошибка каталога не фатальна для init");
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(blocker.is_file(), "чужой файл не перезаписан каталогом");
    }

    /// `spawn_workers` обязан возвращаться немедленно: хост зовёт его ДО
    /// биндинга порта.
    ///
    /// Стартовая проверка вебхука (Ф3) идёт в отдельной задаче и упирается в
    /// заведомо закрытый порт 1 — если бы её выполняли прямо здесь, тест
    /// споткнулся бы о connect-таймаут.
    #[tokio::test]
    async fn spawn_workers_returns_immediately() {
        let (_dir, mut cfg) = test_cfg();
        cfg.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/");
        cfg.base_url_override = "https://127.0.0.1:1/rest/1/abc123".into();
        let state = init(cfg).unwrap();

        let started = std::time::Instant::now();
        spawn_workers(&state).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "spawn_workers обязан возвращаться сразу, занял {:?}",
            started.elapsed()
        );
        assert_eq!(
            state.workers.lock().await.len(),
            5,
            "стартовая проверка + поток событий + фолбэк-синхронизация + подчистка кэша + GC загрузок"
        );
        shutdown(&state).await;
    }

    /// КРИТЕРИЙ Ф7: realtime-воркеры стартуют ДАЖЕ БЕЗ ВЕБХУКА — и при этом не
    /// трогают сеть, потому что ждут подключения на `Notify`.
    ///
    /// Именно это делает возможной вставку вебхука в работающем процессе:
    /// поднимать воркеров после логина было бы экономнее, но потребовало бы
    /// помнить о запуске в каждой точке, где появляется подключение.
    #[tokio::test]
    async fn the_realtime_workers_start_even_without_a_webhook() {
        let (_dir, mut cfg) = test_cfg();
        // Заведомо закрытый порт: сходи воркер в сеть — тест споткнулся бы о
        // connect-таймаут вместо того, чтобы завершиться мгновенно.
        cfg.base_url_override = "https://127.0.0.1:1/rest/1/abc123".into();
        let state = init(cfg).unwrap();

        let started = std::time::Instant::now();
        spawn_workers(&state).await;
        assert_eq!(
            state.workers.lock().await.len(),
            4,
            "без вебхука стартовой проверки нет, а оба realtime-воркера, подчистка кэша и GC загрузок есть"
        );
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(started.elapsed() < std::time::Duration::from_millis(500));

        shutdown(&state).await;
        assert!(state.workers.lock().await.is_empty(), "оба воркера вышли");
    }

    /// Идемпотентность shutdown — не теория: хост зовёт его из обработчика
    /// сигнала, а тот может сработать дважды (Ctrl-C + SIGTERM от systemd).
    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();

        // Воркер, который выходит по сигналу: проверяем, что грейс работает,
        // а не что список пуст изначально.
        let token = state.shutdown.clone();
        state
            .register_worker(tokio::spawn(async move { token.cancelled().await }))
            .await;

        spawn_workers(&state).await;
        assert!(!state.shutdown.is_cancelled());

        shutdown(&state).await;
        assert!(state.shutdown.is_cancelled());
        assert!(state.workers.lock().await.is_empty());

        // Повторный вызов не паникует и не виснет.
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown(&state))
            .await
            .expect("повторный shutdown завис");

        // И третий — тоже.
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown(&state))
            .await
            .expect("третий shutdown завис");
    }

    /// Merge не должен ни затирать роуты хоста, ни терять свои.
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
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/bitrix/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
