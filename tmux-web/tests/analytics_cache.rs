//! Тесты индексатора и кэша аналитики (`analytics::scan` / `analytics::cache`).
//!
//! Проверяется ровно то, ради чего слой существует: **инкремент равен полному
//! пересбору**, и при этом байты второй раз с диска не читаются. Всё остальное
//! (усадка файла, `mtime` назад, версия снимка, битый JSON) — это ветки, где
//! инкремент запрещён и обязан быть полный пересбор.
//!
//! Ни один тест не трогает реальные `~/.claude` и `~/.forge`: и каталог
//! транскриптов, и путь снимка передаются явными аргументами во временный
//! каталог `tempfile`.
//!
//! `devforge` — бинарный крейт без `[lib]`-таргета, поэтому модули
//! подключаются напрямую через `#[path]` (та же схема, что в
//! `analytics_parse.rs`): `analytics::rollup` тянет
//! `claude_memory::encode_project_dir`, а тот — `paths::resolve_root`.

#[allow(dead_code, unused_imports)]
#[path = "../src/paths.rs"]
mod paths;

#[allow(dead_code, unused_imports)]
#[path = "../src/claude_memory.rs"]
mod claude_memory;

// Часть модуля (хранилище `AnalyticsStore` и DTO Фазы 3) в этом тесте не
// используется: он про ядро разбора. Через `#[path]` подключается файл
// целиком, поэтому неиспользуемое здесь глушится явно.
#[allow(dead_code)]
#[path = "../src/analytics/mod.rs"]
mod analytics;

/// Тесты собраны в модуль `analytics_cache`, чтобы их имена начинались с
/// него: и `cargo test -p devforge analytics`, и
/// `cargo test -p devforge analytics_cache` из плана отбирают ровно этот
/// набор (фильтр cargo работает по ИМЕНИ теста, а не по имени бинаря).
mod analytics_cache {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use crate::analytics::cache::{self, CacheSnapshot, CACHE_VERSION};
    use crate::analytics::rollup::{AnalyticsIndex, SessionRollup};
    use crate::analytics::scan::{
        self, FileAction, ScanOutcome, ScanProgress, SessionFile,
    };

    /// База отсчёта фикстур: 2026-09-01T09:00:00.000Z.
    const BASE_MS: i64 = 1_788_339_600_000;

    fn iso(offset_ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(BASE_MS + offset_ms)
            .expect("valid ts")
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn human(offset_ms: i64, text: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/proj","gitBranch":"master","version":"2.1.227","isSidechain":false,"message":{{"role":"user","content":{}}}}}"#,
            iso(offset_ms),
            serde_json::to_string(text).unwrap()
        )
    }

