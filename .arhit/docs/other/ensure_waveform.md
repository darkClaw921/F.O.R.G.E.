# ensure_waveform

voice::ensure_waveform — расчёт звуковой огибающей вложения в плагинах чатов (близнецы в plugins/bitrix/src/voice.rs и plugins/telegram/src/voice.rs).

Bitrix: ensure_waveform(state, key, meta) -> Option<Vec<i32>>. Портал огибающую не отдаёт ВООБЩЕ, поэтому единственный момент, когда её можно посчитать, — когда оригинал лёг в медиа-кэш. Признак «это звук» — MIME из меты (is_audio).
Telegram: ensure_waveform(state, key, meta, has_attribute). АТРИБУТ TELEGRAM ВСЕГДА ПОБЕЖДАЕТ: у голосовых огибающая приезжает вместе с сообщением бесплатно и точно, считать самим имеет смысл только для аудио БЕЗ атрибута (обычные файлы, пересланные треки). Флаг передаёт вызывающий (routes/media.rs: !info.waveform.is_empty()) — в самой функции TL-объекта нет. Превью (MediaKind::Thumb) огибающей не получают по определению.

Путь ffmpeg — state.voice_config() → forge_voice::tools::resolve_ffmpeg. ОТКАЗ НЕ ЯВЛЯЕТСЯ ОШИБКОЙ КЭША: нет ffmpeg, файл не аудио, файл битый — всё это None и одна внятная строка в журнале; скачанный файл уже лежит в кэше и уже отдан человеку.

spawn_waveform(...) ставит тот же расчёт в ФОН сразу после commit: привязывать ответ роута к декоду незачем, а зависший ffmpeg (потолок DEFAULT_TIMEOUT_SECS = 30 с) держал бы запрос целиком. Волна доезжает до ленты со следующей выдачей MediaDto.

attach_waveforms(cfg, [dialog,] messages) подставляет посчитанное в страницу истории: читает сайдкар ТОЛЬКО для вложений вида voice/audio с пустым полем, поэтому лента из сотни текстовых сообщений не делает ни одного лишнего обращения к диску, а атрибут Telegram не перезаписывается. Зовётся из bitrix routes/dialogs.rs и telegram routes/dialogs.rs::get_messages.
