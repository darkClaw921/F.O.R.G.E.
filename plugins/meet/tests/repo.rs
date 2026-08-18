//! Интеграционные тесты хранилища: репозитории, тёплый индекс бейджей и
//! восстановление после перезапуска.
//!
//! Отличие от юнит-тестов внутри `src/**`: здесь работает НАСТОЯЩАЯ база с
//! миграциями и настоящее состояние плагина, поэтому проверяются свойства,
//! которых у чистых функций нет — сходимость индекса с базой, частичный
//! уникальный индекс, каскады и журнал переходов.
//!
//! # Герметичность
//!
//! Каждый тест держит `cache_dir`, `db_path` и `models_dir` во временном
//! каталоге. Без этого прогон открывал бы живую базу пользователя, а
//! `reconcile_on_boot` прошёлся бы по его записям и перевёл идущие прямо
//! сейчас встречи в `failed`.

use std::sync::Arc;

use forge_meet::config::MeetConfig;
use forge_meet::db::repo::meetings::{self, NewMeeting};
use forge_meet::db::repo::{events, participants, segments, sources, spans, SourceKey};
use forge_meet::db::Db;
use forge_meet::error::MeetError;
use forge_meet::providers::parse_meeting_url;
use forge_meet::state::MeetState;
use forge_meet::status::{MeetStatus, SummaryStatus};

/// Стенд: состояние плагина с открытой базой в памяти.
struct Stand {
    state: Arc<MeetState>,
    _dir: tempfile::TempDir,
}

async fn stand() -> Stand {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MeetConfig {
        config_path: dir.path().join("meet.toml"),
        cache_dir: dir.path().join("cache"),
        db_path: dir.path().join("cache/meet.db"),
        models_dir: dir.path().join("cache/models"),
        ..MeetConfig::default()
    };
    let state = MeetState::new(cfg);
    let db = Db::open_memory().await.unwrap();
    db.migrate().await.unwrap();
    state.attach_db(Arc::new(db)).unwrap();
    Stand { state, _dir: dir }
}

fn new_meeting(url: &str, key: Option<SourceKey>) -> NewMeeting {
    let link = parse_meeting_url(url).expect("ссылка обязана разбираться");
    NewMeeting {
        link,
        source: key,
        title: None,
        mode: "normal".into(),
        bot_name: "F.O.R.G.E.".into(),
        meeting_dir: "/tmp/meet-test".into(),
    }
}

/// Детерминированный генератор: без него порядок случайных переходов
/// отличался бы от прогона к прогону, и упавший тест невозможно было бы
/// повторить.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

// ---------------------------------------------------------------------------
// Дедуп и создание
// ---------------------------------------------------------------------------

/// Основа `attached: true`: две попытки старта на одну комнату дают РОВНО
/// ОДНУ строку, вторая получает первую.
#[tokio::test]
async fn two_starts_on_one_room_produce_exactly_one_meeting() {
    let s = stand().await;
    let url = "https://telemost.yandex.ru/j/60497752704486";

    let first = meetings::create_or_attach(
        &s.state,
        new_meeting(url, Some(SourceKey::new("telegram", "7", 100))),
    )
    .await
    .unwrap();
    assert!(!first.attached, "первая попытка создаёт запись");

    // Вторая попытка — из ДРУГОГО чата, ровно как в жизни: одну ссылку
    // кинули в двух местах.
    let second = meetings::create_or_attach(
        &s.state,
        new_meeting(
            "https://telemost.yandex.ru/j/60497752704486?utm_source=tg",
            Some(SourceKey::new("vk", "42", 900)),
        ),
    )
    .await
    .unwrap();
    assert!(second.attached, "вторая попытка обязана привязаться, а не создать");
    assert_eq!(second.meeting.id, first.meeting.id);

    let all = meetings::list_by_status(&s.state, &MeetStatus::ALL).await.unwrap();
    assert_eq!(all.len(), 1, "в базе обязана быть ровно одна встреча: {all:?}");

    // И ОБЕ кнопки показывают одну запись.
    for key in [
        SourceKey::new("telegram", "7", 100),
        SourceKey::new("vk", "42", 900),
    ] {
        let badge = s.state.badge_of(&key).await.expect("бейдж обязан быть");
        assert_eq!(badge.meeting_id, first.meeting.id);
    }
}

