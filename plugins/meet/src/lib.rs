//! # forge-meet
//!
//! Плагин видеовстреч для F.O.R.G.E. Бот заходит в Яндекс Телемост, Google
//! Meet, Zoom или Teams в СВОЁМ Chrome через CDP, пишет звук, следит, кто и
//! когда говорит, а после встречи отдаёт расшифровку с именами спикеров и
//! саммари — привязанные к тому же сообщению в чате, откуда пришла ссылка.
//!
//! ## Почему плагин самостоятельный, а не общий крейт с мессенджерами
//!
//! `chat-analysis` вынесен в общий крейт потому, что анализ — операция **над
//! сообщениями мессенджера**: ядру нужен транскрипт, собрать который умеет
//! только плагин. У встречи такой зависимости нет ни по одному направлению:
//! ссылку приносит фронт, имена участников берутся из самой встречи, а связь
//! «встреча ↔ сообщение» — непрозрачная тройка `(plugin, chat_id, message_id)`
//! ([`db::repo::SourceKey`]). Общий крейт дал бы трёхкратные роуты, WS-кадры,
//! конфиг и базу ради фичи, в которой нет ничего мессенджер-специфичного.
//!
//! Фронтовая симметрия подтверждает выбор: `messenger/analysis-core.js` — один
//! файл на три панели, кнопка встречи живёт так же.
//!
//! ## Пять решений, определяющих всё остальное
//!
//! 1. **Звук идёт через CDP-биндинг, а не через наш HTTP-сервер.** Разбор
//!    четырёх барьеров и почему исключение в `csrf_guard` нельзя делать
//!    никогда — в шапке [`routes`].
//! 2. **По проводу пер-трековый PCM16 16 кГц моно.** Адресуется сэмпловым
//!    смещением, поэтому потерянный кадр оставляет тишину, а не сдвигает
//!    таймлайн. Заодно ffmpeg уходит с критического пути расшифровки.
//! 3. **VAD живёт в `AudioWorkletProcessor`**, а не в `setInterval`: окно бота
//!    occluded, и Chrome троттлит таймеры рендерера до секунды.
//! 4. **Селекторы платформ — данные, а не код** (`meet.probes.toml`, Ф4).
//!    Держащий всё инвариант: запись НЕ ЗАВИСИТ ни от одной DOM-пробы.
//! 5. **`failed` — только когда нет транскрипта** ([`status`]).
//!
//! ## Lifecycle (`tmux-web/src/main.rs`, opt-in флаг `--meet`)
//!
//! ```ignore
//! let state = forge_meet::init(forge_meet::MeetConfig::load())?;
//! let app = forge_meet::register_routes(app, state.clone());
//! forge_meet::spawn_workers(&state).await;
//! // ... при получении сигнала остановки:
//! forge_meet::shutdown(&state).await;
//! ```
//!
//! [`register_routes`] ОБЯЗАТЕЛЬНО вызывать ДО `.fallback(...)` и
//! `.layer(auth_middleware)` хоста — тогда Bearer-auth и `csrf_guard`
//! покрывают `/api/meet/*` и `/ws/meet` автоматически.
//!
//! Ошибка [`init`] для хоста НЕ фатальна: он логирует `warn` и стартует без
//! плагина. Оператор `?` в блоке регистрации ЗАПРЕЩЁН — он превратил бы
//! опечатку в `meet.toml` в отказ всего devforge.
//!
//! ## Разделение `init` / `spawn_workers`
//!
//! | | `init` | `spawn_workers` |
//! |---|---|---|
//! | синхронность | синхронный | асинхронный |
//! | файловая система | НЕ трогает записи | открывает базу, читает каталоги встреч |
//! | процессы | НЕ запускает | пробы бинарей, Chrome, ffmpeg, whisper |
//! | время | < 500 мс, закреплено тестом | возвращается немедленно, работа уходит в задачи |
//!
//! Разделение не формальное. `whisper-cli --help` на ПЕРВОМ в жизни запуске
//! тратит 12,5 секунды на `ggml_metal_library_init` (измерено в Ф0) — столько
//! devforge не будет ждать биндинга порта.
//!
//! ## Host boundary
//!
//! Отдельного крейта `meet-host-api` НЕТ и не планируется — по той же причине,
//! что у Telegram, VK и Bitrix: плагину от хоста не нужно ничего, кроме
//! `Router`. Auth решается порядком регистрации.

pub mod bot;
pub mod cloud;
pub mod config;
pub mod db;
pub mod deps;
pub mod error;
pub mod hub;
pub mod ingest;
pub mod models;
pub mod pipeline;
pub mod progress;
pub mod providers;
pub mod reconcile;
pub mod recording;
pub mod retention;
pub mod routes;
pub mod state;
pub mod status;
pub mod summary;
pub mod testing;
pub mod ws;

