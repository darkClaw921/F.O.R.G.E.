# forge_voice::cloud

Модуль `plugins/voice/src/cloud/mod.rs` (678 строк) — ОБЛАЧНЫЙ движок расшифровки:
клиент API polza.ai (OpenAI-совместимый), общий для встреч (`forge-meet`) и голосовых
сообщений трёх мессенджеров. Вынесен в Ф1 из `forge_meet::cloud`.

# СОСТАВ ПОДМОДУЛЕЙ

- `cloud` (этот файл) — секрет, конфиг, HTTP-клиент, разбор ошибок.
- `cloud::stt` — собственно расшифровка: нарезка на окна, форма multipart, склейка.
- `cloud::service` — служебные вызовы: ключ, баланс, список моделей и цены.

# КЛЮЧ НЕ СВЕТИТСЯ НИГДЕ

`SecretKey` — newtype без `Serialize`, с рукописными `Debug` и `Display`, которые печатают
маску из `MASK_CHAR = '•'`. Поэтому ключ не может утечь ни в лог (`{:?}` даёт маску), ни в
ответ роута (структуру просто нельзя сериализовать), ни в текст ошибки. Наружу роуты
`…/voice/settings` отдают лишь булев признак `cloud_key_set`. Инвариант закреплён тестами
(`plugins/meet/tests/key_privacy.rs`).

# КЛЮЧЕВЫЕ ЭЛЕМЕНТЫ

- `CloudConfig` — база (`DEFAULT_BASE_URL = "https://api.polza.ai/api/v1"`), модели
  (`DEFAULT_STT_MODEL = "openai/whisper-1"`, `DEFAULT_SUMMARY_MODEL`), язык (`ru`),
  таймауты (`DEFAULT_CONNECT_TIMEOUT_SECS = 10`, `DEFAULT_REQUEST_TIMEOUT_SECS = 30`,
  `DEFAULT_STT_TIMEOUT_SECS = 120`, `DEFAULT_STREAM_IDLE_SECS = 120`).
- `CloudClient` — обёртка `reqwest::Client`; `Debug` тоже рукописный (иначе конфиг с
  ключом печатался бы целиком).
- `CloudError` + `status_error(status, body)` — закрытый список отказов с разделением
  «нет ключа» / «ключ отвергнут» / «нет денег» / «лимит» / «сеть». `From<CloudError>
  for ProcError` позволяет смешивать облачные и локальные отказы в одной лестнице.
- `stt_gives_timestamps(model)` — не каждая модель отдаёт тайминги; список
  `TIMESTAMPED_STT_MODELS`. От этого зависит, будет ли у расшифровки разбивка по времени.
- `MAX_STT_WINDOW_SECS = 90` — потолок окна, определяющий нарезку в `cloud::stt`.

# ЛОВУШКА, СТОИВШАЯ ОТЛАДКИ

`ClientBuilder::timeout` в reqwest режет и ЧТЕНИЕ ТЕЛА, то есть убивает потоковые ответы.
Общий таймаут задаётся только на не-стриминговых вызовах, у стрима — `stream_idle`.
Фичу `multipart` крейта reqwest надо включать в самом крейте `forge-voice`, наследование
из workspace её не приносит.
