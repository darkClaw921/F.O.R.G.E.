//! ЖИВОЙ прогон пост-обработки: настоящие ffmpeg, ffprobe и whisper.cpp.
//!
//! Помечен `#[ignore]` и запускается руками:
//!
//! ```bash
//! cargo test -p forge-meet --test pipeline_live -- --ignored --nocapture
//! ```
//!
//! # Зачем он есть, если всё покрыто моками
//!
//! Мок доказывает, что мы правильно РАЗБИРАЕМ вывод. Он не доказывает, что мы
//! правильно ПРОСИМ: что фильтрограф принимается настоящим ffmpeg, что флаги
//! понимает установленный whisper, что `-ojf` кладёт файл туда, куда мы его
//! ждём, и что расчёт реально идёт на Metal. Всё это ловится только живым
//! прогоном — и ровно здесь.
//!
//! Речь синтезируется `say -v Milena` (macOS), как в пробе Ф0: настоящая
//! русская речь известной длительности, без зависимости от чужих файлов.
//!
//! # Что он НЕ заменяет
//!
//! Живую приёмку Телемоста. Здесь нет ни браузера, ни встречи — только
//! пост-обработка каталога с записью. Фикстура не заменяет живую запись, и
//! пункт «полный прогон на реальной встрече» остаётся за Ф4/Ф11.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_meet::config::MeetConfig;
use forge_meet::db::repo;
use forge_meet::ingest::{wav::read_samples, wav::WavWriter, Manifest};
use forge_meet::pipeline;
use forge_meet::state::MeetState;
use forge_meet::status::MeetStatus;

/// Реплика одного спикера: текст и место на общей оси.
struct Line {
    speaker: &'static str,
    text: &'static str,
    start_ms: u64,
}

const SCRIPT: [Line; 4] = [
    Line {
        speaker: "Аня",
        text: "Коллеги, начинаем еженедельную встречу команды.",
        start_ms: 0,
    },
    Line {
        speaker: "Боря",
        text: "Релиз готов, осталось прогнать тесты на стенде.",
        start_ms: 6_000,
    },
    Line {
        speaker: "Аня",
        text: "Хорошо. Тогда выкатываем в пятницу утром.",
        start_ms: 12_000,
    },
    Line {
        speaker: "Боря",
        text: "Согласен. Я подготовлю заметки к релизу.",
        start_ms: 18_000,
    },
];