/// Завершённая встреча не блокирует новую запись той же комнаты: индекс
/// частичный именно ради этого.
#[tokio::test]
async fn a_finished_meeting_does_not_block_a_new_recording_of_the_same_room() {
    let s = stand().await;
    let url = "https://telemost.yandex.ru/j/1";

    let first = meetings::create_or_attach(&s.state, new_meeting(url, None)).await.unwrap();
    meetings::set_status(&s.state, &first.meeting.id, MeetStatus::Cancelled, Some("тест"))
        .await
        .unwrap();

    let second = meetings::create_or_attach(&s.state, new_meeting(url, None)).await.unwrap();
    assert!(!second.attached, "после завершения первой создаётся новая запись");
    assert_ne!(second.meeting.id, first.meeting.id);
}

// ---------------------------------------------------------------------------
// set_status — единственная точка мутации
// ---------------------------------------------------------------------------

/// Каждый переход обязан оставить строку в журнале: через сутки это
/// единственный способ ответить, почему запись умерла.
#[tokio::test]
async fn every_transition_writes_a_journal_row() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/2", None))
        .await
        .unwrap()
        .meeting;

    // Создание уже дало первую строку.
    assert_eq!(events::count(&s.state, &m.id).await.unwrap(), 1);

    let path = [
        MeetStatus::Launching,
        MeetStatus::Joining,
        MeetStatus::Recording,
        MeetStatus::Leaving,
        MeetStatus::Transcoding,
        MeetStatus::Transcribing,
        MeetStatus::Summarizing,
        MeetStatus::Ready,
    ];
    for (i, st) in path.iter().enumerate() {
        meetings::set_status(&s.state, &m.id, *st, Some("шаг")).await.unwrap();
        assert_eq!(
            events::count(&s.state, &m.id).await.unwrap(),
            i + 2,
            "переход в {} не записался в журнал",
            st.as_str()
        );
    }

    let log = events::list(&s.state, &m.id).await.unwrap();
    assert_eq!(log[0].from_status, None, "у создания from_status пуст");
    assert_eq!(log[0].to_status, "queued");
    assert_eq!(log[1].from_status.as_deref(), Some("queued"));
    assert_eq!(log[1].to_status, "launching");
    assert_eq!(log.last().unwrap().to_status, "ready");
}

/// Запрещённый переход ничего не меняет — ни строку, ни журнал, ни индекс.
#[tokio::test]
async fn an_illegal_transition_changes_nothing() {
    let s = stand().await;
    let key = SourceKey::new("telegram", "7", 5);
    let m = meetings::create_or_attach(
        &s.state,
        new_meeting("https://telemost.yandex.ru/j/3", Some(key.clone())),
    )
    .await
    .unwrap()
    .meeting;

    let before = events::count(&s.state, &m.id).await.unwrap();
    let err = meetings::set_status(&s.state, &m.id, MeetStatus::Ready, None)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        MeetError::InvalidState {
            from: "queued",
            to: "ready"
        }
    );

    assert_eq!(events::count(&s.state, &m.id).await.unwrap(), before);
    assert_eq!(
        meetings::get(&s.state, &m.id).await.unwrap().status,
        MeetStatus::Queued
    );
    assert_eq!(s.state.badge_of(&key).await.unwrap().status, MeetStatus::Queued);
}

#[tokio::test]
async fn a_missing_meeting_is_not_found() {
    let s = stand().await;
    let err = meetings::set_status(&s.state, "нет-такой", MeetStatus::Launching, None)
        .await
        .unwrap_err();
    assert_eq!(err, MeetError::NotFound);
    assert_eq!(meetings::get(&s.state, "нет-такой").await.unwrap_err(), MeetError::NotFound);
}

