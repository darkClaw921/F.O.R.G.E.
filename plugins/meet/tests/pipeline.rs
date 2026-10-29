//! Сквозные тесты пост-обработки: от сырых WAV до `transcript.md`.
//!
//! Отдельный крейт, а не юнит-тесты: здесь запускаются процессы (моки
//! ffmpeg/ffprobe/whisper), и уводить их в общий тестовый процесс значит
//! мешать инварианту «`init` не запускает процессов», который считает
//! собственных детей.
//!
//! Все внешние бинари — МОКИ в `TempDir`. Настоящих здесь нет и быть не
//! должно: whisper на машине сборки может отсутствовать, а его первый запуск
//! стоит 12,5 секунды.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_meet::config::MeetConfig;
use forge_meet::db::repo;
use forge_meet::ingest::{wav::WavWriter, Manifest};
use forge_meet::pipeline;
use forge_meet::state::MeetState;
use forge_meet::status::MeetStatus;
use forge_meet::testing::mocks::{self, MockSegment, WhisperMock};
use forge_meet::ws::protocol::MeetServerMsg;

/// ПОТОЛОК ЛЮБОГО ОЖИДАНИЯ В ЭТОМ ФАЙЛЕ.
///
/// Был флак (`forge-4gw6s`): ожидания стояли фиксированными циклами вида
/// `for _ in 0..300 { sleep(10ms) }`, то есть ровно три секунды на то, чтобы
/// пайплайн дошёл до нужного статуса. В одиночном прогоне этого хватает с
/// запасом; в общем (`cargo test -p forge-meet`) параллельно идут ещё пять
/// тестовых бинарей, каждый запускает мок-бинари — shell-скрипты, — и при
/// load average под тридцать трёх секунд не хватало. Падало от одного до
/// восьми тестов, каждый раз разных.
///
/// Минута — не «спрятать зависание», а отделить «медленно» от «сломано»:
/// потолок остаётся КОНЕЧНЫМ, а сообщение при его достижении прямо говорит,
/// что это таймаут ожидания, а не неверное состояние. Настоящее зависание
/// по-прежнему падает, просто через минуту, а не через три секунды.
const WAIT_CAP: Duration = Duration::from_secs(60);

/// Ждёт условие с внятным потолком и внятным сообщением.
///
/// Макрос, а не функция: условие содержит `.await` (поход в базу за статусом),
/// и заворачивать его в замыкание, возвращающее future, значило бы бороться с
/// временами жизни там, где боремся с флаком.
macro_rules! wait_for {
    ($label:expr, $cond:expr) => {{
        let started = std::time::Instant::now();
        let mut reached = false;
        while started.elapsed() < WAIT_CAP {
            if $cond {
                reached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            reached,
            "{}: ТАЙМАУТ ожидания ({:?}) — это не «неверное состояние», а «не дождались»",
            $label, WAIT_CAP
        );
    }};
}

/// Стенд одной встречи: каталог с дорожками, журналом и манифестом.
struct Stand {
    _dir: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    state: Arc<MeetState>,
    meeting_id: String,
    meeting_dir: PathBuf,
}

/// Что именно оставила после себя запись.
///
/// Разница не косметическая: от неё зависит ПУТЬ РАСШИФРОВКИ. В `Dom`
/// (Телемост, режим `Slots`) дорожка человеку не соответствует, и
/// расшифровывается сведённый микс; в `PerTrack` дорожка — это человек, и
/// каждая расшифровывается отдельно по вырезанной речи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Индикатор активного спикера из DOM, пер-трекового VAD нет.
    Dom,
    /// Пер-трековый VAD плюс имена из `msid`; вторая дорожка ПЕРЕБИВАЕТ первую.
    PerTrack,
    /// Тот же `PerTrack`, но говорят все и всю встречу — бюджет GPU не
    /// выдерживает, и пайплайн обязан вернуться к миксу.
    PerTrackShouting,
    /// **НАСТОЯЩАЯ ФОРМА ЖУРНАЛА ТЕЛЕМОСТА.** В нём лежат ОБА таймлайна —
    /// пер-трековый VAD и DOM-индикатор (измерено живым прогоном Ф4), дорожки
    /// названы каскадом, но режим SFU — `Slots`: дорожка человеку НЕ
    /// соответствует.
    ///
    /// Ради этой раскладки стенд и существует. На «голом DOM» гейт режима
    /// проверить нельзя: там нет пер-трековых интервалов, и нарезка не
    /// случилась бы всё равно — то есть тест зеленел бы даже с выломанным
    /// гейтом (ровно та ошибка, которую в проекте ловили девять раз).
    SlotsWithVad,
}

impl Stand {
    /// Готовит встречу в статусе `transcoding` с двумя дорожками и
    /// DOM-таймлайном спикеров.
    async fn new() -> Self {
        Self::build(Layout::Dom).await
    }

    /// Та же встреча, но в режиме «дорожка = человек».
    async fn per_participant(layout: Layout) -> Self {
        Self::build(layout).await
    }

    async fn build(layout: Layout) -> Self {
        Self::build_tuned(layout, |_| {}).await
    }

