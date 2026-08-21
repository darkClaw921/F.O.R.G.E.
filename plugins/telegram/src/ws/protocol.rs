//! Типизированный WS-протокол плагина Telegram (`/ws/telegram`).
//!
//! Формат кадра — JSON с внешним тегом `type` в snake_case:
//!
//! ```json
//! {"type":"login_state","state":"not_configured","phone":null,"hint":null,"error":null,"user":null}
//! {"type":"download_progress","dialog_id":"123","message_id":7,"kind":"full","received":1024,"total":4096}
//! {"type":"new_message","message":{"id":7,"dialog_id":"123","...":null},"muted":false,"archived":false}
//! {"type":"notify_defaults_changed","users":true,"chats":false,"broadcasts":false}
//! ```
//!
//! ## Кто решает, «звенеть» ли сообщению
//!
//! Поля `muted`/`archived` лежат на уровне КАДРА, а не внутри `message`:
//! это свойство диалога, а не сообщения, и источник истины по ним —
//! сервер. Раньше решение принимал фронт по своей загруженной странице
//! списка диалогов, поэтому любой чат за её пределами (а их у аккаунта
//! сотни) считался незаглушённым и всплывал тостом. Сервер знает про все
//! чаты сразу ([`crate::state::TelegramState::notify_overrides`],
//! [`crate::state::TelegramState::archived_ids`],
//! [`crate::dto::NotifyDefaults`]), поэтому гадать больше не нужно.
//!
//! Все варианты объявлены сразу — включая те, что начнут отправляться в
//! следующих фазах. Это сделано намеренно: фронтовый `buildHandlers`
//! пишется один раз по этой таблице и не переписывается при каждой фазе.
//!
//! Инвариант id-полей: `dialog_id` и `user_id` — СТРОКИ (64-битные
//! идентификаторы Telegram не влезают в JS-число), `message_id` — `i32`
//! (в Telegram он 32-битный).

use serde::{Deserialize, Serialize};

use crate::dto::MessageDto;
use crate::login::LoginStateDto;

/// То, что летит в broadcast-канал [`crate::state::TelegramState`] и
/// дальше — каждому WS-подписчику.
///
/// Обёртки над сообщением НЕТ намеренно: у Telegram-плагина broadcast
/// без фильтрации (в отличие от Echo, где события адресуются
/// conversation'у). Причина — unread-бейдж и тосты должны работать с
/// ЛЮБОЙ вкладки, поэтому сервер вещает всё, а решает фронт.
pub type TgServerEvent = TgServerMsg;

