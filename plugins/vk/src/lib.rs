//! # forge-vk
//!
//! Плагин ВКонтакте для F.O.R.G.E. — вкладка мессенджера, работающая с
//! **личным аккаунтом** пользователя через пользовательский access-токен
//! (REST `api.vk.com` + LongPoll, не bot-API сообщества).
//!
//! Транспорт — `reqwest` на rustls: openssl в дереве нет, что проверяется
//! гейтом `cargo tree -i openssl-sys`. Единственный артефакт учётки — строка
//! токена в `~/.config/forge/vk.toml` (права `0600`), поэтому ни `session.rs`,
//! ни `runtime.rs` у этого плагина нет: нечего хранить и нечего держать
//! открытым.
//!
//! Логика состояний намеренно отделена от сети: конфиг, автомат логина,
//! WS-протокол и DTO тестируются без единого сетевого вызова и без живого
//! токена (см. `plugins/vk/tests/vk_stub.rs`, Ф2).
//!
//! ## Ограничение платформы, которое надо знать заранее
//!
//! VK не выдаёт право `messages` обычным standalone-приложениям: метод
//! отвечает `error_code=15`. Рабочие токены пользователи добывают сами под
//! app_id сторонних клиентов — мы этот путь не контролируем и не
//! автоматизируем, мы принимаем готовый токен. Поэтому в автомате логина есть
//! ОТДЕЛЬНОЕ состояние [`LoginFlow::NoMessagesScope`] со своим экраном:
//! увидев вместо него «что-то пошло не так», пользователь будет вставлять
//! один и тот же рабочий токен по кругу.
//!
//! ## Lifecycle (`tmux-web/src/main.rs`, opt-in флаг `--vk`)
//!
//! ```ignore
//! let vk_state = forge_vk::init(forge_vk::VkConfig::load())?;
//! let app = forge_vk::register_routes(app, vk_state.clone());
//! forge_vk::spawn_workers(&vk_state).await;
//! // ... при получении сигнала остановки:
//! forge_vk::shutdown(&vk_state).await;
//! ```
//!
//! [`register_routes`] ОБЯЗАТЕЛЬНО вызывать ДО `.fallback(...)` и
//! `.layer(auth_middleware)` хоста — тогда Bearer-auth и csrf_guard покрывают
//! `/api/vk/*` и `/ws/vk` автоматически.
//!
//! ## Host boundary
//!
//! Отдельного crate `vk-host-api` НЕТ и не планируется — ровно по той же
//! причине, что у Telegram: плагину от хоста не нужно ничего, кроме `Router`
//! (ни tmux, ни сессий, ни токена — auth решается порядком регистрации).
//! Пустой слой ради симметрии разделял бы нечего. Поэтому [`register_routes`]
//! принимает только `Router` и собственное состояние, а зависимости на
//! `AppState` нет вообще.
//!
//! ## Транспорт
//!
//! [`api`] — единственная точка выхода в сеть: [`VkClient`] с троттлером
//! 3 rps ([`api::throttle`]), полной таблицей кодов VK ([`api::error`]),
//! политикой ретраев и сырыми структурами ответов ([`api::schema`]).
//! Проверяется локальным стендом `plugins/vk/tests/vk_stub.rs`, который
//! изображает `api.vk.com`, LongPoll- и upload-серверы, — секретов для этого
//! не требуется.
//!
//! ## Что появится в следующих фазах
//!
//! `routes/upload.rs` и отправка файлов (Ф8), клавиатуры сообществ (Ф9),
//! белый список бесед (Ф10). Все они подключаются через уже существующие
//! четыре точки входа — сигнатуры ниже меняться не будут.

pub mod analysis;
pub mod api;
pub mod attach;
pub mod config;
pub mod dto;
pub mod login;
pub mod longpoll;
pub mod media_cache;
pub mod peers;
pub mod progress;
pub mod routes;
pub mod state;
pub mod voice;
pub mod voice_auto;
#[cfg(test)]
pub(crate) mod testlog;
pub mod ws;