/// Переход в `recording` проставляет `started_at` — от него фронт считает
/// таймер, и инкремент здесь не годится: перезагрузка вкладки обнулила бы
/// цифру.
#[tokio::test]
async fn entering_recording_stamps_the_start_time() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/4", None))
        .await
        .unwrap()
        .meeting;
    assert!(m.started_at.is_none());

    meetings::set_status(&s.state, &m.id, MeetStatus::Launching, None).await.unwrap();
    meetings::set_status(&s.state, &m.id, MeetStatus::Joining, None).await.unwrap();
    let rec = meetings::set_status(&s.state, &m.id, MeetStatus::Recording, None)
        .await
        .unwrap();
    assert!(rec.started_at.is_some(), "started_at обязан появиться");
    assert!(rec.ended_at.is_none());

    let left = meetings::set_status(&s.state, &m.id, MeetStatus::Leaving, None)
        .await
        .unwrap();
    assert!(left.ended_at.is_some(), "выход из записи проставляет ended_at");
    assert_eq!(left.started_at, rec.started_at, "старт не переписывается");
}

/// ПРАВИЛО ПЛАНА: провал саммари даёт `ready` + `summary_status = failed`, а
/// НЕ `failed`. Час расшифровки не имеет права пропасть из-за неответившего
/// саммаризатора.
#[tokio::test]
async fn a_failed_summary_keeps_the_transcript() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/5", None))
        .await
        .unwrap()
        .meeting;
    for st in [
        MeetStatus::Launching,
        MeetStatus::Joining,
        MeetStatus::Recording,
        MeetStatus::Leaving,
        MeetStatus::Transcoding,
        MeetStatus::Transcribing,
        MeetStatus::Summarizing,
    ] {
        meetings::set_status(&s.state, &m.id, st, None).await.unwrap();
    }

    let row = meetings::finish_summary(&s.state, &m.id, false).await.unwrap();
    assert_eq!(row.status, MeetStatus::Ready, "встреча обязана дойти до ready");
    assert_eq!(row.summary_status, SummaryStatus::Failed);
    assert_eq!(row.status_reason.as_deref(), Some("summary_failed"));
}

// ---------------------------------------------------------------------------
// Тёплый индекс
// ---------------------------------------------------------------------------