use std::sync::Arc;

use axum::Router;

pub use config::{AudioCodec, AudioTransport, MeetConfig, WindowMode};
pub use db::repo::{MeetingBadge, MeetingRow, SourceKey};
pub use bot::{
    detect_track_mode, AudioFrame, BotScript, BotStreams, ChromeBot, ChromeBotConfig,
    ChromeBotFactory, EndConfig, EndSignals, EndWatch, JoinMode, MeetBot, Observation, ProbeBook,
    ProbeSet, ScriptedBot, TrackMode, TrackObservation, WindowReport,
};
pub use deps::{DepBinary, DepsReport, WhisperFlags};
pub use models::{ModelKind, ModelSpec, ModelStatus, MODELS};
pub use error::MeetError;
pub use hub::{MeetEvent, MeetHub, MeetSink, ParticipantInfo, StopReason};
pub use providers::{parse_meeting_url, MeetingLink, Platform, Provider, PROVIDERS};
pub use reconcile::{reconcile_on_boot, ReconcileAction, ReconcileReport};
pub use recording::{BotFactory, BotRequest, RecordingSession, ScriptedBotFactory};
pub use routes::StatusDto;
pub use state::MeetState;
pub use status::{MeetCommand, MeetStatus, SummaryStatus};
pub use ws::protocol::{Limits, MeetClientMsg, MeetServerMsg};

/// Создаёт состояние плагина из конфигурации.
///
/// **Синхронная, без ввода-вывода по записям и без процессов.** Всё, что
/// трогает диск и внешние бинари, живёт в [`spawn_workers`]; здесь только
/// конструирование значений в памяти.
///
/// Инвариант закреплён двумя тестами, а не комментарием:
/// `init_is_fast_and_spawns_no_processes` меряет время и считает дочерние
/// процессы, `init_does_not_open_the_database` проверяет, что базы после
/// `init` нет.
///
/// # Errors
///
/// Сейчас не возвращает ошибок ни при каком конфиге: [`MeetConfig::load`] не
/// паникует и чинит выходящие за границы значения, а плагин обязан стартовать
/// при любом окружении — `GET /api/meet/status` единственный способ узнать,
/// чего не хватает. Сигнатура оставлена `Result` намеренно: хост уже
/// обрабатывает `Err` ветвью `warn`, и будущая настоящая ошибка не потребует
/// править `main.rs`.
pub fn init(cfg: MeetConfig) -> anyhow::Result<Arc<MeetState>> {
    let state = MeetState::new(cfg);
    tracing::info!(
        target: "forge_meet",
        cache_dir = %state.config.cache_dir.display(),
        db_path = %state.config.db_path.display(),
        window_mode = state.config.window_mode.as_str(),
        bot_name = %state.config.bot_name,
        whisper_model = %state.config.whisper_model,
        recordings = state.config.max_concurrent_recordings,
        "forge-meet: state initialized"
    );
    Ok(state)
}

/// Мерджит роуты плагина в приложение хоста.
///
/// Host-параметра нет намеренно (см. «Host boundary» в шапке модуля).
pub fn register_routes(app: Router, state: Arc<MeetState>) -> Router {
    let plugin = routes::build_router(state);
    tracing::info!(
        target: "forge_meet",
        "forge-meet: registered routes (/api/meet/status, /api/meet/badges, /ws/meet)"
    );
    app.merge(plugin)
}