/// Сообщения сервер → клиент.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TgServerMsg {
    /// Текущее состояние авторизации. Отправляется первым кадром после
    /// апгрейда и при каждом переходе [`crate::login::LoginFlow`].
    LoginState(LoginStateDto),
    /// Пришло новое сообщение.
    NewMessage {
        /// Сообщение целиком.
        message: MessageDto,
        /// Заглушён ли ДИАЛОГ, из которого пришло сообщение (уже с учётом
        /// наследования от глобальных настроек категории). Фронт обязан
        /// молчать при `true`, не заглядывая в свой список диалогов.
        muted: bool,
        /// Лежит ли диалог в архиве — архив не «звенит» и не входит в
        /// бейдж `~/inbox`.
        archived: bool,
    },
    /// Сообщение отредактировано (в т.ч. появился/изменился media-preview).
    MessageEdited {
        /// Новая версия сообщения.
        message: MessageDto,
    },
    /// Сообщения удалены.
    MessageDeleted {
        /// Диалог, из которого удалены (строкой).
        dialog_id: String,
        /// Идентификаторы удалённых сообщений.
        message_ids: Vec<i32>,
    },
    /// Собеседник печатает / записывает голосовое / выбирает файл.
    Typing {
        /// Диалог (строкой).
        dialog_id: String,
        /// Кто печатает (строкой).
        user_id: String,
        /// Вид активности: `typing`, `upload_photo`, `record_voice`, …
        action: String,
    },
    /// Изменился online-статус пользователя.
    Online {
        /// Пользователь (строкой).
        user_id: String,
        /// `true` ⇒ сейчас в сети.
        online: bool,
        /// Unix-время последнего появления, если Telegram его отдал.
        was_online: Option<i64>,
    },
    /// Отметка о прочтении. `direction`: `in` — прочитали мы,
    /// `out` — прочитали нас.
    Read {
        /// Диалог (строкой).
        dialog_id: String,
        /// Максимальный прочитанный message_id.
        max_id: i32,
        /// `in` | `out`.
        direction: String,
    },
    /// Прогресс отправки файла в Telegram (вторая фаза загрузки: браузер →
    /// сервер идёт по XHR, сервер → Telegram — вот этим событием).
    UploadProgress {
        /// Идентификатор upload-сессии, выданный `POST /api/telegram/upload`.
        upload_id: String,
        /// Отправлено байт.
        sent: u64,
        /// Всего байт.
        total: u64,
    },
    /// Прогресс скачивания медиа из Telegram в файловый кэш.
    DownloadProgress {
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение, чьё медиа качаем.
        message_id: i32,
        /// `thumb` | `full`.
        kind: String,
        /// Получено байт.
        received: u64,
        /// Всего байт, если размер известен заранее.
        total: Option<u64>,
    },
    /// Расшифровка голосового началась (голосовые-Ф3).
    ///
    /// Четвёрка кадров `voice_transcribe_*` стоит сразу за `download_progress`:
    /// расшифровка продолжает жизненный цикл вложения, который начинается его
    /// скачиванием. Состав и имена полей ОБЯЗАНЫ совпадать с VK и Bitrix — на
    /// этом держится общий фронт-модуль (Ф5), один на три мессенджера.
    VoiceTranscribeStarted {
        /// Идентификатор прогона.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения; у Telegram всегда `0` — вложение одно.
        index: u8,
        /// `local` | `cloud`: человек имеет право знать, ушло ли голосовое в
        /// чужое облако, ДО того, как оно туда ушло.
        engine: String,
    },
    /// Прогресс расшифровки, проценты.
    ///
    /// КОАЛЕСЦИРОВАН в [`TgVoiceSink`]: whisper печатает прогресс десятками
    /// строк в секунду, и слать каждую значило бы переполнить broadcast, то
    /// есть получить `Lagged` и [`TgServerMsg::Resync`] посреди расшифровки.
    VoiceTranscribeDelta {
        /// Идентификатор прогона.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения; всегда `0`.
        index: u8,
        /// Проценты `0..=100`.
        percent: u8,
    },
    /// Расшифровка готова; текст уже лежит в сторе.
    VoiceTranscribeDone {
        /// Идентификатор прогона.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения; всегда `0`.
        index: u8,
        /// Текст расшифровки целиком.
        text: String,
        /// Язык, как его назвал движок; пустая строка — не назвал.
        language: String,
        /// Каким движком получено.
        engine: String,
        /// Длительность записи, мс.
        duration_ms: i64,
    },
    /// Расшифровка не удалась.
    ///
    /// `code` отдельно от текста: по нему фронт различает случаи
    /// (`metal_unavailable` — «включите облако или почините GPU»,
    /// `voice_no_key` — «введите ключ»), и разбирать это из русской фразы он не
    /// должен.
    VoiceTranscribeError {
        /// Идентификатор прогона.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения; всегда `0`.
        index: u8,
        /// Машинный код отказа (`forge_voice::VoiceError::code`).
        code: String,
        /// Человеческий текст с хвостом stderr.
        message: String,
    },
    /// Настройки уведомлений диалога изменились (mute/unmute — в т.ч. с
    /// другого устройства). Фронт обновляет `muted` у строки диалога, не
    /// перечитывая список.
    NotifySettings {
        /// Диалог (строкой).
        dialog_id: String,
        /// `true` ⇒ уведомления заглушены (см. правила в
        /// [`crate::dto::notify_muted`]).
        muted: bool,
    },
    /// Изменились ГЛОБАЛЬНЫЕ настройки уведомлений одной из трёх категорий
    /// Telegram («заглушить все личные чаты» / «все группы» / «все каналы»
    /// — в том числе с телефона).
    ///
    /// Отдельный кадр, а не [`TgServerMsg::FoldersChanged`] и не пачка
    /// [`TgServerMsg::NotifySettings`]: перечислить поимённо все диалоги
    /// категории невозможно (сервер их все не знает), а перезапрашивать
    /// папки бессмысленно — изменились уведомления, а не состав папок.
    /// Фронт по этому кадру пересчитывает «звенит ли» у диалогов с
    /// унаследованными настройками.
    NotifyDefaultsChanged {
        /// Заглушены ли по умолчанию личные чаты (включая ботов).
        users: bool,
        /// Заглушены ли по умолчанию группы (малые И мегагруппы).
        chats: bool,
        /// Заглушены ли по умолчанию broadcast-каналы.
        broadcasts: bool,
    },
    /// Состав/порядок папок изменился — фронт перезапрашивает
    /// `GET /api/telegram/folders`. Полей нет намеренно: Telegram шлёт
    /// изменения тремя разными апдейтами, и проще один раз перечитать.
    FoldersChanged,
    // --- «Анализ чата через Claude» ---------------------------------------
    //
    // ПОРЯДОК этих четырёх вариантов заморожен: `telegram_contract.test.mjs`
    // сверяет теги с `SERVER_FRAMES` в `js/telegram/ws.js` по составу И
    // порядку. Вставлены перед `Resync`/`Ping` — там же, где они стоят у VK,
    // чтобы три протокола читались одинаково.
    /// Анализ начался; размеры транскрипта уже известны.
    AnalysisStarted {
        /// Идентификатор run'а; им же отменяют.
        run_id: String,
        /// Диалог (строкой), по которому идёт анализ.
        dialog_id: String,
        /// `analyze` | `refresh` | `ask`.
        kind: String,
        /// Сколько сообщений попало в транскрипт.
        messages_used: u32,
        /// Часть истории отброшена потолком символов.
        truncated: bool,
    },
    /// Кусок ответа модели.
    ///
    /// КОАЛЕСЦИРОВАН ядром (не чаще одного кадра в
    /// [`crate::analysis::DELTA_FLUSH_MS`] мс либо по накоплению
    /// [`crate::analysis::DELTA_FLUSH_CHARS`] символов). Сырые дельты Claude
    /// идут сотнями в секунду и переполнили бы broadcast
    /// ([`crate::state::BROADCAST_CAPACITY`]), а переполнение — это `Lagged`,
    /// то есть [`TgServerMsg::Resync`] и перечитывание всего состояния.
    AnalysisDelta {
        /// Идентификатор run'а.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Приращение текста; склеивается конкатенацией в порядке получения.
        text: String,
    },
    /// Анализ завершён (успехом, ошибкой, отменой или таймаутом).
    AnalysisDone {
        /// Идентификатор run'а.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Завершился ли run штатно.
        ok: bool,
        /// Причина неуспеха: `cancelled` | `timeout` | `error`.
        reason: Option<String>,
        /// Токенов на входе (из финального `result` Claude).
        input_tokens: u64,
        /// Токенов на выходе.
        output_tokens: u64,
    },
    /// Ошибка run'а — текст от раннера ЦЕЛИКОМ.
    ///
    /// Сообщение не урезается намеренно: к нему уже приклеен хвост stderr
    /// Claude CLI, и это единственный способ увидеть «CLI не залогинен» или
    /// «model overloaded», не заглядывая в логи сервера.
    AnalysisError {
        /// Идентификатор run'а.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Текст ошибки как есть.
        message: String,
    },
    /// Клиент отстал/пропустил события — перечитать состояние по REST.
    Resync,
    /// Heartbeat. Клиент обязан ответить [`TgClientMsg::Pong`].
    Ping,
}

