//! Ретенция каталога записей — на НАСТОЯЩЕЙ базе и НАСТОЯЩИХ файлах.
//!
//! Здесь доказывается не «функция что-то удалила», а четыре правила, каждое из
//! которых стоит потерянных записей:
//!
//! 1. при превышении бюджета уходят САМЫЕ СТАРЫЕ, свежие остаются;
//! 2. `retention_min_keep` последних не удаляются даже при переполненном
//!    бюджете;
//! 3. идущая запись не удаляется НИКОГДА;
//! 4. после вычистки бейдж показывает «файлы вычищены», а не «можно записать»
//!    — иначе кнопка на старом сообщении предложила бы войти ботом в давно
//!    прошедшую встречу.
//!
//! # Герметичность
//!
//! `cache_dir` и база — во временном каталоге. Без этого прогон подметал бы
//! `~/.forge/meet`, то есть записи реальных встреч пользователя. Это самый
//! опасный тест плагина: он УДАЛЯЕТ каталоги.

use std::path::PathBuf;
use std::sync::Arc;

use forge_meet::db::repo;
use forge_meet::db::Db;
use forge_meet::retention::{self, SweepStats};
use forge_meet::state::MeetState;
use forge_meet::status::MeetStatus;
use forge_meet::MeetConfig;

