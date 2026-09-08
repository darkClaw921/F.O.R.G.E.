//! Тесты ядра разбора транскриптов Claude Code (`analytics::parse` /
//! `analytics::rollup`).
//!
//! Каждое правило из раздела «Источник данных и правила вычисления» плана
//! закреплено отдельным тестом с говорящим именем. Фикстуры собираются в
//! коде во временном каталоге — живые файлы пользователя не читаются.
//!
//! `devforge` — бинарный крейт без `[lib]`-таргета, поэтому модули
//! подключаются напрямую через `#[path]`. `analytics::rollup` переиспользует
//! `claude_memory::encode_project_dir` (а тот — `paths::resolve_root`),
//! поэтому оба модуля тоже подключены: копии кода кодирования cwd нет.

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

/// Тесты собраны в модуль `analytics_parse`, чтобы их имена начинались с
/// него: и `cargo test -p devforge analytics`, и
/// `cargo test -p devforge analytics_parse` из плана отбирают ровно этот
/// набор (фильтр cargo работает по ИМЕНИ теста, а не по имени бинаря).
mod analytics_parse {
    use std::io::{BufReader, Seek, SeekFrom, Write};

    use crate::analytics::parse::{append_session, parse_session};
    use crate::analytics::rollup::{AnalyticsIndex, Bucket, DayBucket, Query, SessionRollup};

    /// База отсчёта фикстур: 2026-09-01T09:00:00.000Z (полдень MSK — далеко от
    /// границы суток в любой разумной локальной зоне).
    const BASE_MS: i64 = 1_788_339_600_000;

    fn iso(offset_ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(BASE_MS + offset_ms)
            .expect("valid ts")
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn day_of(offset_ms: i64) -> String {
        SessionRollup::day_key(BASE_MS + offset_ms)
    }

    /// Настоящее сообщение человека (контент — строка).
    fn human(offset_ms: i64, text: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/proj","gitBranch":"master","version":"2.1.227","isSidechain":false,"message":{{"role":"user","content":{}}}}}"#,
            iso(offset_ms),
            serde_json::to_string(text).unwrap()
        )
    }

