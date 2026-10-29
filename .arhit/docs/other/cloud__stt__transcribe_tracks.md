# cloud::stt::transcribe_tracks

Облачная расшифровка ПО ДОРОЖКАМ (Ф18, plugins/meet/src/cloud/stt.rs).

ЗАЧЕМ. До Ф18 облачный путь шёл только по миксу, и имя спикера вычислялось перекрытием сегмента с интервалами речи — догадка с confidence. Живой замер приёмки Ф17 на одной записи: локально 37/37 верных имён, облаком 32-34 из 43. Здесь дорожка И ЕСТЬ человек, подпись берётся напрямую (attribute_direct, confidence 1.0), и перебивания разбираются вместо склейки двух голосов в одну фразу.

ПОЧЕМУ ЭТО НЕ ДОРОЖЕ МИКСА. Прежний отказ в run.rs обосновывался тем, что N загрузок = N-кратные деньги. Неверно: в облако уезжает не запись, а вырезанная речь (write_speech_only_wav). Живой замер из шапки pipeline/pertrack.rs: запись 90 с с речью как на встрече — микс 90 000 мс звука против 12 319 мс на двух дорожках (14%). Платят за минуты звука, значит пер-трек в облаке ДЕШЕВЛЕ. N-кратная цена возникает только при сплошном одновременном разговоре, и на этот случай есть within_cloud_budget.

ЦЕПОЧКА ОДНОЙ ДОРОЖКИ: raw/track-N.wav -> pertrack::plan_cuts -> pertrack::write_speech_only_wav -> work/track-N-speech.wav -> plan_windows_on_cuts -> окна mp3 через ffmpeg -> POST /audio/transcriptions -> pertrack::remap_segments (координаты вырезанного файла обратно на общую ось). Сборка — pertrack::merge_tracks.

ПАРАМЕТРЫ. CloudTrackRequest { work_dir, sample_rate, total_ms, pad_ms } — близнец pertrack::PerTrackRequest без всего, что относится к внешнему процессу. SttOptions тот же, что у миксового пути.

ВОЗВРАТ CloudPerTrackOutcome: tracks (сегменты уже на общей оси), windows (сколько окон планировалось по всем дорожкам), failed: Vec<TrackGap> — дыра с ИМЕНЕМ дорожки и точкой НА ОБЩЕЙ ОСИ, forced_cuts: Vec<ForcedCut> — места, где непрерывную речь пришлось разрезать, language. Методы speech_ms(), complete(), is_empty(), gap_note(), forced_note(); заметки уезжают в журнал встречи событиями cloud_stt_gaps и cloud_stt_forced_cuts.

ОГРАНИЧЕНИЯ. Дорожки идут ПО ОЧЕРЕДИ: параллельные загрузки означали бы N одновременных запросов к чужому сервису от одной встречи. Молчавшая дорожка (слушатель) не отправляется вовсе. Ошибка возвращается только когда не доехало ни одно окно ни одной дорожки — правило failed только когда нет транскрипта. Путь включается лишь в режиме TrackMode::PerParticipant (решает run.rs::per_track_decision).

ТЕСТЫ: plugins/meet/tests/polza_stub.rs (имена владельцев дорожек, адресные дыры и разрезы, слушатель) и plugins/meet/tests/pipeline_cloud.rs (e2e: per_track=2, manifest.postprocess.per_track, engine polza.ai, track_idx в базе).
