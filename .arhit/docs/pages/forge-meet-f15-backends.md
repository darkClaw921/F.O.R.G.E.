Ф15 встроила облачный клиент Ф14 в конвейер forge-meet двумя НЕЗАВИСИМЫМИ переключателями стадий. **У обоих значение по умолчанию — `cloud`** (прямое требование пользователя: «по стандарту всегда облачная»); менять его на `local` «для безопасности» нельзя.

## Что появилось в конфиге (plugins/meet/src/config.rs)

| ключ | по умолчанию | группа UI |
|---|---|---|
| `stt_backend` | `cloud` | transcribe |
| `summary_backend` | `cloud` | summary |
| `cloud_stt_model` | `openai/whisper-1` | transcribe |
| `cloud_stt_window_secs` | 90 (потолок — измеренный `MAX_STT_WINDOW_SECS`) | transcribe |
| `cloud_summary_model` | `google/gemini-3.5-flash` | summary |
| `cloud_base_url` | `https://api.polza.ai/api/v1` | cloud |
| `polza_api_key` | пусто | cloud (`FieldKind::Secret`) |

Перечисления `SttBackend` и `SummaryBackend` сделаны по образцу `WindowMode`: snake_case, `parse`/`as_str`/`ALL`, неизвестное значение НЕ переключает путь, а оставляет прежний с `warn`. Старый и пустой конфиг обязаны давать `cloud/cloud` — на это есть тест `both_backends_default_to_cloud_even_for_an_old_config_file`.

`MeetConfig::cloud()` — единственная точка сборки `CloudConfig` на обе стадии: один сервис, один ключ, один адрес. Вторая сборка разъехалась бы по `base_url`, то есть тест ходил бы на стенд, а бой — на живой сервис.

## Ключ API

- живёт в `crate::cloud::SecretKey`: `Debug`/`Display` печатают МАСКУ, значение достаётся только явным `expose()`;
- источники: файл конфига либо `FORGE_MEET_POLZA_KEY` (ключ из env помечается `env_locked`, то есть UI показывает его только для чтения);
- `editable_value("polza_api_key")` возвращает МАСКУ — единственное поле, где значение в ответе роута намеренно не равно значению в конфиге. Поэтому `PUT /api/meet/config` пишет в файл ПРИСЛАННОЕ значение, а не прочитанное обратно;
- присланная обратно маска отвергается (`MASK_CHAR` в значении = 400): без этой проверки форма, сохранённая без правки поля, затирала бы ключ строкой из точек;
- пустая строка — законное «убрать ключ»;
- после каждой записи конфига выставляется 0600 (`config::ensure_private_file`); при чтении файла с ключом и правами шире 600 пишется `warn`, но права молча не чинятся — файл правят руками.

Тест приватности — `plugins/meet/tests/key_privacy.rs` (8 тестов): ключа нет в argv ffmpeg, в полях формы multipart, в теле chat/completions, в `Debug` конфига/клиента/ошибок, в починках `validate_and_fix`, в JSON ответов `/api/meet/config` и `/api/meet/status`. Мутация (возврат `expose()` вместо `mask()`) проверена — тест краснеет.

## Ветвление расшифровки (pipeline/run.rs)

`DepsReport::transcribe_blocker_for(backend, cloud_configured)` — деградация в ОБЕ стороны:

- **облако**: `whisper-cli`, весов на диске и Metal не требуется вообще; требуется ключ (`cloud_not_configured`) и `ffmpeg` (`ffmpeg_unavailable` — окна режутся и сжимаются в mp3 нашей стороной);
- **локально**: отсутствие ключа не мешает ничему.

В облачном режиме из этапа исчезают три вещи, и у каждой отпала причина: гейт «не расшифровывать, пока идёт запись» (Chrome и облако не делят Metal), семафор «whisper строго один» (он держит единственный GPU) и поиск бинаря whisper. Пер-трекового пути в облаке нет: он означал бы N загрузок вместо одной.

Провал облака не теряет запись: WAV дорожек, манифест и журнал остаются на диске, встреча уходит в `failed` — правило «`failed` только когда нет транскрипта» сохранено. Недоехавшие окна дают событие `cloud_stt_gaps` с перечислением дыр.

## Пословное дробление в облаке ОТКЛЮЧЕНО

`SpeakerConfig::allow_word_split` (= `stt_backend.is_local()`). Облачный `whisper-1` кладёт в `tokens` идентификаторы токенов модели, а не метки времени; разбор Ф14 оставляет `tokens` пустым, но «не срабатывает по совпадению» и «выключено намеренно» — разные вещи: первое чинится чужой правкой парсера, после которой сегменты резались бы по выдуманным границам МОЛЧА. Спорный сегмент в облаке остаётся целым и помечается `overlapped`.

## Ветвление саммари (summary/mod.rs)

Развилка стоит внутри `summarize`, а не выше и не внутри `generate`: выше — второй способ провалить саммари, который разойдётся с первым; внутри `generate` — потащить `MeetState` в функцию, зависящую только от промпта. Обе стороны отдают `Result<String, ProcError>` и зовут ОДИН `on_delta` через ОДИН `DeltaCoalescer`, поэтому кадр `Progress { phase: Summarizing, done }` тот же — фронт не переделывается. `summary_timeout_secs` в облаке означает потолок ПАУЗЫ между кадрами SSE, а не всего ответа.

## GET /api/meet/status

Новый блок `cloud` (`CloudStatusDto`): `configured`, `key_mask`, `env_locked`, `base_url`, `stt_model`, `summary_model`, `key` (ответ бесплатного `GET /key`), `error`, `checked_secs_ago`. Плюс `config.stt_backend` и `config.summary_backend`.

Инвариант «status никогда не падает» сохранён: `MeetState::cloud_probe()` не возвращает `Result` вообще, ставит потолок `CLOUD_PROBE_TIMEOUT = 3 с` и кэширует ответ на `CLOUD_PROBE_TTL = 60 с`. Без ключа сетевого вызова нет вовсе — поэтому тесты герметичны. `GET /balance` в статус НЕ ставится: это второй сетевой вызов ради числа, нужного не всегда.

## Гейты

`cargo build --workspace`, `cargo test -p forge-meet` (524 юнита + интеграционные), `cargo tree -d` — те же 49 дублей, `cargo tree -i openssl-sys` пуст, Cargo.lock не вырос. Фронтовые проверки: `meet_contract` 332, `settings_meet_tab` 61, `meet_api_gate` 130.

## Что пришлось поправить в существующих тестах

Файлы, целиком посвящённые ЛОКАЛЬНОМУ пути (`tests/pipeline.rs`, `tests/summary.rs`, `tests/routes_result.rs`), теперь ЯВНО ставят `stt_backend`/`summary_backend` в `Local`. Без этого они молча ушли бы в сеть вместе со сменой умолчания.