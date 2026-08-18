Гейт фазы 5 плагина forge-meet (пост-обработка). Дата: 15.08.2026.

ЧТО ЗЕЛЁНОЕ:
- cargo build --workspace — без предупреждений;
- cargo test --workspace — 48 наборов, все ok; cargo test -p forge-meet: 338 юнит + 62 интеграционных;
- cargo tree -d — 49 дублей (столько же, сколько до фазы: reqwest и sha2 уже были в дереве);
- cargo tree -i openssl-sys — пусто; cargo tree -i libsqlite3-sys — ровно один узел.

ИНВАРИАНТЫ ФАЗЫ И ИХ ТЕСТЫ:
1. -ng/--no-gpu никогда в argv — pipeline::whisper::tests::no_gpu_flags_ever_reach_the_argv (4096 комбинаций).
2. --vad не собирается без -vm — vad_is_never_built_without_its_model.
3. Metal-гард не верит всегдашним строкам — the_guard_is_not_fooled_by_the_lines_that_always_print + runs::a_run_that_never_says_ggml_metal_init_is_killed + runs::a_fast_cpu_run_that_produced_output_is_still_rejected (быстрый CPU-прогон с готовым JSON тоже отвергается) + сквозной a_cpu_whisper_run_fails_the_meeting_with_metal_unavailable.
4. Проба и гард смотрят на РАЗНЫЕ строки — deps::tests::the_probe_and_the_guard_look_at_different_lines.
5. normalize=0 при любом числе входов — ffmpeg::tests::normalize_zero_is_present_for_any_number_of_inputs (1..12) плюс три golden на 1, 2 и 5 дорожек.
6. Детерминированный tie-break — speakers::tests::an_exact_tie_is_broken_deterministically_by_name (оба порядка входа, сто повторов).
7. Round-trip remap идемпотентен — remapping_there_and_back_is_idempotent (7 множителей, 200 сегментов, допуск 1 мс).
8. Побитово одинаковые файлы при повторном прогоне — render::tests::writing_twice_produces_byte_identical_files.
9. Автоподкачки моделей нет — models::tests::nothing_but_the_route_starts_a_download (обход исходников).
10. Без ffmpeg встреча доходит до ready с транскриптом — without_ffmpeg_the_meeting_still_reaches_ready_with_a_transcript.
11. Хвост stderr доходит до пользователя — a_failing_ffmpeg_shows_its_stderr_tail_to_the_user (meeting_events + кадр error), a_failing_whisper_shows_its_stderr_tail.
12. Whisper не стартует во время записи — whisper_waits_while_any_recording_is_still_going.
13. Очередь на один слот с честной позицией — two_transcriptions_run_one_after_another_and_the_second_sees_the_queue.
14. Отмена снимает задание из очереди — cancelling_a_meeting_takes_it_out_of_the_queue.
15. Дрейф: 0,5 с не корректируется, 2 с корректируется, без ffprobe факт пропуска виден в манифесте — три теста дрейфа.

ПРОВЕРКА СТРАЖЕЙ ПОЛОМКОЙ (обязательна по правилам проекта): замена metal_confirmed на contains("ggml_metal"), добавление -ng в build_args и снятие normalize=0 из фильтрографа дали 15 красных тестов из 45. Гарды настоящие.

ЖИВЫЕ ИЗМЕРЕНИЯ (tests/pipeline_live.rs, запуск вручную с --ignored):
- Полный пайплайн на 24 с синтезированной русской речи (say -v Milena), настоящие ffmpeg 8.0, ffprobe и whisper-cli 1.9.2: 4 сегмента, 2 спикера, атрибуция по DOM-таймлайну верна, текст верен дословно. Скорость 13,5x реального времени (Ф0 измеряла 13,3-14,7x — совпало). Весь пайплайн 2,07 с.
- Посылка Metal-гарда на УСТАНОВЛЕННОМ бинаре: GPU encode 846 мс, CPU (-ng) encode 10 699 мс, разрыв 12,6x; metal_confirmed на настоящем stderr даёт true для GPU и false для -ng, всегдашние строки ggml_metal_device_init: в обоих.
- Фильтрограф и архив на настоящем ffmpeg: golden-строка принимается, adelay=2500 даёт 12,5 с из 10 с, архив AAC 32k моно 16 кГц = 51 КиБ на 12,5 с, то есть 14,7 МБ на час (план обещал 14).

ЧЕГО НЕТ: прогона на ЖИВОЙ записи Телемоста. Фазы 4 (адаптер Телемоста) ещё не существует, бота нет, записывать нечего. Требование живой приёмки перенесено на финальный гейт Ф11 (forge-meet-f11-i55rh.5 уже зависит от forge-meet-f4-8sa7.7). Синтезированная речь не заменяет живую встречу: в ней нет перекрытий, шума, переспросов и переключений спикера в середине слова.
Также НЕ ПРОВЕРЕНО: проигрывание и перемотка архивного m4a в браузере — на машине не стартует ни один Chromium (завис системный демон Gatekeeper), ждёт перезагрузки.