    /// Стенд с подправленным конфигом.
    ///
    /// Отдельный вход, а не сеттер на готовом стенде: [`MeetConfig`] внутри
    /// [`MeetState`] неизменяем по построению, и правка после запуска была бы
    /// правкой копии.
    async fn build_tuned(layout: Layout, tweak: impl FnOnce(&mut MeetConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();

        let cfg = MeetConfig {
            config_path: root.join("meet.toml"),
            cache_dir: root.join("cache"),
            db_path: root.join("cache/meet.db"),
            models_dir: root.join("models"),
            // Пути бинарей ставит каждый тест сам — здесь заведомо пустые.
            ffmpeg_path: Some(root.join("no-ffmpeg")),
            ffprobe_path: Some(root.join("no-ffprobe")),
            whisper_path: Some(root.join("no-whisper")),
            chrome_path: Some(root.join("no-chrome")),
            // Путь фиксирован, а файла по нему по умолчанию НЕТ: тест, которому
            // нужен саммаризатор, кладёт туда мок сам. Так конфиг не приходится
            // пересобирать на ходу, а прогоны без мока честно показывают, что
            // делает пайплайн с неустановленным Claude CLI.
            claude_cli_path: bin.join("claude"),
            // Запуски `claude -p` по умолчанию ВЫКЛЮЧЕНЫ глобально
            // (см. forge_claude_runner): стенд с мок-CLI включает их явно,
            // иначе каждый прогон саммари умирал бы на выключателе, не доходя
            // до того, что тест проверяет.
            claude_enabled: true,
            // Тестовые тайминги: секунды здесь никому не нужны.
            whisper_gate_poll_ms: 20,
            metal_grace_secs: 5,
            // 60, а не 10: бюджет ждёт МОК-скрипт, а не настоящий whisper.
            // Под `cargo test --workspace` на загруженной машине спавн
            // /bin/sh не укладывался в 10 с, и тест падал на таймере, а не
            // на дефекте. Настоящее зависание всё равно ловится внешними
            // `timeout(30s)` и WAIT_CAP.
            whisper_timeout_secs: 60,
            // ⚠ НЕ ДВЕ СЕКУНДЫ. Мок ffmpeg — shell-скрипт, и на полном
            // прогоне крейта (шесть тестовых бинарей параллельно, каждый
            // пишет мегабайтные WAV) он не всегда успевает СТАРТОВАТЬ за две:
            // четыре теста файла падали с «ffmpeg не ответил за 2s» на
            // исправном коде. Пятнадцать отделяют «медленно» от «сломано»;
            // тесту зависания короткий таймаут нужен по делу, и он ставит его
            // себе сам.
            // 60 по той же причине, что и whisper_timeout_secs выше.
            ffmpeg_timeout_secs: 60,
            // ЯВНО ЛОКАЛЬНЫЙ ПУТЬ. Умолчание плагина — облако (Ф15), а этот
            // файл целиком про локальный конвейер: whisper на Metal, семафор
            // «строго один», гейт «не расшифровывать во время записи»,
            // пер-трековая нарезка. Без явного значения все его тесты молча
            // ушли бы в сеть.
            stt_backend: forge_meet::config::SttBackend::Local,
            summary_backend: forge_meet::config::SummaryBackend::Local,
            ..MeetConfig::default()
        };
        let cfg = {
            let mut cfg = cfg;
            tweak(&mut cfg);
            cfg
        };
        let state = MeetState::new(cfg);
        let db = forge_meet::db::Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();

        let link = forge_meet::parse_meeting_url("https://telemost.yandex.ru/j/60497752704486").unwrap();
        let meeting_dir = root.join("cache/m1");
        let out = repo::meetings::create_or_attach(
            &state,
            repo::meetings::NewMeeting {
                link,
                source: None,
                title: Some("Планёрка".into()),
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: meeting_dir.display().to_string(),
            },
        )
        .await
        .unwrap();
        let meeting_id = out.meeting.id.clone();

        let stand = Self {
            _dir: dir,
            root,
            bin,
            state,
            meeting_id,
            meeting_dir,
        };
        match layout {
            Layout::Dom => stand.write_recording(),
            _ => stand.write_per_track_recording(layout),
        }
        stand.advance_to_transcoding().await;
        stand
    }

    /// Кладёт на диск то, что оставляет после себя запись Ф3.
    fn write_recording(&self) {
        let raw = self.meeting_dir.join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        // Две дорожки по 10 секунд: файлы УЖЕ на общей оси (ingest адресует
        // кадры абсолютным сэмпловым смещением).
        for (idx, level) in [(0usize, 1_000i16), (1, 2_000)] {
            let mut w = WavWriter::create(raw.join(format!("track-{idx}.wav")), 16_000).unwrap();
            w.write_frame(0, &vec![level; 16_000 * 10]).unwrap();
            w.finish().unwrap();
        }

        let mut m = Manifest::new(
            &self.meeting_id,
            "https://telemost.yandex.ru/j/60497752704486",
            "telemost",
            "normal",
            16_000,
        );
        m.t0_unix_ms = 1_700_000_000_000;
        m.duration_ms = 10_000;
        m.capture_path = Some("rtcpeerconnection".into());
        m.tracks = vec![
            track_state(0, "a", 0),
            track_state(1, "b", 0),
        ];
        m.finished = true;
        m.save(&self.meeting_dir).unwrap();

        // Журнал: DOM-таймлайн спикеров — основной путь Телемоста.
        let mut j = forge_meet::ingest::journal::EventJournal::open(&self.meeting_dir).unwrap();
        j.note(0, "recording_started", serde_json::json!({"t0_unix_ms": 1_700_000_000_000u64}))
            .unwrap();
        j.note(0, "active_speaker", serde_json::json!({"at_ms": 0, "name": "Аня"}))
            .unwrap();
        j.note(5_000, "active_speaker", serde_json::json!({"at_ms": 5_000, "name": "Боря"}))
            .unwrap();
        j.note(10_000, "recording_stopped", serde_json::json!({"reason": "user_stop", "at_ms": 10_000}))
            .unwrap();
    }

    /// Запись встречи, где ДОРОЖКА СООТВЕТСТВУЕТ ЧЕЛОВЕКУ.
    ///
    /// Отличий от [`Self::write_recording`] три, и каждое несёт смысл:
    ///
    /// 1. `manifest.track_mode = PerParticipant` — только по нему пайплайн
    ///    имеет право резать дорожки;
    /// 2. в журнале есть ПЕР-ТРЕКОВЫЙ VAD (`speech_started`/`speech_ended`), а
    ///    не только индикатор из DOM;
    /// 3. `msid` дорожки совпадает с ключом участника, поэтому каскад называет
    ///    дорожки настоящими именами первой же ступенью.
    ///
    /// Расписание речи содержит ПЕРЕБИВАНИЕ: Боря вступает, пока Аня ещё
    /// говорит. Ради него весь путь и затевался.
    fn write_per_track_recording(&self, layout: Layout) {
        use forge_meet::bot::mode::TrackMode;

        let shouting = layout == Layout::PerTrackShouting;

        // ВСТРЕЧА ДЛИНОЙ В МИНУТУ, а не в десять секунд, и это не удобство
        // фикстуры. Бюджет пер-трекового пути учитывает ПОСТОЯННУЮ ЦЕНУ
        // запуска whisper (модель грузится заново на каждую дорожку) — на
        // десятисекундной записи она перевешивает сэкономленный звук, и путь
        // законно не включается. Живой прогон это и показал: 12,3 с речи
        // против 18 с микса, но 2 565 мс против 1 073 мс по времени.
        let raw = self.meeting_dir.join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        for (idx, level) in [(0usize, 1_000i16), (1, 2_000)] {
            let mut w = WavWriter::create(raw.join(format!("track-{idx}.wav")), 16_000).unwrap();
            w.write_frame(0, &vec![level; 16_000 * 60]).unwrap();
            w.finish().unwrap();
        }

        let mut m = Manifest::new(
            &self.meeting_id,
            "https://telemost.yandex.ru/j/60497752704486",
            "telemost",
            "normal",
            16_000,
        );
        m.t0_unix_ms = 1_700_000_000_000;
        m.duration_ms = 60_000;
        m.capture_path = Some("rtcpeerconnection".into());
        m.track_mode = Some(if layout == Layout::SlotsWithVad {
            TrackMode::Slots
        } else {
            TrackMode::PerParticipant
        });
        m.tracks = vec![long_track_state(0, "a"), long_track_state(1, "b")];
        m.finished = true;
        m.save(&self.meeting_dir).unwrap();

        let mut j = forge_meet::ingest::journal::EventJournal::open(&self.meeting_dir).unwrap();
        j.note(0, "recording_started", serde_json::json!({"t0_unix_ms": 1_700_000_000_000u64}))
            .unwrap();
        // `msid` = ключ участника, который построит разбор DOM: `name:<имя в
        // нижнем регистре>`. Так первая ступень каскада срабатывает без
        // корреляции, и подписи в транскрипте — настоящие имена.
        j.note(0, "track_added", serde_json::json!({"track_id": "a", "idx": 0, "msid": "name:аня"}))
            .unwrap();
        j.note(0, "track_added", serde_json::json!({"track_id": "b", "idx": 1, "msid": "name:боря"}))
            .unwrap();
        // Участники по DOM: без них каскаду не с чем сопоставлять `msid`.
        j.note(0, "active_speaker", serde_json::json!({"at_ms": 0, "name": "Аня"}))
            .unwrap();
        j.note(24_000, "active_speaker", serde_json::json!({"at_ms": 24_000, "name": "Боря"}))
            .unwrap();

        let speech: Vec<(&str, u64, u64)> = if shouting {
            // Говорят все и всю встречу: речи вдвое больше длительности, и
            // бюджет пер-трекового пути её не выдерживает.
            vec![("a", 0, 60_000), ("b", 0, 60_000)]
        } else {
            vec![
                ("a", 6_000, 24_000),
                // ПЕРЕБИВАНИЕ: Боря начинает, пока Аня ещё говорит.
                ("b", 15_000, 21_000),
                ("b", 36_000, 48_000),
            ]
        };
        for (track, from, to) in speech {
            j.note(from, "speech_started", serde_json::json!({"track_id": track, "at_ms": from}))
                .unwrap();
            j.note(to, "speech_ended", serde_json::json!({"track_id": track, "at_ms": to}))
                .unwrap();
        }
        j.note(60_000, "recording_stopped", serde_json::json!({"reason": "user_stop", "at_ms": 60_000}))
            .unwrap();
    }

    /// Доводит встречу до состояния, с которого начинается пост-обработка.
    async fn advance_to_transcoding(&self) {
        for st in [
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
        ] {
            repo::meetings::set_status(&self.state, &self.meeting_id, st, Some("test"))
                .await
                .unwrap();
        }
    }

    /// Ставит мок whisper и модель.
    fn with_whisper(&self, mock: &WhisperMock) -> &Self {
        let path = mocks::mock_whisper(&self.bin, "whisper-cli", mock).unwrap();
        std::fs::create_dir_all(&self.state.config.models_dir).unwrap();
        std::fs::write(
            self.state
                .config
                .models_dir
                .join(forge_meet::models::file_name(&self.state.config.whisper_model)),
            b"lmgg1234",
        )
        .unwrap();
        self.set_dep("whisper", &path);
        self
    }

    /// Выключает саммари. Состояние пересобирается целиком: `MeetConfig` внутри
    /// него неизменяем намеренно — пороги и лимиты не должны меняться на ходу.
    ///
    /// Зовётся ДО `with_*`, иначе подставленные пути бинарей потерялись бы
    /// вместе со старым состоянием.
    fn disable_summary(&mut self) {
        self.tweak_config(|c| c.summary_enabled = false);
    }

    /// Правит конфиг встречи. Состояние пересобирается целиком: `MeetConfig`
    /// внутри него неизменяем намеренно.
    fn tweak_config(&mut self, f: impl FnOnce(&mut MeetConfig)) {
        let mut cfg = self.state.config.clone();
        f(&mut cfg);
        let db = self.state.db().expect("база обязана быть открыта");
        let state = MeetState::new(cfg);
        state.attach_db(db).unwrap();
        self.state = state;
    }

    fn with_ffmpeg(&self, path: PathBuf) -> &Self {
        self.set_dep("ffmpeg", &path);
        self
    }

    fn with_ffprobe(&self, path: PathBuf) -> &Self {
        self.set_dep("ffprobe", &path);
        self
    }

    /// Подставляет путь бинаря прямо в отчёт проб.
    ///
    /// Отчёт, а не PATH: `PATH` — состояние всего процесса, а тесты идут
    /// параллельно. Заодно это ровно то состояние, в котором пайплайн живёт в
    /// бою: он берёт пути из отчёта проб.
    fn set_dep(&self, which: &str, path: &Path) {
        let state = self.state.clone();
        let which = which.to_string();
        let path = path.display().to_string();
        futures_lite_block(async move {
            let mut deps = state.deps.write().await;
            deps.probed = true;
            let slot = match which.as_str() {
                "ffmpeg" => &mut deps.ffmpeg,
                "ffprobe" => &mut deps.ffprobe,
                "whisper" => &mut deps.whisper,
                _ => &mut deps.chrome,
            };
            slot.found = Some(true);
            slot.path = Some(path);
            if which == "whisper" {
                deps.gpu_metal = Some(true);
                deps.model_ready = true;
            }
        });
    }

    async fn status(&self) -> MeetStatus {
        repo::meetings::get(&self.state, &self.meeting_id)
            .await
            .unwrap()
            .status
    }

    async fn events_text(&self) -> String {
        repo::events::list(&self.state, &self.meeting_id)
            .await
            .unwrap()
            .into_iter()
            .map(|e| format!("{} {} {}", e.to_status, e.reason.unwrap_or_default(), e.detail.unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn track_state(idx: usize, id: &str, start: u64) -> forge_meet::ingest::tracks::TrackState {
    forge_meet::ingest::tracks::TrackState {
        track_id: id.to_string(),
        idx,
        start_offset_ms: start,
        end_offset_ms: Some(10_000),
        frames: 500,
        samples: 160_000,
        end_samples: 160_000,
        capture_path: None,
    }
}

/// Дорожка МИНУТНОЙ записи — для стенда режима «дорожка = человек».
fn long_track_state(idx: usize, id: &str) -> forge_meet::ingest::tracks::TrackState {
    forge_meet::ingest::tracks::TrackState {
        track_id: id.to_string(),
        idx,
        start_offset_ms: 0,
        end_offset_ms: Some(60_000),
        frames: 3_000,
        samples: 960_000,
        end_samples: 960_000,
        capture_path: None,
    }
}

/// Крошечный `block_on` внутри уже работающего рантайма не нужен: все вызовы
/// идут из async-тестов, но `set_dep` вызывается из синхронного контекста
/// билдера. Поэтому — блокирующая обёртка на текущем хендле.
fn futures_lite_block<F: std::future::Future<Output = ()>>(fut: F) {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut));
}

// --- сквозной путь ------------------------------------------------------------

/// **ГЛАВНЫЙ ТЕСТ ФАЗЫ.** Полный путь: сырые WAV → архив → whisper →
/// атрибуция → `transcript.md`, встреча в `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_goes_all_the_way_from_raw_wav_to_a_transcript() {
    let s = Stand::new().await;
    let mock = WhisperMock {
        segments: vec![
            MockSegment::new(0, 4_000, " Коллеги, начинаем планёрку."),
            MockSegment::new(5_200, 9_000, " Релиз готов к пятнице."),
        ],
        ..Default::default()
    };
    s.with_whisper(&mock);
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 10.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready);
    assert_eq!(report.tracks, 2);
    assert_eq!(report.segments, 2);
    assert_eq!(report.speakers, 2, "спикеры взяты из DOM-таймлайна");
    assert!(!report.queued, "очереди не было");

    // Файлы на диске.
    let md = std::fs::read_to_string(s.meeting_dir.join("transcript.md")).unwrap();
    assert!(md.contains("Коллеги, начинаем планёрку."), "{md}");
    assert!(md.contains("**[00:00:00] Аня**"), "{md}");
    assert!(md.contains("**[00:00:05] Боря**"), "{md}");
    assert!(md.contains("Участники: Аня, Боря"), "{md}");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.meeting_dir.join("transcript.json")).unwrap())
            .unwrap();
    assert_eq!(json["segments"][0]["speaker"], "Аня");
    assert_eq!(json["segments"][1]["speaker"], "Боря");

    assert!(s.meeting_dir.join("audio.m4a").is_file(), "архивный файл не собрался");
    assert!(
        !s.meeting_dir.join("work").exists(),
        "транзитные файлы не подчищены"
    );
    // Пер-трековые WAV ОСТАЮТСЯ: по ним переслушивают спикеров.
    assert!(s.meeting_dir.join("raw/track-0.wav").is_file());

    // База.
    let segs = repo::segments::page(&s.state, &s.meeting_id, 0, 100).await.unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].speaker_name.as_deref(), Some("Аня"));
    assert_eq!(segs[1].speaker_name.as_deref(), Some("Боря"));
    let spans = repo::spans::list(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(spans.len(), 2, "интервалы речи сохранены");

    // Пути в карточке встречи.
    let row = repo::meetings::get(&s.state, &s.meeting_id).await.unwrap();
    assert!(row.audio_path.is_some());
    assert!(row.transcript_path.unwrap().ends_with("transcript.json"));
}