#[tokio::test(flavor = "multi_thread")]
#[ignore = "живой прогон: нужны whisper-cli, модель и ffmpeg"]
async fn a_real_recording_goes_through_the_real_whisper() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    // Модели берём НАСТОЯЩИЕ: 547 МиБ ради теста не качают.
    let models_dir = MeetConfig::default().models_dir;
    let cfg = MeetConfig {
        config_path: root.join("meet.toml"),
        cache_dir: root.join("cache"),
        db_path: root.join("cache/meet.db"),
        models_dir,
        ..MeetConfig::default()
    };

    let deps = forge_meet::deps::probe_all(&cfg).await;
    println!(
        "пробы: ffmpeg={:?} ffprobe={:?} whisper={:?} metal={:?} модель={}",
        deps.ffmpeg.path, deps.ffprobe.path, deps.whisper.path, deps.gpu_metal, deps.model_ready
    );
    if !deps.can_transcribe() || !deps.ffmpeg.is_ready() {
        eprintln!(
            "пропуск живого прогона: не хватает окружения ({:?})",
            deps.transcribe_blocker()
        );
        return;
    }

    let state = MeetState::new(cfg);
    let db = forge_meet::db::Db::open_memory().await.unwrap();
    db.migrate().await.unwrap();
    state.attach_db(Arc::new(db)).unwrap();
    *state.deps.write().await = deps.clone();

    // --- готовим каталог записи ------------------------------------------
    let link = forge_meet::parse_meeting_url("https://telemost.yandex.ru/j/60497752704486").unwrap();
    let meeting_dir = root.join("cache/live");
    let out = repo::meetings::create_or_attach(
        &state,
        repo::meetings::NewMeeting {
            link,
            source: None,
            title: Some("Живой прогон пост-обработки".into()),
            mode: "normal".into(),
            bot_name: "F.O.R.G.E.".into(),
            meeting_dir: meeting_dir.display().to_string(),
        },
    )
    .await
    .unwrap();
    let id = out.meeting.id.clone();

    let ffmpeg = PathBuf::from(deps.ffmpeg.path.clone().unwrap());
    let raw = meeting_dir.join("raw");
    std::fs::create_dir_all(&raw).unwrap();

    // Две дорожки: у каждого спикера своя, обе на общей оси (так пишет
    // ingest Ф3 — абсолютным сэмпловым смещением).
    let total_ms = 24_000u64;
    let mut per_track: Vec<(String, Vec<i16>)> = vec![
        ("Аня".into(), vec![0i16; (16_000 * total_ms / 1000) as usize]),
        ("Боря".into(), vec![0i16; (16_000 * total_ms / 1000) as usize]),
    ];
    for line in &SCRIPT {
        let samples = synth(&root, &ffmpeg, line.text);
        let idx = if line.speaker == "Аня" { 0 } else { 1 };
        let at = (16_000 * line.start_ms / 1000) as usize;
        let track = &mut per_track[idx].1;
        for (i, s) in samples.iter().enumerate() {
            if at + i < track.len() {
                track[at + i] = *s;
            }
        }
    }
    for (idx, (_, samples)) in per_track.iter().enumerate() {
        let mut w = WavWriter::create(raw.join(format!("track-{idx}.wav")), 16_000).unwrap();
        w.write_frame(0, samples).unwrap();
        w.finish().unwrap();
    }

    let mut m = Manifest::new(&id, &out.meeting.canonical_url, "telemost", "normal", 16_000);
    m.t0_unix_ms = 1_700_000_000_000;
    m.duration_ms = total_ms;
    m.capture_path = Some("rtcpeerconnection".into());
    m.tracks = (0..2)
        .map(|idx| forge_meet::ingest::tracks::TrackState {
            track_id: format!("t{idx}"),
            idx,
            start_offset_ms: 0,
            end_offset_ms: Some(total_ms),
            frames: 1,
            samples: 16_000 * total_ms / 1000,
            end_samples: 16_000 * total_ms / 1000,
            capture_path: None,
        })
        .collect();
    m.finished = true;
    m.save(&meeting_dir).unwrap();

    // Журнал: DOM-таймлайн активного спикера — основной путь Телемоста.
    let mut j = forge_meet::ingest::journal::EventJournal::open(&meeting_dir).unwrap();
    j.note(0, "recording_started", serde_json::json!({"t0_unix_ms": 1_700_000_000_000u64}))
        .unwrap();
    for line in &SCRIPT {
        j.note(
            line.start_ms,
            "active_speaker",
            serde_json::json!({"at_ms": line.start_ms, "name": line.speaker}),
        )
        .unwrap();
    }
    j.note(
        total_ms,
        "recording_stopped",
        serde_json::json!({"reason": "user_stop", "at_ms": total_ms}),
    )
    .unwrap();

    for st in [
        MeetStatus::Launching,
        MeetStatus::Joining,
        MeetStatus::Recording,
        MeetStatus::Leaving,
        MeetStatus::Transcoding,
    ] {
        repo::meetings::set_status(&state, &id, st, Some("live")).await.unwrap();
    }

    // --- прогон ------------------------------------------------------------
    let started = std::time::Instant::now();
    let report = pipeline::run::run(&state, &id).await.expect("живой прогон упал");
    let wall = started.elapsed();

    println!("--- ЖИВОЙ ПРОГОН ---");
    println!("дорожек: {}", report.tracks);
    println!("сегментов: {}", report.segments);
    println!("спикеров: {}", report.speakers);
    println!("архив: {:?}", report.audio_path);
    println!("дрейф: {:?}", report.drift_scale);
    println!(
        "whisper: {} мс на {} мс аудио = {:.1}× реального времени",
        report.whisper_ms,
        total_ms,
        total_ms as f64 / report.whisper_ms as f64
    );
    println!("весь пайплайн: {wall:?}");

    let md = std::fs::read_to_string(meeting_dir.join("transcript.md")).unwrap();
    println!("--- transcript.md ---\n{md}");

    assert_eq!(
        repo::meetings::get(&state, &id).await.unwrap().status,
        MeetStatus::Ready
    );
    assert!(report.segments > 0, "речь не распознана вовсе");
    assert!(md.contains("Аня") && md.contains("Боря"), "спикеры не подставились");
    // Слово из первой реплики обязано найтись: если его нет, распознавание
    // отработало не по тому файлу.
    assert!(
        md.to_lowercase().contains("встреч") || md.to_lowercase().contains("коллег"),
        "в расшифровке нет ни одного слова первой реплики:\n{md}"
    );
    assert!(meeting_dir.join("audio.m4a").is_file());
    assert!(!meeting_dir.join("work").exists(), "транзитные файлы не подчищены");

    // --- САММАРИ НА НАСТОЯЩЕМ CLAUDE CLI ----------------------------------
    //
    // Мок доказывает, что мы правильно РАЗБИРАЕМ стрим. Он не доказывает, что
    // установленный `claude -p` принимает наш argv, читает промпт из stdin и
    // отвечает по нашей системной инструкции. Здесь — доказывает.
    println!("--- САММАРИ ---");
    println!("claude: {}", state.config.claude_cli_path.display());
    match report.summary {
        Some(forge_meet::status::SummaryStatus::Ready) => {
            let summary = std::fs::read_to_string(meeting_dir.join("summary.md")).unwrap();
            println!("{summary}");
            assert!(!summary.trim().is_empty(), "саммари пустое");
            assert_eq!(
                repo::meetings::get(&state, &id).await.unwrap().summary_status,
                forge_meet::status::SummaryStatus::Ready
            );
        }
        Some(forge_meet::status::SummaryStatus::Failed) => {
            // Провал допустим (нет сети, кончилась квота), но встреча ОБЯЗАНА
            // остаться готовой — ровно это правило и проверяется живьём.
            println!("саммари не получилось; журнал встречи:");
            for e in repo::events::list(&state, &id).await.unwrap() {
                println!("  {} {} {}", e.to_status, e.reason.unwrap_or_default(), e.detail.unwrap_or_default());
            }
            assert!(
                meeting_dir.join("transcript.md").is_file(),
                "транскрипт пропал вместе с саммари"
            );
        }
        other => panic!("саммари не запускалось вовсе: {other:?}"),
    }
    assert_eq!(
        repo::meetings::get(&state, &id).await.unwrap().status,
        MeetStatus::Ready,
        "встреча обязана быть готова независимо от исхода саммари"
    );
}

