# tmux-web/tests/analytics_parse.rs

Тесты ядра разбора транскриптов Claude Code (22 теста). Каждое правило из раздела «Источник данных и правила вычисления» плана закреплено отдельным тестом с говорящим именем. Фикстуры собираются прямо в коде (строки JSONL + tempfile) — живые файлы пользователя НЕ читаются.

Плумбинг: devforge — бинарный крейт без [lib]-таргета, поэтому модули подключаются через #[path]: ../src/paths.rs, ../src/claude_memory.rs (нужен для encode_project_dir) и ../src/analytics/mod.rs. Все тесты обёрнуты в mod analytics_parse, чтобы их ИМЕНА начинались с него: фильтр cargo работает по имени теста, а не по имени тестового бинаря, поэтому и 
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out; finished in 0.00s


running 22 tests
test analytics_parse::is_meta_user_record_is_not_human_message ... ok
test analytics_parse::attachment_and_snapshot_records_do_not_break_the_parse ... ok
test analytics_parse::ai_title_is_taken_from_the_ai_title_record ... ok
test analytics_parse::pause_buckets_cover_all_five_ranges ... ok
test analytics_parse::message_length_is_measured_in_characters ... ok
test analytics_parse::broken_line_in_the_middle_is_skipped_and_the_rest_is_parsed ... ok
test analytics_parse::index_query_filters_by_project_and_date_range ... ok
test analytics_parse::long_pause_goes_to_away_count_and_is_excluded_from_percentiles ... ok
test analytics_parse::day_bucket_merge_sums_counters_and_distributions ... ok
test analytics_parse::parse_offset_points_at_the_end_of_the_last_complete_line ... ok
test analytics_parse::project_dir_uses_the_claude_code_encoding_from_claude_memory ... ok
test analytics_parse::sidechain_records_are_not_human_messages_but_count_as_subagent ... ok
test analytics_parse::session_totals_are_split_across_days_and_stay_idempotent ... ok
test analytics_parse::tokens_tools_interruptions_and_compacts_are_counted ... ok
test analytics_parse::turn_duration_record_wins_over_assistant_fallback ... ok
test analytics_parse::uninteresting_records_are_filtered_before_json_parsing ... ok
test analytics_parse::turn_without_turn_duration_falls_back_to_last_assistant ... ok
test analytics_parse::samples_fall_back_to_histogram_after_the_exact_limit ... ok
test analytics_parse::append_across_a_boundary_that_cuts_a_turn_in_half_loses_nothing ... ok
test analytics_parse::two_cost_state_records_take_the_last_not_the_sum ... ok
test analytics_parse::percentiles_are_computed_from_stored_samples_not_from_display_buckets ... ok
test analytics_parse::tool_result_content_is_not_human_message ... ok

test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 35 filtered out; finished in 0.00s, и 
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out; finished in 0.00s


running 22 tests
test analytics_parse::attachment_and_snapshot_records_do_not_break_the_parse ... ok
test analytics_parse::ai_title_is_taken_from_the_ai_title_record ... ok
test analytics_parse::message_length_is_measured_in_characters ... ok
test analytics_parse::parse_offset_points_at_the_end_of_the_last_complete_line ... ok
test analytics_parse::is_meta_user_record_is_not_human_message ... ok
test analytics_parse::broken_line_in_the_middle_is_skipped_and_the_rest_is_parsed ... ok
test analytics_parse::long_pause_goes_to_away_count_and_is_excluded_from_percentiles ... ok
test analytics_parse::pause_buckets_cover_all_five_ranges ... ok
test analytics_parse::day_bucket_merge_sums_counters_and_distributions ... ok
test analytics_parse::project_dir_uses_the_claude_code_encoding_from_claude_memory ... ok
test analytics_parse::index_query_filters_by_project_and_date_range ... ok
test analytics_parse::session_totals_are_split_across_days_and_stay_idempotent ... ok
test analytics_parse::turn_duration_record_wins_over_assistant_fallback ... ok
test analytics_parse::sidechain_records_are_not_human_messages_but_count_as_subagent ... ok
test analytics_parse::tokens_tools_interruptions_and_compacts_are_counted ... ok
test analytics_parse::samples_fall_back_to_histogram_after_the_exact_limit ... ok
test analytics_parse::uninteresting_records_are_filtered_before_json_parsing ... ok
test analytics_parse::two_cost_state_records_take_the_last_not_the_sum ... ok
test analytics_parse::turn_without_turn_duration_falls_back_to_last_assistant ... ok
test analytics_parse::percentiles_are_computed_from_stored_samples_not_from_display_buckets ... ok
test analytics_parse::append_across_a_boundary_that_cuts_a_turn_in_half_loses_nothing ... ok
test analytics_parse::tool_result_content_is_not_human_message ... ok

test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 27 filtered out; finished in 0.00s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 35 filtered out; finished in 0.00s отбирают ровно этот набор.

Покрытые правила:
- is_meta_user_record_is_not_human_message
- sidechain_records_are_not_human_messages_but_count_as_subagent (+ agent-name → subagent_starts)
- tool_result_content_is_not_human_message (контрольная пропорция 3 настоящих против 20 tool_result на каждое)
- turn_without_turn_duration_falls_back_to_last_assistant / turn_duration_record_wins_over_assistant_fallback
- long_pause_goes_to_away_count_and_is_excluded_from_percentiles (45 мин → away_count; 4 c → бакет <5 c и quick_replies)
- pause_buckets_cover_all_five_ranges, percentiles_are_computed_from_stored_samples_not_from_display_buckets
- two_cost_state_records_take_the_last_not_the_sum (+ modelUsage из последней)
- broken_line_in_the_middle_is_skipped_and_the_rest_is_parsed, parse_offset_points_at_the_end_of_the_last_complete_line
- tokens_tools_interruptions_and_compacts_are_counted, message_length_is_measured_in_characters
- project_dir_uses_the_claude_code_encoding_from_claude_memory, ai_title_is_taken_from_the_ai_title_record
- day_bucket_merge_sums_counters_and_distributions, append_across_a_boundary_that_cuts_a_turn_in_half_loses_nothing (дочитывание == разбор за один проход)
- session_totals_are_split_across_days_and_stay_idempotent, samples_fall_back_to_histogram_after_the_exact_limit
- uninteresting_records_are_filtered_before_json_parsing, attachment_and_snapshot_records_do_not_break_the_parse

База отсчёта фикстур BASE_MS = 2026-09-01T09:00:00.000Z (полдень MSK) — далеко от границы суток в любой разумной локальной зоне; ожидаемый ключ дня берётся через SessionRollup::day_key, чтобы тест не зависел от TZ машины.