/// Стенд: база в памяти, каталог записей во временной папке.
struct Stand {
    state: Arc<MeetState>,
    cache: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Stand {
    async fn new(f: impl FnOnce(&mut MeetConfig)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = forge_meet::testing::hermetic_config(tmp.path());
        f(&mut cfg);
        let cache = cfg.cache_dir.clone();
        std::fs::create_dir_all(&cache).unwrap();
        let state = MeetState::new(cfg);
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();
        Stand {
            state,
            cache,
            _tmp: tmp,
        }
    }

    /// Заводит встречу, каталог с файлами заданного веса и доводит её до
    /// статуса.
    async fn meeting(&self, room: &str, bytes: usize, status: MeetStatus) -> String {
        self.meeting_with_source(room, bytes, status, None).await
    }

    async fn meeting_with_source(
        &self,
        room: &str,
        bytes: usize,
        status: MeetStatus,
        source: Option<repo::SourceKey>,
    ) -> String {
        // Комната Телемоста — только цифры: `paths: [r"/j/(\d{4,32})"]`.
        // Буквенное имя `parse_meeting_url` законно отвергает, и тест на этом
        // падал бы с невнятным `unwrap on None`.
        let link = forge_meet::parse_meeting_url(&format!("https://telemost.yandex.ru/j/{room}"))
            .unwrap_or_else(|| panic!("комната {room:?} не разбирается как ссылка Телемоста"));
        let out = repo::meetings::create_or_attach(
            &self.state,
            repo::meetings::NewMeeting {
                link,
                source,
                title: None,
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: String::new(),
            },
        )
        .await
        .unwrap();
        let id = out.meeting.id.clone();

        let dir = self.cache.join(&id);
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(dir.join("raw/track-0.wav"), vec![7u8; bytes]).unwrap();
        std::fs::write(dir.join("transcript.md"), "# Расшифровка\n").unwrap();
        repo::meetings::set_meeting_dir(&self.state, &id, &dir.display().to_string())
            .await
            .unwrap();

        for st in [
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
            MeetStatus::Transcribing,
            MeetStatus::Ready,
        ] {
            if st == status {
                repo::meetings::set_status(&self.state, &id, st, Some("test"))
                    .await
                    .unwrap();
                break;
            }
            repo::meetings::set_status(&self.state, &id, st, Some("test"))
                .await
                .unwrap();
        }
        id
    }

    /// Двигает встречу в прошлое — И В БАЗЕ, И НА ДИСКЕ.
    ///
    /// Правка идёт мимо `set_status` намеренно и только здесь: это подмена
    /// ЧАСОВ, а не смена статуса, и ждать девяносто дней тест не может.
    ///
    /// ⚠ Одной базы НЕДОСТАТОЧНО. Время последнего обращения считается как
    /// `max(updated_at, mtime каталога)`, и у только что созданного каталога
    /// mtime — «сейчас». Состарь мы только строку — все записи оказались бы
    /// ровесниками, порядок LRU выродился бы в произвольный, и тест «уходят
    /// самые старые» зеленел бы через раз, ничего не проверяя.
    async fn age(&self, id: &str, days: i64) {
        let db = self.state.db().unwrap();
        let dir = self.dir(id);
        let id = id.to_string();
        let ts = chrono::Utc::now().timestamp() - days * 86_400;
        db.conn()
            .call(move |c| {
                c.execute(
                    "UPDATE meetings SET created_at = ?2, updated_at = ?2, ended_at = ?2 \
                     WHERE id = ?1",
                    rusqlite::params![id, ts],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts.max(0) as u64);
        let times = std::fs::FileTimes::new().set_accessed(when).set_modified(when);
        let f = std::fs::File::open(&dir).expect("каталог встречи обязан существовать");
        f.set_times(times).expect("mtime каталога не переставился");
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.cache.join(id)
    }

    async fn status(&self, id: &str) -> MeetStatus {
        repo::meetings::get(&self.state, id).await.unwrap().status
    }

    async fn sweep(&self) -> SweepStats {
        retention::sweep_once(&self.state).await
    }
}

/// ПРАВИЛО 1: при превышении бюджета уходят самые старые.
#[tokio::test]
async fn the_budget_removes_the_oldest_and_keeps_the_freshest() {
    let s = Stand::new(|c| {
        // Четыре записи по ~10 КБ. Потолок 25 000, цель с гистерезисом —
        // 22 500: чтобы опуститься до неё, обязаны уйти РОВНО ДВЕ старшие.
        c.max_cache_bytes = 25_000;
        c.retention_min_keep = 0;
        c.retention_days = 0;
    })
    .await;

    let mut ids = Vec::new();
    for i in 0..4 {
        let id = s.meeting(&format!("1000{i}0000"), 10_000, MeetStatus::Ready).await;
        // Возраст задаёт порядок LRU: нулевая — самая старая.
        s.age(&id, (4 - i) as i64).await;
        ids.push(id);
    }

    let stats = s.sweep().await;
    assert_eq!(stats.scanned, 4);
    assert!(stats.total_after <= retention::sweep_target(25_000), "{stats:?}");
    assert_eq!(
        stats.pruned, 2,
        "подчистка обязана остановиться на цели, а не вымести всё: {stats:?}"
    );

    // Старые ушли, свежие остались — именно в этом порядке.
    for id in &ids[..2] {
        assert!(!s.dir(id).exists(), "самые старые обязаны уйти первыми");
        assert_eq!(s.status(id).await, MeetStatus::Pruned);
    }
    for id in &ids[2..] {
        assert!(s.dir(id).exists(), "свежая запись удалена");
        assert_eq!(s.status(id).await, MeetStatus::Ready);
    }

    // ГИСТЕРЕЗИС: повторный прогон уже ничего не трогает.
    let again = s.sweep().await;
    assert_eq!(again.pruned, 0, "подчистка обязана быть устойчивой: {again:?}");
}

/// ПРАВИЛО 2: последние N не удаляются даже при переполненном бюджете.
///
/// Настройка `max_cache_bytes`, выставленная слишком низко, иначе стирала бы
/// вчерашнюю планёрку — ровно то, ради чего фича и работает.
#[tokio::test]
async fn the_last_few_recordings_survive_any_budget() {
    let s = Stand::new(|c| {
        // Бюджет заведомо меньше одной записи.
        c.max_cache_bytes = 1_000;
        c.retention_min_keep = 3;
        c.retention_days = 0;
    })
    .await;

    let mut ids = Vec::new();
    for i in 0..5 {
        let id = s.meeting(&format!("1000{i}0000"), 10_000, MeetStatus::Ready).await;
        s.age(&id, (5 - i) as i64).await;
        ids.push(id);
    }

    let stats = s.sweep().await;
    assert_eq!(stats.pruned, 2, "уйти могли только две самые старые: {stats:?}");
    assert!(stats.protected >= 3, "{stats:?}");
    assert!(
        stats.total_after > retention::sweep_target(1_000),
        "бюджет НЕ достигнут — и это правильно: защита сильнее бюджета"
    );

    for id in &ids[2..] {
        assert!(s.dir(id).exists(), "запись из последних трёх удалена");
        assert_eq!(s.status(id).await, MeetStatus::Ready);
    }
    for id in &ids[..2] {
        assert!(!s.dir(id).exists());
    }
}

/// ПРАВИЛО 3: активная запись не удаляется никогда — ни по возрасту, ни по
/// бюджету. В её каталог прямо сейчас пишется звук.
#[tokio::test]
async fn an_active_recording_is_never_touched() {
    let s = Stand::new(|c| {
        c.max_cache_bytes = 1;
        c.retention_min_keep = 0;
        c.retention_days = 1;
    })
    .await;

    let recording = s.meeting("70000001", 50_000, MeetStatus::Recording).await;
    let processing = s.meeting("70000002", 50_000, MeetStatus::Transcribing).await;
    let done = s.meeting("70000003", 50_000, MeetStatus::Ready).await;
    // Все три «старые» — по возрасту под удаление подходят все.
    for id in [&recording, &processing, &done] {
        s.age(id, 400).await;
    }

    let stats = s.sweep().await;
    assert!(s.dir(&recording).exists(), "удалён каталог ИДУЩЕЙ записи");
    assert!(s.dir(&processing).exists(), "удалён каталог, который читает пайплайн");
    assert!(!s.dir(&done).exists(), "законченная и просроченная обязана уйти");
    assert_eq!(s.status(&recording).await, MeetStatus::Recording);
    assert_eq!(s.status(&processing).await, MeetStatus::Transcribing);
    assert_eq!(stats.pruned, 1, "{stats:?}");

    // И идущее задание пост-обработки защищает даже терминальную встречу.
    let retried = s.meeting("70000004", 50_000, MeetStatus::Ready).await;
    s.age(&retried, 400).await;
    let _token = s.state.register_job(&retried).await;
    let stats = s.sweep().await;
    assert!(
        s.dir(&retried).exists(),
        "удалён каталог встречи, по которой идёт повтор этапа: {stats:?}"
    );
}

/// ПРАВИЛО 4: после вычистки лента показывает «файлы вычищены», а НЕ «можно
/// записать».
///
/// Отсутствие бейджа во фронте означает «записать эту встречу»; исчезни строка
/// — и кнопка на сообщении годичной давности предложила бы войти ботом в
/// встречу, которая кончилась год назад.
#[tokio::test]
async fn a_pruned_meeting_still_shows_a_badge() {
    let s = Stand::new(|c| {
        c.retention_days = 30;
        c.retention_min_keep = 0;
        c.max_cache_bytes = 0;
    })
    .await;

    let key = repo::SourceKey::new("telegram", "7", 120);
    let id = s
        .meeting_with_source("70000009", 10_000, MeetStatus::Ready, Some(key.clone()))
        .await;
    s.age(&id, 100).await;

    // До подчистки — «готово».
    let badge = s.state.badge_of(&key).await.expect("бейдж обязан быть");
    assert_eq!(badge.status, MeetStatus::Ready);

    let stats = s.sweep().await;
    assert_eq!(stats.pruned, 1, "{stats:?}");

    let badge = s
        .state
        .badge_of(&key)
        .await
        .expect("бейдж ОБЯЗАН остаться: иначе кнопка предложит записать прошлогоднюю встречу");
    assert_eq!(badge.status, MeetStatus::Pruned);
    assert_eq!(badge.meeting_id, id);

    // Строка в базе цела, пути стёрты — карточка не покажет путь к файлу,
    // которого нет.
    let row = repo::meetings::get(&s.state, &id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Pruned);
    assert_eq!(row.audio_path, None);
    assert_eq!(row.transcript_path, None);
    assert_eq!(row.status_reason.as_deref(), Some(retention::REASON_AGE));

    // Журнал переходов объясняет, почему файлов нет.
    let events = repo::events::list(&s.state, &id).await.unwrap();
    assert!(
        events.iter().any(|e| e.to_status == "pruned"),
        "переход в pruned обязан быть в журнале: {events:?}"
    );
}

/// Срок хранения `0` означает «хранить вечно», и никакой возраст этого не
/// отменяет.
#[tokio::test]
async fn retention_days_zero_keeps_everything_forever() {
    let s = Stand::new(|c| {
        c.retention_days = 0;
        c.max_cache_bytes = 0;
        c.retention_min_keep = 0;
    })
    .await;
    let id = s.meeting("70000005", 10_000, MeetStatus::Ready).await;
    s.age(&id, 10_000).await;

    let stats = s.sweep().await;
    assert_eq!(stats.pruned, 0, "{stats:?}");
    assert!(s.dir(&id).exists());
}

/// ТРАНЗИТНЫЕ КАТАЛОГИ: `work/` уходит сразу, профиль Chrome после падения
/// живёт сутки.
#[tokio::test]
async fn transient_directories_are_cleaned_by_their_own_rules() {
    let s = Stand::new(|c| {
        c.retention_days = 0;
        c.max_cache_bytes = 0;
        c.retention_min_keep = 100;
    })
    .await;

    let ok = s.meeting("70000006", 1_000, MeetStatus::Ready).await;
    let broken = s.meeting("70000007", 1_000, MeetStatus::Transcoding).await;
    repo::meetings::set_status(&s.state, &broken, MeetStatus::Failed, Some("whisper"))
        .await
        .unwrap();

    for id in [&ok, &broken] {
        std::fs::create_dir_all(s.dir(id).join("work")).unwrap();
        std::fs::write(s.dir(id).join("work/mix.wav"), vec![0u8; 5_000]).unwrap();
        std::fs::create_dir_all(s.dir(id).join("chrome-profile")).unwrap();
        std::fs::write(s.dir(id).join("chrome-profile/log"), vec![0u8; 500]).unwrap();
    }

    let stats = s.sweep().await;
    assert_eq!(stats.pruned, 0, "сами записи трогать было нельзя");
    assert_eq!(stats.work_cleaned, 2, "work/ уходит у обеих: {stats:?}");
    assert!(!s.dir(&ok).join("work").exists());
    assert!(!s.dir(&broken).join("work").exists());

    // Профиль чистого завершения удалён, профиль падения — оставлен для
    // разбора.
    assert!(!s.dir(&ok).join("chrome-profile").exists());
    assert!(
        s.dir(&broken).join("chrome-profile").exists(),
        "профиль упавшей записи — единственный след того, почему бот не вошёл"
    );
    assert_eq!(stats.profiles_cleaned, 1, "{stats:?}");

    // Через сутки уходит и он.
    s.age(&broken, 2).await;
    let stats = s.sweep().await;
    assert!(!s.dir(&broken).join("chrome-profile").exists());
    assert_eq!(stats.profiles_cleaned, 1, "{stats:?}");
}

/// ЧУЖИЕ КАТАЛОГИ НЕ ТРОГАЮТСЯ.
///
/// Подметальщик обходит строки базы, а не содержимое `~/.forge/meet`: там
/// лежат `models/` с гигабайтами весов и `meet.db`. Обход каталога однажды
/// удалил бы их, и объяснять пропажу моделей было бы уже некому.
#[tokio::test]
async fn directories_without_a_meeting_row_are_left_alone() {
    let s = Stand::new(|c| {
        c.max_cache_bytes = 1;
        c.retention_min_keep = 0;
        c.retention_days = 1;
    })
    .await;

    std::fs::create_dir_all(s.cache.join("models")).unwrap();
    std::fs::write(s.cache.join("models/ggml-large.bin"), vec![0u8; 100_000]).unwrap();
    std::fs::create_dir_all(s.cache.join("pending")).unwrap();
    std::fs::write(s.cache.join("meet.db"), vec![0u8; 1_000]).unwrap();

    let id = s.meeting("70000008", 50_000, MeetStatus::Ready).await;
    s.age(&id, 400).await;

    let stats = s.sweep().await;
    assert_eq!(stats.pruned, 1);
    assert!(!s.dir(&id).exists());
    assert!(
        s.cache.join("models/ggml-large.bin").exists(),
        "подметальщик тронул каталог моделей"
    );
    assert!(s.cache.join("pending").exists());
    assert!(s.cache.join("meet.db").exists());
    assert_eq!(
        stats.scanned, 1,
        "в обход попали чужие каталоги: {stats:?}"
    );
}

/// Без базы подметальщик НИЧЕГО НЕ ДЕЛАЕТ.
///
/// База открывается в `spawn_workers`, и в окно между `init` и ним обход по
/// каталогу означал бы удаление записей, о которых мы ничего не знаем.
#[tokio::test]
async fn without_a_database_nothing_is_swept() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = forge_meet::testing::hermetic_config(tmp.path());
    cfg.max_cache_bytes = 1;
    cfg.retention_days = 1;
    cfg.retention_min_keep = 0;
    let cache = cfg.cache_dir.clone();
    std::fs::create_dir_all(cache.join("m1")).unwrap();
    std::fs::write(cache.join("m1/audio.m4a"), vec![0u8; 100_000]).unwrap();

    let state = MeetState::new(cfg);
    assert!(state.db().is_none());
    let stats = retention::sweep_once(&state).await;
    assert_eq!(stats, SweepStats::default(), "{stats:?}");
    assert!(cache.join("m1/audio.m4a").exists());
}
