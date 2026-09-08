# tmux-web/tests/analytics_cache.rs

Тесты индексатора и кэша аналитики (analytics::scan / analytics::cache). 15 обычных тестов + 1 #[ignore] замер на живом архиве.

Главное утверждение набора: инкремент равен полному пересбору, и при этом байты второй раз с диска не читаются. Всё остальное (усадка файла, mtime назад, версия снимка, битый JSON) — ветки, где инкремент запрещён и обязан быть полный пересбор.

## Изоляция
Ни один тест не трогает реальные ~/.claude и ~/.forge: и каталог транскриптов, и путь снимка передаются явными аргументами во временный каталог tempfile. HOME не подменяется — подмена глобальной переменной окружения флакует при параллельном прогоне.

## Плумбинг
devforge — бинарный крейт без [lib]-таргета, поэтому модули подключаются напрямую через #[path] (та же схема, что в analytics_parse.rs): analytics::rollup тянет claude_memory::encode_project_dir, а тот — paths::resolve_root, поэтому оба модуля тоже подключены. Их собственные тесты из-за этого прогоняются повторно — это нормально. Все тесты обёрнуты в mod analytics_cache, чтобы фильтр cargo (работает по ИМЕНИ теста, а не по имени бинаря) отбирал набор и по `analytics`, и по `analytics_cache`.

## Каркас
Fixture — временный каталог claude/projects/-tmp-proj + путь снимка. Методы write (перезапись целиком), append (дописывание, как это делает CLI), shift_mtime (сдвиг времени модификации вперёд/назад через File::set_modified), scan (запуск scan_incremental). Digest — набор чисел (сообщения, токены, count/sum/p50 пауз, длительности ходов, список дней), по которым сравниваются инкремент и полный пересбор; поле-в-поле равенство роллапов проверяет отдельный тест.

## Сценарии
1. appended_lines_read_only_tail_and_match_full_rebuild — дописали ход, прочитано РОВНО столько байт, сколько занимает хвост; итог равен полному пересбору.
2. incremental_rollup_is_field_by_field_equal_to_full_rebuild — days/parse_offset/turn_state/totals совпадают побитово.
3. unchanged_file_is_not_read_again — bytes_read и files_read равны нулю, но done всё равно доходит до total.
4. shrunk_file_is_rebuilt_from_scratch — старые числа не остаются.
5. mtime_moved_backwards_forces_full_rebuild и same_length_with_newer_mtime_forces_full_rebuild — обе ветки Full проверены и через decide_action, и через реальный скан.
6. cache_round_trip_keeps_the_same_numbers — снимок сохраняется, грузится с теми же числами и годится как prev: после «рестарта» файл не читается.
7. version_mismatch_discards_cache_and_rebuilds, broken_cache_json_is_ignored_without_panic (битый JSON, пустой файл, отсутствующий файл), missing_projects_dir_yields_empty_listing.
8. new_file_is_added_and_removed_file_drops_out, non_jsonl_files_are_ignored (в том числе подкаталог memory/).
9. turn_split_by_read_boundary_counts_pause_once — регрессия на состояние автомата: скан застал файл на середине хода, пауза до следующего сообщения человека посчитана ровно один раз и равна 42 000 мс.
10. torn_last_line_is_reread_on_next_pass — оборванная последняя строка: parse_offset стоит на конце последней ПОЛНОЙ строки, хвост дочитан заново целиком.
11. progress_done_reaches_total_on_many_files (40 файлов) и concurrent_scan_is_rejected_while_one_is_running (второй скан при захваченном ScanGuard возвращает None, после drop снова возможен).
12. measure_full_scan_of_live_archive — #[ignore], читает живой ~/.claude/projects. Запуск: cargo test -p devforge --test analytics_cache -- --ignored --nocapture.
