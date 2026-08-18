# pipeline

Пост-обработка встречи: сквозной путь от пер-трековых WAV до transcript.md.

Состав (plugins/meet/src/pipeline/):
- proc.rs — запуск внешних процессов: таймаут, гарантированное убийство (kill_on_drop + явный kill), ХВОСТ STDERR (20 строк, кольцевой буфер) до пользователя, а не в лог сервера. run_watched даёт наблюдателю право снять процесс на конкретной строке — на этом стоит Metal-гард.
- ffmpeg.rs — build_mixdown_filtergraph (чистая), build_mixdown_args, build_archive_args, build_probe_args, mix_tracks_in_memory (сведение БЕЗ ffmpeg), probe_duration_ms.
- whisper.rs — build_args (чистая) и run с Metal-гардом, разбор -ojf.
- speakers.rs — intervals_from_events, prepare_intervals, attribute_segments, merge_tokens_to_words, merge_into_paragraphs, remap_segment_times, remap_intervals, drift_scale.
- render.rs — transcript.json и transcript.md, оба атомарно.
- run.rs — оркестрация: сведение, архив, дрейф, гейт записи, очередь whisper, атрибуция, рендер, база, чистка work/.

Порядок этапов и статусы: transcoding (сведение + архив + дрейф) → transcribing (ожидание конца записей → очередь whisper → прогон) → ready. Провал любого этапа: meeting_events с хвостом stderr, кадр error, статус failed.

Ключевые решения фазы:
1) whisper не запускается, пока идёт ЛЮБАЯ запись: Chrome рендерит встречу на том же GPU. Гейт — опрос hub.active_meetings с периодом whisper_gate_poll_ms.
2) Слот whisper один, поднять нельзя; переполнение — FIFO-очередь, позиция видна кадром progress (queued + queue_position).
3) Пайплайн ЖДЁТ окончания проб окружения: reconcile_on_boot идёт раньше проб, и без ожидания спасённая запись падала бы на непроверенном whisper.
4) Вход whisper — work/mix.wav, архив — audio.m4a (AAC 32k моно +faststart). Это РАЗНЫЕ файлы: whisper.cpp не читает m4a (только flac/mp3/ogg/wav).
5) Без ffmpeg транскрипт всё равно есть: mix_tracks_in_memory складывает PCM16 с ограничением. Пропадает только архив.
6) Коррекция дрейфа применяется к ИНТЕРВАЛАМ речи, а не к сегментам whisper: сегменты уже на оси звука, а метки событий ставит страница по performance.now.

Живой прогон (tests/pipeline_live.rs, #[ignore]): 24 с синтезированной русской речи, настоящие ffmpeg/ffprobe/whisper-cli 1.9.2 — 4 сегмента, 2 спикера, атрибуция верна, 13,5x реального времени.