    fn assistant(offset_ms: i64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false,"message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}}}}}"#,
            iso(offset_ms)
        )
    }

    fn turn_end(offset_ms: i64) -> String {
        format!(
            r#"{{"type":"system","subtype":"turn_duration","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false}}"#,
            iso(offset_ms)
        )
    }

    /// Один полный ход: сообщение человека → ответ → конец хода.
    ///
    /// `pause_ms` до следующего сообщения человека задаётся уже смещением
    /// следующего хода, а не здесь.
    fn turn(start_ms: i64, text: &str) -> Vec<String> {
        vec![
            human(start_ms, text),
            assistant(start_ms + 1_000),
            turn_end(start_ms + 2_000),
        ]
    }

    // -----------------------------------------------------------------------
    // Каркас: временный «~/.claude/projects» и запуск скана
    // -----------------------------------------------------------------------

    struct Fixture {
        _tmp: tempfile::TempDir,
        projects: PathBuf,
        cache_path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let projects = tmp.path().join("claude").join("projects");
            std::fs::create_dir_all(projects.join("-tmp-proj")).expect("mkdir");
            let cache_path = tmp.path().join("forge").join("analytics-cache.json");
            Fixture {
                _tmp: tmp,
                projects,
                cache_path,
            }
        }

        fn session_path(&self, session_id: &str) -> PathBuf {
            self.projects
                .join("-tmp-proj")
                .join(format!("{session_id}.jsonl"))
        }

        /// Перезаписывает файл сессии целиком.
        fn write(&self, session_id: &str, lines: &[String]) {
            let body = if lines.is_empty() {
                String::new()
            } else {
                lines.join("\n") + "\n"
            };
            std::fs::write(self.session_path(session_id), body).expect("write session");
        }

        /// Дописывает строки в конец файла (как это делает сам CLI).
        fn append(&self, session_id: &str, lines: &[String]) {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(self.session_path(session_id))
                .expect("open for append");
            for line in lines {
                writeln!(f, "{line}").expect("append line");
            }
        }

        /// Сдвигает mtime файла на `delta_secs` (отрицательное — назад).
        fn shift_mtime(&self, session_id: &str, delta_secs: i64) {
            let path = self.session_path(session_id);
            let meta = std::fs::metadata(&path).expect("stat");
            let modified = meta.modified().expect("mtime");
            let shifted = if delta_secs >= 0 {
                modified + std::time::Duration::from_secs(delta_secs as u64)
            } else {
                modified - std::time::Duration::from_secs((-delta_secs) as u64)
            };
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("open for mtime");
            f.set_modified(shifted).expect("set mtime");
        }

        async fn scan(
            &self,
            prev: BTreeMap<String, SessionRollup>,
            progress: &Arc<ScanProgress>,
        ) -> ScanOutcome {
            scan::scan_incremental(self.projects.clone(), prev, Arc::clone(progress))
                .await
                .expect("скан не должен быть занят")
        }
    }

    fn fresh_progress() -> Arc<ScanProgress> {
        Arc::new(ScanProgress::default())
    }

    /// Числа, по которым сравниваются инкремент и полный пересбор.
    ///
    /// Точные сэмплы (`kept`) и гистограммы намеренно не сравниваются
    /// поэлементно — сравниваются производные от них медианы, а поле-в-поле
    /// равенство проверяет отдельный тест.
    #[derive(Debug, PartialEq)]
    struct Digest {
        human_messages: u64,
        assistant_messages: u64,
        tokens_in: u64,
        tokens_out: u64,
        pauses_count: u64,
        pauses_sum: u64,
        pause_p50: Option<u64>,
        turn_count: u64,
        turn_sum: u64,
        human_ms: u64,
        gen_ms: u64,
        days: Vec<String>,
    }

    fn digest(index: &AnalyticsIndex) -> Digest {
        let mut d = Digest {
            human_messages: 0,
            assistant_messages: 0,
            tokens_in: 0,
            tokens_out: 0,
            pauses_count: 0,
            pauses_sum: 0,
            pause_p50: None,
            turn_count: 0,
            turn_sum: 0,
            human_ms: 0,
            gen_ms: 0,
            days: index.days.keys().cloned().collect(),
        };
        let mut pauses = crate::analytics::rollup::Samples::default();
        for bucket in index.days.values() {
            d.human_messages += bucket.human_messages;
            d.assistant_messages += bucket.assistant_messages;
            d.tokens_in += bucket.tokens_in;
            d.tokens_out += bucket.tokens_out;
            d.pauses_count += bucket.pauses.count;
            d.pauses_sum += bucket.pauses.sum;
            d.turn_count += bucket.turn_ms.count;
            d.turn_sum += bucket.turn_ms.sum;
            d.human_ms += bucket.human_ms;
            d.gen_ms += bucket.gen_ms;
            pauses.merge(&bucket.pauses);
        }
        d.pause_p50 = pauses.p50();
        d
    }

    // -----------------------------------------------------------------------
    // 1. Дописали строки → пересчитан только хвост, итог равен пересбору
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn appended_lines_read_only_tail_and_match_full_rebuild() {
        let fx = Fixture::new();
        let mut lines = turn(0, "первый вопрос");
        fx.write("sess-1", &lines);

        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;
        let first_bytes = progress.bytes_read.load(Ordering::Relaxed);
        assert_eq!(progress.done.load(Ordering::Relaxed), 1);
        assert_eq!(progress.total.load(Ordering::Relaxed), 1);
        assert!(first_bytes > 0, "первый проход обязан прочитать файл");

        // Дописываем второй ход через паузу 42 с после конца первого.
        let tail = turn(44_000, "второй вопрос");
        lines.extend(tail.clone());
        fx.append("sess-1", &tail);

        let progress2 = fresh_progress();
        let incremental = fx.scan(first.files.clone(), &progress2).await;
        let tail_bytes = progress2.bytes_read.load(Ordering::Relaxed);

        let expected_tail: u64 = tail.iter().map(|l| l.len() as u64 + 1).sum();
        assert_eq!(
            tail_bytes, expected_tail,
            "прочитан должен быть ровно хвост, а не файл целиком ({first_bytes} байт)"
        );

        // Полный пересбор тех же данных с нуля.
        let progress3 = fresh_progress();
        let full = fx.scan(BTreeMap::new(), &progress3).await;

        assert_eq!(
            digest(&incremental.index),
            digest(&full.index),
            "инкремент обязан совпасть с полным пересбором"
        );
        // Пауза, разрезанная границей дочитывания, посчитана ровно один раз.
        assert_eq!(digest(&incremental.index).pauses_count, 1);
    }

    #[tokio::test]
    async fn incremental_rollup_is_field_by_field_equal_to_full_rebuild() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "раз"));
        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;

        fx.append("sess-1", &turn(60_000, "два"));
        fx.append("sess-1", &turn(120_000, "три"));

        let progress2 = fresh_progress();
        let incremental = fx.scan(first.files, &progress2).await;
        let progress3 = fresh_progress();
        let full = fx.scan(BTreeMap::new(), &progress3).await;

        let key = fx.session_path("sess-1").to_string_lossy().into_owned();
        let a = &incremental.files[&key];
        let b = &full.files[&key];
        assert_eq!(a.days, b.days, "дневные бакеты обязаны совпасть побитово");
        assert_eq!(a.parse_offset, b.parse_offset);
        assert_eq!(a.turn_state, b.turn_state);
        assert_eq!(a.totals, b.totals);
    }

    // -----------------------------------------------------------------------
    // 2. Неизменившийся файл не открывается
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn unchanged_file_is_not_read_again() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "вопрос"));

        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;

        let progress2 = fresh_progress();
        let second = fx.scan(first.files.clone(), &progress2).await;

        assert_eq!(
            progress2.bytes_read.load(Ordering::Relaxed),
            0,
            "файл без изменений не должен читаться повторно"
        );
        assert_eq!(progress2.files_read.load(Ordering::Relaxed), 0);
        assert_eq!(progress2.done.load(Ordering::Relaxed), 1, "но обойти его надо");
        assert_eq!(digest(&second.index), digest(&first.index));
    }

    // -----------------------------------------------------------------------
    // 3. Файл усох → пересбор целиком
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn shrunk_file_is_rebuilt_from_scratch() {
        let fx = Fixture::new();
        let mut lines = turn(0, "первый");
        lines.extend(turn(60_000, "второй"));
        lines.extend(turn(120_000, "третий"));
        fx.write("sess-1", &lines);

        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;
        assert_eq!(digest(&first.index).human_messages, 3);

        // Файл переписан короче — счётчики старого роллапа обязаны исчезнуть.
        fx.write("sess-1", &turn(0, "единственный"));
        fx.shift_mtime("sess-1", 1);

        let progress2 = fresh_progress();
        let second = fx.scan(first.files, &progress2).await;

        assert_eq!(
            digest(&second.index).human_messages,
            1,
            "старые числа не должны остаться после усадки файла"
        );
        assert!(
            progress2.bytes_read.load(Ordering::Relaxed) > 0,
            "усохший файл обязан быть перечитан"
        );
    }

    // -----------------------------------------------------------------------
    // 4. mtime уехал назад → пересбор целиком
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn mtime_moved_backwards_forces_full_rebuild() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "вопрос"));
        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;

        // Длина та же, но время модификации уехало в прошлое — доверять
        // сохранённому оффсету нельзя.
        fx.shift_mtime("sess-1", -3_600);

        let key = fx.session_path("sess-1").to_string_lossy().into_owned();
        let meta = std::fs::metadata(fx.session_path("sess-1")).expect("stat");
        let file = SessionFile {
            path: fx.session_path("sess-1"),
            key: key.clone(),
            project_dir: "-tmp-proj".to_string(),
            session_id: "sess-1".to_string(),
            len: meta.len(),
            mtime_ms: {
                let files = scan::list_session_files(&fx.projects);
                files[0].mtime_ms
            },
        };
        assert_eq!(
            scan::decide_action(first.files.get(&key), &file),
            FileAction::Full,
            "mtime назад обязан давать полный пересбор"
        );

        let progress2 = fresh_progress();
        let second = fx.scan(first.files, &progress2).await;
        assert!(
            progress2.bytes_read.load(Ordering::Relaxed) > 0,
            "файл обязан быть перечитан целиком"
        );
        assert_eq!(digest(&second.index).human_messages, 1);
    }

    /// Та же длина при новом mtime — перезапись «в размер»: доверять оффсету
    /// нельзя, единственный безопасный ответ — полный пересбор.
    #[tokio::test]
    async fn same_length_with_newer_mtime_forces_full_rebuild() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "аааааа"));
        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;

        fx.write("sess-1", &turn(0, "бббббб"));
        fx.shift_mtime("sess-1", 5);
        let files = scan::list_session_files(&fx.projects);
        let key = files[0].key.clone();
        assert_eq!(files[0].len, first.files[&key].file_len, "длина обязана совпасть");
        assert_eq!(
            scan::decide_action(first.files.get(&key), &files[0]),
            FileAction::Full
        );
    }

    // -----------------------------------------------------------------------
    // 5. Кэш: круговорот, версия, битый JSON
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn cache_round_trip_keeps_the_same_numbers() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "вопрос"));
        let progress = fresh_progress();
        let scanned = fx.scan(BTreeMap::new(), &progress).await;

        let snapshot = CacheSnapshot::new(scanned.files.clone(), 1_700_000_000_000);
        cache::save(&fx.cache_path, &snapshot).expect("save");
        assert!(fx.cache_path.exists(), "снимок обязан появиться на диске");

        let loaded = cache::load(&fx.cache_path).expect("снимок обязан загрузиться");
        assert_eq!(loaded.version, CACHE_VERSION);
        assert_eq!(loaded.updated_at_ms, 1_700_000_000_000);

        let mut index = AnalyticsIndex::default();
        for rollup in loaded.files.values() {
            index.add_session(rollup);
        }
        assert_eq!(digest(&index), digest(&scanned.index));

        // И главное: поднятый из кэша индекс годится как `prev` — файл без
        // изменений повторно не читается.
        let progress2 = fresh_progress();
        let after_restart = fx.scan(loaded.files, &progress2).await;
        assert_eq!(progress2.bytes_read.load(Ordering::Relaxed), 0);
        assert_eq!(digest(&after_restart.index), digest(&scanned.index));
    }

    #[tokio::test]
    async fn version_mismatch_discards_cache_and_rebuilds() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "вопрос"));
        let progress = fresh_progress();
        let scanned = fx.scan(BTreeMap::new(), &progress).await;

        // Снимок с чужой версией схемы (так выглядит кэш, записанный прошлой
        // версией devforge).
        let mut raw = serde_json::to_value(CacheSnapshot::new(scanned.files, 0)).expect("json");
        raw["version"] = serde_json::json!(CACHE_VERSION + 1);
        std::fs::create_dir_all(fx.cache_path.parent().unwrap()).expect("mkdir");
        std::fs::write(&fx.cache_path, serde_json::to_vec(&raw).unwrap()).expect("write");

        assert!(
            cache::load(&fx.cache_path).is_none(),
            "чужая версия снимка обязана игнорироваться"
        );

        // Значит `prev` пуст → файл читается заново.
        let progress2 = fresh_progress();
        let rebuilt = fx.scan(BTreeMap::new(), &progress2).await;
        assert!(progress2.bytes_read.load(Ordering::Relaxed) > 0);
        assert_eq!(digest(&rebuilt.index).human_messages, 1);
    }

    #[test]
    fn broken_cache_json_is_ignored_without_panic() {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.cache_path.parent().unwrap()).expect("mkdir");
        std::fs::write(&fx.cache_path, b"{\"version\":1,\"files\":{oops").expect("write");
        assert!(cache::load(&fx.cache_path).is_none());

        // Недочитанный файл (обрыв на середине) — тот же исход.
        std::fs::write(&fx.cache_path, b"").expect("write");
        assert!(cache::load(&fx.cache_path).is_none());

        // Отсутствующий файл — тоже не ошибка.
        assert!(cache::load(&fx.cache_path.with_file_name("nope.json")).is_none());
    }

    #[test]
    fn missing_projects_dir_yields_empty_listing() {
        assert!(scan::list_session_files(Path::new("/nonexistent/claude/projects")).is_empty());
    }

    // -----------------------------------------------------------------------
    // 6. Появление и исчезновение файлов
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn new_file_is_added_and_removed_file_drops_out() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "первая сессия"));
        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;
        assert_eq!(first.files.len(), 1);

        fx.write("sess-2", &turn(0, "вторая сессия"));
        let progress2 = fresh_progress();
        let second = fx.scan(first.files, &progress2).await;
        assert_eq!(second.files.len(), 2);
        assert_eq!(digest(&second.index).human_messages, 2);
        assert_eq!(second.index.sessions, 2);

        std::fs::remove_file(fx.session_path("sess-1")).expect("rm");
        let progress3 = fresh_progress();
        let third = fx.scan(second.files, &progress3).await;
        assert_eq!(third.files.len(), 1, "исчезнувший файл выпадает из индекса");
        assert_eq!(digest(&third.index).human_messages, 1);
    }

    #[tokio::test]
    async fn non_jsonl_files_are_ignored() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "вопрос"));
        std::fs::write(fx.projects.join("-tmp-proj").join("notes.md"), "мусор").expect("write");
        std::fs::create_dir_all(fx.projects.join("-tmp-proj").join("memory")).expect("mkdir");
        std::fs::write(
            fx.projects.join("-tmp-proj").join("memory").join("MEMORY.md"),
            "память",
        )
        .expect("write");

        let files = scan::list_session_files(&fx.projects);
        assert_eq!(files.len(), 1, "только .jsonl: {files:?}");
        assert_eq!(files[0].session_id, "sess-1");
        assert_eq!(files[0].project_dir, "-tmp-proj");
    }

    // -----------------------------------------------------------------------
    // 7. Граница дочитывания посреди хода и посреди строки
    // -----------------------------------------------------------------------

    /// Скан застал файл на середине хода: сообщение человека записано, ответ
    /// и `turn_duration` придут позже. Пауза до СЛЕДУЮЩЕГО сообщения человека
    /// обязана посчитаться ровно один раз и с правильным значением.
    #[tokio::test]
    async fn turn_split_by_read_boundary_counts_pause_once() {
        let fx = Fixture::new();
        fx.write("sess-1", &[human(0, "вопрос")]);

        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;
        assert_eq!(digest(&first.index).pauses_count, 0, "паузы ещё нет");

        // Хвост: ответ, конец хода и следующее сообщение человека через 42 с.
        fx.append(
            "sess-1",
            &[
                assistant(1_000),
                turn_end(2_000),
                human(44_000, "следующий вопрос"),
            ],
        );

        let progress2 = fresh_progress();
        let incremental = fx.scan(first.files, &progress2).await;
        let progress3 = fresh_progress();
        let full = fx.scan(BTreeMap::new(), &progress3).await;

        let d = digest(&incremental.index);
        assert_eq!(d.pauses_count, 1, "пауза посчитана ровно один раз");
        assert_eq!(d.pauses_sum, 42_000, "пауза = 44с − 2с (конец хода)");
        assert_eq!(d, digest(&full.index));
    }

    /// Файл дописывается прямо во время скана: последняя строка обрезана
    /// посередине. Оффсет обязан остаться на конце последней ПОЛНОЙ строки,
    /// чтобы хвост был дочитан заново целиком.
    #[tokio::test]
    async fn torn_last_line_is_reread_on_next_pass() {
        let fx = Fixture::new();
        let full_lines = turn(0, "вопрос");
        let mut body = full_lines[0].clone() + "\n";
        body.push_str(&full_lines[1][..20]); // оборванная запись без \n
        std::fs::write(fx.session_path("sess-1"), &body).expect("write");

        let progress = fresh_progress();
        let first = fx.scan(BTreeMap::new(), &progress).await;
        let key = fx.session_path("sess-1").to_string_lossy().into_owned();
        assert_eq!(
            first.files[&key].parse_offset,
            full_lines[0].len() as u64 + 1,
            "оффсет стоит на конце последней полной строки"
        );

        // Файл дописан до конца.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(fx.session_path("sess-1"))
            .expect("open");
        writeln!(f, "{}", &full_lines[1][20..]).expect("append");
        writeln!(f, "{}", full_lines[2]).expect("append");
        drop(f);

        let progress2 = fresh_progress();
        let incremental = fx.scan(first.files, &progress2).await;
        let progress3 = fresh_progress();
        let full = fx.scan(BTreeMap::new(), &progress3).await;
        assert_eq!(digest(&incremental.index), digest(&full.index));
        assert_eq!(digest(&incremental.index).assistant_messages, 1);
    }

    // -----------------------------------------------------------------------
    // Параллелизм и прогресс
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn progress_done_reaches_total_on_many_files() {
        let fx = Fixture::new();
        for i in 0..40 {
            fx.write(&format!("sess-{i}"), &turn(i as i64 * 1_000, "вопрос"));
        }
        let progress = fresh_progress();
        let outcome = fx.scan(BTreeMap::new(), &progress).await;

        let snapshot = progress.snapshot();
        assert_eq!(snapshot.total, 40);
        assert_eq!(snapshot.done, 40, "done обязан дойти ровно до total");
        assert_eq!(snapshot.errors, 0);
        assert!(!snapshot.running, "флаг снимается по завершении скана");
        assert_eq!(outcome.files.len(), 40);
        assert_eq!(digest(&outcome.index).human_messages, 40);
    }

    /// Замер полного скана на ЖИВОМ архиве пользователя.
    ///
    /// Помечен `#[ignore]` намеренно: тест читает `~/.claude/projects` (сотни
    /// мегабайт), в обычном прогоне это лишние секунды и зависимость от
    /// машины. Запуск вручную:
    /// `cargo test -p devforge --test analytics_cache -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "читает живой ~/.claude/projects; запускать вручную для замера"]
    async fn measure_full_scan_of_live_archive() {
        let Some(dir) = scan::claude_projects_dir() else {
            eprintln!("HOME не задан — замер пропущен");
            return;
        };
        let progress = fresh_progress();
        let outcome = scan::scan_incremental(dir, BTreeMap::new(), Arc::clone(&progress))
            .await
            .expect("скан");
        let snapshot = progress.snapshot();
        eprintln!(
            "полный скан: файлов {} (ошибок {}), прочитано {:.1} МБ за {} мс; сессий в индексе {}, дней {}",
            snapshot.total,
            snapshot.errors,
            outcome.bytes_read as f64 / 1024.0 / 1024.0,
            outcome.elapsed_ms,
            outcome.index.sessions,
            outcome.index.days.len()
        );
        assert_eq!(snapshot.done, snapshot.total, "done обязан дойти до total");
    }

    /// Второй `rescan` во время идущего скана не порождает параллельный
    /// проход: он получает `None` и обязан просто показать прогресс первого.
    #[tokio::test]
    async fn concurrent_scan_is_rejected_while_one_is_running() {
        let fx = Fixture::new();
        fx.write("sess-1", &turn(0, "вопрос"));
        let progress = fresh_progress();

        let guard = scan::try_begin(&progress).expect("первый захват");
        assert!(
            scan::scan_incremental(fx.projects.clone(), BTreeMap::new(), Arc::clone(&progress))
                .await
                .is_none(),
            "второй скан во время идущего запрещён"
        );
        drop(guard);

        // Флаг снят — скан снова возможен.
        assert!(
            scan::scan_incremental(fx.projects.clone(), BTreeMap::new(), Arc::clone(&progress))
                .await
                .is_some()
        );
    }

    // -----------------------------------------------------------------------
    // Справочные плитки из ~/.claude.json
    // -----------------------------------------------------------------------

    #[test]
    fn global_usage_is_read_from_claude_json_and_ignores_everything_else() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"userID":"secret","projects":{"/tmp":{"history":[{"display":"текст"}]}},
                "skillUsage":{"create-tasks":{"usageCount":21,"lastUsedAt":1778140620378}},
                "pluginUsage":{"a@market":{"usageCount":3799,"lastUsedAt":1}},
                "toolUsage":{"Bash":{"usageCount":2407,"lastUsedAt":2}}}"#,
        )
        .expect("write");
        let usage = scan::read_global_usage(&path).expect("usage");
        assert_eq!(usage.skill_usage["create-tasks"].usage_count, 21);
        assert_eq!(usage.plugin_usage["a@market"].usage_count, 3799);
        assert_eq!(usage.tool_usage["Bash"].usage_count, 2407);
        assert!(scan::file_mtime_ms(&path) > 0);
    }

    #[test]
    fn missing_or_broken_claude_json_yields_no_tiles_and_no_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(scan::read_global_usage(&dir.path().join("nope.json")).is_none());
        assert_eq!(scan::file_mtime_ms(&dir.path().join("nope.json")), 0);

        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ это не json").expect("write");
        assert!(scan::read_global_usage(&broken).is_none());

        // Конфиг без наших полей — валидный ответ с пустыми картами.
        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, r#"{"theme":"dark"}"#).expect("write");
        let usage = scan::read_global_usage(&empty).expect("usage");
        assert!(usage.skill_usage.is_empty() && usage.tool_usage.is_empty());
    }
}