use std::sync::Arc;

use axum::Router;

pub use api::{VkClient, VkError};
pub use config::{SecretToken, VkConfig};
pub use login::{LoginFlow, LoginStateDto, VkUserDto};
pub use state::{ConnHealth, VkState};
pub use ws::protocol::{VkClientMsg, VkServerEvent, VkServerMsg};

/// Создаёт состояние плагина из конфигурации.
///
/// Синхронная и сетевонезависимая: начальное состояние логина выводится
/// исключительно из наличия токена (см. [`LoginFlow::from_config`]). Проверка
/// токена (`users.get` + проба `messages.*`) — работа [`spawn_workers`], она
/// делается фоново и не задерживает биндинг порта хостом.
///
/// # Errors
///
/// Возвращает ошибку, если конфиг невалиден (см. [`VkConfig::validate`]).
/// Хост трактует это как «стартовать без плагина», а не как фатальный сбой:
/// VK — опциональная вкладка, и сломанный `vk.toml` не должен мешать
/// пользователю работать с терминалом.
pub fn init(cfg: VkConfig) -> anyhow::Result<Arc<VkState>> {
    cfg.validate()?;
    if let Err(e) = cfg.ensure_dirs() {
        // Не фатально: каталог понадобится только при записи кэша, и там
        // ошибка всплывёт с более точным контекстом.
        tracing::warn!(target: "forge_vk", error = %e, "forge-vk: could not prepare the cache directory");
    }
    let state = VkState::new(cfg);
    tracing::info!(
        target: "forge_vk",
        login_state = if state.config.has_token() { "disconnected" } else { "not_configured" },
        api_version = %state.config.api_version,
        base_url = %state.config.base_url,
        rate_limit_rps = state.config.rate_limit_rps,
        cache_dir = %state.config.cache_dir.display(),
        token = %state.config.access_token.fingerprint(),
        "forge-vk: state initialized"
    );
    Ok(state)
}

/// Мерджит роуты плагина в приложение хоста.
///
/// Host-параметра нет намеренно (см. «Host boundary» в шапке модуля).
pub fn register_routes(app: Router, state: Arc<VkState>) -> Router {
    let plugin = routes::build_router(state);
    tracing::info!(
        target: "forge_vk",
        "forge-vk: registered routes (/api/vk/status, /ws/vk)"
    );
    app.merge(plugin)
}