/// Мост между ядром анализа и broadcast-каналом плагина
/// ([`forge_chat_analysis::AnalysisSink`]).
///
/// Вся развязка плагина с крейтом — вот эти четыре метода. Ядро не знает ни
/// про [`TgServerMsg`], ни про broadcast: оно зовёт `started`/`delta`/`done`/
/// `error`, а перевод в НАШ кадр и рассылка живут здесь, рядом с самим
/// enum'ом — там же, где придётся править, если состав полей кадра изменится.
///
/// Методы трейта СИНХРОННЫЕ, и это существенно: их зовёт пампер ядра из своей
/// таски, в том числе в горячем цикле дельт. `broadcast::Sender::send` —
/// мгновенная неблокирующая операция, поэтому пампер не стои́т и буфер
/// коалесцинга не растёт.
///
/// `SendError` глотается: отсутствие подписчиков — норма (никто не открыл
/// вкладку), и это не повод ронять run, который уже идёт.
#[derive(Debug, Clone)]
pub struct TgAnalysisSink {
    tx: tokio::sync::broadcast::Sender<TgServerEvent>,
}

impl TgAnalysisSink {
    /// Оборачивает broadcast-канал состояния.
    #[must_use]
    pub fn new(tx: tokio::sync::broadcast::Sender<TgServerEvent>) -> Self {
        Self { tx }
    }
}

impl forge_chat_analysis::AnalysisSink for TgAnalysisSink {
    fn started(
        &self,
        run_id: &str,
        dialog_id: &str,
        kind: &str,
        messages_used: u32,
        truncated: bool,
    ) {
        let _ = self.tx.send(TgServerMsg::AnalysisStarted {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            kind: kind.to_string(),
            messages_used,
            truncated,
        });
    }

    fn delta(&self, run_id: &str, dialog_id: &str, text: &str) {
        let _ = self.tx.send(TgServerMsg::AnalysisDelta {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            text: text.to_string(),
        });
    }

    fn done(
        &self,
        run_id: &str,
        dialog_id: &str,
        ok: bool,
        reason: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let _ = self.tx.send(TgServerMsg::AnalysisDone {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            ok,
            reason: reason.map(str::to_string),
            input_tokens,
            output_tokens,
        });
    }

    fn error(&self, run_id: &str, dialog_id: &str, message: &str) {
        let _ = self.tx.send(TgServerMsg::AnalysisError {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            message: message.to_string(),
        });
    }
}

/// Сообщения клиент → сервер.
///
/// Их намеренно мало: вся мутация идёт по REST (её проще авторизовать,
/// логировать и ретраить), а WS — односторонний поток событий с
/// heartbeat'ом.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TgClientMsg {
    /// Ответ на [`TgServerMsg::Ping`]; сбрасывает idle-таймер соединения.
    Pong,
}

/// На сколько процентов должна вырасти полоса, чтобы уехал новый кадр.
///
/// Пять: у голосового на минуту это два десятка кадров вместо сотен, а на глаз
/// полоса всё равно движется непрерывно. Ноль означал бы кадр на каждую строку
/// stderr whisper'а — то есть переполнение broadcast и `Resync` посреди
/// расшифровки.
pub const VOICE_PROGRESS_STEP: u8 = 5;

/// Приёмник кадров расшифровки для ядра ([`forge_voice::VoiceSink`]).
///
/// Близнец [`TgAnalysisSink`] и живёт рядом по той же причине: ядро
/// `forge-voice` не знает ни [`TgServerMsg`], ни broadcast'а, а перевод в наш
/// кадр обязан лежать там же, где объявлен сам кадр.
///
/// Коалесцинг прогресса ведётся ПО ПРОГОНУ (`run_id`), а не одним счётчиком на
/// приёмник: при потолке параллелизма больше единицы два прогона перемешали бы
/// проценты, и полоса каждого дёргалась бы назад.
#[derive(Debug, Clone)]
pub struct TgVoiceSink {
    tx: tokio::sync::broadcast::Sender<TgServerEvent>,
    /// Последний ОТПРАВЛЕННЫЙ процент каждого идущего прогона.
    sent: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u8>>>,
}