/// ГЛАВНЫЙ инвариант кэша: после двухсот случайных переходов индекс совпадает
/// с базой. Разъезд возможен только если кто-то мутирует статус мимо
/// `set_status`, и именно это здесь и ловится.
#[tokio::test]
async fn the_badge_index_converges_with_the_database_after_200_random_transitions() {
    let s = stand().await;
    let mut rng = Rng(0x5EED_1234_ABCD_0001);

    // Счётчик номеров сообщений и комнат: каждая новая встреча получает свои.
    let mut next = 0i64;
    // Все пары «ключ источника → встреча», накопленные за прогон.
    let mut pairs: Vec<(SourceKey, String)> = Vec::new();
    // Встречи, которые ещё могут двигаться.
    let mut live: Vec<String> = Vec::new();

    // Заводит встречу с двумя источниками в разных чатах: одну ссылку кидают
    // в двух местах, и обе кнопки обязаны показывать одну запись.
    macro_rules! spawn_meeting {
        () => {{
            let i = next;
            next += 1;
            let out = meetings::create_or_attach(
                &s.state,
                new_meeting(
                    &format!("https://telemost.yandex.ru/j/{}", 100_000 + i),
                    Some(SourceKey::new("telegram", "7", i)),
                ),
            )
            .await
            .unwrap();
            sources::attach(&s.state, &SourceKey::new("vk", "42", i), &out.meeting.id)
                .await
                .unwrap();
            pairs.push((SourceKey::new("telegram", "7", i), out.meeting.id.clone()));
            pairs.push((SourceKey::new("vk", "42", i), out.meeting.id.clone()));
            live.push(out.meeting.id.clone());
        }};
    }

    for _ in 0..5 {
        spawn_meeting!();
    }

    // Ровно 200 ПРИМЕНЁННЫХ переходов. Чисто случайная цель давала бы их
    // около полусотни на две сотни попыток (из двенадцати статусов законны
    // два-три), и «двести переходов» превратились бы в лозунг.
    let mut applied = 0usize;
    let mut rejected = 0usize;
    let mut attempts = 0usize;
    while applied < 200 {
        attempts += 1;
        assert!(attempts < 10_000, "прогон не сходится: {applied} применено");
        if live.is_empty() {
            spawn_meeting!();
        }
        let idx = (rng.next() % live.len() as u64) as usize;
        let id = live[idx].clone();
        let current = meetings::get(&s.state, &id).await.unwrap().status;
        let legal: Vec<MeetStatus> = MeetStatus::ALL
            .into_iter()
            .filter(|t| forge_meet::status::can_transition(current, *t))
            .collect();
        // Каждый пятый шаг — заведомо случайная цель: ОТКАЗ обязан быть таким
        // же проверенным путём, как и успех, иначе тест не заметил бы, что
        // отвергнутый переход всё-таки тронул индекс.
        let target = if legal.is_empty() || rng.next() % 5 == 0 {
            *rng.pick(&MeetStatus::ALL)
        } else {
            *rng.pick(&legal)
        };

        match meetings::set_status(&s.state, &id, target, Some("случайный переход")).await {
            Ok(_) => {
                applied += 1;
                // Дошедшая до терминала встреча выбывает, вместо неё заводится
                // новая: иначе двигать стало бы нечего.
                if target.is_terminal() {
                    live.remove(idx);
                    spawn_meeting!();
                }
            }
            Err(_) => rejected += 1,
        }
    }
    assert_eq!(applied, 200);
    assert!(rejected > 0, "ни одного отказа — случайность выродилась");
    assert!(pairs.len() >= 20, "встреч завелось слишком мало: {}", pairs.len() / 2);

    // Сверка: то, что в индексе, обязано совпасть с тем, что в базе.
    for (key, id) in &pairs {
        let row = meetings::get(&s.state, id).await.unwrap();
        let badge = s.state.badge_of(key).await.expect("бейдж обязан быть");
        assert_eq!(badge.meeting_id, row.id);
        assert_eq!(badge.status, row.status, "индекс разъехался с базой по {}", row.id);
        assert_eq!(badge.summary_status, row.summary_status);
        assert_eq!(badge.started_at, row.started_at);
        assert_eq!(badge.ended_at, row.ended_at);
        assert_eq!(badge.duration_ms, row.duration_ms);
    }

    // И прогрев с нуля даёт то же самое: база — источник истины, индекс кэш.
    let before_warm = s.state.badges_in_range("telegram", "7", 0, next).await;
    s.state.warm_badge_index().await.unwrap();
    let after_warm = s.state.badges_in_range("telegram", "7", 0, next).await;
    assert_eq!(before_warm.len(), (pairs.len() / 2).min(before_warm.len()).max(1));
    assert_eq!(before_warm, after_warm, "прогрев из базы дал другой результат");
}

/// Батч ИЗ БАЗЫ и батч ИЗ ИНДЕКСА обязаны совпадать: индекс — кэш, а не
/// второй источник истины.
#[tokio::test]
async fn the_range_batch_agrees_between_memory_and_the_database() {
    let s = stand().await;
    for i in [10i64, 20, 30] {
        meetings::create_or_attach(
            &s.state,
            new_meeting(
                &format!("https://telemost.yandex.ru/j/{}", 2000 + i),
                Some(SourceKey::new("telegram", "7", i)),
            ),
        )
        .await
        .unwrap();
    }
    // Чужой чат в выборку попасть не должен.
    meetings::create_or_attach(
        &s.state,
        new_meeting(
            "https://telemost.yandex.ru/j/2999",
            Some(SourceKey::new("telegram", "8", 20)),
        ),
    )
    .await
    .unwrap();

    let from_db = sources::range(&s.state, "telegram", "7", 5, 25).await.unwrap();
    let from_memory = s.state.badges_in_range("telegram", "7", 5, 25).await;
    assert_eq!(from_db.len(), 2, "в диапазон попадают только 10 и 20: {from_db:?}");
    assert_eq!(from_db, from_memory);

    // Пустышек нет: сообщения без встречи в ответе не появляются.
    let empty = s.state.badges_in_range("telegram", "7", 11, 19).await;
    assert!(empty.is_empty());
}