/// Запускает фоновые воркеры плагина.
///
/// Состав по фазам:
///
/// | воркер | когда запускается | фаза |
/// |---|---|---|
/// | LongPoll-supervisor | ВСЕГДА, даже без токена | Ф5 |
/// | LRU-подчистка медиа-кэша | всегда | Ф7 |
/// | GC незавершённых загрузок | всегда | Ф8 |
/// | **проверка токена** | **только если токен есть** | **Ф3** |
///
/// Воркеры именно ЗАПУСКАЮТСЯ, а не выполняются: `spawn_workers` возвращается
/// немедленно, поэтому медленная или отсутствующая сеть не задерживает
/// биндинг порта хостом. Проверка токена — сетевой вызов, и делать её здесь
/// синхронно означало бы держать порт закрытым, пока VK думает.
///
/// Без токена сеть не трогается ВОВСЕ: запрос «расскажи о владельце пустого
/// токена» гарантированно вернул бы код 5 и положил бы в лог ошибку на ровном
/// месте у каждого, кто запустил devforge с `--vk`, но ещё не логинился.
///
/// Повторов НЕТ: одна проверка на старт. Протухший токен чинится только новым
/// значением от пользователя, и цикл переверификаций лишь тратил бы бюджет
/// 3 rps.
///
/// LongPoll-supervisor, наоборот, запускается ВСЕГДА — даже без токена. Он сам
/// ждёт авторизации на [`VkState::authorized_notify`] и не делает до неё ни
/// одного сетевого вызова, зато вставка токена в работающем процессе поднимает
/// realtime БЕЗ рестарта devforge.
pub async fn spawn_workers(state: &Arc<VkState>) {
    let configured = state.has_token().await;
    tracing::info!(
        target: "forge_vk",
        configured,
        "forge-vk: spawn_workers"
    );
    longpoll::spawn(state).await;
    // Подчистка кэша — ВСЕГДА, как и supervisor: накопленные медиа переживают
    // logout, и оставить их расти без потолка означало бы, что дисковый бюджет
    // соблюдается только у залогиненного пользователя.
    media_cache::spawn_sweeper(state).await;
    // GC незавершённых загрузок — тоже всегда, и по более резкой причине:
    // первый его прогон убирает СИРОТ прошлого процесса. Реестр `uploads`
    // живёт в памяти, поэтому после рестарта каждый `.part` уже никому не
    // принадлежит, а место занимает вполне реальное.
    routes::upload::spawn_uploads_gc(state).await;
    if !configured {
        return;
    }

    let st = state.clone();
    let handle = tokio::spawn(async move {
        // `from_config` уже поставил `Disconnected`, а `verify_token` первым
        // делом ставит `Checking` — поэтому UI на старте видит
        // disconnected → checking → результат и НЕ мигает «отключено».
        match st.api_client().await {
            Ok(client) => {
                let backend = login::HttpBackend::new(client);
                login::verify_token(&st, &backend, None).await;
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_vk",
                    error = %e,
                    "forge-vk: could not build the API client for the startup check"
                );
                st.set_login(LoginFlow::Disconnected {
                    error: Some(format!("could not build the VK client: {e}")),
                })
                .await;
            }
        }
    });
    state.register_worker(handle).await;
}