impl TgVoiceSink {
    /// Оборачивает broadcast-канал состояния.
    #[must_use]
    pub fn new(tx: tokio::sync::broadcast::Sender<TgServerEvent>) -> Self {
        Self {
            tx,
            sent: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Координаты вложения из ключа ядра.
    ///
    /// `message_id` в ядре — строка (у трёх мессенджеров он разного типа), а в
    /// кадре Telegram это `i32`. Неразбираемое значение даёт `0`, а не панику:
    /// ключ собирается из проверенного `CacheKey`, но кадр — не то место, где
    /// стоит падать.
    fn coords(key: &forge_voice::VoiceKey) -> (String, i32, u8) {
        (
            key.peer.clone(),
            key.message_id.parse().unwrap_or(0),
            key.index,
        )
    }

    /// Забыть прогон: он кончился.
    fn forget(&self, run_id: &str) {
        if let Ok(mut map) = self.sent.lock() {
            map.remove(run_id);
        }
    }
}

impl forge_voice::VoiceSink for TgVoiceSink {
    fn started(&self, run_id: &str, key: &forge_voice::VoiceKey, engine: &str) {
        let (dialog_id, message_id, index) = Self::coords(key);
        if let Ok(mut map) = self.sent.lock() {
            map.insert(run_id.to_string(), 0);
        }
        let _ = self.tx.send(TgServerMsg::VoiceTranscribeStarted {
            run_id: run_id.to_string(),
            dialog_id,
            message_id,
            index,
            engine: engine.to_string(),
        });
    }

    fn progress(&self, run_id: &str, key: &forge_voice::VoiceKey, percent: u8) {
        let percent = percent.min(100);
        {
            let Ok(mut map) = self.sent.lock() else { return };
            let last = map.get(run_id).copied().unwrap_or(0);
            // Сотня уезжает всегда: на ней фронт гасит полосу.
            if percent < 100 && percent < last.saturating_add(VOICE_PROGRESS_STEP) {
                return;
            }
            map.insert(run_id.to_string(), percent);
        }
        let (dialog_id, message_id, index) = Self::coords(key);
        let _ = self.tx.send(TgServerMsg::VoiceTranscribeDelta {
            run_id: run_id.to_string(),
            dialog_id,
            message_id,
            index,
            percent,
        });
    }

    fn error(&self, run_id: &str, key: &forge_voice::VoiceKey, code: &str, message: &str) {
        self.forget(run_id);
        let (dialog_id, message_id, index) = Self::coords(key);
        let _ = self.tx.send(TgServerMsg::VoiceTranscribeError {
            run_id: run_id.to_string(),
            dialog_id,
            message_id,
            index,
            code: code.to_string(),
            message: message.to_string(),
        });
    }

    fn done(
        &self,
        run_id: &str,
        key: &forge_voice::VoiceKey,
        transcript: &forge_voice::Transcript,
    ) {
        self.forget(run_id);
        let (dialog_id, message_id, index) = Self::coords(key);
        let _ = self.tx.send(TgServerMsg::VoiceTranscribeDone {
            run_id: run_id.to_string(),
            dialog_id,
            message_id,
            index,
            text: transcript.text.clone(),
            language: transcript.language.clone(),
            engine: transcript.engine.as_str().to_string(),
            duration_ms: transcript.duration_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_login_state() -> LoginStateDto {
        LoginStateDto {
            state: "not_configured".into(),
            phone: None,
            hint: None,
            error: None,
            user: None,
        }
    }

    fn sample_message() -> MessageDto {
        MessageDto {
            id: 7,
            dialog_id: "-1001234567890123".into(),
            out: false,
            date: 1_800_000_000,
            text: "hello".into(),
            ..MessageDto::default()
        }
    }

    fn all_server_msgs() -> Vec<TgServerMsg> {
        vec![
            TgServerMsg::LoginState(sample_login_state()),
            TgServerMsg::NewMessage {
                message: sample_message(),
                muted: false,
                archived: false,
            },
            TgServerMsg::MessageEdited {
                message: sample_message(),
            },
            TgServerMsg::MessageDeleted {
                dialog_id: "123".into(),
                message_ids: vec![1, 2, 3],
            },
            TgServerMsg::Typing {
                dialog_id: "123".into(),
                user_id: "456".into(),
                action: "typing".into(),
            },
            TgServerMsg::Online {
                user_id: "456".into(),
                online: true,
                was_online: Some(1_800_000_000),
            },
            TgServerMsg::Read {
                dialog_id: "123".into(),
                max_id: 99,
                direction: "in".into(),
            },
            TgServerMsg::UploadProgress {
                upload_id: "u-1".into(),
                sent: 10,
                total: 100,
            },
            TgServerMsg::DownloadProgress {
                dialog_id: "123".into(),
                message_id: 7,
                kind: "full".into(),
                received: 10,
                total: None,
            },
            TgServerMsg::VoiceTranscribeStarted {
                run_id: "voice-1".into(),
                dialog_id: "-1001234".into(),
                message_id: 7,
                index: 0,
                engine: "cloud".into(),
            },
            TgServerMsg::VoiceTranscribeDelta {
                run_id: "voice-1".into(),
                dialog_id: "-1001234".into(),
                message_id: 7,
                index: 0,
                percent: 40,
            },
            TgServerMsg::VoiceTranscribeDone {
                run_id: "voice-1".into(),
                dialog_id: "-1001234".into(),
                message_id: 7,
                index: 0,
                text: "привет, это голосовое".into(),
                language: "ru".into(),
                engine: "cloud".into(),
                duration_ms: 4200,
            },
            TgServerMsg::VoiceTranscribeError {
                run_id: "voice-1".into(),
                dialog_id: "-1001234".into(),
                message_id: 7,
                index: 0,
                code: "metal_unavailable".into(),
                message: "whisper не подтвердил Metal".into(),
            },
            TgServerMsg::NotifySettings {
                dialog_id: "123".into(),
                muted: true,
            },
            TgServerMsg::NotifyDefaultsChanged {
                users: true,
                chats: false,
                broadcasts: true,
            },
            TgServerMsg::FoldersChanged,
            TgServerMsg::AnalysisStarted {
                run_id: "run-1".into(),
                dialog_id: "123".into(),
                kind: "analyze".into(),
                messages_used: 137,
                truncated: true,
            },
            TgServerMsg::AnalysisDelta {
                run_id: "run-1".into(),
                dialog_id: "123".into(),
                text: "кусок ответа".into(),
            },
            TgServerMsg::AnalysisDone {
                run_id: "run-1".into(),
                dialog_id: "123".into(),
                ok: false,
                reason: Some("cancelled".into()),
                input_tokens: 4096,
                output_tokens: 512,
            },
            TgServerMsg::AnalysisError {
                run_id: "run-1".into(),
                dialog_id: "123".into(),
                message: "spawn failed".into(),
            },
            TgServerMsg::Resync,
            TgServerMsg::Ping,
        ]
    }

    #[test]
    fn server_msgs_round_trip() {
        for msg in all_server_msgs() {
            let json = serde_json::to_string(&msg).unwrap();
            let back: TgServerMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, back, "round-trip сломался на {json}");
        }
    }

    #[test]
    fn client_msgs_round_trip() {
        let msg = TgClientMsg::Pong;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"pong"}"#);
        let back: TgClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    /// Точная форма кадров про уведомления: `new_message` несёт `muted` и
    /// `archived` НА УРОВНЕ КАДРА (рядом с `message`, а не внутри него), а
    /// `notify_defaults_changed` — три булевых категории. Это дословный
    /// контракт с фронтом, поэтому проверка байт-в-байт.
    #[test]
    fn new_message_carries_frame_level_mute_and_archive() {
        let msg = TgServerMsg::NewMessage {
            message: MessageDto {
                id: 7,
                dialog_id: "-1001234567890123".into(),
                ..MessageDto::default()
            },
            muted: true,
            archived: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.starts_with(r#"{"type":"new_message","message":{"#),
            "{json}"
        );
        assert!(
            json.ends_with(r#""muted":true,"archived":true}"#),
            "флаги обязаны лежать в кадре, а не в message: {json}"
        );
        let back: TgServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);

        // Оба флага обязательны: кадр без них — это старый сервер, и
        // молчаливая подстановка `false` вернула бы прежний дефект.
        // Урезаем ВАЛИДНЫЙ кадр, чтобы тест не мог «пройти» из-за опечатки
        // в рукописном JSON сообщения.
        for drop_field in ["muted", "archived"] {
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            value.as_object_mut().unwrap().remove(drop_field);
            assert!(
                serde_json::from_value::<TgServerMsg>(value).is_err(),
                "кадр без {drop_field} обязан отвергаться"
            );
        }
    }

    #[test]
    fn notify_defaults_changed_frame_round_trips() {
        let msg = TgServerMsg::NotifyDefaultsChanged {
            users: true,
            chats: false,
            broadcasts: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"notify_defaults_changed","users":true,"chats":false,"broadcasts":true}"#
        );
        let back: TgServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);

        // Категория, потерянная по дороге, — ошибка разбора: «нет поля»
        // нельзя трактовать как «не заглушено».
        assert!(serde_json::from_str::<TgServerMsg>(
            r#"{"type":"notify_defaults_changed","users":true,"chats":false}"#
        )
        .is_err());
    }

    /// Точная форма новых кадров: `notify_settings` несёт dialog_id-строку
    /// и bool, `folders_changed` — только тег. Round-trip — байт-в-байт.
    #[test]
    fn notify_settings_and_folders_changed_frames_round_trip() {
        let msg = TgServerMsg::NotifySettings {
            dialog_id: "-1001234567890123".into(),
            muted: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"notify_settings","dialog_id":"-1001234567890123","muted":true}"#
        );
        let back: TgServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);

        let msg = TgServerMsg::FoldersChanged;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"folders_changed"}"#);
        let back: TgServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);