/// **ЖИВАЯ ПРОВЕРКА ПОСЫЛКИ METAL-ГАРДА.**
///
/// Гард стоит на наблюдении Ф0: настоящий `whisper-cli` с `-ng` печатает три
/// строки `ggml_metal_*_init:` и НЕ печатает `ggml_metal_init:`. Наблюдение
/// сделано один раз в августе 2026 на версии 1.9.2 — и вся конструкция
/// рухнет, если апстрим поменяет вывод.
///
/// Тест проверяет это на УСТАНОВЛЕННОМ бинаре: гоняет `-ng` (единственное
/// место в проекте, где этот флаг вообще появляется — и он тестовый), берёт
/// настоящий stderr и отдаёт его нашей же функции [`metal_confirmed`].
/// Скажи она «GPU подтверждён» — правило проекта держалось бы на устаревшем
/// наблюдении.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "живой прогон: нужны whisper-cli и модель"]
async fn the_installed_whisper_still_behaves_as_the_guard_assumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cfg = MeetConfig {
        config_path: root.join("meet.toml"),
        cache_dir: root.join("cache"),
        db_path: root.join("cache/meet.db"),
        ..MeetConfig::default()
    };
    let deps = forge_meet::deps::probe_all(&cfg).await;
    if !deps.can_transcribe() || !deps.ffmpeg.is_ready() {
        eprintln!("пропуск: нет whisper или модели");
        return;
    }
    let ffmpeg = PathBuf::from(deps.ffmpeg.path.clone().unwrap());
    let whisper = PathBuf::from(deps.whisper.path.clone().unwrap());
    let model = forge_meet::models::model_path(&cfg.models_dir, &cfg.whisper_model);

    // Полторы секунды речи: на CPU это секунды, на GPU — доли.
    let samples = synth(&root, &ffmpeg, "Проверка гарда.");
    let wav = root.join("probe.wav");
    let mut w = WavWriter::create(&wav, 16_000).unwrap();
    w.write_frame(0, &samples).unwrap();
    w.finish().unwrap();

    let run = |extra: Vec<&str>| {
        let mut args = vec![
            "-m".to_string(),
            model.display().to_string(),
            "-f".to_string(),
            wav.display().to_string(),
            "-l".to_string(),
            "ru".to_string(),
        ];
        args.extend(extra.into_iter().map(str::to_string));
        let out = std::process::Command::new(&whisper)
            .args(&args)
            .output()
            .expect("whisper не запустился");
        String::from_utf8_lossy(&out.stderr).to_string()
    };

    let gpu = run(vec![]);
    let cpu = run(vec!["-ng"]);

    println!("--- GPU stderr (хвост) ---\n{}", tail_lines(&gpu, 12));
    println!("--- CPU (-ng) stderr (хвост) ---\n{}", tail_lines(&cpu, 12));

    assert!(
        forge_meet::pipeline::whisper::metal_confirmed(&gpu),
        "настоящий GPU-прогон обязан печатать ggml_metal_init:\n{gpu}"
    );
    assert!(
        !forge_meet::pipeline::whisper::metal_confirmed(&cpu),
        "ГАРД ПРОПУСКАЕТ CPU-ПРОГОН: вывод установленного whisper изменился\n{cpu}"
    );
    // И посылка Ф0 всё ещё верна: всегдашние строки печатаются в обоих.
    assert!(
        cpu.contains("ggml_metal_device_init:"),
        "наблюдение Ф0 устарело: всегдашних строк больше нет\n{cpu}"
    );
    assert!(
        cpu.contains("ggml_metal"),
        "наивная проверка contains(ggml_metal) обязана срабатывать — иначе тест ничего не доказывает"
    );
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Синтезирует реплику и возвращает её как PCM16 16 кГц моно.
// ---------------------------------------------------------------------------
// ПЕР-ТРЕКОВАЯ РАСШИФРОВКА НА НАСТОЯЩЕМ WHISPER (Ф11)
// ---------------------------------------------------------------------------

