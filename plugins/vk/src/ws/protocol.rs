//! Типизированный WS-протокол плагина VK (`/ws/vk`).
//!
//! Формат кадра — JSON с внешним тегом `type` в snake_case:
//!
//! ```json
//! {"type":"login_state","state":"not_configured","error":null,"user":null,"captcha_sid":null,"captcha_img":null}
//! {"type":"new_message","message":{"id":7,"dialog_id":"2000000001","...":null},"muted":false,"archived":false}
//! {"type":"download_progress","dialog_id":"123","message_id":7,"index":2,"kind":"full","received":1024,"total":4096}
//! {"type":"unread_total","count":12}
//! ```
//!
//! Все варианты объявлены сразу — включая те, что начнут отправляться в
//! следующих фазах. Это сделано намеренно: фронтовый `buildHandlers` пишется
//! один раз по этой таблице и не переписывается при каждой фазе.
//!
//! Инвариант id-полей: `dialog_id` (у VK это `peer_id`) и `user_id` — СТРОКИ
//! (`peer_id` беседы = `2_000_000_000 + chat_id`, и точность JS-числа на
//! 64-битных значениях теряется), `message_id` — `i32`.
//!
//! ## Чем набор кадров отличается от телеграмного
//!
//! | кадр | судьба | почему |
//! |---|---|---|
//! | `login_state` | 1:1 | набор состояний другой (7 значений), но форма та же |
//! | `new_message`, `message_edited`, `message_deleted` | 1:1 | |
//! | `typing`, `online`, `read`, `upload_progress`, `notify_settings`, `resync`, `ping` | 1:1 | |
//! | `download_progress` | **+ поле `index`** | у сообщения VK до 10 вложений, и без индекса прогресс второго фото прилетал бы в карточку первого |
//! | `notify_defaults_changed` | **УДАЛЁН** | у VK нет глобальных категорий mute — глушение только per-peer (`push_settings`) |
//! | `folders_changed` | **УДАЛЁН** | папок нет; их место занимают 4 СЕРВЕРНЫХ фильтра, состав которых константен |
//! | `unread_total` | **НОВЫЙ** | событие LongPoll 80 отдаёт общий счётчик готовым, и бейдж перестаёт быть суммой по загруженной странице |
//! | `history_cleared` | **НОВЫЙ** | событие 13: VK не присылает id удалённых сообщений, только границу — `message_deleted` тут неприменим |
//! | `dialog_changed` | **НОВЫЙ** | события 51/52: фронт перечитывает ОДНУ строку списка, а не весь список |
//! | `analysis_started`, `analysis_delta`, `analysis_done`, `analysis_error` | **НОВЫЕ** | стриминг анализа чата через Claude (АЧ-Ф3) |
//!
//! ## Кадры анализа уходят ВСЕМ подписчикам — это осознанное следствие
//!
//! Broadcast плагина работает без фильтрации по вкладке и по диалогу
//! ([`crate::ws`], `ws/mod.rs:34-38`): один канал, все подписчики. Для
//! `new_message` это ТРЕБОВАНИЕ (бейдж непрочитанного и тосты обязаны работать
//! с любой вкладки), и кадры анализа наследуют то же свойство.
//!
//! Следствие названо прямо, потому что оно не тривиально: **текст анализа
//! переписки виден во ВСЕХ открытых вкладках пользователя, включая remote-
//! сессию**. Это не недосмотр и не дыра в авторизации — за `/ws/vk` стоит
//! Bearer-auth хоста, и подписчиками могут быть только вкладки самого
//! пользователя. Но пересказ личной переписки, который «сам собой» появился в
//! соседнем окне на общем экране, — сюрприз, и знать о нём надо заранее.
//!
//! Фильтрация — ОБЯЗАННОСТЬ ФРОНТА, по паре (`dialog_id`, `run_id`): попап
//! рисует только те кадры, чей `run_id` он сам получил из ответа
//! `POST /api/vk/dialogs/:id/analysis`. Серверная фильтрация здесь была бы
//! ложным чувством приватности: она потребовала бы адресации подписчиков,
//! которой в модели broadcast нет вовсе.
//!
//! `Thinking`-события Claude в WS НЕ вещаются ни в каком виде (АЧ-Ф3):
//! удвоенный трафик ради текста, который в попапе не показывается.
//!
//! ## `muted`/`archived` лежат на уровне КАДРА
//!
//! Это свойство диалога, а не сообщения, и источник истины по нему — сервер.
//! Фронт видит только загруженную страницу списка бесед, поэтому решать
//! «звенеть ли» по ней означало бы всплывать тостом от каждого заглушённого
//! чата за её пределами. `archived` у VK всегда `false` (архива нет) и
//! сохранён РАДИ ИДЕНТИЧНОСТИ обработчика: удалив поле, пришлось бы
//! разветвлять `js/vk/main.js` относительно телеграмного близнеца.

use serde::{Deserialize, Serialize};

use crate::dto::MessageDto;
use crate::login::LoginStateDto;