/// Тот же путь ДО КОНЦА: за расшифровкой идёт саммари, и встреча приходит в
/// `ready` уже с ним.
#[tokio::test(flavor = "multi_thread")]
async fn the_pipeline_ends_with_a_summary_on_disk() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    mocks::mock_claude_success(&s.bin, "claude", "## Резюме\n\nВыпускаем в пятницу.").unwrap();

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(
        report.summary,
        Some(forge_meet::status::SummaryStatus::Ready),
        "саммари обязано было посчитаться"
    );

    let row = repo::meetings::get(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Ready);
    assert_eq!(row.summary_status, forge_meet::status::SummaryStatus::Ready);
    let md = std::fs::read_to_string(s.meeting_dir.join("summary.md")).unwrap();
    assert!(md.contains("Выпускаем в пятницу."), "{md}");

    // Встреча прошла через `summarizing`, а не перескочила его: журнал
    // переходов — единственный способ через сутки ответить, что было.
    let events = s.events_text().await;
    assert!(events.contains("summarizing"), "{events}");
}

/// **РЕГРЕССИЯ `forge-0s0l5`: ОБЛАКО ПО УМОЛЧАНИЮ БЕЗ КЛЮЧА НЕ ХОРОНИТ
/// ЗАПИСЬ.**
///
/// Ровно наблюдавшийся у пользователя случай: Ф15 сделала умолчанием
/// `stt_backend = cloud`, ключа polza.ai нет, а whisper с моделью и Metal на
/// месте. До правки встреча уходила в `failed / cloud_not_configured` — то
/// есть смена умолчания превращала рабочую установку в неработающую.
///
/// Проверяется всё, что делает откат ЧЕСТНЫМ:
/// * встреча доходит до `ready` с транскриптом на диске;
/// * `engine` называет ФАКТИЧЕСКИ отработавший путь (`whisper.cpp`);
/// * в журнале встречи есть событие отката с причиной.
#[tokio::test(flavor = "multi_thread")]
async fn a_cloud_default_without_a_key_falls_back_to_the_ready_local_whisper() {
    let mut s = Stand::new().await;
    // Умолчание Ф15 — облако для обеих стадий; ключа при этом нет.
    // `tweak_config` пересобирает состояние, поэтому он ИДЁТ ПЕРВЫМ: иначе
    // подставленные ниже пути бинарей потерялись бы.
    s.tweak_config(|c| {
        c.stt_backend = forge_meet::config::SttBackend::Cloud;
        c.summary_backend = forge_meet::config::SummaryBackend::Cloud;
        c.polza_api_key = forge_meet::cloud::SecretKey::default();
    });
    s.with_whisper(&WhisperMock {
        segments: vec![MockSegment::new(0, 4_000, " Ключа нет, но расшифровка есть.")],
        ..Default::default()
    });
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id)
        .await
        .expect("готовый локальный движок обязан подхватить встречу");

    assert_eq!(s.status().await, MeetStatus::Ready, "запись не имеет права пропасть");
    assert_eq!(report.segments, 1);
    let md = std::fs::read_to_string(s.meeting_dir.join("transcript.md")).unwrap();
    assert!(md.contains("Ключа нет, но расшифровка есть."), "{md}");

    // ENGINE НАЗЫВАЕТ ФАКТИЧЕСКИЙ ПУТЬ: иначе человек не поймёт, почему
    // расшифровка бесплатна.
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.meeting_dir.join("transcript.json")).unwrap())
            .unwrap();
    assert_eq!(json["engine"], "whisper.cpp", "{json}");

    // ОТКАТ ВИДЕН В ЖУРНАЛЕ.
    let events = s.events_text().await;
    assert!(events.contains("stt_fallback_local"), "{events}");
    assert!(events.contains("cloud_not_configured"), "{events}");
}

