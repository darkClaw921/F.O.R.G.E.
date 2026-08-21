# Крейт forge-voice — общее ядро голосовых сообщений

Создан в Фазе 1 фичи «голосовые сообщения в плагинах чатов» (эпик forge-cvoice-f1-2bb2n).
Расположение: plugins/voice, имя пакета forge-voice, член корневого workspace.

## Зачем крейт существует

Расшифровка звука была написана и обкатана в forge-meet: локальный whisper.cpp со
структурным GPU-гардом и облачный клиент polza.ai. Голосовые сообщения в VK, Telegram и
Bitrix — та же задача на другом входе. Писать её заново означало бы завести ВТОРУЮ
реализацию Metal-гарда и второй разбор ответа polza.ai — то есть держать правило проекта
«локальные модели только на GPU» в двух местах, которые однажды разойдутся в одном из них.

Поэтому движки переехали в листовой крейт, а в forge-meet на прежних путях остались
обратные pub use. Образец — plugins/chat-analysis (forge-chat-analysis): ядро не знает ни
про AppState, ни про WS-протокол плагина, ни про axum.

## Модули

- error.rs — VoiceError (закрытый список причин) + code(), пробрасывающий коды вложенных
  отказов (metal_unavailable, cloud_no_funds) наружу без потери.
- proc.rs — запуск внешнего процесса с наблюдением за stderr: ProcError, TailBuffer,
  run_watched, run, capture_stdout, capture_stdout_bytes. Переехал из
  plugins/meet/src/pipeline/proc.rs. capture_stdout_bytes добавлен для waveform.
- whisper.rs — локальный STT: WhisperRequest, WhisperFlags, RunOptions, build_args, run,
  parse_output_json, parse_progress, metal_confirmed, cpu_declared, is_service_token,
  sanitize_extra_args, METAL_NEEDLE, CPU_MARKERS, FORBIDDEN_ARGS.
- cloud/mod.rs — CloudClient, CloudConfig, SecretKey, CloudError, status_error, number, text.
- cloud/service.rs — GET /key, /balance, /models (KeyInfo, Balance, ModelInfo, Pricing).
- cloud/stt.rs — plan_windows, plan_windows_at_silence, build_window_args, build_form_fields,
  parse_verbose_json, shift_segments, SttOptions, CloudSttOutcome, transcribe_file,
  run_windows, transcribe_bytes, MAX_STT_WINDOW_SECS, CUT_PAD_MS.
- waveform.rs — расчёт настоящей звуковой огибающей через ffmpeg.
- store.rs — JSON-стор готовых расшифровок.
- config.rs — VoiceConfig и voice.toml с дефолтами из meet.toml.
- sink.rs — трейт VoiceSink: развязка ядра от WS-протокола плагина.
- hub.rs — VoiceHub: реестр активных расшифровок.

## Что ОСТАЛОСЬ в forge-meet и почему

Пер-трековый облачный путь: plan_windows_on_cuts, within_cloud_budget, CloudTrackRequest,
TrackGap, ForcedCut, CloudPerTrackOutcome, transcribe_tracks. Он стоит на
crate::pipeline::pertrack — то есть на дорожках, интервалах речи и подписи спикера,
понятиях ВСТРЕЧИ, которых у голосового сообщения нет. Слова pertrack в forge-voice нет ни
разу.

Там же остались: cloud/summary.rs (саммари встречи потоком SSE), адаптеры конфига
(model_path, vad_model_path, binary_from, out_prefix) и impl From<ProcError> for MeetError —
MeetError это тип плагина, ядру он неизвестен.

## Инвариант манифеста: ни одной литеральной версии

В plugins/voice/Cargo.toml все зависимости подключены через workspace = true. Ради этого в
корневой [workspace.dependencies] подняты tokio-util, thiserror, toml, toml_edit и tempfile.
Причина та же, что у reqwest и rusqlite: одна точка версии делает вторую копию крейта в
дереве невозможной ПО ПОСТРОЕНИЮ. Для forge-voice это особенно важно — его потребителей
сразу четверо, и разъехавшийся tokio-util дал бы два несовместимых CancellationToken, то
есть отмена расшифровки молча перестала бы работать.

## Гейт фазы (forge-cvoice-f1-2bb2n.11), зафиксировано

- cargo build --workspace — зелёный;
- cargo test -p forge-meet -p forge-voice — 24 цели, 0 падений (530 lib-тестов meet + 83
  теста forge-voice);
- git diff --stat по plugins/meet/tests/ не изменился ни на строку — тела тестов meet не
  правились;
- cargo tree -p forge-voice не содержит ни forge-meet, ни devforge;
- cargo tree -i libsqlite3-sys — ровно один узел; openssl-sys в дереве нет.