// ---------------------------------------------------------------------------
// Остальные репозитории
// ---------------------------------------------------------------------------

#[tokio::test]
async fn participants_spans_and_segments_round_trip() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/7", None))
        .await
        .unwrap()
        .meeting;

    // Первая ступень каскада не знает имени — только ключ.
    let pid = participants::upsert(&s.state, &m.id, "track-0", None, Some(0), Some("msid"), None)
        .await
        .unwrap();
    // Вторая приносит имя из DOM.
    let same = participants::upsert(&s.state, &m.id, "track-0", Some("Игорь"), None, Some("dom"), Some(0.9))
        .await
        .unwrap();
    assert_eq!(pid, same, "upsert по тому же ключу не заводит второго участника");

    // И пустое имя НЕ стирает уже найденное.
    participants::upsert(&s.state, &m.id, "track-0", Some("   "), None, None, None)
        .await
        .unwrap();
    let list = participants::list(&s.state, &m.id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].display_name.as_deref(), Some("Игорь"));
    assert_eq!(list[0].track_idx, Some(0));

    participants::rename(&s.state, pid, "  Игорь Герасимов  ").await.unwrap();
    let list = participants::list(&s.state, &m.id).await.unwrap();
    assert_eq!(list[0].display_name.as_deref(), Some("Игорь Герасимов"));
    assert_eq!(list[0].source.as_deref(), Some("manual"));

    assert_eq!(
        participants::rename(&s.state, 99_999, "никто").await.unwrap_err(),
        MeetError::NotFound
    );

    // Интервалы приходят несортированными и обязаны вернуться по началу:
    // attribute_segments идёт двумя указателями и на несортированном входе
    // дала бы тихо неверный результат.
    let n = spans::insert_many(
        &s.state,
        &m.id,
        vec![
            spans::NewSpan {
                participant_id: Some(pid),
                track_idx: Some(0),
                start_ms: 5000,
                end_ms: 6000,
                source: "dom".into(),
            },
            spans::NewSpan {
                participant_id: Some(pid),
                track_idx: Some(0),
                start_ms: 1000,
                end_ms: 2000,
                source: "vad".into(),
            },
        ],
    )
    .await
    .unwrap();
    assert_eq!(n, 2);
    let got = spans::list(&s.state, &m.id).await.unwrap();
    assert_eq!(got[0].start_ms, 1000);
    assert_eq!(got[1].start_ms, 5000);

    spans::clear(&s.state, &m.id).await.unwrap();
    assert!(spans::list(&s.state, &m.id).await.unwrap().is_empty());

    // Сегменты ЗАМЕНЯЮТСЯ целиком: повтор расшифровки не имеет права их
    // удвоить.
    let make = |texts: &[&str]| -> Vec<segments::NewSegment> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| segments::NewSegment {
                idx: i as i64,
                start_ms: i as i64 * 1000,
                end_ms: i as i64 * 1000 + 900,
                text: (*t).to_string(),
                participant_id: Some(pid),
                speaker_name: Some("Игорь".into()),
                track_idx: Some(0),
                confidence: Some(0.8),
                overlapped: false,
                words_json: None,
            })
            .collect()
    };
    segments::replace_all(&s.state, &m.id, make(&["раз", "два", "три"])).await.unwrap();
    assert_eq!(segments::count(&s.state, &m.id).await.unwrap(), 3);
    segments::replace_all(&s.state, &m.id, make(&["раз", "два"])).await.unwrap();
    assert_eq!(
        segments::count(&s.state, &m.id).await.unwrap(),
        2,
        "повторный прогон обязан ЗАМЕНИТЬ сегменты, а не дописать"
    );
    let page = segments::page(&s.state, &m.id, 0, 10).await.unwrap();
    assert_eq!(page.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["раз", "два"]);
}