/// **ПРОВАЛ САММАРИ НЕ РОНЯЕТ ВСТРЕЧУ — В СКВОЗНОМ ПРОГОНЕ.**
///
/// Claude падает, транскрипт цел, встреча `ready`, саммари `failed`, а хвост
/// stderr виден пользователю. Час расшифровки не имеет права пропасть из-за
/// неответившего саммаризатора.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_summariser_still_leaves_a_ready_meeting_with_its_transcript() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    mocks::mock_claude_failure(&s.bin, "claude", "API Error: 529 overloaded_error").unwrap();

    let report = pipeline::run::run(&s.state, &s.meeting_id)
        .await
        .expect("провал саммари не имеет права быть провалом пайплайна");
    assert_eq!(report.summary, Some(forge_meet::status::SummaryStatus::Failed));
    assert_eq!(report.segments, 1, "расшифровка на месте");

    let row = repo::meetings::get(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Ready, "ВСТРЕЧА УПАЛА ИЗ-ЗА САММАРИ");
    assert_eq!(row.summary_status, forge_meet::status::SummaryStatus::Failed);
    assert!(s.meeting_dir.join("transcript.md").is_file());
    assert!(!s.meeting_dir.join("summary.md").exists());
    assert!(s.events_text().await.contains("overloaded_error"));
}

/// Саммари выключено конфигом — встреча идёт `transcribing → ready` напрямую,
/// и Claude не запускается вовсе.
#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_summary_takes_the_short_path_to_ready() {
    let mut s = Stand::new().await;
    s.disable_summary();
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    // Мок ЕСТЬ, но запускать его никто не должен.
    mocks::mock_claude_success(&s.bin, "claude", "не должно появиться").unwrap();

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(report.summary, Some(forge_meet::status::SummaryStatus::None));
    assert_eq!(s.status().await, MeetStatus::Ready);
    assert!(!s.meeting_dir.join("summary.md").exists(), "саммари посчиталось вопреки конфигу");
    let events = s.events_text().await;
    assert!(!events.contains("summarizing"), "лишний переход в summarizing:\n{events}");
}

/// **БЕЗ FFMPEG ВСТРЕЧА ВСЁ РАВНО ДОХОДИТ ДО `ready` С ТРАНСКРИПТОМ.**
///
/// Ровно тот инвариант, ради которого ffmpeg убран с критического пути: по
/// проводу уже идёт PCM 16 кГц моно, и свести дорожки умеет сам плагин.
/// Пропадает только архивный файл.
#[tokio::test(flavor = "multi_thread")]
async fn without_ffmpeg_the_meeting_still_reaches_ready_with_a_transcript() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    // ffmpeg и ffprobe не ставим вовсе.

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready, "встреча обязана дойти до конца");
    assert!(report.audio_path.is_none(), "архива без ffmpeg быть не может");
    assert!(!s.meeting_dir.join("audio.m4a").exists());
    assert!(
        s.meeting_dir.join("transcript.md").is_file(),
        "ТРАНСКРИПТ ОБЯЗАН БЫТЬ: ffmpeg не на критическом пути расшифровки"
    );
    assert_eq!(report.segments, 1);

    // И факт деградации виден в журнале встречи, а не только в логе сервера.
    assert!(s.events_text().await.contains("ffmpeg_missing"));
}

/// ХВОСТ STDERR ПАДАЮЩЕГО FFMPEG ДОХОДИТ ДО ПОЛЬЗОВАТЕЛЯ.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_ffmpeg_shows_its_stderr_tail_to_the_user() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(
        mocks::mock_ffmpeg_failure(&s.bin, "ffmpeg", "raw/track-0.wav: Invalid data found").unwrap(),
    );
    let mut rx = s.state.subscribe();

    let err = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap_err();
    assert!(err.message().contains("Invalid data found"), "{}", err.message());
    assert_eq!(s.status().await, MeetStatus::Failed);

    // В журнале встречи.
    let events = s.events_text().await;
    assert!(events.contains("Invalid data found"), "{events}");

    // И в кадре WS — то есть в карточке встречи.
    let mut saw = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(MeetServerMsg::Error { message, .. })) => {
                if message.contains("Invalid data found") {
                    saw = true;
                    break;
                }
            }
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
    assert!(saw, "хвост stderr не доехал до пользователя кадром error");
}

/// Зависший ffmpeg убивается по таймауту, а встреча получает внятную ошибку.
#[tokio::test(flavor = "multi_thread")]
async fn a_hanging_ffmpeg_is_killed_and_the_meeting_fails_cleanly() {
    let mut s = Stand::new().await;
    // Короткий таймаут — ЗДЕСЬ, а не в общем стенде: этому тесту он нужен по
    // делу (иначе прогон стоил бы полминуты), а соседним он же ломал прогон
    // под нагрузкой.
    s.tweak_config(|c| c.ffmpeg_timeout_secs = 5);
    {
        let mut cfg_timeout = s.state.deps.write().await;
        cfg_timeout.probed = true;
    }
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_hang(&s.bin, "ffmpeg", 120).unwrap());

    // Таймаут ffmpeg берётся из конфига; здесь он 5 с, поэтому ограничиваем
    // сам тест и проверяем, что процесс всё-таки снят.
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(30),
        pipeline::run::run(&s.state, &s.meeting_id),
    )
    .await
    .expect("пайплайн завис вместе с ffmpeg")
    .unwrap_err();

    assert!(err.message().contains("не ответил"), "{}", err.message());
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(s.status().await, MeetStatus::Failed);
}

// --- Metal-гард ---------------------------------------------------------------

/// **МЕТАЛ-ГАРД В СКВОЗНОМ ПРОГОНЕ.**
///
/// Мок печатает всегдашние `ggml_metal_*_init:` и НЕ печатает
/// `ggml_metal_init:`. Процесс обязан быть снят, встреча — упасть с
/// `metal_unavailable`, а транскрипт не появиться.
#[tokio::test(flavor = "multi_thread")]
async fn a_cpu_whisper_run_fails_the_meeting_with_metal_unavailable() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::silent_about_the_backend());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());

    let err = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap_err();
    assert_eq!(err.code(), "metal_unavailable", "{}", err.message());
    assert_eq!(s.status().await, MeetStatus::Failed);
    assert!(
        !s.meeting_dir.join("transcript.md").exists(),
        "транскрипт CPU-прогона не имеет права попасть на диск"
    );
    assert!(s.events_text().await.contains("metal_unavailable"));
}

/// Хвост stderr падающего whisper тоже доезжает до пользователя.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_whisper_shows_its_stderr_tail() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::failing(
        "error: failed to load model from 'ggml-large-v3-turbo-q5_0.bin'",
    ));
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());

    let err = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap_err();
    assert!(err.message().contains("failed to load model"), "{}", err.message());
    assert!(s.events_text().await.contains("failed to load model"));
}

// --- очередь и гейт записи ----------------------------------------------------

/// **ДВА ЗАДАНИЯ ИДУТ СТРОГО ПОСЛЕДОВАТЕЛЬНО**, и второе видит свою позицию.
#[tokio::test(flavor = "multi_thread")]
async fn two_transcriptions_run_one_after_another_and_the_second_sees_the_queue() {
    let a = Stand::new().await;
    a.with_whisper(&WhisperMock::default());
    a.with_ffmpeg(mocks::mock_ffmpeg_success(&a.bin, "ffmpeg").unwrap());

    // Вторая встреча в том же состоянии и с тем же хабом.
    let second_dir = a.root.join("cache/m2");
    let link = forge_meet::parse_meeting_url("https://telemost.yandex.ru/j/999").unwrap();
    let out = repo::meetings::create_or_attach(
        &a.state,
        repo::meetings::NewMeeting {
            link,
            source: None,
            title: None,
            mode: "normal".into(),
            bot_name: "F.O.R.G.E.".into(),
            meeting_dir: second_dir.display().to_string(),
        },
    )
    .await
    .unwrap();
    let second_id = out.meeting.id.clone();
    // Копируем каталог первой встречи целиком: содержимое здесь неважно.
    copy_dir(&a.meeting_dir, &second_dir);
    for st in [
        MeetStatus::Launching,
        MeetStatus::Joining,
        MeetStatus::Recording,
        MeetStatus::Leaving,
        MeetStatus::Transcoding,
    ] {
        repo::meetings::set_status(&a.state, &second_id, st, Some("test"))
            .await
            .unwrap();
    }

    let mut rx = a.state.subscribe();
    let s1 = a.state.clone();
    let id1 = a.meeting_id.clone();
    let s2 = a.state.clone();
    let id2 = second_id.clone();
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { pipeline::run::run(&s1, &id1).await }),
        tokio::spawn(async move { pipeline::run::run(&s2, &id2).await }),
    );
    let r1 = r1.unwrap().unwrap();
    let r2 = r2.unwrap().unwrap();

    assert!(
        r1.queued || r2.queued,
        "второй прогон обязан был постоять в очереди: слот whisper один"
    );
    assert_eq!(a.state.hub.whisper_queue_len(), 0, "очередь не рассосалась");

    // Кадр очереди виден фронту.
    let mut saw_queue_position = false;
    while let Ok(Ok(msg)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
        if let MeetServerMsg::Progress {
            queued,
            queue_position,
            ..
        } = msg
        {
            if queued && queue_position == Some(1) {
                saw_queue_position = true;
            }
        }
    }
    assert!(saw_queue_position, "кадр с queue_position=1 не пришёл");
}

