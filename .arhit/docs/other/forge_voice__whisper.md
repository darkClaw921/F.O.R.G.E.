# forge_voice::whisper

Модуль `plugins/voice/src/whisper.rs` (980 строк) — ЛОКАЛЬНЫЙ движок расшифровки на
`whisper.cpp`, вынесенный в Ф1 из `forge_meet::pipeline::whisper` (там остался обратный
`pub use`, поэтому ни один вызов и ни один тест плагина встреч не изменился).

# ЗАЧЕМ И ГЛАВНОЕ СВОЙСТВО: ПРАВИЛО «ТОЛЬКО GPU» ЗДЕСЬ СТРУКТУРНОЕ

Правило проекта «локальные модели не считаются на CPU» держится ДВУМЯ независимыми
механизмами, а не комментарием и не проверкой в одном месте:

1. `-ng` / `--no-gpu` (`FORBIDDEN_ARGS`) не попадают в argv ПО ПОСТРОЕНИЮ. `build_args`
   — чистая функция, и её накрывает тест-перебор: чем бы ни был набит
   `extra_args` в конфиге, `sanitize_extra_args` выбрасывает запрещённое.
2. stderr живого прогона парсится на `METAL_NEEDLE = "ggml_metal_init:"`. Не увидели
   строку за `DEFAULT_METAL_GRACE_SECS = 60` секунд — процесс УБИВАЕТСЯ, наружу идёт
   отказ `metal_unavailable`. Плюс встречный список `CPU_MARKERS`: явное объявление
   процессорного бэкенда убивает прогон немедленно.

Молчаливого отката на процессор нет ни при каких настройках. Это важно именно для
голосовых: расшифровка на CPU не «медленнее», она превращает минутное голосовое в
десятки минут прогрева ноутбука за спиной человека.

# КЛЮЧЕВЫЕ ЭЛЕМЕНТЫ

- `WhisperFlags` — флаги прогона (модель, язык, число потоков, beam size, VAD,
  `extra_args`); `Default` даёт рабочий набор.
- `WhisperRequest` — что расшифровываем: путь к WAV 16 кГц моно, каталог вывода.
- `build_args(req, flags) -> Vec<String>` — чистая сборка argv. Тестируется
  перебором: тест доказывает отсутствие запрещённых флагов, а не читает код.
- `sanitize_extra_args(extra) -> Vec<String>` — фильтр пользовательских добавок.
- `run(...) -> WhisperOutcome` — запуск через `forge_voice::proc::run_watched`:
  построчный разбор stderr, `parse_progress` вытаскивает проценты (их плагин
  отдаёт кадром `voice_transcribe_delta`), Metal-гард убивает прогон.
- `parse_output_json(raw)` → `Vec<WhisperSegment>`; `is_service_token` отбрасывает
  служебные токены вроде `[_BEG_]`, `WhisperToken`/`WhisperSegment` — модель вывода.
- Константы: `DEFAULT_TIMEOUT_SECS = 3600`, `DEFAULT_METAL_GRACE_SECS = 60`.

# СВЯЗИ

Вниз: `proc` (наблюдаемый запуск), `tools::resolve_whisper` + `tools::require_model`
(поиск бинаря и весов), `audio` (WAV на входе). Вверх: `transcribe::run` выбирает этот
движок, когда `gate::check` вернул `EnginePlan::Local`.

# ОГРАНИЧЕНИЯ

Читает ТОЛЬКО WAV 16 кГц (ogg/opus и m4a голосовых обязаны пройти через `audio::convert`);
требует ffmpeg, бинарь `whisper-cli` и файл весов `ggml-*.bin` — каждого отсутствия свой
код отказа (`no_ffmpeg`, `no_whisper`, `no_model`), показываемый человеком читаемым текстом
во фронте (`ERROR_TEXT` в `messenger/voice-transcript-core.js`).