/// Диагностическая запись в журнал не меняет статуса.
#[tokio::test]
async fn a_journal_note_does_not_change_the_status() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/8", None))
        .await
        .unwrap()
        .meeting;
    events::note(&s.state, &m.id, "queued", "dom_probe_missed", Some("кнопка выхода не найдена"))
        .await
        .unwrap();
    assert_eq!(
        meetings::get(&s.state, &m.id).await.unwrap().status,
        MeetStatus::Queued
    );
    let log = events::list(&s.state, &m.id).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].from_status.as_deref(), Some("queued"));
    assert_eq!(log[1].to_status, "queued");
    assert_eq!(log[1].detail.as_deref(), Some("кнопка выхода не найдена"));
}

/// Без базы репозитории отвечают внятно, а не паникуют: между `init` и
/// `spawn_workers` есть короткое окно, когда базы ещё нет.
#[tokio::test]
async fn repositories_without_a_database_answer_db_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let state = MeetState::new(MeetConfig {
        config_path: dir.path().join("meet.toml"),
        cache_dir: dir.path().join("cache"),
        db_path: dir.path().join("cache/meet.db"),
        models_dir: dir.path().join("cache/models"),
        ..MeetConfig::default()
    });
    assert_eq!(
        meetings::get(&state, "любой").await.unwrap_err(),
        MeetError::DbUnavailable
    );
    assert_eq!(
        meetings::count_active(&state).await.unwrap_err(),
        MeetError::DbUnavailable
    );
    // А батч по индексу работает: он в базу не ходит вовсе.
    assert!(state.badges_in_range("telegram", "7", 0, 100).await.is_empty());
}

// ---------------------------------------------------------------------------
// reconcile_on_boot
// ---------------------------------------------------------------------------

/// Сорок минут записи не имеют права пропасть из-за Ctrl-C.
#[tokio::test]
async fn a_recording_with_audio_is_salvaged_into_transcoding() {
    let s = stand().await;
    let dir = s.state.config.meeting_dir("salvage-me");
    std::fs::create_dir_all(dir.join("raw")).unwrap();
    // 40 минут PCM16 16 кГц моно.
    let secs = 40 * 60u64;
    let bytes = 44 + secs * u64::from(s.state.config.audio_sample_rate) * 2;
    std::fs::write(dir.join("raw/track-0.wav"), vec![0u8; bytes as usize]).unwrap();

    let mut req = new_meeting("https://telemost.yandex.ru/j/9", None);
    req.meeting_dir = dir.display().to_string();
    let m = meetings::create_or_attach(&s.state, req).await.unwrap().meeting;
    for st in [MeetStatus::Launching, MeetStatus::Joining, MeetStatus::Recording] {
        meetings::set_status(&s.state, &m.id, st, None).await.unwrap();
    }

    let report = forge_meet::reconcile_on_boot(&s.state).await;
    assert_eq!(report.scanned, 1);
    assert_eq!(report.salvaged, 1, "запись со звуком обязана уйти в transcoding");
    assert_eq!(report.failed, 0);

    let row = meetings::get(&s.state, &m.id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Transcoding);
    assert_eq!(row.status_reason.as_deref(), Some("salvaged_after_restart"));

    // ИДЕМПОТЕНТНОСТЬ: второй проход перезапускает этап, а не ломает запись.
    let again = forge_meet::reconcile_on_boot(&s.state).await;
    assert_eq!(again.salvaged, 0);
    assert_eq!(again.failed, 0);
    assert_eq!(again.resumed, vec![(m.id.clone(), MeetStatus::Transcoding)]);
    assert_eq!(
        meetings::get(&s.state, &m.id).await.unwrap().status,
        MeetStatus::Transcoding
    );
}