/// **WHISPER НЕ ЗАПУСКАЕТСЯ, ПОКА ИДЁТ ЗАПИСЬ.**
///
/// Chrome рендерит встречу на том же GPU: запуск расшифровки во время записи
/// портит звук у той встречи, которая пишется прямо сейчас.
#[tokio::test(flavor = "multi_thread")]
async fn whisper_waits_while_any_recording_is_still_going() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());

    // Кто-то пишет прямо сейчас.
    s.state.hub.mark_active("другая-встреча").await;

    let state = s.state.clone();
    let id = s.meeting_id.clone();
    let job = tokio::spawn(async move { pipeline::run::run(&state, &id).await });

    // Ждём, пока пайплайн дойдёт до расшифровки и упрётся в гейт.
    wait_for!(
        "пайплайн дошёл до расшифровки",
        s.status().await == MeetStatus::Transcribing
    );

    // И ЖДЁТ: статус тот же, транскрипта нет, задача не закончилась.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!job.is_finished(), "расшифровка стартовала во время записи");
    assert!(
        !s.meeting_dir.join("transcript.md").exists(),
        "транскрипт появился, пока шла запись"
    );

    // Запись кончилась — расшифровка пошла сама.
    s.state.hub.mark_inactive("другая-встреча").await;
    let report = tokio::time::timeout(Duration::from_secs(30), job)
        .await
        .expect("расшифровка не проснулась после конца записи")
        .unwrap()
        .unwrap();
    assert_eq!(report.segments, 1);
    assert_eq!(s.status().await, MeetStatus::Ready);
}

/// ОТМЕНА встречи снимает задание — в том числе стоящее в очереди.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_meeting_takes_it_out_of_the_queue() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    // Никто не даст ей стартовать: якобы идёт запись.
    s.state.hub.mark_active("другая-встреча").await;

    let state = s.state.clone();
    let id = s.meeting_id.clone();
    let job = tokio::spawn(async move { pipeline::run::run(&state, &id).await });

    // Ждём, пока задание встанет на учёт.
    wait_for!(
        "задание пост-обработки встало на учёт",
        s.state.has_job(&s.meeting_id).await
    );

    assert!(s.state.cancel_job(&s.meeting_id).await, "гасить было нечего");
    let err = tokio::time::timeout(Duration::from_secs(10), job)
        .await
        .expect("отменённое задание не завершилось")
        .unwrap()
        .unwrap_err();
    assert!(err.message().contains("отменена"), "{}", err.message());
    assert_eq!(s.state.hub.whisper_queue_len(), 0);
}

// --- дрейф --------------------------------------------------------------------

/// Расхождение больше порога РАСТЯГИВАЕТ таймлайн под фактическое аудио.
#[tokio::test(flavor = "multi_thread")]
async fn a_drift_above_the_threshold_is_corrected() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock {
        segments: vec![MockSegment::new(0, 10_000, " Вся встреча одним куском.")],
        ..Default::default()
    });
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    // Таймлайн событий — 10 000 мс, фактическое аудио — 12 000 мс.
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 12.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    let k = report.drift_scale.expect("коррекция обязана примениться");
    assert!((k - 1.2).abs() < 1e-9, "коэффициент {k}");

    // ПОСЛЕ КОРРЕКЦИИ последняя метка таймлайна совпадает с фактической
    // длительностью аудио. Корректируются именно интервалы: сегменты whisper
    // уже живут на оси звука, и трогать их значило бы уводить расшифровку от
    // плеера.
    let spans = repo::spans::list(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(
        spans.last().unwrap().end_ms,
        12_000,
        "последняя метка обязана совпасть с фактической длительностью: {spans:?}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.meeting_dir.join("transcript.json")).unwrap())
            .unwrap();
    assert_eq!(
        json["segments"].as_array().unwrap().last().unwrap()["end_ms"],
        10_000,
        "таймкоды расшифровки обязаны остаться на оси звука"
    );

    // Факт коррекции записан и в журнал встречи, и в манифест.
    assert!(s.events_text().await.contains("drift_corrected"));
    let m = Manifest::load(&s.meeting_dir).unwrap();
    let p = m.postprocess.expect("след пост-обработки обязан быть в манифесте");
    assert!(p.drift_checked);
    assert_eq!(p.actual_duration_ms, Some(12_000));
    assert!(p.drift_scale.is_some());
}

/// Полсекунды расхождения — не повод трогать таймлайн.
#[tokio::test(flavor = "multi_thread")]
async fn a_drift_below_the_threshold_changes_nothing() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock {
        segments: vec![MockSegment::new(0, 10_000, " Вся встреча одним куском.")],
        ..Default::default()
    });
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 10.5).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert!(report.drift_scale.is_none(), "0,5 с — не дрейф");
    let spans = repo::spans::list(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(spans.last().unwrap().end_ms, 10_000, "таймлайн не тронут");

    // Сегмент на десять секунд накрывает обоих спикеров поровну, поэтому он
    // РАЗРЕЗАН по потокенным меткам — и это правильный исход, а не побочный
    // эффект: ровно ради него в команде whisper стоит `-ojf`.
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s.meeting_dir.join("transcript.json")).unwrap())
            .unwrap();
    let segs = json["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 2, "{segs:#?}");
    assert_eq!(segs[0]["speaker"], "Аня");
    assert_eq!(segs[1]["speaker"], "Боря");
    assert!(segs.iter().all(|x| x["overlapped"] == true));
    assert_eq!(segs.last().unwrap()["end_ms"], 10_000);
    let p = Manifest::load(&s.meeting_dir).unwrap().postprocess.unwrap();
    assert!(p.drift_checked && p.drift_scale.is_none());
}

// --- гейт тишины ---------------------------------------------------------------

/// ЗАПИСАЛИ ТИШИНУ — ОТКАЗЫВАЕМСЯ ЧЕСТНО, А НЕ ВЫДУМЫВАЕМ РАЗГОВОР.
///
/// Живой случай (`forge-8a2xs`): встреча на `vcc.airlabs.pro` шла 63 минуты,
/// медиа-канал не поднялся, все три дорожки оказались нулями
/// (`mean_volume = max_volume = -91,0 dB`), VAD честно дал ноль интервалов
/// речи — а whisper, которому скормили час тишины, ВЫДУМАЛ 78 реплик. Человек
/// получил `ready`, расшифровку и саммари по несуществующему разговору.
///
/// Здесь проверяется единственный правильный исход: встреча кончается `failed`
/// с кодом `no_audio_captured`, расшифровка НЕ ЗАПУСКАЕТСЯ, транскрипта на
/// диске нет.
#[tokio::test(flavor = "multi_thread")]
async fn digital_silence_fails_the_meeting_instead_of_hallucinating_a_transcript() {
    let s = Stand::new().await;
    // Мок whisper поставлен НАРОЧНО: если гейт пропустит тишину, расшифровка
    // пройдёт, и тест увидит ровно тот исход, из-за которого он написан.
    s.with_whisper(&WhisperMock {
        segments: vec![MockSegment::new(0, 10_000, " Корректор А.Кулакова")],
        ..Default::default()
    });
    s.with_ffmpeg(mocks::mock_ffmpeg_silence(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 10.0).unwrap());

    let err = pipeline::run::run(&s.state, &s.meeting_id)
        .await
        .expect_err("час цифровой тишины обязан кончиться отказом");
    assert!(
        err.to_string().contains("звук не записался"),
        "причина отказа обязана называть себя: {err}"
    );

    assert_eq!(s.status().await, MeetStatus::Failed);
    let events = s.events_text().await;
    assert!(
        events.contains(pipeline::run::NO_AUDIO_CAPTURED),
        "код отказа обязан быть в журнале встречи: {events}"
    );
    assert!(
        !s.meeting_dir.join("transcript.json").exists()
            && !s.meeting_dir.join("transcript.md").exists(),
        "расшифровка выдуманного разговора всё-таки появилась на диске"
    );
    assert!(
        repo::segments::list(&s.state, &s.meeting_id)
            .await
            .unwrap()
            .is_empty(),
        "выдуманные реплики попали в базу"
    );

    // Улика остаётся на диске: цифра в манифесте отвечает на вопрос «звук
    // вообще записался?» без ffmpeg и без часа времени.
    let p = Manifest::load(&s.meeting_dir).unwrap().postprocess.unwrap();
    assert_eq!(p.mix_peak_db, Some(-91.0));
}

/// ОБРАТНАЯ СТОРОНА ГЕЙТА: тихая, но живая встреча проходит его незамеченной.
///
/// Тест парный к предыдущему и нужен ровно затем, чтобы «падает всегда» не
/// выглядело как «работает». Средняя громкость здесь почти как у тишины, а ПИК
/// высокий: гейт смотрит на пик, иначе встреча с одной фразой за час считалась
/// бы пустой.
#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_but_real_meeting_passes_the_gate() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_with_loudness(&s.bin, "ffmpeg", -58.0, -12.0).unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 10.0).unwrap());

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    let p = Manifest::load(&s.meeting_dir).unwrap().postprocess.unwrap();
    assert_eq!(p.mix_peak_db, Some(-12.0));
}