        // Нарушение инварианта id-строк обязано быть ошибкой разбора.
        assert!(serde_json::from_str::<TgServerMsg>(
            r#"{"type":"notify_settings","dialog_id":123,"muted":true}"#
        )
        .is_err());
    }

    #[test]
    fn type_tags_are_snake_case() {
        let expected = [
            "login_state",
            "new_message",
            "message_edited",
            "message_deleted",
            "typing",
            "online",
            "read",
            "upload_progress",
            "download_progress",
            "voice_transcribe_started",
            "voice_transcribe_delta",
            "voice_transcribe_done",
            "voice_transcribe_error",
            "notify_settings",
            "notify_defaults_changed",
            "folders_changed",
            "analysis_started",
            "analysis_delta",
            "analysis_done",
            "analysis_error",
            "resync",
            "ping",
        ];
        assert_eq!(
            all_server_msgs().len(),
            expected.len(),
            "zip ниже молча обрезал бы хвост, если списки разошлись"
        );
        for (msg, tag) in all_server_msgs().iter().zip(expected) {
            let json = serde_json::to_string(msg).unwrap();
            assert!(
                json.contains(&format!("\"type\":\"{tag}\"")),
                "ожидался тег {tag}, получено {json}"
            );
        }
    }

    #[test]
    fn login_state_is_flattened_into_the_frame() {
        // LoginState — newtype-вариант: поля DTO лежат прямо в кадре,
        // без вложенного объекта. Фронт читает msg.state напрямую.
        let json = serde_json::to_string(&TgServerMsg::LoginState(sample_login_state())).unwrap();
        assert!(json.contains(r#""state":"not_configured""#), "{json}");
    }

    #[test]
    fn sixty_four_bit_ids_are_strings() {
        let json = serde_json::to_string(&TgServerMsg::Typing {
            dialog_id: "-1001234567890123".into(),
            user_id: "9876543210987".into(),
            action: "typing".into(),
        })
        .unwrap();
        assert!(json.contains(r#""dialog_id":"-1001234567890123""#), "{json}");
        assert!(json.contains(r#""user_id":"9876543210987""#), "{json}");

        // message_id остаётся числом (32-битный в Telegram).
        let json = serde_json::to_string(&TgServerMsg::MessageDeleted {
            dialog_id: "1".into(),
            message_ids: vec![5],
        })
        .unwrap();
        assert!(json.contains(r#""message_ids":[5]"#), "{json}");
    }

    #[test]
    fn unknown_client_frame_is_rejected() {
        let err = serde_json::from_str::<TgClientMsg>(r#"{"type":"nope"}"#);
        assert!(err.is_err(), "неизвестный тип не должен парситься");
    }

    /// Тег каждого варианта — ИСЧЕРПЫВАЮЩИМ `match`.
    ///
    /// Это сторож компилятора: добавили вариант в [`TgServerMsg`] — тесты
    /// перестали собираться, пока автор не дописал сюда тег и не добавил
    /// образец в [`all_server_msgs`]. Без него новый вариант тихо остался бы
    /// без round-trip-проверки и без записи в таблице WS-событий.
    fn tag_of(msg: &TgServerMsg) -> &'static str {
        match msg {
            TgServerMsg::LoginState(_) => "login_state",
            TgServerMsg::NewMessage { .. } => "new_message",
            TgServerMsg::MessageEdited { .. } => "message_edited",
            TgServerMsg::MessageDeleted { .. } => "message_deleted",
            TgServerMsg::Typing { .. } => "typing",
            TgServerMsg::Online { .. } => "online",
            TgServerMsg::Read { .. } => "read",
            TgServerMsg::UploadProgress { .. } => "upload_progress",
            TgServerMsg::DownloadProgress { .. } => "download_progress",
            TgServerMsg::VoiceTranscribeStarted { .. } => "voice_transcribe_started",
            TgServerMsg::VoiceTranscribeDelta { .. } => "voice_transcribe_delta",
            TgServerMsg::VoiceTranscribeDone { .. } => "voice_transcribe_done",
            TgServerMsg::VoiceTranscribeError { .. } => "voice_transcribe_error",
            TgServerMsg::NotifySettings { .. } => "notify_settings",
            TgServerMsg::NotifyDefaultsChanged { .. } => "notify_defaults_changed",
            TgServerMsg::FoldersChanged => "folders_changed",
            TgServerMsg::AnalysisStarted { .. } => "analysis_started",
            TgServerMsg::AnalysisDelta { .. } => "analysis_delta",
            TgServerMsg::AnalysisDone { .. } => "analysis_done",
            TgServerMsg::AnalysisError { .. } => "analysis_error",
            TgServerMsg::Resync => "resync",
            TgServerMsg::Ping => "ping",
        }
    }

    #[test]
    fn every_server_variant_has_a_sample() {
        // Список тегов дублирует match выше НАМЕРЕННО: match ловит новый
        // вариант на компиляции, а эта проверка — что образец для него
        // действительно попал в all_server_msgs (иначе round-trip его не
        // тронет).
        let expected = [
            "login_state",
            "new_message",
            "message_edited",
            "message_deleted",
            "typing",
            "online",
            "read",
            "upload_progress",
            "download_progress",
            "voice_transcribe_started",
            "voice_transcribe_delta",
            "voice_transcribe_done",
            "voice_transcribe_error",
            "notify_settings",
            "notify_defaults_changed",
            "folders_changed",
            "analysis_started",
            "analysis_delta",
            "analysis_done",
            "analysis_error",
            "resync",
            "ping",
        ];
        let actual: Vec<&'static str> = all_server_msgs().iter().map(tag_of).collect();
        assert_eq!(
            actual.len(),
            expected.len(),
            "число образцов разошлось с числом вариантов: {actual:?}"
        );
        // ПОРЯДОК, а не только состав: `telegram_contract.test.mjs` сверяет
        // этот же список с `SERVER_FRAMES` в `js/telegram/ws.js` позиция в
        // позицию. Перестановка двух кадров ломает разбор на фронте молча —
        // здесь она обязана краснеть.
        assert_eq!(
            actual.as_slice(),
            expected.as_slice(),
            "порядок вариантов TgServerMsg заморожен и зеркалится во фронт"
        );
        for tag in expected {
            assert!(actual.contains(&tag), "для варианта {tag} нет образца");
        }
        // Дубликатов быть не должно — иначе «столько же образцов» ничего не
        // доказывает.
        let mut sorted = actual.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), actual.len(), "образцы дублируются: {actual:?}");
    }

    #[test]
    fn unknown_server_frame_is_an_error_not_a_panic() {
        // Клиент может получить кадр от НОВОГО сервера (обновили бинарь,
        // страница закеширована) — и наоборот. Ни одна сторона не имеет права
        // паниковать: тип, которого нет, обязан быть обычной ошибкой разбора.
        for raw in [
            r#"{"type":"brand_new_event","x":1}"#,
            r#"{"type":42}"#,
            r#"{"no_type":true}"#,
            r#"{"type":"ping","unexpected":"field"}"#,
            "null",
            "[]",
            "not json at all",
            "",
        ] {
            let parsed = serde_json::from_str::<TgServerMsg>(raw);
            if raw.contains("\"ping\"") {
                // Лишнее поле — не ошибка: serde по умолчанию его игнорирует,
                // и это ЖЕЛАЕМОЕ поведение (сервер вправе дополнить кадр).
                assert!(parsed.is_ok(), "кадр с лишним полем должен читаться: {raw}");
            } else {
                assert!(parsed.is_err(), "ожидалась ошибка разбора для: {raw}");
            }
        }
    }

    #[test]
    fn client_frame_tolerates_extra_fields_but_not_missing_tag() {
        assert!(serde_json::from_str::<TgClientMsg>(r#"{"type":"pong","seq":7}"#).is_ok());
        assert!(serde_json::from_str::<TgClientMsg>(r#"{"seq":7}"#).is_err());
        assert!(serde_json::from_str::<TgClientMsg>("\"pong\"").is_err());
    }

    #[test]
    fn numeric_boundaries_round_trip() {
        // Реальные крайние значения: message_id в Telegram доходит до
        // i32::MAX в старых мегагруппах, date бывает до-1970 у импортов,
        // was_online == -1/0 отдаёт сам Telegram (скрытый статус), а размеры
        // файлов близ u64::MAX не должны терять точность (f64-парсер молча
        // округлил бы). Любая смена типа поля ломает этот тест.
        let msgs = vec![
            TgServerMsg::MessageDeleted {
                dialog_id: "0".into(),
                message_ids: vec![i32::MIN, -1, 0, 1, i32::MAX],
            },
            // Пустой список удалений — валидный кадр, а не ошибка.
            TgServerMsg::MessageDeleted {
                dialog_id: "1".into(),
                message_ids: vec![],
            },
            TgServerMsg::Read {
                dialog_id: "1".into(),
                max_id: i32::MAX,
                direction: "out".into(),
            },
            TgServerMsg::Online {
                user_id: "1".into(),
                online: false,
                was_online: Some(-1),
            },
            TgServerMsg::Online {
                user_id: "1".into(),
                online: false,
                was_online: Some(i64::MIN),
            },
            TgServerMsg::UploadProgress {
                upload_id: "u".into(),
                sent: u64::MAX,
                total: u64::MAX,
            },
            TgServerMsg::DownloadProgress {
                dialog_id: "1".into(),
                message_id: i32::MAX,
                kind: "full".into(),
                received: u64::MAX,
                total: Some(u64::MAX),
            },
            TgServerMsg::NewMessage {
                message: MessageDto {
                    id: i32::MAX,
                    dialog_id: "-1009999999999999".into(),
                    out: true,
                    date: -2_208_988_800, // 1900-01-01: импортированные чаты
                    edit_date: Some(0),
                    reply_to_id: Some(i32::MIN),
                    ..MessageDto::default()
                },
                muted: true,
                archived: true,
            },
        ];
        for msg in msgs {
            let json = serde_json::to_string(&msg).unwrap();
            let back: TgServerMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, back, "граничное значение потерялось: {json}");
        }
    }

    #[test]
    fn unicode_and_control_strings_round_trip() {
        // Пользовательский текст — произвольный: эмодзи, RTL-override
        // (популярный трюк маскировки расширений файлов), составные
        // диакритики, NUL и переводы строк, кавычки и бэкслеши. Всё это
        // обязано пережить JSON-экранирование байт-в-байт.
        let samples = [
            "🦀🔥💬",
            "плюс \u{202E}gpj.exe маскировка",
            "e\u{0301}toile (комбинированный акцент)",
            "nul\u{0}внутри и \r\n перевод строки",
            "\"кавычки\" и \\бэкслеш\\ и /слеши/",
            "עברית مرحبا שלום",
        ];
        for text in samples {
            let msg = TgServerMsg::NewMessage {
                message: MessageDto {
                    id: 1,
                    dialog_id: "1".into(),
                    text: text.into(),
                    ..MessageDto::default()
                },
                muted: false,
                archived: false,
            };
            let json = serde_json::to_string(&msg).unwrap();
            let back: TgServerMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, back, "текст исказился: {json}");

            let typing = TgServerMsg::Typing {
                dialog_id: "1".into(),
                user_id: "2".into(),
                action: text.into(),
            };
            let back: TgServerMsg =
                serde_json::from_str(&serde_json::to_string(&typing).unwrap()).unwrap();
            assert_eq!(typing, back);
        }
    }

    #[test]
    fn truncated_frames_error_not_panic() {
        // Обрыв кадра на полпути (прокси оборвал соединение, буфер
        // обрезался) — каждый префикс валидного кадра обязан давать Err,
        // а не панику и не «успешный» разбор половины сообщения.
        let full = serde_json::to_string(&TgServerMsg::NewMessage {
            message: MessageDto {
                id: 7,
                dialog_id: "-100123".into(),
                text: "обрыв 🦀 посередине".into(),
                ..MessageDto::default()
            },
            muted: false,
            archived: false,
        })
        .unwrap();
        for i in 0..full.len() {
            if !full.is_char_boundary(i) {
                continue;
            }
            let prefix = &full[..i];
            assert!(
                serde_json::from_str::<TgServerMsg>(prefix).is_err(),
                "обрезанный на {i} байте кадр не должен парситься: {prefix}"
            );
        }
    }

    #[test]
    fn mistyped_fields_are_rejected() {
        // Version-skew между сервером и фронтом: поле сменило тип — это
        // обязано быть ошибкой разбора, а не тихим приведением. Особенно
        // важны id-инварианты: 64-битный id ЧИСЛОМ (старый формат) должен
        // отвергаться, иначе фронт снова начнёт терять точность в JS.
        for raw in [
            // user_id числом вместо строки — нарушение инварианта id.
            r#"{"type":"online","user_id":9876543210987,"online":true,"was_online":null}"#,
            // bool строкой.
            r#"{"type":"online","user_id":"1","online":"yes","was_online":null}"#,
            // Смешанный массив id.
            r#"{"type":"message_deleted","dialog_id":"1","message_ids":[1,"2"]}"#,
            // Отрицательный u64.
            r#"{"type":"upload_progress","upload_id":"u","sent":-1,"total":10}"#,
            // Переполнение i32 — молчаливое усечение недопустимо.
            r#"{"type":"read","dialog_id":"1","max_id":2147483648,"direction":"in"}"#,
            // Дробный message_id.
            r#"{"type":"download_progress","dialog_id":"1","message_id":7.5,"kind":"full","received":0,"total":null}"#,
            // Тег есть, обязательного поля нет.
            r#"{"type":"new_message"}"#,
        ] {
            assert!(
                serde_json::from_str::<TgServerMsg>(raw).is_err(),
                "кадр с неверным типом поля обязан отвергаться: {raw}"
            );
        }
    }

    #[test]
    fn frame_tags_are_case_sensitive() {
        // Контракт — строго snake_case в нижнем регистре. «Почти pong»
        // (регистры, пробелы) — это чужой протокол, а не наш: молчаливое
        // принятие маскировало бы рассинхрон версии фронта.
        for raw in [
            r#"{"type":"Pong"}"#,
            r#"{"type":"PONG"}"#,
            r#"{"type":"pong "}"#,
            r#"{"type":" pong"}"#,
        ] {
            assert!(
                serde_json::from_str::<TgClientMsg>(raw).is_err(),
                "не-каноничный тег обязан отвергаться: {raw}"
            );
        }
        assert!(serde_json::from_str::<TgServerMsg>(r#"{"type":"Ping"}"#).is_err());
    }

    #[test]
    fn optional_fields_survive_both_states() {
        // Option-поля кадра — единственное место, где «нет значения» и
        // «значение 0» различаются (was_online/total). Round-trip обязан
        // сохранять именно None, а не превращать его в 0.
        for (was_online, total) in [(None, None), (Some(0_i64), Some(0_u64))] {
            let online = TgServerMsg::Online {
                user_id: "1".into(),
                online: false,
                was_online,
            };
            let back: TgServerMsg =
                serde_json::from_str(&serde_json::to_string(&online).unwrap()).unwrap();
            assert_eq!(online, back);

            let dl = TgServerMsg::DownloadProgress {
                dialog_id: "1".into(),
                message_id: 1,
                kind: "thumb".into(),
                received: 0,
                total,
            };
            let back: TgServerMsg =
                serde_json::from_str(&serde_json::to_string(&dl).unwrap()).unwrap();
            assert_eq!(dl, back);
        }
    }
}