/// То, что летит в broadcast-канал [`crate::state::VkState`] и дальше —
/// каждому WS-подписчику.
///
/// Обёртки над сообщением НЕТ намеренно: broadcast идёт без фильтрации,
/// потому что unread-бейдж и тосты должны работать с ЛЮБОЙ вкладки.
pub type VkServerEvent = VkServerMsg;

/// Сообщения сервер → клиент.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VkServerMsg {
    /// Текущее состояние авторизации. Отправляется первым кадром после
    /// апгрейда и при каждом переходе [`crate::login::LoginFlow`].
    LoginState(LoginStateDto),
    /// Пришло новое сообщение (событие LongPoll 4).
    NewMessage {
        /// Сообщение целиком.
        message: MessageDto,
        /// Заглушена ли БЕСЕДА, из которой пришло сообщение. Фронт обязан
        /// молчать при `true`, не заглядывая в свой список.
        muted: bool,
        /// ВСЕГДА `false` — у VK нет архива. Поле сохранено ради
        /// идентичности фронтового обработчика (см. шапку модуля).
        archived: bool,
    },
    /// Сообщение отредактировано (события 5 и 18 — правка и появление
    /// сниппета у ссылки).
    MessageEdited {
        /// Новая версия сообщения.
        message: MessageDto,
    },
    /// Сообщения удалены (событие 2 с битом 128 `DELETED`).
    MessageDeleted {
        /// Беседа, из которой удалены (строкой).
        dialog_id: String,
        /// Идентификаторы удалённых сообщений.
        message_ids: Vec<i32>,
    },
    /// Собеседник печатает или записывает голосовое (события 61/62/63/64).
    Typing {
        /// Беседа (строкой).
        dialog_id: String,
        /// Кто печатает (строкой).
        user_id: String,
        /// `typing` | `record_voice`. У VK видов активности всего два — в
        /// отличие от десятка у Telegram.
        action: String,
    },
    /// Изменился online-статус пользователя (события 8/9).
    Online {
        /// Пользователь (строкой).
        user_id: String,
        /// `true` ⇒ сейчас в сети.
        online: bool,
        /// Unix-время последнего появления, если VK его отдал.
        was_online: Option<i64>,
    },
    /// Отметка о прочтении (события 6/7). `direction`: `in` — прочитали мы,
    /// `out` — прочитали нас.
    Read {
        /// Беседа (строкой).
        dialog_id: String,
        /// Максимальный прочитанный `message_id`.
        max_id: i32,
        /// `in` | `out`.
        direction: String,
    },
    /// Прогресс заливки файла в VK.
    ///
    /// ВАЖНОЕ ОТЛИЧИЕ ОТ TELEGRAM: кадр приходит во время `POST
    /// /api/vk/dialogs/:id/messages`, а НЕ во время `POST /api/vk/upload`.
    /// Upload-сервер VK выдаётся под конкретный `peer_id`, а на шаге
    /// `/upload` адресат ещё неизвестен, поэтому весь 3-шаговый конвейер
    /// живёт внутри отправки сообщения. Форма кадра при этом не меняется —
    /// фронт обязан лишь продолжать слушать его после нажатия «отправить».
    UploadProgress {
        /// Идентификатор upload-сессии, выданный `POST /api/vk/upload`.
        upload_id: String,
        /// Отправлено байт.
        sent: u64,
        /// Всего байт.
        total: u64,
    },
    /// Прогресс скачивания вложения в файловый кэш.
    DownloadProgress {
        /// Беседа (строкой).
        dialog_id: String,
        /// Сообщение, чьё вложение качаем.
        message_id: i32,
        /// Порядковый номер вложения В СООБЩЕНИИ (0..=9) — без него
        /// прогресс второго фото прилетал бы в карточку первого.
        index: u8,
        /// `thumb` | `full`.
        kind: String,
        /// Получено байт.
        received: u64,
        /// Всего байт, если размер известен заранее.
        total: Option<u64>,
    },
    /// Расшифровка голосового началась (голосовые-Ф3).
    ///
    /// Четвёрка кадров `voice_transcribe_*` стоит ЗДЕСЬ, между
    /// `download_progress` и `notify_settings`, а не в конце enum'а. Причина
    /// механическая: порядок кадров анализа заморожен тестом
    /// `analysis_frames_sit_between_dialog_changed_and_resync`, и вставка
    /// между `dialog_changed` и `resync` его сломала бы. Место при этом
    /// осмысленное: расшифровка — продолжение жизненного цикла вложения,
    /// который начинается `download_progress`.
    ///
    /// Состав и имена полей ОБЯЗАНЫ совпадать с Telegram и Bitrix: на этом
    /// держится общий фронт-модуль (Ф5), один на три мессенджера.
    VoiceTranscribeStarted {
        /// Идентификатор прогона; им же различаются кадры двух вкладок.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения в сообщении.
        index: u8,
        /// `local` | `cloud` — человек имеет право знать, ушло ли голосовое в
        /// чужое облако, ДО того, как оно туда ушло.
        engine: String,
    },
    /// Прогресс расшифровки, проценты.
    ///
    /// КОАЛЕСЦИРОВАН в [`VkVoiceSink`]: whisper печатает прогресс десятками
    /// строк в секунду, и слать каждую значило бы переполнить broadcast
    /// ([`crate::state::BROADCAST_CAPACITY`]), то есть получить `Lagged` и
    /// [`VkServerMsg::Resync`] — перечитывание всего списка бесед посреди
    /// расшифровки.
    VoiceTranscribeDelta {
        /// Идентификатор прогона.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения в сообщении.
        index: u8,
        /// Проценты `0..=100`.
        percent: u8,
    },
    /// Расшифровка готова; текст уже лежит в сторе.
    VoiceTranscribeDone {
        /// Идентификатор прогона.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения в сообщении.
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
    /// `code` отдельно от текста намеренно: по нему фронт различает случаи
    /// (`metal_unavailable` — «включите облако или почините GPU»,
    /// `voice_no_key` — «введите ключ»), и разбирать это из русской фразы он
    /// не должен.
    VoiceTranscribeError {
        /// Идентификатор прогона.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Сообщение с голосовым.
        message_id: i32,
        /// Номер вложения в сообщении.
        index: u8,
        /// Машинный код отказа (`forge_voice::VoiceError::code`).
        code: String,
        /// Человеческий текст с хвостом stderr.
        message: String,
    },
    /// Настройки оповещений беседы изменились (событие 114 — в том числе с
    /// телефона). Фронт обновляет `muted` у одной строки, не перечитывая
    /// список.
    NotifySettings {
        /// Беседа (строкой).
        dialog_id: String,
        /// `true` ⇒ уведомления заглушены.
        muted: bool,
    },
    /// Общий счётчик непрочитанного (событие 80).
    ///
    /// НОВЫЙ относительно Telegram: VK отдаёт его готовым, поэтому бейдж на
    /// кнопке перестаёт быть суммой по загруженной странице списка.
    UnreadTotal {
        /// Сколько бесед/сообщений непрочитано по данным VK.
        count: i32,
    },
    /// История беседы очищена до `up_to_id` (событие 13).
    ///
    /// НОВЫЙ: VK не присылает id удалённых сообщений — только границу,
    /// поэтому [`VkServerMsg::MessageDeleted`] здесь неприменим.
    HistoryCleared {
        /// Беседа (строкой).
        dialog_id: String,
        /// Всё, что `<= up_to_id`, из ленты убирается.
        up_to_id: i32,
    },
    /// Данные беседы изменились (события 51/52: название, состав, фото).
    ///
    /// НОВЫЙ: фронт перечитывает ОДНУ строку списка, а не весь список.
    DialogChanged {
        /// Беседа (строкой).
        dialog_id: String,
    },
    /// Анализ чата начался (АЧ-Ф3).
    ///
    /// Эмитится ПОСЛЕ сборки транскрипта, поэтому уже несёт его размеры:
    /// отдельного кадра «читаю историю» нет — эту фазу фронт рисует сам,
    /// сразу после успешного `POST /api/vk/dialogs/:id/analysis`.
    AnalysisStarted {
        /// Идентификатор run'а; им же отменяют.
        run_id: String,
        /// Беседа (строкой), по которой идёт анализ.
        dialog_id: String,
        /// `analyze` | `ask`.
        kind: String,
        /// Сколько сообщений попало в транскрипт.
        messages_used: u32,
        /// Часть истории отброшена потолком символов.
        truncated: bool,
    },
    /// Кусок ответа модели.
    ///
    /// КОАЛЕСЦИРОВАН на сервере (не чаще одного кадра в
    /// [`crate::analysis::DELTA_FLUSH_MS`] мс либо по накоплению
    /// [`crate::analysis::DELTA_FLUSH_CHARS`] символов). Сырые дельты Claude
    /// идут сотнями в секунду и переполнили бы broadcast
    /// ([`crate::state::BROADCAST_CAPACITY`]), а переполнение — это `Lagged`,
    /// то есть [`VkServerMsg::Resync`] и перечитывание всего списка бесед.
    AnalysisDelta {
        /// Идентификатор run'а.
        run_id: String,
        /// Беседа (строкой).
        dialog_id: String,
        /// Приращение текста; склеивается конкатенацией в порядке получения.
        text: String,
    },
    /// Анализ завершён (успехом, ошибкой, отменой или таймаутом).
    AnalysisDone {
        /// Идентификатор run'а.
        run_id: String,
        /// Беседа (строкой).
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
        /// Беседа (строкой).
        dialog_id: String,
        /// Текст ошибки как есть.
        message: String,
    },
    /// Клиент отстал/пропустил события — перечитать состояние по REST.
    Resync,
    /// Heartbeat. Клиент обязан ответить [`VkClientMsg::Pong`].
    Ping,
}