/// БЕЗ ЗАМЕРА гейт бездействует, но молчать об этом нельзя.
///
/// Без ffmpeg сведение идёт своими силами, `volumedetect` звать нечем, и
/// проверить запись невозможно. Ронять встречу на этом основании нельзя — но и
/// пропустить молча тоже: иначе через месяц не понять, почему пустую запись
/// пропустили.
#[tokio::test(flavor = "multi_thread")]
async fn without_ffmpeg_the_silence_gate_is_skipped_and_that_is_recorded() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    // ffmpeg не ставим: сведение уйдёт на путь `mix_tracks_in_memory`.

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    assert!(s.events_text().await.contains("silence_check_skipped"));
    let p = Manifest::load(&s.meeting_dir).unwrap().postprocess.unwrap();
    assert!(p.mix_peak_db.is_none());
}

/// ПОРОГ — ПОЛЕ КОНФИГА, и значение ниже пола 16-битной сетки гейт выключает.
///
/// Аварийный выход на случай, когда запись пустая, но нужна: без него
/// единственным способом получить свой файл был бы патч бинаря.
#[tokio::test(flavor = "multi_thread")]
async fn a_threshold_below_the_noise_floor_disables_the_gate() {
    let s = Stand::build_tuned(Layout::Dom, |cfg| cfg.silence_gate_dbfs = -120.0).await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_silence(&s.bin, "ffmpeg").unwrap());

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(
        s.status().await,
        MeetStatus::Ready,
        "гейт не выключился порогом ниже пола разрядной сетки"
    );
}

/// БЕЗ FFPROBE пайплайн не ломается, а факт пропуска ВИДЕН — и в манифесте,
/// и в журнале встречи.
#[tokio::test(flavor = "multi_thread")]
async fn without_ffprobe_the_check_is_skipped_and_that_is_recorded() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    // ffprobe не ставим.

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);
    assert!(report.drift_scale.is_none());

    let p = Manifest::load(&s.meeting_dir).unwrap().postprocess.unwrap();
    assert!(!p.drift_checked, "пропуск проверки обязан быть виден в манифесте");
    assert!(p.actual_duration_ms.is_none());
    assert!(s.events_text().await.contains("drift_check_skipped"));
}

// --- повторяемость -------------------------------------------------------------

/// ПОВТОР ЭТАПА даёт тот же результат, а не удвоенный.
#[tokio::test(flavor = "multi_thread")]
async fn running_the_pipeline_twice_gives_the_same_files_and_no_duplicates() {
    let s = Stand::new().await;
    s.with_whisper(&WhisperMock {
        segments: vec![
            MockSegment::new(0, 4_000, " Раз."),
            MockSegment::new(5_200, 9_000, " Два."),
        ],
        ..Default::default()
    });
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    let first_json = std::fs::read(s.meeting_dir.join("transcript.json")).unwrap();
    let first_md = std::fs::read(s.meeting_dir.join("transcript.md")).unwrap();
    assert_eq!(s.status().await, MeetStatus::Ready);

    // ПОВТОР ЭТАПА — законный переход из `ready` (`retry?from=transcoding`).
    repo::meetings::set_status(&s.state, &s.meeting_id, MeetStatus::Transcoding, Some("retry"))
        .await
        .expect("повтор этапа обязан быть разрешён таблицей переходов");
    pipeline::run::run(&s.state, &s.meeting_id)
        .await
        .expect("повторный прогон упал");

    assert_eq!(
        std::fs::read(s.meeting_dir.join("transcript.json")).unwrap(),
        first_json,
        "повторный прогон дал другой transcript.json"
    );
    assert_eq!(
        std::fs::read(s.meeting_dir.join("transcript.md")).unwrap(),
        first_md,
        "повторный прогон дал другой transcript.md"
    );
    assert_eq!(s.status().await, MeetStatus::Ready);

    let segs = repo::segments::page(&s.state, &s.meeting_id, 0, 100).await.unwrap();
    assert_eq!(segs.len(), 2, "сегменты удвоились");
    let spans = repo::spans::list(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(spans.len(), 2, "интервалы речи удвоились");
    // Исходные дорожки на месте: повтор не имеет права съесть то, из чего сам
    // и собирается.
    assert!(s.meeting_dir.join("raw/track-0.wav").is_file());
    assert!(s.meeting_dir.join("raw/track-1.wav").is_file());
}

/// **`POST /:id/retry?from=transcribing` НА ВСТРЕЧЕ В `ready` ПЕРЕСОБИРАЕТ
/// ТРАНСКРИПТ И НЕ ТЕРЯЕТ ИСХОДНЫЕ WAV.**
///
/// Прогон идёт через НАСТОЯЩИЙ роут, а не через `run_from` напрямую: проверять
/// надо тот путь, которым ходит кнопка в карточке. Заодно видно, что архивный
/// `audio.m4a` НЕ пересобирается — он уже на диске, и минута перекодирования
/// часа звука не приблизила бы новый транскрипт ни на шаг. Проверка прямая:
/// файл помечен заведомо чужим содержимым, и после повтора оно обязано уцелеть.
#[tokio::test(flavor = "multi_thread")]
async fn retrying_the_transcription_through_the_route_keeps_the_wavs_and_the_archive() {
    use tower::ServiceExt;

    let s = Stand::new().await;
    s.with_whisper(&WhisperMock::default());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());

    pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    let archive = s.meeting_dir.join("audio.m4a");
    assert!(archive.is_file());
    std::fs::write(&archive, "МЕТКА-АРХИВА".as_bytes()).unwrap();
    let before = std::fs::read(s.meeting_dir.join("transcript.json")).unwrap();
    std::fs::remove_file(s.meeting_dir.join("transcript.json")).unwrap();

    let app = forge_meet::register_routes(axum::Router::new(), s.state.clone());
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/meet/meetings/{}/retry?from=transcribing",
                    s.meeting_id
                ))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    // Ждём, пока фоновое задание доведёт встречу обратно до `ready`.
    wait_for!("повтор дошёл до конца", s.status().await == MeetStatus::Ready);

    assert!(
        s.meeting_dir.join("transcript.json").is_file(),
        "транскрипт не пересобрался"
    );
    assert_eq!(
        std::fs::read(s.meeting_dir.join("transcript.json")).unwrap(),
        before,
        "пересобранный транскрипт отличается от исходного"
    );
    assert_eq!(
        std::fs::read(&archive).unwrap(),
        "МЕТКА-АРХИВА".as_bytes(),
        "архив пересобрали без нужды"
    );
    assert!(
        s.meeting_dir.join("raw/track-0.wav").is_file(),
        "исходные WAV потеряны — пересобрать станет не из чего"
    );
    assert!(s.meeting_dir.join("raw/track-1.wav").is_file());
    let segs = repo::segments::page(&s.state, &s.meeting_id, 0, 100).await.unwrap();
    assert_eq!(segs.len(), 1, "сегменты удвоились");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// ПЕР-ТРЕКОВАЯ РАСШИФРОВКА С НАРЕЗКОЙ ПО VAD (Ф11)
// ---------------------------------------------------------------------------
//
// ⚠ Ни одной площадки в режиме `PerParticipant` живой пробой не подтверждено:
// Телемост работает в `Slots` (измерено), а живых встреч в Meet, Teams и Zoom
// не было. Поэтому здесь — мок-встреча с честным журналом такого режима, и
// именно она доказывает, что путь работает; «проверено на живой встрече» об
// этом коде сказать нельзя, и в документации это записано прямо.

