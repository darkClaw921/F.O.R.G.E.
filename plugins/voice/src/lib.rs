//! `forge-voice` — общее ядро голосовых сообщений F.O.R.G.E.
//!
//! Крейт НЕ знает ни про `AppState`, ни про WS-протоколы плагинов, ни про
//! `axum`: ровно как [`forge-chat-analysis`], по образцу которого он и сделан.
//! Наружу отдаются чистые функции, движки расшифровки и закрытые списки ошибок;
//! HTTP-конверт вокруг них строит плагин.
//!
//! # Откуда взялся крейт
//!
//! Расшифровка была написана и обкатана в `forge-meet` — там она работает на
//! живых встречах с Ф5. Голосовые сообщения в VK, Telegram и Bitrix — та же
//! задача на другом входе, и переписывать движки заново означало бы завести
//! ВТОРУЮ реализацию Metal-гарда и второй разбор ответа polza.ai. Поэтому
//! движки переехали сюда целиком, а в `forge-meet` на прежних путях остались
//! обратные `pub use`: ни одно место вызова и ни один тест плагина встреч от
//! переезда не изменились.
//!
//! # Что здесь есть
//!
//! | модуль | зачем |
//! |---|---|
//! | [`proc`] | запуск внешнего процесса с наблюдением за stderr и хвостом ошибки |
//! | [`tools`] | поиск `ffmpeg`, `whisper-cli` и файла весов — отказом со своим кодом |
//! | [`audio`] | файл из медиа-кэша → длительность → mp3 для облака либо WAV для whisper |
//! | [`gate`] | лестница проверок «можно ли расшифровать это прямо сейчас» |
//! | [`transcribe`] | само задание: подготовка → движок → стор |
//! | [`whisper`] | локальный STT whisper.cpp **со структурным GPU-гардом** |
//! | [`cloud`] | клиент polza.ai: ключ, служебные вызовы, расшифровка окнами |
//! | [`waveform`] | расчёт настоящей звуковой огибающей через ffmpeg |
//! | [`store`] | JSON-стор готовых расшифровок рядом с медиа-кэшем плагина |
//! | [`config`] | [`VoiceConfig`] и `voice.toml` с дефолтами из `meet.toml` |
//! | [`sink`] | трейт доставки кадров расшифровки — ядро не знает протокола |
//! | [`hub`] | реестр активных расшифровок: дедуп, потолок, отмена, таймаут |
//! | [`auto`] | решение «разбирать ли новое голосовое само» и дедуп ключей |
//! | [`prefs`] | тумблер авторазбора и белый список видимости из `user_settings.json` |
//! | [`settings`] | движок расшифровки: чтение и запись в `voice.toml` из браузера |
//!
//! # ПРАВИЛО ПРОЕКТА: ЛОКАЛЬНЫЕ МОДЕЛИ СЧИТАЮТСЯ ТОЛЬКО НА GPU
//!
//! Оно живёт в [`whisper`] и держится ДВУМЯ структурными механизмами, а не
//! комментарием: `-ng`/`--no-gpu` не попадают в argv по построению
//! ([`whisper::build_args`] — чистая функция с тестом-перебором), а stderr
//! прогона парсится на [`whisper::METAL_NEEDLE`], без которого процесс
//! УБИВАЕТСЯ. Молчаливого фолбэка на процессор нет ни при каких настройках.
//!
//! [`forge-chat-analysis`]: https://github.com/
//! [`VoiceConfig`]: config::VoiceConfig

pub mod audio;
pub mod auto;
pub mod cloud;
pub mod config;
pub mod error;
pub mod gate;
pub mod hub;
pub mod prefs;
pub mod proc;
pub mod settings;
pub mod sink;
pub mod store;
pub mod tools;
pub mod transcribe;
pub mod waveform;
pub mod whisper;

pub use config::VoiceConfig;
pub use error::{RefusalClass, VoiceError};
pub use gate::{check, EnginePlan, VoiceRequest};
pub use hub::{StartOutcome, VoiceHub};
pub use sink::VoiceSink;
pub use store::{Engine, Transcript, TranscriptStore, VoiceKey};
pub use transcribe::TranscribeJob;