/// Реплика с перебиванием: кто, что и когда говорит на общей оси.
struct Turn {
    speaker: &'static str,
    track: usize,
    text: &'static str,
    start_ms: u64,
}

/// Расписание С ПЕРЕБИВАНИЕМ. Вторая реплика начинается, пока первая ещё
/// звучит, — и именно её микс теряет.
/// Реплики расставлены по ПОЛУТОРАМИНУТНОЙ записи, а не впритык.
///
/// Живой прогон первой редакции (18 секунд подряд) показал, что на короткой
/// записи пер-трековый путь ПРОИГРЫВАЕТ по времени: постоянная цена загрузки
/// модели на каждую дорожку больше сэкономленного расчёта. Настоящая встреча
/// устроена иначе — люди молчат большую часть времени, — и проверять надо
/// именно её.
const CROSSTALK: [Turn; 4] = [
    Turn { speaker: "Аня", track: 0, text: "Коллеги, я предлагаю перенести релиз на следующую неделю.", start_ms: 0 },
    Turn { speaker: "Боря", track: 1, text: "Подожди, тесты уже прошли.", start_ms: 2_000 },
    Turn { speaker: "Аня", track: 0, text: "Хорошо, тогда выкатываем в пятницу.", start_ms: 45_000 },
    Turn { speaker: "Боря", track: 1, text: "Я подготовлю заметки к релизу.", start_ms: 75_000 },
];