/// Мок whisper, отвечающий РАЗНЫМ текстом на разные дорожки.
///
/// Времена сегментов — В КООРДИНАТАХ ВЫРЕЗАННОГО ФАЙЛА, потому что именно его
/// получает whisper:
///
/// | дорожка | речь на общей оси | кусок с полями 300 | в файле |
/// |---|---|---|---|
/// | 0 (Аня) | 6 000–24 000 | 5 700–24 300 | 0–18 600 |
/// | 1 (Боря) | 15 000–21 000 | 14 700–21 300 | 0–6 600 |
/// | 1 (Боря) | 36 000–48 000 | 35 700–48 300 | 6 600–19 200 |
fn per_track_whisper() -> WhisperMock {
    WhisperMock {
        per_input: vec![
            (
                "track-0-speech".into(),
                vec![MockSegment::new(200, 18_000, " Я думаю, релиз надо перенести.")],
            ),
            (
                "track-1-speech".into(),
                vec![
                    MockSegment::new(100, 6_400, " Подожди."),
                    MockSegment::new(6_800, 12_500, " Хорошо, я подготовлю заметки."),
                ],
            ),
        ],
        segments: vec![MockSegment::new(0, 59_000, " Каша из микса.")],
        ..Default::default()
    }
}

/// **ГЛАВНЫЙ ТЕСТ ЗАДАЧИ.** Режим «дорожка = человек»: каждая дорожка
/// расшифровывается отдельно по вырезанной речи, перебивание сохраняется,
/// подпись спикера — факт, а не оценка.
#[tokio::test(flavor = "multi_thread")]
async fn a_per_participant_meeting_is_transcribed_track_by_track() {
    let s = Stand::per_participant(Layout::PerTrack).await;
    s.with_whisper(&per_track_whisper());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 60.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();

    assert_eq!(s.status().await, MeetStatus::Ready);
    assert_eq!(report.per_track, 2, "расшифровывали не по дорожкам");
    assert_eq!(report.segments, 3, "сегменты обеих дорожек: {report:?}");

    // Текст МИКСА в транскрипт попасть не мог: whisper на нём не запускался.
    let md = std::fs::read_to_string(s.meeting_dir.join("transcript.md")).unwrap();
    assert!(!md.contains("Каша из микса"), "запустили whisper на миксе:\n{md}");
    assert!(md.contains("релиз надо перенести"), "{md}");
    assert!(md.contains("Подожди"), "{md}");
    assert!(md.contains("Аня") && md.contains("Боря"), "имена дорожек: {md}");

    // --- ПЕРЕБИВАНИЕ РАЗОБРАНО ------------------------------------------
    let rows = repo::segments::page(&s.state, &s.meeting_id, 0, 100).await.unwrap();
    let anya = rows.iter().find(|r| r.text.contains("релиз")).unwrap();
    let borya = rows.iter().find(|r| r.text.contains("Подожди")).unwrap();
    assert_eq!(anya.speaker_name.as_deref(), Some("Аня"));
    assert_eq!(borya.speaker_name.as_deref(), Some("Боря"));
    assert!(
        borya.start_ms < anya.end_ms && borya.end_ms > anya.start_ms,
        "реплики обязаны пересекаться во времени: {anya:?} / {borya:?}"
    );
    assert!(anya.overlapped && borya.overlapped, "перебивание не отмечено");

    // Подпись — ФАКТ: дорожка и есть человек.
    for r in [anya, borya] {
        assert_eq!(r.confidence, Some(1.0), "подпись дорожки не может быть догадкой: {r:?}");
    }
    // Номер дорожки в базе заполнен — по нему слушают `?track=`.
    assert_eq!(anya.track_idx, Some(0));
    assert_eq!(borya.track_idx, Some(1));

    // --- ТАЙМКОДЫ ВЕРНУЛИСЬ НА ОБЩУЮ ОСЬ --------------------------------
    //
    // Сегмент Ани начинался на 200-й миллисекунде ВЫРЕЗАННОГО файла, кусок
    // которого начинается на 5 700-й общей оси.
    assert_eq!((anya.start_ms, anya.end_ms), (5_900, 23_700), "{anya:?}");
    // Вторая реплика Бори лежит во ВТОРОМ куске его файла: 6 800 мс файла —
    // это 35 900 мс общей оси. Не сработай перенос — она осталась бы на
    // 6 800-й, то есть посреди первой реплики.
    let notes = rows.iter().find(|r| r.text.contains("заметки")).unwrap();
    assert_eq!((notes.start_ms, notes.end_ms), (35_900, 41_600), "{notes:?}");

    // --- ЦЕНА ------------------------------------------------------------
    //
    // Обоснование задачи: «стоит примерно столько же, сколько микс». Это
    // измеримо — и измеряется, а не предполагается.
    assert_eq!(
        report.audio_sent_ms, 37_800,
        "в whisper ушло звука: {} мс при встрече 60 000 мс",
        report.audio_sent_ms
    );
    assert!(
        report.audio_sent_ms < 60_000,
        "по дорожкам ушло БОЛЬШЕ звука, чем в миксе"
    );

    // След в манифесте: по таймкодам путь неотличим, и без записи через месяц
    // не понять, факт перед тобой или оценка.
    let m = Manifest::load(&s.meeting_dir).unwrap();
    assert_eq!(m.postprocess.unwrap().per_track, 2);
}

/// **НАРЕЗКА ИДЁТ ПО ОСИ АУДИО, А НЕ ПО ОСИ СОБЫТИЙ.**
///
/// Метки VAD ставит страница по `performance.now()`, а режет нарезка ФАЙЛ,
/// который живёт на оси звука. Разошлись оси на 10 % — и, режь мы по меткам
/// событий, у каждой реплики отрезалось бы начало, а таймкоды в плеере
/// разъехались бы с текстом. Поэтому дрейф применяется к интервалам ДО
/// планирования кусков, и вот доказательство.
#[tokio::test(flavor = "multi_thread")]
async fn the_cuts_follow_the_audio_axis_after_drift_correction() {
    let s = Stand::per_participant(Layout::PerTrack).await;
    s.with_whisper(&per_track_whisper());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    // Аудио на 10 % длиннее таймлайна: коэффициент коррекции 1.1.
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 66.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(report.per_track, 2);
    assert_eq!(report.drift_scale, Some(1.1), "дрейф не посчитан");

    // Речь Ани 6 000–24 000 на оси событий это 6 600–26 400 на оси аудио; с
    // полями 300 кусок начинается на 6 300. Сегмент, начавшийся на 200-й
    // миллисекунде вырезанного файла, обязан встать на 6 500-ю общей оси. БЕЗ
    // коррекции он встал бы на 5 900-ю — тест отличает одно от другого.
    let rows = repo::segments::page(&s.state, &s.meeting_id, 0, 100).await.unwrap();
    let anya = rows.iter().find(|r| r.text.contains("релиз")).unwrap();
    assert_eq!(anya.start_ms, 6_500, "нарезали по оси событий: {anya:?}");
}

/// **РЕЖИМ SLOTS ПУТЬ НЕ ВКЛЮЧАЕТ.** Телемост отдаёт дорожки, не
/// соответствующие людям; нарежь мы их — чужая речь получила бы чужое имя, и
/// не «возможно», а с уверенностью 1.0.
#[tokio::test(flavor = "multi_thread")]
async fn slots_mode_stays_on_the_mix() {
    let s = Stand::new().await;
    s.with_whisper(&per_track_whisper());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 10.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(report.per_track, 0, "Слоты нарезали по дорожкам");
    // Текст микса на месте (он разрезан по смене спикера — это штатная работа
    // атрибуции), а пер-трековых текстов нет вовсе: их файлы не создавались.
    let md = std::fs::read_to_string(s.meeting_dir.join("transcript.md")).unwrap();
    assert!(md.contains("Каша"), "расшифровывали не микс:\n{md}");
    assert!(
        !md.contains("Подожди") && !md.contains("релиз надо перенести"),
        "в транскрипт попал текст пер-трекового мока:\n{md}"
    );
    assert!(
        !s.events_text().await.contains("per_track_skipped"),
        "штатный отказ Телемоста не должен засорять журнал встречи"
    );
}