/// Сообщения клиент → сервер.
///
/// Их намеренно ровно один: вся мутация идёт по REST (её проще авторизовать,
/// логировать и ретраить), а WS — односторонний поток событий с heartbeat'ом.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VkClientMsg {
    /// Ответ на [`VkServerMsg::Ping`]; сбрасывает idle-таймер соединения.
    Pong,
}

/// Приёмник кадров анализа для общего ядра
/// ([`forge_chat_analysis::AnalysisSink`]).
///
/// Вся развязка плагина с крейтом — вот эти четыре метода. Ядро не знает ни
/// про [`VkServerMsg`], ни про broadcast: оно зовёт `started`/`delta`/`done`/
/// `error`, а перевод в НАШ кадр и рассылка живут здесь, рядом с самим
/// enum'ом — там же, где придётся править, если состав полей кадра изменится.
///
/// Теги и ПОРЯДОК вариантов [`VkServerMsg`] при этом заморожены contract-
/// тестом (`vk_contract.test.mjs` сверяет их с `SERVER_FRAMES` в
/// `js/vk/ws.js`), поэтому сюда переезжает именно конструирование, а не форма
/// кадров.
///
/// `SendError` глотается: отсутствие подписчиков — норма (никто не открыл
/// вкладку), и это не повод ронять run, который уже идёт.
#[derive(Debug, Clone)]
pub struct VkAnalysisSink {
    tx: tokio::sync::broadcast::Sender<VkServerEvent>,
}