    /// Ответ инструмента, приходящий записью `user` (не сообщение человека).
    fn tool_result(offset_ms: i64) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","content":"ok"}}]}}}}"#,
            iso(offset_ms)
        )
    }

    fn assistant(offset_ms: i64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false,"message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40,"output_tokens_details":{{"thinking_tokens":7}}}}}}}}"#,
            iso(offset_ms)
        )
    }

    fn assistant_with_tools(offset_ms: i64, tools: &[&str]) -> String {
        let blocks: Vec<String> = tools
            .iter()
            .map(|t| format!(r#"{{"type":"tool_use","name":"{t}","input":{{}}}}"#))
            .collect();
        format!(
            r#"{{"type":"assistant","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false,"message":{{"role":"assistant","model":"claude-opus-5","content":[{}],"usage":{{"input_tokens":1,"output_tokens":2}}}}}}"#,
            iso(offset_ms),
            blocks.join(",")
        )
    }

    fn turn_end(offset_ms: i64) -> String {
        format!(
            r#"{{"type":"system","subtype":"turn_duration","durationMs":123,"timestamp":"{}","cwd":"/tmp/proj","isSidechain":false}}"#,
            iso(offset_ms)
        )
    }

    fn parse_lines(lines: &[String]) -> SessionRollup {
        let data = lines.join("\n") + "\n";
        parse_session(BufReader::new(data.as_bytes()), "sess-1", 0).expect("parse")
    }

    fn total_human(r: &SessionRollup) -> u64 {
        r.days.values().map(|d| d.human_messages).sum()
    }

    // ---------------------------------------------------------------------------
    // Правило: настоящее сообщение человека
    // ---------------------------------------------------------------------------

    #[test]
    fn is_meta_user_record_is_not_human_message() {
        let meta = format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/proj","isMeta":true,"isSidechain":false,"message":{{"role":"user","content":"Caveat: местная команда"}}}}"#,
            iso(0)
        );
        let r = parse_lines(&[meta, human(1_000, "настоящее сообщение")]);
        assert_eq!(total_human(&r), 1, "isMeta-запись не должна считаться");
    }

    #[test]
    fn sidechain_records_are_not_human_messages_but_count_as_subagent() {
        let side_user = format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/proj","isSidechain":true,"message":{{"role":"user","content":"задача субагенту"}}}}"#,
            iso(1_000)
        );
        let side_assistant = format!(
            r#"{{"type":"assistant","timestamp":"{}","cwd":"/tmp/proj","isSidechain":true,"message":{{"role":"assistant","model":"claude-fable-5","content":[{{"type":"text","text":"ok"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#,
            iso(2_000)
        );
        let agent = r#"{"type":"agent-name","agentName":"Explore","sessionId":"sess-1"}"#.to_string();

        let r = parse_lines(&[human(0, "поехали"), side_user, side_assistant, agent]);
        assert_eq!(total_human(&r), 1, "sidechain не считается сообщением человека");
        let day = &r.days[&day_of(0)];
        assert_eq!(day.subagent_records, 2);
        assert_eq!(day.subagent_starts, 1);
        assert_eq!(day.agents.get("Explore").copied(), Some(1));
    }

    #[test]
    fn tool_result_content_is_not_human_message() {
        // Контрольная пропорция из плана: настоящих сообщений мало, ответов
        // инструментов — на порядок больше.
        let mut lines = Vec::new();
        for i in 0..3 {
            lines.push(human(i * 100_000, "реальный вопрос"));
            for j in 0..20 {
                lines.push(tool_result(i * 100_000 + 1_000 + j * 10));
            }
        }
        let r = parse_lines(&lines);
        assert_eq!(
            total_human(&r),
            3,
            "20 tool_result на сообщение не должны раздувать метрику"
        );
    }

    // ---------------------------------------------------------------------------
    // Правило: конец хода и fallback
    // ---------------------------------------------------------------------------

    #[test]
    fn turn_without_turn_duration_falls_back_to_last_assistant() {
        // Ход №1 — с записью turn_duration, ход №2 — без неё.
        let r = parse_lines(&[
            human(0, "первый"),
            assistant(3_000),
            turn_end(4_000),
            human(10_000, "второй"),
            assistant(15_000),
            human(19_000, "третий"),
        ]);
        let day = &r.days[&day_of(0)];
        assert_eq!(day.turn_ms.count, 2, "оба хода закрыты");
        // Ход 1: 4000 − 0; ход 2 (fallback по последнему assistant): 15000 − 10000.
        assert_eq!(day.turn_ms.sum, 4_000 + 5_000);
        // Паузы: 10000 − 4000 = 6000 и 19000 − 15000 = 4000.
        assert_eq!(day.pauses.count, 2);
        assert_eq!(day.pauses.sum, 6_000 + 4_000);
        assert_eq!(day.human_ms, 10_000);
    }

    #[test]
    fn turn_duration_record_wins_over_assistant_fallback() {
        let r = parse_lines(&[
            human(0, "вопрос"),
            assistant(3_000),
            turn_end(8_000),
            human(10_000, "ещё"),
        ]);
        let day = &r.days[&day_of(0)];
        assert_eq!(day.turn_ms.sum, 8_000, "конец хода берётся из turn_duration");
        assert_eq!(day.pauses.sum, 2_000, "пауза считается от turn_duration");
    }

    // ---------------------------------------------------------------------------
    // Правило: паузы
    // ---------------------------------------------------------------------------

    #[test]
    fn long_pause_goes_to_away_count_and_is_excluded_from_percentiles() {
        let forty_five_min = 45 * 60 * 1_000;
        let r = parse_lines(&[
            human(0, "первый"),
            assistant(1_000),
            turn_end(2_000),
            // Ушёл на 45 минут — «отошёл».
            human(2_000 + forty_five_min, "вернулся"),
            assistant(2_000 + forty_five_min + 500),
            turn_end(2_000 + forty_five_min + 1_000),
            // Ответил через 4 секунды — «не читая».
            human(2_000 + forty_five_min + 5_000, "да"),
        ]);
        let day = &r.days[&day_of(0)];
        assert_eq!(day.away_count, 1, "45-минутная пауза — это «отошёл»");
        assert_eq!(day.pauses.count, 1, "в распределение попала только короткая пауза");
        assert_eq!(day.pauses.p50(), Some(4_000), "медиана не испорчена «отошёл»");
        assert_eq!(day.pause_buckets[0], 1, "4 c попали в бакет <5 c");
        assert_eq!(day.quick_replies, 1, "4 c — ответ «не читая» (<10 c)");
        assert_eq!(day.human_ms, 4_000, "человеко-время без «отошёл»");
    }

    #[test]
    fn pause_buckets_cover_all_five_ranges() {
        assert_eq!(DayBucket::pause_bucket_index(4_999), 0);
        assert_eq!(DayBucket::pause_bucket_index(5_000), 1);
        assert_eq!(DayBucket::pause_bucket_index(14_999), 1);
        assert_eq!(DayBucket::pause_bucket_index(15_000), 2);
        assert_eq!(DayBucket::pause_bucket_index(59_999), 2);
        assert_eq!(DayBucket::pause_bucket_index(60_000), 3);
        assert_eq!(DayBucket::pause_bucket_index(299_999), 3);
        assert_eq!(DayBucket::pause_bucket_index(300_000), 4);
    }

    #[test]
    fn percentiles_are_computed_from_stored_samples_not_from_display_buckets() {
        let mut lines = Vec::new();
        // Паузы 1..10 секунд — все в разных отображаемых бакетах их нет,
        // p50/p90 обязаны различать их точно.
        for i in 0..10i64 {
            let base = i * 100_000;
            lines.push(human(base, "q"));
            lines.push(assistant(base + 500));
            lines.push(turn_end(base + 1_000));
            lines.push(human(base + 1_000 + (i + 1) * 1_000, "a"));
        }
        let r = parse_lines(&lines);
        let day = &r.days[&day_of(0)];
        // Сэмплы: 1000..10000 плюс паузы между «a» и следующим «q» — их нет,
        // потому что «a» открывает новый ход, который закрывается следующим
        // сообщением по fallback (assistant отсутствует) — то есть не закрывается.
        assert_eq!(day.pauses.count, 10);
        // Квантиль по ближайшему рангу: индекс round((n-1)*q) = round(4.5) = 5.
        assert_eq!(day.pauses.p50(), Some(6_000));
        assert_eq!(day.pauses.p90(), Some(9_000));
    }

    // ---------------------------------------------------------------------------
    // Правило: cost-state кумулятивен
    // ---------------------------------------------------------------------------

    #[test]
    fn two_cost_state_records_take_the_last_not_the_sum() {
        let first = r#"{"type":"cost-state","sessionId":"sess-1","totalCostUSD":1.5,"totalAPIDuration":1000,"totalToolDuration":500,"totalLinesAdded":10,"totalLinesRemoved":2,"startTime":1788339600000,"modelUsage":{"claude-opus-5":{"inputTokens":1,"outputTokens":2,"costUSD":1.5}}}"#;
        let second = r#"{"type":"cost-state","sessionId":"sess-1","totalCostUSD":4.25,"totalAPIDuration":3000,"totalToolDuration":1500,"totalLinesAdded":30,"totalLinesRemoved":6,"startTime":1788339600000,"modelUsage":{"claude-opus-5":{"inputTokens":3,"outputTokens":6,"costUSD":4.25}}}"#;
        let r = parse_lines(&[
            human(0, "поехали"),
            assistant(1_000),
            first.to_string(),
            second.to_string(),
        ]);
        assert!(r.totals.present);
        assert_eq!(r.totals.total_cost_usd, 4.25, "итог = последняя запись, не сумма");
        assert_eq!(r.totals.total_api_duration_ms, 3_000);
        assert_eq!(r.totals.total_tool_duration_ms, 1_500);
        assert_eq!(r.totals.lines_added, 30);
        assert_eq!(r.totals.lines_removed, 6);
        assert_eq!(
            r.totals.model_usage["claude-opus-5"].cost_usd, 4.25,
            "стоимость по моделям — из последней cost-state"
        );

        // Итоги разложены по дням; сессия однодневная — всё на один день.
        let day = &r.days[&day_of(0)];
        assert_eq!(day.cost_usd, 4.25);
        assert_eq!(day.lines_added, 30);
        assert_eq!(day.tool_ms, 1_500);
        assert_eq!(day.api_ms, 3_000);
        assert_eq!(day.models["claude-opus-5"].cost_usd, 4.25);
    }

    // ---------------------------------------------------------------------------
    // Правило: устойчивость к битым строкам
    // ---------------------------------------------------------------------------

    #[test]
    fn broken_line_in_the_middle_is_skipped_and_the_rest_is_parsed() {
        let data = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            human(0, "первый"),
            // Усечённая запись: префильтр её пропустит, serde — сломается.
            r#"{"type":"assistant","timestamp":"2026-09-01T09:00:0"#,
            assistant(2_000),
            turn_end(3_000),
            human(9_000, "второй"),
        );
        let r = parse_session(BufReader::new(data.as_bytes()), "sess-1", 0).expect("parse");
        assert_eq!(r.skipped_lines, 1, "битая строка учтена");
        assert_eq!(total_human(&r), 2, "остальные записи разобраны");
        assert_eq!(r.days[&day_of(0)].pauses.sum, 6_000);
    }

    #[test]
    fn parse_offset_points_at_the_end_of_the_last_complete_line() {
        let complete = format!("{}\n{}\n", human(0, "раз"), assistant(1_000));
        // Хвост без перевода строки — файл дописывается прямо сейчас.
        let data = format!("{complete}{{\"type\":\"user\",\"timesta");
        let r = parse_session(BufReader::new(data.as_bytes()), "sess-1", 0).expect("parse");
        assert_eq!(
            r.parse_offset,
            complete.len() as u64,
            "оффсет — конец последней ПОЛНОЙ строки"
        );
        assert_eq!(r.skipped_lines, 1, "недописанный хвост учтён как пропуск");
    }

    // ---------------------------------------------------------------------------
    // Правило: токены, инструменты, прерывания, компакты, версии, проект
    // ---------------------------------------------------------------------------

    #[test]
    fn tokens_tools_interruptions_and_compacts_are_counted() {
        let interrupted = format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","content":"stopped"}}]}},"toolUseResult":{{"interrupted":true}}}}"#,
            iso(4_000)
        );
        let compact = format!(
            r#"{{"type":"system","subtype":"compact_boundary","timestamp":"{}","cwd":"/tmp/proj","isSidechain":false}}"#,
            iso(5_000)
        );
        let r = parse_lines(&[
            human(0, "поехали"),
            assistant(1_000),
            assistant_with_tools(2_000, &["Bash", "Read"]),
            assistant_with_tools(3_000, &["Bash"]),
            interrupted,
            compact,
        ]);
        let day = &r.days[&day_of(0)];

        assert_eq!(day.tokens_in, 10 + 1 + 1);
        assert_eq!(day.tokens_out, 20 + 2 + 2);
        assert_eq!(day.cache_read, 30);
        assert_eq!(day.cache_create, 40);
        assert_eq!(day.thinking, 7);

        assert_eq!(day.tool_calls, 3);
        assert_eq!(day.tools["Bash"].calls, 2);
        assert_eq!(day.tools["Read"].calls, 1);
        assert_eq!(day.interruptions, 1);
        assert_eq!(day.compacts, 1);

        assert_eq!(day.models["claude-opus-5"].messages, 3);
        assert!(day.versions.contains("2.1.227"), "версия CLI попала в день");
        assert!(r.cli_versions.contains("2.1.227"));
    }

    #[test]
    fn message_length_is_measured_in_characters() {
        let r = parse_lines(&[human(0, "привет"), human(1_000, "ok")]);
        let day = &r.days[&day_of(0)];
        assert_eq!(day.msg_lens.count, 2);
        assert_eq!(day.msg_lens.sum, 6 + 2, "длина в символах, не в байтах");
    }

    #[test]
    fn project_dir_uses_the_claude_code_encoding_from_claude_memory() {
        let r = parse_lines(&[human(0, "hi")]);
        assert_eq!(r.cwd.as_deref(), Some("/tmp/proj"));
        assert_eq!(
            r.project_dir.as_deref(),
            Some(crate::claude_memory::encode_project_dir(std::path::Path::new("/tmp/proj")).as_str()),
            "имя каталога кодируется той же функцией, что и в claude_memory"
        );
    }

    #[test]
    fn ai_title_is_taken_from_the_ai_title_record() {
        let r = parse_lines(&[
            human(0, "hi"),
            r#"{"type":"ai-title","aiTitle":"Первый вариант","sessionId":"sess-1"}"#.to_string(),
            r#"{"type":"ai-title","aiTitle":"Уточнённый заголовок","sessionId":"sess-1"}"#.to_string(),
        ]);
        assert_eq!(r.ai_title.as_deref(), Some("Уточнённый заголовок"));
    }

    // ---------------------------------------------------------------------------
    // rollup.rs: слияние, дочитывание, выборка
    // ---------------------------------------------------------------------------

    #[test]
    fn day_bucket_merge_sums_counters_and_distributions() {
        let r = parse_lines(&[
            human(0, "раз"),
            assistant(1_000),
            turn_end(2_000),
            human(6_000, "два"),
        ]);
        let src = r.days[&day_of(0)].clone();
        let mut acc = src.clone();
        acc.merge(&src);

        assert_eq!(acc.human_messages, src.human_messages * 2);
        assert_eq!(acc.tokens_out, src.tokens_out * 2);
        assert_eq!(acc.pauses.count, src.pauses.count * 2);
        assert_eq!(acc.pauses.sum, src.pauses.sum * 2);
        assert_eq!(acc.pause_buckets[1], src.pause_buckets[1] * 2);
        assert_eq!(acc.pauses.p50(), src.pauses.p50(), "медиана удвоения та же");
    }

    #[test]
    fn append_across_a_boundary_that_cuts_a_turn_in_half_loses_nothing() {
        let lines = vec![
            human(0, "раз"),
            assistant(1_000),
            turn_end(2_000),
            human(6_000, "два"),
            assistant(7_000),
            turn_end(9_000),
            human(12_000, "три"),
            assistant(13_000),
            turn_end(14_000),
            human(20_000, "четыре"),
        ];
        let data = lines.join("\n") + "\n";

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sess-1.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(data.as_bytes()).expect("write");
        drop(f);

        let full = parse_session(
            BufReader::new(std::fs::File::open(&path).expect("open")),
            "sess-1",
            0,
        )
        .expect("full parse");

        // Первая порция обрывается ровно посреди хода (после assistant, до
        // turn_duration четвёртой строки).
        let cut: usize = lines[..5].iter().map(|l| l.len() + 1).sum();
        let head = &data.as_bytes()[..cut];
        let mut partial =
            parse_session(BufReader::new(head), "sess-1", 0).expect("head parse");
        assert_eq!(partial.parse_offset, cut as u64);

        let mut file = std::fs::File::open(&path).expect("open");
        file.seek(SeekFrom::Start(partial.parse_offset)).expect("seek");
        append_session(&mut partial, BufReader::new(file)).expect("append");

        assert_eq!(partial.parse_offset, data.len() as u64);
        assert_eq!(partial.days, full.days, "дочитывание = разбор за один проход");
        assert_eq!(partial.totals, full.totals);
        assert_eq!(partial.turn_state, full.turn_state);
    }

    #[test]
    fn index_query_filters_by_project_and_date_range() {
        let a = parse_lines(&[human(0, "раз"), assistant(1_000), turn_end(2_000)]);

        let other_cwd = format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/tmp/other","isSidechain":false,"message":{{"role":"user","content":"чужой проект"}}}}"#,
            iso(0)
        );
        let b = parse_session(
            BufReader::new((other_cwd + "\n").as_bytes()),
            "sess-2",
            0,
        )
        .expect("parse");

        let mut index = AnalyticsIndex::default();
        index.add_session(&a);
        index.add_session(&b);
        assert_eq!(index.sessions, 2);
        assert_eq!(index.projects.len(), 2);

        let all = index.query(&Query::default());
        assert_eq!(all.len(), 1, "обе сессии в одном дне");
        assert_eq!(all[0].human_messages, 2);

        let only_proj = index.query(&Query {
            project: a.project_dir.clone(),
            ..Default::default()
        });
        assert_eq!(only_proj[0].human_messages, 1, "фильтр по проекту");

        let empty = index.query(&Query {
            from: Some("2099-01-01".to_string()),
            ..Default::default()
        });
        assert!(empty.is_empty(), "диапазон дат отсекает всё");

        let weekly = index.query(&Query {
            bucket: Bucket::Week,
            ..Default::default()
        });
        assert_eq!(weekly.len(), 1);
        let monday = weekly[0].date.clone();
        assert!(monday <= all[0].date, "ключ недели — понедельник этой недели");

        let total = index.total(&Query::default());
        assert_eq!(total.human_messages, 2);
        assert_eq!(total.sessions_started, 2);
    }

    #[test]
    fn session_totals_are_split_across_days_and_stay_idempotent() {
        let day_ms = 24 * 60 * 60 * 1_000;
        let cost = r#"{"type":"cost-state","sessionId":"sess-1","totalCostUSD":10.0,"totalAPIDuration":1000,"totalToolDuration":800,"totalLinesAdded":100,"totalLinesRemoved":20}"#;
        let r = parse_lines(&[
            human(0, "день один"),
            assistant(1_000),
            human(day_ms, "день два"),
            assistant(day_ms + 1_000),
            assistant(day_ms + 2_000),
            cost.to_string(),
        ]);

        let d1 = &r.days[&day_of(0)];
        let d2 = &r.days[&day_of(day_ms)];
        // Веса: 1 и 2 записи ассистента → 1/3 и 2/3.
        assert!((d1.cost_usd - 10.0 / 3.0).abs() < 1e-9);
        assert!((d1.cost_usd + d2.cost_usd - 10.0).abs() < 1e-9);
        assert_eq!(d1.lines_added + d2.lines_added, 100);
        assert_eq!(d1.tool_ms + d2.tool_ms, 800);
        assert_eq!(d1.sessions_started, 1, "сессия засчитана один раз — в первый день");
        assert_eq!(d2.sessions_started, 0);

        // Повторное распределение ничего не удваивает.
        let mut again = r.clone();
        again.redistribute_totals();
        assert_eq!(again.days, r.days);
    }

    #[test]
    fn samples_fall_back_to_histogram_after_the_exact_limit() {
        use crate::analytics::rollup::{Samples, MAX_KEPT_SAMPLES};
        let mut s = Samples::default();
        for i in 0..(MAX_KEPT_SAMPLES as u32 + 1_000) {
            s.push(i % 10_000);
        }
        assert_eq!(s.count as usize, MAX_KEPT_SAMPLES + 1_000);
        assert_eq!(s.kept.len(), MAX_KEPT_SAMPLES);
        let p50 = s.p50().expect("p50");
        // Заявленная погрешность логарифмической гистограммы — не хуже ~13 %.
        let exact = 5_000f64;
        assert!(
            (p50 as f64 - exact).abs() / exact < 0.13,
            "оценка p50 по гистограмме вышла за заявленную погрешность: {p50}"
        );
    }

    #[test]
    fn uninteresting_records_are_filtered_before_json_parsing() {
        use crate::analytics::parse::interesting_line;
        assert!(!interesting_line(
            r#"{"type":"attachment","content":{"kind":"file"}}"#
        ));
        assert!(!interesting_line(
            r#"{"type":"file-history-snapshot","messageId":"x"}"#
        ));
        assert!(interesting_line(r#"{"type":"user","message":{}}"#));
        assert!(interesting_line(r#"{"type": "cost-state"}"#));
    }

    #[test]
    fn attachment_and_snapshot_records_do_not_break_the_parse() {
        let r = parse_lines(&[
            r#"{"type":"attachment","attachment":{"type":"queued_command","text":"/x"}}"#.to_string(),
            r#"{"type":"file-history-snapshot","messageId":"m","snapshot":{"trackedFileBackups":{}}}"#
                .to_string(),
            human(0, "hi"),
        ]);
        assert_eq!(r.skipped_lines, 0);
        assert_eq!(r.parsed_lines, 1, "неинтересные строки даже не парсились");
        assert_eq!(total_human(&r), 1);
    }

    // ---------------------------------------------------------------------------
    // Правило: метрики разнообразия (чарт 11) — TTR, повторы, энтропия,
    // слэш-команды, односложные подтверждения
    // ---------------------------------------------------------------------------

    #[test]
    fn one_word_confirmation_counts_as_ack_and_slash_command_does_not() {
        let r = parse_lines(&[
            human(0, "ок"),
            human(1_000, "/clear"),
            human(2_000, "Перепиши разбор транскриптов на потоковый режим"),
        ]);
        let d = &r.days[&day_of(0)];
        assert_eq!(d.human_messages, 3);
        assert_eq!(d.one_word_acks, 1, "«ок» — односложное подтверждение");
        assert_eq!(d.slash_commands, 1, "«/clear» — слэш-команда");
        assert!(
            (d.ack_share() - 1.0 / 3.0).abs() < 1e-9,
            "доля подтверждений {}",
            d.ack_share()
        );
        assert!((d.slash_share() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn ttr_counts_distinct_words_over_all_words_of_the_day() {
        // 6 слов, из них 3 различных: «да» повторено трижды в разных формах.
        let r = parse_lines(&[
            human(0, "Да, да, ДА!"),
            human(1_000, "разбор транскриптов да"),
        ]);
        let d = &r.days[&day_of(0)];
        assert_eq!(d.words_total, 6);
        assert_eq!(d.words_distinct.estimate(), 3, "различных слов ровно три");
        assert!((d.ttr() - 0.5).abs() < 1e-9, "TTR = 3/6, а вышло {}", d.ttr());
    }

    #[test]
    fn repeated_prompts_are_counted_ignoring_case_and_punctuation() {
        let r = parse_lines(&[
            human(0, "продолжай"),
            human(1_000, "Продолжай!"),
            human(2_000, "останови"),
        ]);
        let d = &r.days[&day_of(0)];
        assert_eq!(d.prompts_counted, 3);
        assert_eq!(d.prompt_hashes.len(), 2, "различных промптов два");
        assert!(
            (d.repeat_share() - 1.0 / 3.0).abs() < 1e-9,
            "доля повторов {}",
            d.repeat_share()
        );
    }

    #[test]
    fn tool_entropy_is_zero_for_one_tool_and_log2n_for_uniform_use() {
        let one = parse_lines(&[human(0, "hi"), assistant_with_tools(1_000, &["Bash", "Bash"])]);
        assert_eq!(one.days[&day_of(0)].tool_entropy_bits(), 0.0);

        let four = parse_lines(&[
            human(0, "hi"),
            assistant_with_tools(1_000, &["Bash", "Read", "Edit", "Grep"]),
        ]);
        let h = four.days[&day_of(0)].tool_entropy_bits();
        assert!((h - 2.0).abs() < 1e-9, "log2(4) = 2, а вышло {h}");
    }

    #[test]
    fn diversity_metrics_survive_merging_of_two_sessions_of_the_same_day() {
        let a = parse_lines(&[human(0, "продолжай"), human(1_000, "ок")]);
        let b = parse_lines(&[human(2_000, "Продолжай."), human(3_000, "разбор")]);

        let mut merged = DayBucket::new(&day_of(0));
        merged.merge(&a.days[&day_of(0)]);
        merged.merge(&b.days[&day_of(0)]);

        assert_eq!(merged.human_messages, 4);
        assert_eq!(merged.one_word_acks, 3, "«ок» и оба «продолжай» — подтверждения");
        assert_eq!(merged.words_total, 4);
        assert_eq!(merged.words_distinct.estimate(), 3);
        // «продолжай» встретился в двух РАЗНЫХ сессиях одного дня — повтор
        // обязан быть виден после слияния.
        assert_eq!(merged.prompt_hashes.len(), 3);
        assert!((merged.repeat_share() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn distinct_word_estimate_stays_within_the_declared_error_on_a_large_vocabulary() {
        use crate::analytics::rollup::Distinct;
        let mut d = Distinct::default();
        for i in 0..20_000u64 {
            d.add_hash(crate::analytics::text::hash64(&format!("слово-{i}")));
        }
        let est = d.estimate() as f64;
        assert!(
            (est - 20_000.0).abs() / 20_000.0 < 0.10,
            "оценка числа различных слов вышла за заявленную погрешность: {est}"
        );
    }

    #[test]
    fn analytics_does_not_keep_message_text_anywhere_in_the_rollup() {
        let secret = "секретный-пароль-hunter2";
        let r = parse_lines(&[human(0, secret)]);
        let json = serde_json::to_string(&r).expect("serialize rollup");
        assert!(
            !json.contains("hunter2") && !json.contains("секретный"),
            "текст сообщения человека не должен попадать ни в снимок кэша, ни в API"
        );
    }

    #[test]
    fn night_messages_are_counted_by_local_hours_00_06() {
        use chrono::{TimeZone, Timelike};
        // Берём момент, который в ЛОКАЛЬНОЙ зоне машины заведомо ночной.
        let night_ts = {
            let mut ts = BASE_MS;
            loop {
                let dt = chrono::Local::now()
                    .timezone()
                    .timestamp_millis_opt(ts)
                    .single()
                    .expect("local ts");
                if dt.hour() < 6 {
                    break ts;
                }
                ts += 3_600_000;
            }
        };
        let mut r = SessionRollup::new("night");
        r.on_human_message(night_ts, &crate::analytics::text::classify("ок"));
        let day = SessionRollup::day_key(night_ts);
        assert_eq!(r.days[&day].night_messages(), 1);
    }

    // ---------------------------------------------------------------------------
    // Правило: три роли записи `user` (найдено на живой приёмке Фазы 7)
    // ---------------------------------------------------------------------------

    #[test]
    fn interruption_record_is_counted_as_interruption_not_as_a_human_message() {
        let r = parse_lines(&[
            human(0, "сделай X"),
            assistant(1_000),
            human(2_000, "[Request interrupted by user for tool use]"),
            human(3_000, "стоп, не так"),
        ]);
        let d = &r.days[&day_of(0)];
        assert_eq!(d.human_messages, 2, "прерывание — не сообщение человека");
        assert_eq!(d.interruptions, 1);
    }

    #[test]
    fn local_command_output_is_not_a_human_message() {
        let r = parse_lines(&[
            human(0, "<local-command-stdout>ветка master</local-command-stdout>"),
            human(1_000, "настоящее сообщение"),
        ]);
        assert_eq!(total_human(&r), 1, "вывод слэш-команды не считается репликой");
    }

    #[test]
    fn slash_command_markup_is_a_human_message_and_counts_as_slash() {
        let r = parse_lines(&[human(
            0,
            "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>",
        )]);
        let d = &r.days[&day_of(0)];
        assert_eq!(d.human_messages, 1);
        assert_eq!(d.slash_commands, 1, "слэш-команда в разметке CLI");
        assert_eq!(d.one_word_acks, 0);
        assert_eq!(d.words_total, 1, "в словарь TTR попало только имя команды");
    }

    #[test]
    fn an_interruption_does_not_produce_a_fake_pause() {
        // Ход: человек пишет → ассистент → turn_duration → прерывание →
        // настоящее сообщение. Пауза должна считаться до НАСТОЯЩЕГО
        // сообщения, а не до прерывания.
        let r = parse_lines(&[
            human(0, "сделай X"),
            assistant(1_000),
            turn_end(2_000),
            human(3_000, "[Request interrupted by user]"),
            human(10_000, "теперь по-другому"),
        ]);
        let d = &r.days[&day_of(0)];
        assert_eq!(d.pauses.count, 1, "ровно одна пауза");
        assert_eq!(d.pauses.p50(), Some(8_000), "пауза до настоящего сообщения");
    }
}