/// Запускает фоновую часть плагина.
///
/// Порядок шагов существенен:
///
/// 1. **открыть базу и применить миграции** — без них не работает ничего;
/// 2. **прогреть тёплый индекс бейджей** — до этого момента лента показывала
///    бы «встречи нет» на записях, которые есть;
/// 3. **[`reconcile_on_boot`]** — ПЕРВЫМ ДЕЛОМ после базы, потому что именно
///    он спасает записи, прерванные предыдущим завершением процесса, и
///    подметает осиротевшие Chrome;
/// 4. пробы внешних бинарей (Ф5) и подчистка кэша по ретенции (Ф7).
///
/// Функция возвращается быстро: медленная работа уходит в отдельные задачи,
/// поэтому отсутствующий ffmpeg не задерживает биндинг порта хостом.
///
/// Сбой открытия базы НЕ фатален: плагин остаётся живым, `GET /api/meet/status`
/// отвечает 200 с `db_ready: false`, а остальные роуты — `503 db_unavailable`.
/// Это лучше, чем упавший devforge из-за файла, который можно удалить.
pub async fn spawn_workers(state: &Arc<MeetState>) {
    // Боевой источник ботов. До Ф4 здесь стоял `MissingBotFactory`, честно
    // отвечавший `chrome_unavailable` на любую попытку записи; теперь его
    // место занимает Chrome через `forge-cdp`.
    //
    // Ставится ЗДЕСЬ, а не в `init`: чтение `~/.config/forge/meet.probes.toml`
    // — это файловая система, а `init` обязан оставаться синхронным и не
    // трогать диск. Сам по себе вызов ничего не запускает: браузер поднимается
    // только в `MeetBot::start`.
    let factory = Arc::new(bot::ChromeBotFactory::new(state.config.clone()));
    tracing::info!(
        target: "forge_meet",
        platforms = ?factory.probes().platforms(),
        probes_source = ?factory.probes().source(),
        "forge-meet: пробы платформ загружены"
    );
    state.set_bot_factory(factory);

    if let Err(e) = state.config.ensure_dirs() {
        // Не фатально: ошибка всплывёт позже, при реальной записи, с более
        // точным контекстом.
        tracing::warn!(
            target: "forge_meet",
            error = %e,
            "forge-meet: каталоги данных не создались"
        );
    }

    match db::Db::open(&state.config.db_path).await {
        Ok(db) => match db.migrate().await {
            Ok(()) => {
                if let Err(e) = state.attach_db(Arc::new(db)) {
                    tracing::warn!(target: "forge_meet", error = %e, "forge-meet: база не привязалась");
                } else {
                    if let Err(e) = state.warm_badge_index().await {
                        tracing::warn!(
                            target: "forge_meet",
                            error = %e,
                            "forge-meet: тёплый индекс не прогрелся"
                        );
                    }
                    // ПЕРВЫМ ДЕЛОМ после базы: сорок минут записи не имеют
                    // права пропасть из-за Ctrl-C.
                    let report = reconcile_on_boot(state).await;
                    if report.scanned > 0 || report.orphans_killed > 0 {
                        tracing::info!(
                            target: "forge_meet",
                            scanned = report.scanned,
                            salvaged = report.salvaged,
                            orphans_killed = report.orphans_killed,
                            "forge-meet: восстановление после перезапуска"
                        );
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "forge_meet",
                error = %e,
                "forge-meet: миграции не применились, работаю без хранилища"
            ),
        },
        Err(e) => tracing::warn!(
            target: "forge_meet",
            error = %e,
            path = %state.config.db_path.display(),
            "forge-meet: база не открылась, работаю без хранилища"
        ),
    }

    // Пробы внешних бинарей уходят в ЗАДАЧУ, а не считаются здесь: первый в
    // жизни запуск `whisper-cli` стоит 12,5 секунды на
    // `ggml_metal_library_init`, и ждать их значило бы держать порт
    // незанятым всё это время. До окончания проб `deps.probed == false`, и
    // интерфейс показывает «проверяем», а не «ничего нет».
    let prober = state.clone();
    let handle = tokio::spawn(async move {
        deps::refresh(&prober).await;
    });
    state.register_worker(handle).await;

    // Ретенция. Тоже отдельной задачей: первый прогон обходит каталог записей
    // со `stat` по каждому файлу, и держать на нём биндинг порта незачем.
    // Останавливается по общему `shutdown`-токену.
    let sweeper = state.clone();
    let handle = tokio::spawn(async move {
        retention::sweeper_loop(sweeper).await;
    });
    state.register_worker(handle).await;

    tracing::info!(
        target: "forge_meet",
        db_ready = state.db().is_some(),
        "forge-meet: spawn_workers"
    );
}

/// Graceful shutdown плагина.
///
/// 1. `shutdown.cancel()` — будит WS-циклы и воркеров;
/// 2. `hub.close()` — новые записи и прогоны больше не начинаются, но уже
///    выданные слоты остаются действительными: идущая запись обязана
///    дофлашить WAV, а не оборваться;
/// 3. `shutdown_workers()` — по две секунды каждому выйти самому. Пауза не
///    косметическая: писатель WAV на выходе закрывает заголовок файла с
///    правильной длиной, а `abort()` в произвольной await-точке этого сделать
///    не даст.
///
/// Хост зовёт это **раньше** финального снимка истории сессий: плагин должен
/// успеть убить Chrome и дофлашить активные WAV.
///
/// **Идемпотентна**: `CancellationToken` можно отменять сколько угодно раз, а
/// список воркеров после первого прохода пуст. Это не теория — обработчик
/// сигнала срабатывает и на Ctrl-C, и на SIGTERM.
pub async fn shutdown(state: &Arc<MeetState>) {
    tracing::info!(target: "forge_meet", "forge-meet: shutdown initiated");
    state.shutdown.cancel();
    state.hub.close();
    state.shutdown_workers().await;
    tracing::info!(target: "forge_meet", "forge-meet: shutdown complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Конфиг теста: все внешние бинари УКАЗАНЫ ЯВНО и указывают в пустоту.
    ///
    /// Явные пути, а не подмена `PATH`: `PATH` — состояние всего процесса, а
    /// тесты крейта идут параллельно в одном процессе, и сосед, собирающий
    /// конфиг в этот же момент, увидел бы чужое окружение. Заодно проба
    /// перестаёт зависеть от того, что установлено на машине сборки.
    fn test_cfg(dir: &std::path::Path) -> MeetConfig {
        MeetConfig {
            config_path: dir.join("meet.toml"),
            cache_dir: dir.join("cache"),
            db_path: dir.join("cache/meet.db"),
            models_dir: dir.join("cache/models"),
            claude_cli_path: dir.join("no-such-claude"),
            ffmpeg_path: Some(dir.join("no-such-ffmpeg")),
            ffprobe_path: Some(dir.join("no-such-ffprobe")),
            whisper_path: Some(dir.join("no-such-whisper")),
            chrome_path: Some(dir.join("no-such-chrome")),
            ..MeetConfig::default()
        }
    }

    /// Ждёт окончания проб. Они идут в фоне, и без ожидания тест ловил бы
    /// `probed: false` через раз.
    async fn wait_for_probe(state: &Arc<MeetState>) -> deps::DepsReport {
        for _ in 0..200 {
            let d = state.deps_snapshot().await;
            if d.probed {
                return d;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        state.deps_snapshot().await
    }

    /// ИНВАРИАНТ ФАЗЫ: `init` быстрее 500 мс и не порождает НИ ОДНОГО
    /// процесса.
    ///
    /// Второе проверяется подсчётом собственных детей до и после, а не верой в
    /// дисциплину: `whisper-cli --help` на первом в жизни запуске тратит 12,5
    /// секунды, и попади он сюда — devforge вставал бы на старте.
    #[tokio::test]
    async fn init_is_fast_and_spawns_no_processes() {
        let dir = tempfile::tempdir().unwrap();
        // Тесты крейта идут в одном процессе и параллельно: без замка соседний
        // тест со своим `sleep` попадал бы в счётчик.
        let _guard = testing::process_count_guard();
        let before = testing::count_child_processes();

        let started = std::time::Instant::now();
        let state = init(test_cfg(dir.path())).unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "init занял {elapsed:?} — он не имеет права ждать ни диск, ни процессы"
        );
        assert_eq!(
            testing::count_child_processes(),
            before,
            "init запустил дочерний процесс: пробы бинарей живут в spawn_workers"
        );
        assert_eq!(state.config.max_concurrent_whisper, 1);
    }

    /// `init` не трогает файловую систему записей: базы после него нет, и
    /// каталог кэша не создан.
    #[tokio::test]
    async fn init_does_not_touch_the_recordings_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg(dir.path());
        let cache = cfg.cache_dir.clone();
        let db_path = cfg.db_path.clone();

        let state = init(cfg).unwrap();
        assert!(state.db().is_none(), "init открыл базу");
        assert!(!db_path.exists(), "init создал файл базы");
        assert!(!cache.exists(), "init создал каталог записей");
    }

    /// Плагин обязан стартовать без единого внешнего бинаря — и оставаться
    /// работоспособным: `spawn_workers` открывает базу и возвращается.
    #[tokio::test]
    async fn the_plugin_starts_with_no_external_binaries_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let state = init(test_cfg(dir.path())).unwrap();

        let started = std::time::Instant::now();
        spawn_workers(&state).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "spawn_workers обязан возвращаться быстро, занял {:?}",
            started.elapsed()
        );
        assert!(state.db().is_some(), "база обязана открыться в spawn_workers");

        // Пробы уходят в задачу и заканчиваются вскоре после возврата.
        // Плагин при этом уже работает: ждать окружение никто не обязан.
        let deps = wait_for_probe(&state).await;
        assert!(deps.probed);
        assert_eq!(deps.ffmpeg.found, Some(false), "{:?}", deps.ffmpeg);
        assert_eq!(deps.whisper.found, Some(false), "{:?}", deps.whisper);
        assert_eq!(deps.chrome.found, Some(false), "{:?}", deps.chrome);
        assert!(!deps.model_ready);
        assert_eq!(deps.record_blocker(), Some("chrome_unavailable"));
        assert_eq!(deps.transcribe_blocker(), Some("whisper_unavailable"));

        shutdown(&state).await;
    }

    /// Сломанный путь базы не роняет плагин: он живёт без хранилища и честно
    /// об этом рассказывает.
    #[tokio::test]
    async fn a_broken_database_path_does_not_kill_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "я файл, а не каталог").unwrap();

        let mut cfg = test_cfg(dir.path());
        cfg.db_path = blocker.join("meet.db");
        let state = init(cfg).unwrap();

        spawn_workers(&state).await;
        assert!(state.db().is_none(), "база не могла открыться");
        assert!(blocker.is_file(), "чужой файл не перезаписан каталогом");
        shutdown(&state).await;
    }

    /// Shutdown зовётся из обработчика сигнала, а тот может сработать дважды
    /// (Ctrl-C плюс SIGTERM от systemd).
    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state = init(test_cfg(dir.path())).unwrap();

        let token = state.shutdown.clone();
        state
            .register_worker(tokio::spawn(async move { token.cancelled().await }))
            .await;
        spawn_workers(&state).await;
        assert!(!state.shutdown.is_cancelled());

        shutdown(&state).await;
        assert!(state.shutdown.is_cancelled());
        assert!(state.workers.lock().await.is_empty());

        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(2), shutdown(&state))
                .await
                .expect("повторный shutdown завис");
        }
    }

    /// Модули, изолированные от HTTP, обязаны такими и остаться.
    ///
    /// Это та же изоляция, что даёт листовой крейт, но без второго крейта с
    /// одним потребителем — и держится она только проверкой: `use axum::…` в
    /// `providers.rs` не вызвал бы ни ошибки сборки, ни единого предупреждения.
    #[test]
    fn the_pure_modules_never_import_http() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let pure = [
            "providers.rs",
            "status.rs",
            "error.rs",
            "reconcile.rs",
            "deps.rs",
            "hub/mod.rs",
            "hub/event.rs",
            "hub/sink.rs",
            "bot/mod.rs",
            "bot/mode.rs",
            "bot/chrome.rs",
            "bot/endgame.rs",
            "bot/observe.rs",
            "bot/platforms.rs",
            "bot/frame.rs",
            "bot/scripted.rs",
            "bot/inject/mod.rs",
            "bot/transport.rs",
            "ingest/mod.rs",
            "ingest/wav.rs",
            "ingest/tracks.rs",
            "ingest/manifest.rs",
            "ingest/journal.rs",
            "ingest/recorder.rs",
            "recording.rs",
            "bot/transport.rs",
            "pipeline/mod.rs",
            "pipeline/proc.rs",
            "pipeline/ffmpeg.rs",
            "pipeline/whisper.rs",
            "pipeline/speakers.rs",
            "pipeline/render.rs",
            "pipeline/run.rs",
            "models.rs",
            "ws/protocol.rs",
        ];
        for rel in pure {
            let path = root.join(rel);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{} не читается: {e}", path.display()));
            // Отбрасываем тестовый модуль: тесты вправе поднимать роутер.
            let production = src.split("#[cfg(test)]").next().unwrap_or(&src);
            for line in production.lines() {
                let line = line.trim();
                // Комментарии обсуждают axum сколько угодно — важен код.
                if line.starts_with("//") {
                    continue;
                }
                assert!(
                    !(line.starts_with("use axum") || line.starts_with("use http")),
                    "{rel} импортирует HTTP: {line}"
                );
            }
        }
    }

    /// Статус мутируется ровно в одном месте. Иначе тёплый индекс и база
    /// разъезжаются, и искать это потом — сутки.
    #[test]
    fn no_status_update_bypasses_set_status() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut checked = 0usize;
        visit_rs(&root, &mut |path, src| {
            checked += 1;
            let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
            if rel == "db/repo/meetings.rs" {
                return;
            }
            let production = src.split("#[cfg(test)]").next().unwrap_or(src);
            // Комментарии обсуждают запрет сколько угодно — важен код.
            let offending = production
                .lines()
                .map(str::trim)
                .filter(|l| !l.starts_with("//"))
                .any(|l| l.contains("UPDATE meetings"));
            if offending {
                offenders.push(rel);
            }
        });
        assert!(checked > 10, "обход исходников ничего не нашёл");
        assert!(
            offenders.is_empty(),
            "UPDATE meetings мимо repo::meetings: {offenders:?}"
        );
    }

    fn visit_rs(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit_rs(&path, f);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let src = std::fs::read_to_string(&path).unwrap();
                f(&path, &src);
            }
        }
    }
}