impl VkAnalysisSink {
    /// Оборачивает broadcast-канал состояния.
    #[must_use]
    pub fn new(tx: tokio::sync::broadcast::Sender<VkServerEvent>) -> Self {
        Self { tx }
    }
}

impl forge_chat_analysis::AnalysisSink for VkAnalysisSink {
    fn started(
        &self,
        run_id: &str,
        dialog_id: &str,
        kind: &str,
        messages_used: u32,
        truncated: bool,
    ) {
        let _ = self.tx.send(VkServerMsg::AnalysisStarted {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            kind: kind.to_string(),
            messages_used,
            truncated,
        });
    }

    fn delta(&self, run_id: &str, dialog_id: &str, text: &str) {
        let _ = self.tx.send(VkServerMsg::AnalysisDelta {
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
        let _ = self.tx.send(VkServerMsg::AnalysisDone {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            ok,
            reason: reason.map(str::to_string),
            input_tokens,
            output_tokens,
        });
    }

    fn error(&self, run_id: &str, dialog_id: &str, message: &str) {
        let _ = self.tx.send(VkServerMsg::AnalysisError {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            message: message.to_string(),
        });
    }
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
/// Близнец [`VkAnalysisSink`] и живёт рядом с ним по той же причине: ядро
/// `forge-voice` не знает ни [`VkServerMsg`], ни broadcast'а, а перевод в наш
/// кадр обязан лежать там же, где объявлен сам кадр.
///
/// # Коалесцинг прогресса
///
/// Единственная логика поверх «переложить и отправить» — фильтр по шагу
/// [`VOICE_PROGRESS_STEP`]. Он ведётся ПО ПРОГОНУ (`run_id`), а не одним
/// счётчиком на приёмник: при потолке параллелизма больше единицы два прогона
/// перемешали бы проценты, и полоса каждого дёргалась бы назад.
#[derive(Debug, Clone)]
pub struct VkVoiceSink {
    tx: tokio::sync::broadcast::Sender<VkServerEvent>,
    /// Последний ОТПРАВЛЕННЫЙ процент каждого идущего прогона.
    sent: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u8>>>,
}

impl VkVoiceSink {
    /// Оборачивает broadcast-канал состояния.
    #[must_use]
    pub fn new(tx: tokio::sync::broadcast::Sender<VkServerEvent>) -> Self {
        Self {
            tx,
            sent: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Координаты вложения из ключа ядра.
    ///
    /// `message_id` в ядре — строка (у трёх мессенджеров он разного типа), а в
    /// кадре VK это `i32`. Неразбираемое значение даёт `0`, а не панику: ключ
    /// собирается из проверенного [`crate::media_cache::CacheKey`], но кадр —
    /// не то место, где стоит падать.
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

impl forge_voice::VoiceSink for VkVoiceSink {
    fn started(&self, run_id: &str, key: &forge_voice::VoiceKey, engine: &str) {
        let (dialog_id, message_id, index) = Self::coords(key);
        if let Ok(mut map) = self.sent.lock() {
            map.insert(run_id.to_string(), 0);
        }
        let _ = self.tx.send(VkServerMsg::VoiceTranscribeStarted {
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
        let _ = self.tx.send(VkServerMsg::VoiceTranscribeDelta {
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
        let _ = self.tx.send(VkServerMsg::VoiceTranscribeError {
            run_id: run_id.to_string(),
            dialog_id,
            message_id,
            index,
            code: code.to_string(),
            message: message.to_string(),
        });
    }

    fn done(&self, run_id: &str, key: &forge_voice::VoiceKey, transcript: &forge_voice::Transcript) {
        self.forget(run_id);
        let (dialog_id, message_id, index) = Self::coords(key);
        let _ = self.tx.send(VkServerMsg::VoiceTranscribeDone {
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
            ..LoginStateDto::default()
        }
    }

    fn sample_message() -> MessageDto {
        MessageDto {
            id: 7,
            dialog_id: "2000000001".into(),
            out: false,
            date: 1_800_000_000,
            text: "привет".into(),
            ..MessageDto::default()
        }
    }

    fn all_server_msgs() -> Vec<VkServerMsg> {
        vec![
            VkServerMsg::LoginState(sample_login_state()),
            VkServerMsg::NewMessage {
                message: sample_message(),
                muted: false,
                archived: false,
            },
            VkServerMsg::MessageEdited {
                message: sample_message(),
            },
            VkServerMsg::MessageDeleted {
                dialog_id: "123".into(),
                message_ids: vec![1, 2, 3],
            },
            VkServerMsg::Typing {
                dialog_id: "123".into(),
                user_id: "456".into(),
                action: "typing".into(),
            },
            VkServerMsg::Online {
                user_id: "456".into(),
                online: true,
                was_online: Some(1_800_000_000),
            },
            VkServerMsg::Read {
                dialog_id: "123".into(),
                max_id: 99,
                direction: "in".into(),
            },
            VkServerMsg::UploadProgress {
                upload_id: "u-1".into(),
                sent: 10,
                total: 100,
            },
            VkServerMsg::DownloadProgress {
                dialog_id: "123".into(),
                message_id: 7,
                index: 3,
                kind: "full".into(),
                received: 10,
                total: None,
            },
            VkServerMsg::VoiceTranscribeStarted {
                run_id: "voice-1".into(),
                dialog_id: "2000000001".into(),
                message_id: 7,
                index: 0,
                engine: "cloud".into(),
            },
            VkServerMsg::VoiceTranscribeDelta {
                run_id: "voice-1".into(),
                dialog_id: "2000000001".into(),
                message_id: 7,
                index: 0,
                percent: 40,
            },
            VkServerMsg::VoiceTranscribeDone {
                run_id: "voice-1".into(),
                dialog_id: "2000000001".into(),
                message_id: 7,
                index: 0,
                text: "привет, это голосовое".into(),
                language: "ru".into(),
                engine: "cloud".into(),
                duration_ms: 4200,
            },
            VkServerMsg::VoiceTranscribeError {
                run_id: "voice-1".into(),
                dialog_id: "2000000001".into(),
                message_id: 7,
                index: 0,
                code: "metal_unavailable".into(),
                message: "whisper не подтвердил Metal".into(),
            },
            VkServerMsg::NotifySettings {
                dialog_id: "123".into(),
                muted: true,
            },
            VkServerMsg::UnreadTotal { count: 12 },
            VkServerMsg::HistoryCleared {
                dialog_id: "2000000001".into(),
                up_to_id: 4242,
            },
            VkServerMsg::DialogChanged {
                dialog_id: "2000000001".into(),
            },
            VkServerMsg::AnalysisStarted {
                run_id: "run-1".into(),
                dialog_id: "2000000001".into(),
                kind: "analyze".into(),
                messages_used: 200,
                truncated: true,
            },
            VkServerMsg::AnalysisDelta {
                run_id: "run-1".into(),
                dialog_id: "2000000001".into(),
                text: "часть ответа".into(),
            },
            VkServerMsg::AnalysisDone {
                run_id: "run-1".into(),
                dialog_id: "2000000001".into(),
                ok: false,
                reason: Some("cancelled".into()),
                input_tokens: 12_345,
                output_tokens: 678,
            },
            VkServerMsg::AnalysisError {
                run_id: "run-1".into(),
                dialog_id: "2000000001".into(),
                message: "claude run produced no output; stderr: not logged in".into(),
            },
            VkServerMsg::Resync,
            VkServerMsg::Ping,
        ]
    }

    #[test]
    fn server_msgs_round_trip() {
        for msg in all_server_msgs() {
            let json = serde_json::to_string(&msg).unwrap();
            let back: VkServerMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, back, "round-trip сломался на {json}");
        }
    }

    #[test]
    fn client_msgs_round_trip() {
        let msg = VkClientMsg::Pong;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"pong"}"#);
        let back: VkClientMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    /// Точная форма кадра `new_message`: `muted` и `archived` лежат НА УРОВНЕ
    /// КАДРА (рядом с `message`, а не внутри него). Это дословный контракт с
    /// фронтом, поэтому проверка байт-в-байт.
    #[test]
    fn new_message_carries_frame_level_mute_and_archive() {
        let msg = VkServerMsg::NewMessage {
            message: MessageDto {
                id: 7,
                dialog_id: "2000000001".into(),
                ..MessageDto::default()
            },
            muted: true,
            archived: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.starts_with(r#"{"type":"new_message","message":{"#),
            "{json}"
        );
        assert!(
            json.ends_with(r#""muted":true,"archived":false}"#),
            "флаги обязаны лежать в кадре, а не в message: {json}"
        );

        // Оба флага обязательны: кадр без них — это старый сервер, и
        // молчаливая подстановка `false` вернула бы тосты от заглушённых
        // бесед. Урезаем ВАЛИДНЫЙ кадр, чтобы тест не мог «пройти» из-за
        // опечатки в рукописном JSON.
        for drop_field in ["muted", "archived"] {
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            value.as_object_mut().unwrap().remove(drop_field);
            assert!(
                serde_json::from_value::<VkServerMsg>(value).is_err(),
                "кадр без {drop_field} обязан отвергаться"
            );
        }
    }

    /// Индекс вложения в `download_progress` — то самое отличие от Telegram,
    /// без которого галерея из 10 фото рисовала бы весь прогресс в одну
    /// карточку.
    #[test]
    fn download_progress_carries_the_attachment_index() {
        let json = serde_json::to_string(&VkServerMsg::DownloadProgress {
            dialog_id: "1".into(),
            message_id: 2,
            index: 5,
            kind: "thumb".into(),
            received: 1,
            total: Some(2),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"type":"download_progress","dialog_id":"1","message_id":2,"index":5,"kind":"thumb","received":1,"total":2}"#
        );
    }

    /// Три новых кадра VK — их форма фиксируется байтами, потому что фронт
    /// разбирает их по именам полей.
    #[test]
    fn vk_specific_frames_have_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&VkServerMsg::UnreadTotal { count: 12 }).unwrap(),
            r#"{"type":"unread_total","count":12}"#
        );
        assert_eq!(
            serde_json::to_string(&VkServerMsg::HistoryCleared {
                dialog_id: "2000000001".into(),
                up_to_id: 42
            })
            .unwrap(),
            r#"{"type":"history_cleared","dialog_id":"2000000001","up_to_id":42}"#
        );
        assert_eq!(
            serde_json::to_string(&VkServerMsg::DialogChanged {
                dialog_id: "-1234".into()
            })
            .unwrap(),
            r#"{"type":"dialog_changed","dialog_id":"-1234"}"#
        );
    }

    /// Точная форма кадров анализа (АЧ-Ф3).
    ///
    /// `analysis_delta` — самый горячий кадр фичи: он приходит десятками за
    /// один run и разбирается во фронте по именам полей. Проверка байт-в-байт
    /// ловит переименование раньше, чем оно превратится в молча пустой попап.
    #[test]
    fn analysis_frames_have_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&VkServerMsg::AnalysisDelta {
                run_id: "r1".into(),
                dialog_id: "2000000001".into(),
                text: "привет".into(),
            })
            .unwrap(),
            r#"{"type":"analysis_delta","run_id":"r1","dialog_id":"2000000001","text":"привет"}"#
        );
        assert_eq!(
            serde_json::to_string(&VkServerMsg::AnalysisStarted {
                run_id: "r1".into(),
                dialog_id: "-1234".into(),
                kind: "ask".into(),
                messages_used: 50,
                truncated: false,
            })
            .unwrap(),
            r#"{"type":"analysis_started","run_id":"r1","dialog_id":"-1234","kind":"ask","messages_used":50,"truncated":false}"#
        );
        // `reason: None` обязан ехать как `null`, а не исчезать: фронт
        // отличает штатное завершение от отмены именно по этому полю.
        assert_eq!(
            serde_json::to_string(&VkServerMsg::AnalysisDone {
                run_id: "r1".into(),
                dialog_id: "1".into(),
                ok: true,
                reason: None,
                input_tokens: 10,
                output_tokens: 2,
            })
            .unwrap(),
            r#"{"type":"analysis_done","run_id":"r1","dialog_id":"1","ok":true,"reason":null,"input_tokens":10,"output_tokens":2}"#
        );
        assert_eq!(
            serde_json::to_string(&VkServerMsg::AnalysisError {
                run_id: "r1".into(),
                dialog_id: "1".into(),
                message: "spawn failed".into(),
            })
            .unwrap(),
            r#"{"type":"analysis_error","run_id":"r1","dialog_id":"1","message":"spawn failed"}"#
        );
    }

    /// РАЗВЯЗКА С ЯДРОМ: четыре метода трейта дают ровно четыре наших кадра, с
    /// теми же полями и без потерь по дороге. Ядро (`forge-chat-analysis`) про
    /// `VkServerMsg` не знает, поэтому расхождение здесь молча выключило бы
    /// весь стриминг анализа во вкладке, не сломав ни одного теста ядра.
    #[tokio::test]
    async fn the_sink_maps_core_calls_onto_our_four_frames() {
        use forge_chat_analysis::AnalysisSink as _;

        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let sink = VkAnalysisSink::new(tx);

        sink.started("r1", "2000000001", "analyze", 200, true);
        sink.delta("r1", "2000000001", "кусок");
        sink.error("r1", "2000000001", "stderr: not logged in");
        sink.done("r1", "2000000001", false, Some("error"), 11, 3);

        assert_eq!(
            rx.recv().await.unwrap(),
            VkServerMsg::AnalysisStarted {
                run_id: "r1".into(),
                dialog_id: "2000000001".into(),
                kind: "analyze".into(),
                messages_used: 200,
                truncated: true,
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            VkServerMsg::AnalysisDelta {
                run_id: "r1".into(),
                dialog_id: "2000000001".into(),
                text: "кусок".into(),
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            VkServerMsg::AnalysisError {
                run_id: "r1".into(),
                dialog_id: "2000000001".into(),
                message: "stderr: not logged in".into(),
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            VkServerMsg::AnalysisDone {
                run_id: "r1".into(),
                dialog_id: "2000000001".into(),
                ok: false,
                reason: Some("error".into()),
                input_tokens: 11,
                output_tokens: 3,
            }
        );
    }

    /// Отсутствие подписчиков — норма (никто не открыл вкладку) и не повод
    /// ронять идущий run: `SendError` глотается.
    #[test]
    fn the_sink_survives_having_no_subscribers() {
        use forge_chat_analysis::AnalysisSink as _;

        let (tx, rx) = tokio::sync::broadcast::channel(4);
        drop(rx);
        let sink = VkAnalysisSink::new(tx);
        sink.started("r", "1", "analyze", 1, false);
        sink.delta("r", "1", "текст");
        sink.error("r", "1", "ошибка");
        sink.done("r", "1", true, None, 0, 0);
    }

    /// ПОРЯДОК ВАРИАНТОВ ЗАМОРОЖЕН: кадры анализа стоят между `dialog_changed`
    /// и `resync`, потому что `js/vk/ws.js` пишется таблицей по этому enum'у, и
    /// перестановка превращает осмысленный diff двух панелей в шум.
    #[test]
    fn analysis_frames_sit_between_dialog_changed_and_resync() {
        let tags: Vec<String> = all_server_msgs()
            .iter()
            .map(|m| serde_json::to_value(m).unwrap()["type"].as_str().unwrap().to_string())
            .collect();
        let at = |t: &str| tags.iter().position(|x| x == t).unwrap_or_else(|| panic!("нет кадра {t}"));
        assert_eq!(
            &tags[at("dialog_changed") + 1..at("resync")],
            [
                "analysis_started",
                "analysis_delta",
                "analysis_done",
                "analysis_error"
            ]
        );
    }

    /// Удалённые кадры Telegram не должны воскреснуть по недосмотру: у VK
    /// нет ни глобальных категорий mute, ни папок, и кадр про них означал бы
    /// обработчик во фронте, который никогда не сработает.
    #[test]
    fn telegram_only_frames_are_not_accepted() {
        for raw in [
            r#"{"type":"notify_defaults_changed","users":true,"chats":false,"broadcasts":true}"#,
            r#"{"type":"folders_changed"}"#,
        ] {
            assert!(
                serde_json::from_str::<VkServerMsg>(raw).is_err(),
                "кадр {raw} не должен разбираться"
            );
        }
    }

    /// Все теги — snake_case и уникальны. Дубль тега компилятор не поймает
    /// (варианты называются по-разному), а фронт получил бы кадр, который
    /// разбирается не в тот обработчик.
    #[test]
    fn frame_tags_are_unique_snake_case() {
        let mut tags: Vec<String> = all_server_msgs()
            .iter()
            .map(|m| {
                serde_json::to_value(m).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        for t in &tags {
            assert!(
                t.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "тег {t} не snake_case"
            );
        }
        let total = tags.len();
        tags.sort();
        tags.dedup();
        assert_eq!(tags.len(), total, "дублирующиеся теги кадров: {tags:?}");
    }

    // -----------------------------------------------------------------------
    // Приёмник кадров расшифровки (ГС-Ф3).
    // -----------------------------------------------------------------------

    mod voice {
        use super::*;
        use forge_voice::{Engine, Transcript, VoiceKey, VoiceSink};

        fn key() -> VoiceKey {
            VoiceKey::new("2000000042", "7001", 1)
        }

        /// Все кадры пришедшего прогона, по порядку.
        fn drain(rx: &mut tokio::sync::broadcast::Receiver<VkServerEvent>) -> Vec<VkServerMsg> {
            let mut out = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                out.push(msg);
            }
            out
        }

        /// **ПОЛНЫЙ ЦИКЛ: started → delta* → done, и каждый кадр несёт КЛЮЧ
        /// ВЛОЖЕНИЯ.**
        ///
        /// Без тройки (dialog_id, message_id, index) фронт не знает, к какой
        /// карточке относится кадр: сервер вещает их всем подписчикам без
        /// фильтрации.
        #[test]
        fn a_full_cycle_reaches_the_socket_addressed_by_the_attachment() {
            let (tx, mut rx) = tokio::sync::broadcast::channel(64);
            let sink = VkVoiceSink::new(tx);

            sink.started("voice-1", &key(), "cloud");
            sink.progress("voice-1", &key(), 50);
            sink.done(
                "voice-1",
                &key(),
                &Transcript::new("расшифровано", Engine::Cloud)
                    .with_language("ru")
                    .with_duration_ms(4200),
            );

            let frames = drain(&mut rx);
            assert_eq!(frames.len(), 3, "{frames:?}");
            for f in &frames {
                let json = serde_json::to_value(f).unwrap();
                assert_eq!(json["dialog_id"], "2000000042", "{json}");
                assert_eq!(json["message_id"], 7001, "{json}");
                assert_eq!(json["index"], 1, "{json}");
                assert_eq!(json["run_id"], "voice-1", "{json}");
            }
            let json = serde_json::to_value(&frames[0]).unwrap();
            assert_eq!(json["type"], "voice_transcribe_started");
            assert_eq!(json["engine"], "cloud", "движок обязан быть назван ДО отправки");
            assert_eq!(
                serde_json::to_value(&frames[1]).unwrap()["type"],
                "voice_transcribe_delta"
            );
            let done = serde_json::to_value(&frames[2]).unwrap();
            assert_eq!(done["type"], "voice_transcribe_done");
            assert_eq!(done["text"], "расшифровано");
            assert_eq!(done["language"], "ru");
            assert_eq!(done["duration_ms"], 4200);
        }

        /// **ОШИБКА — РОВНО ОДИН КАДР, И КОД В НЁМ ОТДЕЛЬНЫМ ПОЛЕМ.**
        #[test]
        fn a_failure_produces_exactly_one_frame_with_its_code() {
            let (tx, mut rx) = tokio::sync::broadcast::channel(64);
            let sink = VkVoiceSink::new(tx);
            sink.started("voice-2", &key(), "local");
            sink.error("voice-2", &key(), "metal_unavailable", "GPU не подтверждён");

            let frames = drain(&mut rx);
            assert_eq!(frames.len(), 2, "{frames:?}");
            let json = serde_json::to_value(&frames[1]).unwrap();
            assert_eq!(json["type"], "voice_transcribe_error");
            assert_eq!(json["code"], "metal_unavailable");
            assert!(json["message"].as_str().unwrap().contains("GPU"));
        }

        /// **ПРОГРЕСС КОАЛЕСЦИРОВАН: СОТНЯ КАДРОВ ДВИЖКА — ЭТО ЕДИНИЦЫ КАДРОВ
        /// В СОКЕТЕ.**
        ///
        /// И сотня уезжает всегда: на ней фронт гасит полосу.
        #[test]
        fn the_progress_is_coalesced_but_never_loses_the_hundred() {
            let (tx, mut rx) = tokio::sync::broadcast::channel(256);
            let sink = VkVoiceSink::new(tx);
            sink.started("voice-3", &key(), "local");
            for p in 0..=100u8 {
                sink.progress("voice-3", &key(), p);
            }

            let deltas = drain(&mut rx)
                .into_iter()
                .filter(|f| matches!(f, VkServerMsg::VoiceTranscribeDelta { .. }))
                .collect::<Vec<_>>();
            assert!(
                deltas.len() <= 101 / usize::from(VOICE_PROGRESS_STEP) + 2,
                "коалесцинг не сработал: {} кадров",
                deltas.len()
            );
            assert!(deltas.len() > 5, "полоса не должна стоять: {}", deltas.len());
            let last = serde_json::to_value(deltas.last().unwrap()).unwrap();
            assert_eq!(last["percent"], 100);
        }

        /// **ДВА ПРОГОНА НЕ ПУТАЮТ СВОИ ПРОЦЕНТЫ.**
        ///
        /// Счётчик ведётся по `run_id`: один общий на приёмник заставлял бы
        /// полосу второго прогона дёргаться назад.
        #[test]
        fn two_runs_keep_separate_progress() {
            let (tx, mut rx) = tokio::sync::broadcast::channel(256);
            let sink = VkVoiceSink::new(tx);
            sink.started("a", &key(), "cloud");
            sink.started("b", &key(), "cloud");
            sink.progress("a", &key(), 90);
            sink.progress("b", &key(), 10);

            let percents: Vec<(String, u64)> = drain(&mut rx)
                .iter()
                .filter_map(|f| match serde_json::to_value(f).unwrap() {
                    v if v["type"] == "voice_transcribe_delta" => Some((
                        v["run_id"].as_str().unwrap().to_string(),
                        v["percent"].as_u64().unwrap(),
                    )),
                    _ => None,
                })
                .collect();
            assert_eq!(percents, vec![("a".into(), 90), ("b".into(), 10)]);
        }

        /// **ОТСУТСТВИЕ ПОДПИСЧИКОВ НЕ РОНЯЕТ ПРОГОН.**
        ///
        /// Никто не открыл вкладку — это норма, а не повод бросить расшифровку,
        /// которая уже идёт (и, возможно, уже оплачена).
        #[test]
        fn a_run_without_subscribers_survives() {
            let (tx, rx) = tokio::sync::broadcast::channel(4);
            drop(rx);
            let sink = VkVoiceSink::new(tx);
            sink.started("voice-4", &key(), "cloud");
            for p in (0..=100).step_by(5) {
                sink.progress("voice-4", &key(), p as u8);
            }
            sink.done("voice-4", &key(), &Transcript::new("текст", Engine::Cloud));
            // Память тоже не копится: запись прогона снята финальным кадром.
            assert!(sink.sent.lock().unwrap().is_empty());
        }

        /// Неразбираемый идентификатор сообщения даёт `0`, а не панику: кадр —
        /// не то место, где стоит падать.
        #[test]
        fn a_broken_message_id_never_panics() {
            let (tx, mut rx) = tokio::sync::broadcast::channel(8);
            let sink = VkVoiceSink::new(tx);
            sink.started("voice-5", &VoiceKey::new("peer", "не-число", 0), "cloud");
            let json = serde_json::to_value(&drain(&mut rx)[0]).unwrap();
            assert_eq!(json["message_id"], 0);
        }
    }
}
