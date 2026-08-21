# forge_telegram::voice

Файл `plugins/telegram/src/voice.rs` (442 строки) — адаптер голосовых панели Telegram
плюс ВСЯ работа с огибающей.

# ЗАЧЕМ

Тонкий адаптер между лексикой плагина и общим ядром `forge-voice`. Ядро не знает ни про
`CacheKey`, ни про `MediaMeta`, ни про `ApiError` панели — перевод живёт здесь.

# ОБЩИЕ ЭЛЕМЕНТЫ

- `KIND_VOICE = "voice"` — вид вложения в лексике панели.
- `voice_key(key)` — `CacheKey` панели → `forge_voice::VoiceKey` (адрес в сторе).
- `cache_stem(key)` — имя файла в медиа-кэше (`<stem>.bin`, `<stem>.meta.json`,
  `<stem>.wave.json`).
- `kind_of(meta)` — вид вложения по метаданным; сверяется с
  `forge_voice::gate::VOICE_KINDS`.
- `map_voice_error(e)` — `VoiceError` → `ApiError` панели: код отказа переживает
  перевод целиком, потому что фронт печатает человеку текст ПО КОДУ
  (`ERROR_TEXT` в `messenger/voice-transcript-core.js`), а не по сообщению. В сообщении
  движка бывают пути до бинарей и хвост stderr — показывать их человеку вредно.

# ОГИБАЮЩАЯ: АТРИБУТ ПОБЕЖДАЕТ РАСЧЁТ

- `is_audio(meta)` — шире, чем `kind_of`: аудиофайл тоже расшифровывается по кнопке.
- `cached_waveform(cfg, key)` — читает сайдкар `<stem>.wave.json`, пустой массив
  означает «рисуй шум».
- `attach_waveforms(cfg, messages)` — дописывает волну в DTO пачки сообщений перед
  отдачей фронту.
- `ensure_waveform(...)` — посчитать, если нет: `forge_voice::waveform::compute` через
  ffmpeg. Зовётся ТОЛЬКО когда `DocumentAttributeAudio.waveform` пуст, то есть для
  аудиофайлов; у настоящего голосового Telegram волна приходит упакованной по 5 бит и
  распаковывается `forge_voice::waveform::from_telegram` без единого запуска процесса.
- `spawn_waveform(...)` — фоновый расчёт, чтобы отдача ленты не ждала ffmpeg.

`ONLY_INDEX = 0` — вложение в сообщении Telegram одно.