/// Запись БЕЗ спасаемого звука — настоящая потеря, и она называется `failed`.
#[tokio::test]
async fn a_recording_without_audio_becomes_failed() {
    let s = stand().await;
    let dir = s.state.config.meeting_dir("nothing-here");
    std::fs::create_dir_all(dir.join("raw")).unwrap();
    // Девять секунд — меньше границы спасения.
    let bytes = 44 + 9 * u64::from(s.state.config.audio_sample_rate) * 2;
    std::fs::write(dir.join("raw/track-0.wav"), vec![0u8; bytes as usize]).unwrap();

    let mut req = new_meeting("https://telemost.yandex.ru/j/10", None);
    req.meeting_dir = dir.display().to_string();
    let m = meetings::create_or_attach(&s.state, req).await.unwrap().meeting;
    for st in [MeetStatus::Launching, MeetStatus::Joining, MeetStatus::Recording] {
        meetings::set_status(&s.state, &m.id, st, None).await.unwrap();
    }

    let report = forge_meet::reconcile_on_boot(&s.state).await;
    assert_eq!(report.failed, 1);
    assert_eq!(report.salvaged, 0);
    let row = meetings::get(&s.state, &m.id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Failed);
    assert_eq!(row.status_reason.as_deref(), Some("no_audio_after_restart"));
}

/// Встреча, до записи не дошедшая, отменяется — а не объявляется сбоем:
/// ошибки не было, терять нечего.
#[tokio::test]
async fn a_meeting_that_never_started_recording_is_cancelled_not_failed() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/11", None))
        .await
        .unwrap()
        .meeting;
    meetings::set_status(&s.state, &m.id, MeetStatus::Launching, None).await.unwrap();

    let report = forge_meet::reconcile_on_boot(&s.state).await;
    assert_eq!(report.cancelled, 1);
    assert_eq!(report.failed, 0);
    let row = meetings::get(&s.state, &m.id).await.unwrap();
    assert_eq!(row.status, MeetStatus::Cancelled);
    assert_eq!(row.status_reason.as_deref(), Some("restart_before_recording"));
}

/// Терминальные встречи восстановление не трогает.
#[tokio::test]
async fn finished_meetings_are_left_alone() {
    let s = stand().await;
    let m = meetings::create_or_attach(&s.state, new_meeting("https://telemost.yandex.ru/j/12", None))
        .await
        .unwrap()
        .meeting;
    meetings::set_status(&s.state, &m.id, MeetStatus::Cancelled, Some("пользователь"))
        .await
        .unwrap();
    let before = events::count(&s.state, &m.id).await.unwrap();

    let report = forge_meet::reconcile_on_boot(&s.state).await;
    assert_eq!(report.scanned, 0, "терминальные встречи в выборку не попадают");
    assert_eq!(events::count(&s.state, &m.id).await.unwrap(), before);
    assert_eq!(
        meetings::get(&s.state, &m.id).await.unwrap().status_reason.as_deref(),
        Some("пользователь")
    );
}

/// Восстановление обновляет и тёплый индекс: иначе лента показывала бы
/// «идёт запись» у встречи, которая уже ушла в пост-обработку.
#[tokio::test]
async fn reconcile_updates_the_warm_index_too() {
    let s = stand().await;
    let key = SourceKey::new("telegram", "7", 777);
    let dir = s.state.config.meeting_dir("with-audio");
    std::fs::create_dir_all(dir.join("raw")).unwrap();
    let bytes = 44 + 60 * u64::from(s.state.config.audio_sample_rate) * 2;
    std::fs::write(dir.join("raw/track-0.wav"), vec![0u8; bytes as usize]).unwrap();

    let mut req = new_meeting("https://telemost.yandex.ru/j/13", Some(key.clone()));
    req.meeting_dir = dir.display().to_string();
    let m = meetings::create_or_attach(&s.state, req).await.unwrap().meeting;
    for st in [MeetStatus::Launching, MeetStatus::Joining, MeetStatus::Recording] {
        meetings::set_status(&s.state, &m.id, st, None).await.unwrap();
    }
    assert_eq!(s.state.badge_of(&key).await.unwrap().status, MeetStatus::Recording);

    forge_meet::reconcile_on_boot(&s.state).await;
    assert_eq!(s.state.badge_of(&key).await.unwrap().status, MeetStatus::Transcoding);
}