/// **ЖИВОЕ СРАВНЕНИЕ ДВУХ ПУТЕЙ НА ОДНОЙ И ТОЙ ЖЕ ЗАПИСИ.**
///
/// Одна и та же пара дорожек прогоняется дважды: по дорожкам (с нарезкой по
/// VAD) и по миксу. Проверяется то, ради чего пер-трековый путь и сделан:
///
/// 1. перебивание разобрано — в расшифровке есть слова ОБЕИХ наложившихся
///    реплик, и каждая подписана своим человеком;
/// 2. цена не выросла существенно — измеряется и печатается, а не
///    предполагается.
///
/// Мок доказать этого не может в принципе: он не смешивает звук. Здесь звук
/// настоящий (синтез `say`), микс настоящий (ffmpeg) и whisper настоящий.
///
/// ⚠ Это НЕ живая встреча: браузера и площадки здесь нет, и режим
/// `PerParticipant` ни на одной площадке живой пробой не подтверждён.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "живой прогон: нужны whisper-cli, модель, ffmpeg и say"]
async fn per_track_transcription_beats_the_mix_on_crosstalk() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let models_dir = MeetConfig::default().models_dir;
    let base = MeetConfig {
        config_path: root.join("meet.toml"),
        cache_dir: root.join("cache"),
        db_path: root.join("cache/meet.db"),
        models_dir,
        ..MeetConfig::default()
    };
    let deps = forge_meet::deps::probe_all(&base).await;
    if !deps.can_transcribe() || !deps.ffmpeg.is_ready() {
        eprintln!("пропуск: не хватает окружения ({:?})", deps.transcribe_blocker());
        return;
    }
    let ffmpeg = PathBuf::from(deps.ffmpeg.path.clone().unwrap());

    // --- готовим ОДНУ запись, которую прогоним двумя путями ---------------
    let total_ms = 90_000u64;
    let n = (16_000 * total_ms / 1_000) as usize;
    let mut tracks: Vec<Vec<i16>> = vec![vec![0i16; n], vec![0i16; n]];
    // Границы речи на общей оси — их же положим в журнал как пер-трековый VAD.
    let mut speech: Vec<(usize, u64, u64)> = Vec::new();
    let mut names: Vec<(usize, &'static str)> = Vec::new();
    for turn in &CROSSTALK {
        if !names.iter().any(|(i, _)| *i == turn.track) {
            names.push((turn.track, turn.speaker));
        }
        let samples = synth(&root, &ffmpeg, turn.text);
        let at = (16_000 * turn.start_ms / 1_000) as usize;
        let track = &mut tracks[turn.track];
        for (i, s) in samples.iter().enumerate() {
            if at + i < track.len() {
                track[at + i] = *s;
            }
        }
        let len_ms = samples.len() as u64 * 1_000 / 16_000;
        speech.push((turn.track, turn.start_ms, (turn.start_ms + len_ms).min(total_ms)));
    }
    println!("--- расписание ---");
    for (t, from, to) in &speech {
        println!("  дорожка {t}: {from}–{to} мс");
    }
    let overlap = speech[0].2.min(speech[1].2) as i64 - speech[0].1.max(speech[1].1) as i64;
    assert!(overlap > 300, "реплики не наложились: перебивания в записи нет");
    println!("наложение первых двух реплик: {overlap} мс");

    let mut results = Vec::new();
    for per_track in [true, false] {
        let cfg = MeetConfig {
            config_path: root.join("meet.toml"),
            cache_dir: root.join(format!("cache-{per_track}")),
            db_path: root.join(format!("cache-{per_track}/meet.db")),
            models_dir: base.models_dir.clone(),
            pertrack_transcription: per_track,
            ..MeetConfig::default()
        };
        let state = MeetState::new(cfg);
        let db = forge_meet::db::Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();
        *state.deps.write().await = deps.clone();

        let link = forge_meet::parse_meeting_url("https://telemost.yandex.ru/j/60497752704486").unwrap();
        let meeting_dir = root.join(format!("rec-{per_track}"));
        let out = repo::meetings::create_or_attach(
            &state,
            repo::meetings::NewMeeting {
                link,
                source: None,
                title: Some("Перебивание".into()),
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: meeting_dir.display().to_string(),
            },
        )
        .await
        .unwrap();
        let id = out.meeting.id.clone();

        let raw = meeting_dir.join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        for (idx, samples) in tracks.iter().enumerate() {
            let mut w = WavWriter::create(raw.join(format!("track-{idx}.wav")), 16_000).unwrap();
            w.write_frame(0, samples).unwrap();
            w.finish().unwrap();
        }

        let mut m = Manifest::new(&id, &out.meeting.canonical_url, "telemost", "normal", 16_000);
        m.t0_unix_ms = 1_700_000_000_000;
        m.duration_ms = total_ms;
        m.capture_path = Some("rtcpeerconnection".into());
        // Режим «дорожка = человек»: только он включает пер-трековый путь.
        m.track_mode = Some(forge_meet::bot::mode::TrackMode::PerParticipant);
        m.tracks = (0..2)
            .map(|idx| forge_meet::ingest::tracks::TrackState {
                track_id: format!("t{idx}"),
                idx,
                start_offset_ms: 0,
                end_offset_ms: Some(total_ms),
                frames: 1,
                samples: 16_000 * total_ms / 1_000,
                end_samples: 16_000 * total_ms / 1_000,
                capture_path: None,
            })
            .collect();
        m.finished = true;
        m.save(&meeting_dir).unwrap();

        let mut j = forge_meet::ingest::journal::EventJournal::open(&meeting_dir).unwrap();
        j.note(0, "recording_started", serde_json::json!({"t0_unix_ms": 1_700_000_000_000u64}))
            .unwrap();
        for (idx, name) in names.iter().copied() {
            j.note(
                0,
                "track_added",
                serde_json::json!({"track_id": format!("t{idx}"), "idx": idx, "msid": format!("name:{}", name.to_lowercase())}),
            )
            .unwrap();
        }
        // Индикатор активного спикера — ПО РЕПЛИКАМ, а не разом в нуле.
        // Два `active_speaker` с одной меткой закрывают друг друга интервалом
        // нулевой длины, участник не заводится вовсе, и каскад не с чем
        // сопоставлять `msid`: дорожка осталась бы «Участником 1». Поймано
        // живым прогоном.
        for turn in &CROSSTALK {
            j.note(
                turn.start_ms,
                "active_speaker",
                serde_json::json!({"at_ms": turn.start_ms, "name": turn.speaker}),
            )
            .unwrap();
        }
        for (track, from, to) in &speech {
            j.note(*from, "speech_started", serde_json::json!({"track_id": format!("t{track}"), "at_ms": from}))
                .unwrap();
            j.note(*to, "speech_ended", serde_json::json!({"track_id": format!("t{track}"), "at_ms": to}))
                .unwrap();
        }
        j.note(total_ms, "recording_stopped", serde_json::json!({"reason": "user_stop", "at_ms": total_ms}))
            .unwrap();

        for st in [
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
        ] {
            repo::meetings::set_status(&state, &id, st, Some("live")).await.unwrap();
        }

        let report = pipeline::run::run(&state, &id).await.expect("живой прогон упал");
        let md = std::fs::read_to_string(meeting_dir.join("transcript.md")).unwrap();
        println!(
            "--- {} ---\nдорожек расшифровано: {}\nзвука в whisper: {} мс\nwhisper: {} мс\n{md}",
            if per_track { "ПО ДОРОЖКАМ" } else { "ПО МИКСУ" },
            report.per_track,
            report.audio_sent_ms,
            report.whisper_ms
        );
        results.push((report, md));
    }

    let (pt, pt_md) = &results[0];
    let (mix, mix_md) = &results[1];
    assert_eq!(pt.per_track, 2, "пер-трековый путь не включился");
    assert_eq!(mix.per_track, 0, "выключатель не сработал");

    // --- 1. ПЕРЕБИВАНИЕ РАЗОБРАНО -----------------------------------------
    let low = pt_md.to_lowercase();
    assert!(low.contains("релиз"), "первая реплика потерялась:\n{pt_md}");
    assert!(
        low.contains("подожди") || low.contains("тест"),
        "перебившая реплика не распознана по дорожкам:\n{pt_md}"
    );
    assert!(pt_md.contains("Аня") && pt_md.contains("Боря"), "{pt_md}");

    // Сравнение с миксом печатается всегда: оно и есть смысл теста. Падать по
    // нему нельзя — «микс тоже справился» это удача записи, а не дефект кода.
    let mix_low = mix_md.to_lowercase();
    println!(
        "перебившая реплика: по дорожкам {}, по миксу {}",
        low.contains("подожди"),
        mix_low.contains("подожди")
    );

    // --- 2. ЦЕНА -----------------------------------------------------------
    println!(
        "звук в whisper: по дорожкам {} мс, по миксу {} мс ({:.0} %)",
        pt.audio_sent_ms,
        mix.audio_sent_ms,
        100.0 * pt.audio_sent_ms as f64 / mix.audio_sent_ms as f64
    );
    println!(
        "время whisper: по дорожкам {} мс, по миксу {} мс",
        pt.whisper_ms, mix.whisper_ms
    );
    assert!(
        pt.audio_sent_ms <= (mix.audio_sent_ms as f64 * 1.5) as i64,
        "звука по дорожкам ушло существенно больше, чем в миксе"
    );
}

fn synth(root: &Path, ffmpeg: &Path, text: &str) -> Vec<i16> {
    let aiff = root.join("say.aiff");
    let wav = root.join("say.wav");
    let _ = std::fs::remove_file(&aiff);
    let _ = std::fs::remove_file(&wav);

    let ok = std::process::Command::new("say")
        .args(["-v", "Milena", "-o"])
        .arg(&aiff)
        .arg(text)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "say не отработал: живой прогон только на macOS");

    let ok = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(&aiff)
        .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
        .arg(&wav)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg не сконвертировал синтезированную речь");

    read_samples(&wav).unwrap()
}
