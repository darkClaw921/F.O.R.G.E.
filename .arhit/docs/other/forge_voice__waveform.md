# forge_voice::waveform

Модуль `plugins/voice/src/waveform.rs` (873 строки) — НАСТОЯЩАЯ звуковая огибающая
голосового вместо шума-заглушки. Появился в Ф2 фичи голосовых.

# ЗАЧЕМ

До Ф2 настоящую волну умел показывать только VK — там она приходит прямо в ответе API.
Telegram рисовал детерминированный шум, Bitrix не имел ветки настоящей огибающей вовсе.
Модуль закрывает оба пробела: распаковывает то, что платформа уже прислала, и считает
недостающее по файлу.

# ТРИ ИСТОЧНИКА ОГИБАЮЩЕЙ — И ЭТО ВАЖНО

1. **VK** — массив приходит от API, считать нечего.
2. **Telegram** — `DocumentAttributeAudio.waveform`: значения упакованы по
   `TELEGRAM_BITS_PER_SAMPLE = 5` бит (`TELEGRAM_MAX_VALUE = 31`). Распаковка —
   `unpack_5bit(packed)`, приведение к общей шкале — `from_telegram(packed)`.
   АТРИБУТ ПОБЕЖДАЕТ РАСЧЁТ: если Telegram прислал волну, ffmpeg не запускается.
3. **Bitrix24 и аудиофайлы без атрибута** — расчёт на бэкенде: `compute(...)` гонит файл
   через ffmpeg в сырой PCM (`build_decode_args`, `DECODE_SAMPLE_RATE = 8_000`),
   `decode_s16le(bytes)` разбирает байты в отсчёты, `rms_buckets(samples, buckets)`
   сворачивает их в столбики по среднеквадратичному значению.

Общая шкала — `WAVEFORM_MAX_VALUE = 255`, длина — не больше `MAX_WAVEFORM = 200`;
`fit(values, src_max, buckets)` приводит любой источник к ней, чтобы фронт не знал,
откуда волна взялась.

# КЭШ: ОТДЕЛЬНЫЙ САЙДКАР, А НЕ ПОЛЕ МЕТЫ

`WaveformCache` кладёт результат в `<stem>.wave.json` (`SIDECAR_SUFFIX`) рядом с
`<stem>.bin` (`BIN_SUFFIX`), схема версионирована `CACHE_SCHEMA_VERSION = 1`.
Почему не поле в `.meta.json`: мету пишет `commit` докачки, который случается РАНЬШЕ
расчёта огибающей — дописывать в неё пришлось бы гонкой. API кэша:
`sidecar_for(bin)`, `read_cached(bin)`, `is_cached(bin)`, `write_cached(bin, waveform)`,
`ensure(bin, ffmpeg)` (посчитать, если нет). Расчёт ограничен
`DEFAULT_TIMEOUT_SECS = 30`.

# ЛОВУШКА ЭКСПЛУАТАЦИИ

Сайдкар `.wave.json` НАДО ВРУЧНУЮ добавлять в LRU-подчистку медиа-кэша каждого плагина —
иначе после вытеснения `.bin` остаются сироты-огибающие, которые никто не удалит.

# СВЯЗИ

Вверх: `voice::cached_waveform` / `ensure_waveform` / `spawn_waveform` /
`attach_waveforms` в Telegram и Bitrix. Фронт получает массив полем `waveform` в DTO
вложения и рисует его в `messenger/waveform-core.js`; пустой массив там означает шум.