/// **ГЕЙТ РЕЖИМА ПРОВЕРЯЕТСЯ ЗДЕСЬ, А НЕ В ТЕСТЕ ВЫШЕ.**
///
/// Журнал живого Телемоста несёт ОБА таймлайна сразу (измерено в Ф4), дорожки
/// названы каскадом — то есть пер-трековые интервалы ЕСТЬ, файлы на диске
/// ЕСТЬ, бюджет соблюдён. Единственное, что удерживает пайплайн от нарезки, —
/// `track_mode = Slots`. Выломай эту проверку, и чужая речь получила бы чужое
/// имя с уверенностью 1.0: в `Slots` дорожка не соответствует человеку.
///
/// Тест выше («голый DOM») этого не доказывает: там пер-трековых интервалов
/// нет вовсе, и он остался бы зелёным с выломанным гейтом.
#[tokio::test(flavor = "multi_thread")]
async fn telemost_with_both_timelines_still_stays_on_the_mix() {
    let s = Stand::per_participant(Layout::SlotsWithVad).await;
    s.with_whisper(&per_track_whisper());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 60.0).unwrap());

    // Предпосылки гейта: интервалы по дорожкам действительно есть, и они
    // названы — иначе тест доказывал бы не то, что заявляет.
    let timeline = pipeline::run::timeline_from_journal(
        &s.meeting_dir,
        60_000,
        &forge_meet::bot::attrib::AttribConfig::default(),
    );
    assert_eq!(timeline.binding.source, "vad", "предпосылка теста не выполнена");
    assert_eq!(timeline.tracks.len(), 2, "пер-трековых интервалов нет — гейт не проверяется");

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(report.per_track, 0, "режим Slots нарезали по дорожкам");
    assert_eq!(report.audio_sent_ms, 60_000, "в whisper ушёл не микс");
    let md = std::fs::read_to_string(s.meeting_dir.join("transcript.md")).unwrap();
    assert!(
        !md.contains("Подожди") && !md.contains("релиз надо перенести"),
        "текст пер-трекового мока в транскрипте Слотов:\n{md}"
    );
}

/// Выключатель в конфиге возвращает микс даже в режиме «дорожка = человек» —
/// на случай, когда детектор режима ошибся. Причина отказа НАЗВАНА в журнале.
#[tokio::test(flavor = "multi_thread")]
async fn the_config_switch_returns_the_pipeline_to_the_mix() {
    let mut s = Stand::per_participant(Layout::PerTrack).await;
    s.tweak_config(|c| c.pertrack_transcription = false);
    s.with_whisper(&per_track_whisper());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 60.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(report.per_track, 0);
    let events = s.events_text().await;
    assert!(events.contains("per_track_skipped"), "{events}");
    assert!(events.contains("pertrack_disabled"), "{events}");
}

/// **БЮДЖЕТ GPU — ИНВАРИАНТ, А НЕ НАДЕЖДА.** Если все говорят всю встречу,
/// суммарная речь по дорожкам кратно больше её длительности, и пер-трековая
/// расшифровка стоила бы кратно дороже. Пайплайн обязан вернуться к миксу и
/// сказать почему.
#[tokio::test(flavor = "multi_thread")]
async fn a_shouting_match_falls_back_to_the_mix() {
    let s = Stand::per_participant(Layout::PerTrackShouting).await;
    s.with_whisper(&per_track_whisper());
    s.with_ffmpeg(mocks::mock_ffmpeg_success(&s.bin, "ffmpeg").unwrap());
    s.with_ffprobe(mocks::mock_ffprobe(&s.bin, "ffprobe", 60.0).unwrap());

    let report = pipeline::run::run(&s.state, &s.meeting_id).await.unwrap();
    assert_eq!(report.per_track, 0, "бюджет не остановил нарезку");
    assert_eq!(report.audio_sent_ms, 60_000, "в whisper ушёл микс целиком");
    let events = s.events_text().await;
    assert!(events.contains("budget_exceeded"), "причина не названа: {events}");
}

// ---------------------------------------------------------------------------
// Каскад привязки имён: ИСТОЧНИК ИНТЕРВАЛОВ ВЫБИРАЕТСЯ, А НЕ СКЛАДЫВАЕТСЯ
// ---------------------------------------------------------------------------

/// Пишет журнал встречи из готовых строк событий.
fn journal(dir: &Path, events: &[(u64, &str, serde_json::Value)]) {
    let mut j = forge_meet::ingest::journal::EventJournal::open(dir).unwrap();
    for (at, kind, data) in events {
        j.note(*at, kind, data.clone()).unwrap();
    }
}

/// Живая встреча кладёт в журнал ОБА таймлайна сразу — пер-трековый VAD и
/// DOM-индикатор. Сложи их, и каждая реплика попала бы в расшифровку дважды:
/// раз под «Дорожкой 0», раз под именем человека.
#[test]
fn a_named_track_wins_and_the_dom_timeline_is_not_counted_twice() {
    let dir = tempfile::tempdir().unwrap();
    journal(
        dir.path(),
        &[
            (0, "track_added", serde_json::json!({"t": "A", "idx": 0, "off": 0})),
            (0, "track_added", serde_json::json!({"t": "B", "idx": 1, "off": 0})),
            (500, "speech_start", serde_json::json!({"t": "A", "at": 500})),
            (2_000, "speech_end", serde_json::json!({"t": "A", "at": 2_000})),
            (2_500, "speech_start", serde_json::json!({"t": "B", "at": 2_500})),
            (4_000, "speech_end", serde_json::json!({"t": "B", "at": 4_000})),
            (520, "active_speaker", serde_json::json!({"name": "Алиса", "at_ms": 520})),
            (2_050, "active_speaker", serde_json::json!({"name": null, "at_ms": 1_980})),
            (2_520, "active_speaker", serde_json::json!({"name": "Борис", "at_ms": 2_520})),
            (4_000, "active_speaker", serde_json::json!({"name": null, "at_ms": 3_980})),
        ],
    );

    let timeline = pipeline::run::timeline_from_journal(
        dir.path(),
        4_000,
        &forge_meet::bot::attrib::AttribConfig::default(),
    );
    assert_eq!(timeline.binding.stage, "correlation");
    assert_eq!(timeline.binding.source, "vad", "точнее пер-трековый детект");
    assert_eq!(timeline.binding.named_tracks, 2);

    let names: Vec<&str> = timeline.intervals.iter().map(|i| i.speaker.as_str()).collect();
    assert_eq!(names, vec!["Алиса", "Борис"], "интервалы: {:?}", timeline.intervals);
    assert_eq!(
        timeline.intervals.len(),
        2,
        "реплика посчитана дважды: {:?}",
        timeline.intervals
    );
}

/// РЕЖИМ SLOTS (Телемост): дорожка не соответствует человеку, каскад никого не
/// называет — и источником становится DOM. Иначе расшифровка осталась бы без
/// спикеров вовсе.
#[test]
fn without_named_tracks_the_dom_timeline_becomes_the_source() {
    let dir = tempfile::tempdir().unwrap();
    journal(
        dir.path(),
        &[
            (0, "track_added", serde_json::json!({"t": "mix", "idx": 0, "off": 0})),
            (0, "speech_start", serde_json::json!({"t": "mix", "at": 0})),
            (9_000, "speech_end", serde_json::json!({"t": "mix", "at": 9_000})),
            (0, "active_speaker", serde_json::json!({"name": "Максим К.", "at_ms": 0})),
            (3_000, "active_speaker", serde_json::json!({"name": "Игорь Г.", "at_ms": 3_000})),
            (6_000, "active_speaker", serde_json::json!({"name": "Алексей П.", "at_ms": 6_000})),
            (9_000, "active_speaker", serde_json::json!({"name": null, "at_ms": 9_000})),
        ],
    );

    let timeline = pipeline::run::timeline_from_journal(
        dir.path(),
        9_000,
        &forge_meet::bot::attrib::AttribConfig::default(),
    );
    assert_eq!(timeline.binding.named_tracks, 0, "в режиме Slots имён из дорожек не бывает");
    assert_eq!(timeline.binding.source, "dom");
    assert_eq!(timeline.binding.stage, "fallback");
    let names: Vec<&str> = timeline.intervals.iter().map(|i| i.speaker.as_str()).collect();
    assert_eq!(names, vec!["Максим К.", "Игорь Г.", "Алексей П."]);
}

/// Журнал без единого DOM-события (сценарный бот, протухшие пробы) — интервалы
/// всё равно есть, просто безымянные. Лучше так, чем никаких.
#[test]
fn a_journal_without_any_dom_events_still_yields_intervals() {
    let dir = tempfile::tempdir().unwrap();
    journal(
        dir.path(),
        &[
            (0, "track_added", serde_json::json!({"t": "A", "idx": 0, "off": 0})),
            (100, "speech_start", serde_json::json!({"t": "A", "at": 100})),
            (900, "speech_end", serde_json::json!({"t": "A", "at": 900})),
        ],
    );
    let timeline = pipeline::run::timeline_from_journal(
        dir.path(),
        1_000,
        &forge_meet::bot::attrib::AttribConfig::default(),
    );
    assert_eq!(timeline.binding.source, "vad");
    assert_eq!(timeline.binding.named_tracks, 0);
    assert_eq!(timeline.intervals.len(), 1);
    assert_eq!(timeline.intervals[0].speaker, "Дорожка 0");
}