/// Graceful shutdown плагина.
///
/// 1. `shutdown.cancel()` — будит WS-циклы и воркеров;
/// 2. `analysis.shutdown()` — гасит запущенные процессы `claude`;
/// 3. `shutdown_workers()` — даёт каждому две секунды выйти самому и только
///    потом абортит.
///
/// Шаг 2 обязан идти ДО шага 3 и не имеет права быть забытым: дочерний
/// `claude` умирает только вместе с tokio-таской раннера (`kill_on_drop`), а
/// таски эти в списке `workers` не числятся. Без явного вызова процесс анализа
/// пережил бы остановку devforge и продолжил бы жечь токены в никуда.
///
/// Идемпотентна: `CancellationToken` можно отменять сколько угодно раз, список
/// воркеров после первого прохода пуст, реестр анализов — тем более.
pub async fn shutdown(state: &Arc<VkState>) {
    tracing::info!(target: "forge_vk", "forge-vk: shutdown initiated");
    state.shutdown.cancel();
    state.analysis.shutdown().await;
    state.shutdown_workers().await;
    tracing::info!(target: "forge_vk", "forge-vk: shutdown complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> (tempfile::TempDir, VkConfig) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = VkConfig {
            cache_dir: dir.path().join("cache"),
            // Заметки — ЕДИНСТВЕННЫЙ файл плагина, который пишется без
            // участия пользователя; без подмены пути прогон тестов затирал бы
            // живой `~/.forge/vk-chat-prompts.json`.
            prompts_path: dir.path().join("prompts.json"),
            ..VkConfig::default()
        };
        (dir, cfg)
    }

    #[tokio::test]
    async fn init_creates_state_without_a_token() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(!state.is_authorized().await);
        assert!(state.config.cache_dir.is_dir(), "cache_dir создан");
    }

    #[tokio::test]
    async fn init_derives_disconnected_with_a_token() {
        let (_dir, mut cfg) = test_cfg();
        cfg.access_token = SecretToken::new("vk1.a.x");
        let state = init(cfg).unwrap();
        assert_eq!(state.login_state_dto().await.state, "disconnected");
        assert!(state.has_token().await);
    }

    #[test]
    fn init_rejects_invalid_config() {
        let (_dir, mut cfg) = test_cfg();
        cfg.rate_limit_rps = 0;
        assert!(init(cfg).is_err());

        let (_dir2, mut cfg) = test_cfg();
        cfg.base_url = String::new();
        assert!(init(cfg).is_err());
    }

    /// `init` обязан быть быстрым и НЕ ходить в сеть: проверка токена — работа
    /// `spawn_workers`. Тест сломается, если кто-то добавит в `init` вызов
    /// `users.get` «чтобы сразу показать пользователя».
    #[tokio::test]
    async fn init_never_touches_the_network() {
        let (_dir, mut cfg) = test_cfg();
        cfg.access_token = SecretToken::new("vk1.a.x");
        // Заведомо недостижимый адрес: если бы init ходил в сеть, тест
        // споткнулся бы о connect-таймаут.
        cfg.base_url = "http://127.0.0.1:1/method".into();

        let started = std::time::Instant::now();
        let state = init(cfg).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "init занял {:?} — он не должен ждать сеть",
            started.elapsed()
        );
        assert_eq!(state.login_state_dto().await.state, "disconnected");
    }

    /// Заметки к чатам читаются ПРЯМО в конструкторе состояния — и сломанный
    /// файл не имеет права ни уронить `init`, ни оставить вкладку без старта.
    #[tokio::test]
    async fn init_loads_chat_prompts_and_survives_a_broken_file() {
        let (dir, mut cfg) = test_cfg();
        std::fs::write(
            &cfg.prompts_path,
            r#"{"version":1,"chats":{"2000000001":{"prompt":"это заказчик","updated_at":7}}}"#,
        )
        .unwrap();
        let state = init(cfg.clone()).unwrap();
        assert_eq!(
            state.chat_prompt("2000000001").await.unwrap().prompt,
            "это заказчик"
        );

        // Тот же путь, но мусор внутри: пустое хранилище, без паники и Err.
        cfg.prompts_path = dir.path().join("broken.json");
        std::fs::write(&cfg.prompts_path, b"}{ not json at all").unwrap();
        let state = init(cfg).expect("битый файл заметок не фатален");
        assert!(state.prompts.lock().await.chats.is_empty());
        assert!(state.chat_prompt("2000000001").await.is_none());
    }

    /// Каталог кэша может быть несоздаваем (родитель — обычный файл, нет
    /// прав): `init` обязан выжить — ошибка всплывёт позже, при реальной
    /// записи кэша, с более точным контекстом.
    #[tokio::test]
    async fn init_survives_an_uncreatable_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file, not a dir").unwrap();

        let cfg = VkConfig {
            cache_dir: blocker.join("cache"),
            ..VkConfig::default()
        };
        let state = init(cfg).expect("ошибка каталога не фатальна для init");
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        assert!(blocker.is_file(), "чужой файл не перезаписан каталогом");
    }

    /// Без токена проверять нечего — но LongPoll-supervisor поднимается
    /// ВСЁ РАВНО: именно он превращает «вставил токен в UI» в работающий
    /// realtime без рестарта процесса. Сеть при этом не трогается: воркер
    /// стоит на `authorized_notify`.
    #[tokio::test]
    async fn spawn_workers_starts_only_the_longpoll_supervisor_without_a_token() {
        let (_dir, cfg) = test_cfg();
        let state = init(cfg).unwrap();
        spawn_workers(&state).await;
        assert_eq!(
            state.workers.lock().await.len(),
            3,
            "LongPoll-supervisor + подчистка медиа-кэша + GC загрузок"
        );
        assert_eq!(state.login_state_dto().await.state, "not_configured");
        // Курсор не запрашивался: без авторизации воркер в сеть не ходит.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(state.lp_cursor().await.is_none());
        shutdown(&state).await;
    }

    /// С токеном стартовая проверка запускается ФОНОМ и переводит состояние
    /// сама: `spawn_workers` возвращается мгновенно, не дожидаясь сети.
    ///
    /// Здесь VK заведомо недостижим (порт 1), поэтому итог — `disconnected`.
    /// Проверяется главное: путь проходит через `checking`, то есть UI не
    /// мигает «отключено» на старте.
    #[tokio::test]
    async fn spawn_workers_verifies_an_existing_token_in_the_background() {
        let (_dir, mut cfg) = test_cfg();
        cfg.access_token = SecretToken::new("vk1.a.startup-check");
        cfg.base_url = "http://127.0.0.1:1/method".into();
        let state = init(cfg).unwrap();
        let mut rx = state.subscribe();

        let started = std::time::Instant::now();
        spawn_workers(&state).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "spawn_workers обязан возвращаться сразу, занял {:?}",
            started.elapsed()
        );
        assert_eq!(
            state.workers.lock().await.len(),
            4,
            "проверка токена + LongPoll-supervisor + подчистка медиа-кэша + GC загрузок"
        );

        // Ждём кадры до терминального состояния.
        let mut seen: Vec<String> = Vec::new();
        let deadline = std::time::Duration::from_secs(10);
        let collected = tokio::time::timeout(deadline, async {
            while let Ok(ev) = rx.recv().await {
                if let VkServerMsg::LoginState(dto) = ev {
                    let state = dto.state.clone();
                    seen.push(state.clone());
                    if state != "checking" {
                        break;
                    }
                }
            }
            seen
        })
        .await
        .expect("стартовая проверка не завершилась");

        assert_eq!(
            collected.first().map(String::as_str),
            Some("checking"),
            "первым кадром обязан быть checking: {collected:?}"
        );
        assert_eq!(
            collected.last().map(String::as_str),
            Some("disconnected"),
            "недостижимый VK ⇒ экран повтора: {collected:?}"
        );
        shutdown(&state).await;
    }

    /// ИНВАРИАНТ УТЕЧКИ, ПРОВЕРЯЕМЫЙ ПО ИСХОДНИКАМ (задача Ф3.4).
    ///
    /// Тесты проверяют то, что успело выполниться; этот — то, что НАПИСАНО.
    /// Ищем во всём крейте строки, где `expose()` стоит рядом с
    /// tracing-макросом: `tracing::warn!(token = %t.expose())` компилируется,
    /// проходит любой юнит-тест соседнего модуля и попадает в лог навсегда.
    ///
    /// Разрешено ровно одно исключение — `fingerprint()`: он по построению не
    /// раскрывает середину токена и существует ради вопроса «тот ли токен
    /// вставлен».
    ///
    /// Эвристика построчная и это осознанно: многострочный вызов макроса с
    /// `expose()` на отдельной строке она пропустит, зато не даёт ложных
    /// срабатываний и читается без парсера. Настоящая защита — отсутствие
    /// `Serialize` у `SecretToken`; тест ловит вторую по вероятности ошибку.
    #[test]
    fn no_source_line_logs_the_exposed_token() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }

        const LOG_MACROS: [&str; 6] = [
            "tracing::",
            "trace!",
            "debug!",
            "info!",
            "warn!",
            "error!",
        ];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        assert!(files.len() >= 10, "исходники не нашлись: {files:?}");

        let mut offenders = Vec::new();
        for file in files {
            let body = std::fs::read_to_string(&file).unwrap();
            for (n, line) in body.lines().enumerate() {
                let trimmed = line.trim_start();
                // Комментарии пропускаем: в них живут ровно те примеры, ради
                // запрета которых тест и написан (включая этот).
                if trimmed.starts_with("//") {
                    continue;
                }
                if !line.contains(".expose()") {
                    continue;
                }
                if LOG_MACROS.iter().any(|m| line.contains(m)) {
                    offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "секрет уезжает в лог:\n{}",
            offenders.join("\n")
        );
    }

    /// Идемпотентность shutdown — не теория: хост зовёт его из обработчика
    /// сигнала, а тот может сработать дважды (Ctrl-C + SIGTERM от systemd).
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

        // Повторный вызов не паникует и не виснет.
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown(&state))
            .await
            .expect("повторный shutdown завис");
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
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/vk/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
