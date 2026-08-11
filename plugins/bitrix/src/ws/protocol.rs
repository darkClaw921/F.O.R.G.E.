//! Типизированный WS-протокол плагина Bitrix24 (`/ws/bitrix`).
//!
//! Формат кадра — JSON с внешним тегом `type` в snake_case:
//!
//! ```json
//! {"type":"login_state","state":"not_configured","error":null,"user":null,"portal":null}
//! {"type":"counters","total":3,"chat":2,"dialog":1,"tasks":0,"notify":0}
//! {"type":"upload_progress","upload_id":"…","phase":"client","sent":1024,"total":4096,"indeterminate":false}
//! {"type":"ping"}
//! ```
//!
//! # Весь набор кадров объявлен СРАЗУ
//!
//! Включая те, что начнут отправляться только в Фазах 4–10, и те, что
//! сервер не отправит НИКОГДА (см. таблицу ниже). Причина одна: фронтовый
//! `buildHandlers` пишется один раз по этой таблице и не переписывается на
//! каждой фазе, а незнакомый кадр в старом фронте из кэша PWA не должен
//! выглядеть ошибкой.
//!
//! # Кто и когда шлёт каждый кадр (итог Фазы 7)
//!
//! | кадр | источник | задержка |
//! |---|---|---|
//! | `login_state` | автомат подключения | мгновенно |
//! | `message_new` | роут отправки (своё) + поток событий (чужое) | мгновенно / до цикла опроса |
//! | `message_update` | роут правки (своё) + **фолбэк-синхронизация** (чужое) | мгновенно / **до минуты** |
//! | `message_delete` | поток событий | до цикла опроса |
//! | `reaction` | поток событий | до цикла опроса |
//! | `dialogs_patch`, `counters`, `presence` | фолбэк-синхронизация | до `recent_sync_secs` |
//! | `resync` | `broadcast::Lagged` (Р19) и протухший курсор (Р18) | мгновенно |
//! | `error` | воркеры (только троттлинг портала, Р16) | мгновенно |
//! | `ping` | heartbeat WS | 15 с |
//!
//! **Чужая правка приезжает до минуты, и это не баг**: события
//! `ONIMV2MESSAGEUPDATE` портал документирует, но живьём не присылает ни разу
//! (проверено). Единственный источник — медленный воркер, сравнивающий строку
//! списка с прошлым снимком. Фронт при этом разницы не видит: кадр один и тот
//! же, кто бы его ни отправил.
//!
//! # Кадры, которых сервер не пришлёт НИКОГДА (§6 плана)
//!
//! | кадр | почему объявлен, но молчит |
//! |---|---|
//! | `typing` | отправка «печатает» работает, ВХОДЯЩИХ событий портал не даёт |
//! | `read` | «прочитано собеседником» realtime'ом не приходит, только `readed_list` по запросу |
//! | `task_update` | Ф10; сейчас карточка перезапрашивается после действия |
//! | `upload_progress`, `download_progress` | Ф8/Ф9 |
//!
//! Их обработчики во фронте всё равно объявлены: незнакомый кадр в старом
//! фронте из кэша PWA не должен выглядеть ошибкой, а появление такого кадра в
//! будущем не должно требовать правки фронта.
//!
//! # Инвариант id-полей
//!
//! `dialog_id` — строка канонического вида Bitrix (`"chat7"`, `"sg12"`,
//! `"19"`), `user_id` — строка, `message_id` — `i64`. Строки взяты не для
//! красоты: `dialog_id` полиморфен у самого портала (число для личных
//! диалогов, строка для групповых), и единая строковая форма — единственный
//! способ не разветвлять фронт.
//!
//! # Типы полезной нагрузки
//!
//! Объявлялись `serde_json::Value` и заменяются проводными DTO по мере их
//! появления: [`DialogPayload`] стал `crate::dto::DialogDto` в Фазе 4,
//! [`MessagePayload`] — `crate::dto::MessageDto` в Фазе 5,
//! [`TaskPayload`] — `crate::tasks::TaskDto` в Фазе 10. ФОРМА кадра (набор
//! вариантов и имена полей) зафиксирована с Фазы 1, поэтому каждая такая
//! замена — правка одной строки, а не переписывание протокола.

use serde::{Deserialize, Serialize};

use crate::state::LoginStateDto;

/// Строка списка диалогов — то же DTO, что отдаёт `GET /api/bitrix/dialogs`.
///
/// Алиас, а не отдельный тип: кадр `dialogs_patch` и страница списка обязаны
/// нести ОДНУ форму, иначе фронт применял бы их двумя разными функциями и они
/// разъехались бы при первой же правке DTO.
pub type DialogPayload = crate::dto::DialogDto;

/// Сообщение ленты — то же DTO, что отдаёт
/// `GET /api/bitrix/dialogs/:id/messages`.
///
/// Алиас, а не отдельный тип, по той же причине, что и [`DialogPayload`]: кадр
/// `message_new` и страница истории обязаны нести ОДНУ форму, иначе строка,
/// приехавшая realtime'ом, отличалась бы от той же строки после перезагрузки
/// вкладки.
pub type MessagePayload = crate::dto::MessageDto;

/// Карточка задачи — то же DTO, что отдаёт `GET /api/bitrix/tasks/:id`.
///
/// Алиас, а не отдельный тип, по той же причине, что и [`DialogPayload`]:
/// шапка над лентой перерисовывается и по ответу роута, и по кадру
/// `task_update`, и расхождение форм означало бы, что после смены статуса
/// карточка выглядит иначе, чем после перезагрузки вкладки (Ф10).
pub type TaskPayload = crate::tasks::TaskDto;

/// То, что летит в broadcast-канал [`crate::state::B24State`] и дальше —
/// каждому WS-подписчику.
///
/// Обёртки над кадром НЕТ намеренно: broadcast идёт без фильтрации, потому
/// что бейдж непрочитанного и тосты обязаны работать с ЛЮБОЙ вкладки.
pub type B24ServerEvent = B24ServerMsg;

/// Сообщения сервер → клиент.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum B24ServerMsg {
    /// Текущее состояние подключения. Отправляется ПЕРВЫМ кадром после
    /// апгрейда и при каждом переходе [`crate::state::LoginFlow`].
    LoginState(LoginStateDto),

    /// Изменились строки списка диалогов.
    ///
    /// Патч, а не полный список: у портала бывают тысячи чатов, и слать их
    /// целиком на каждое сообщение означало бы мегабайты по сокету.
    DialogsPatch {
        /// Изменившиеся или добавившиеся строки.
        dialogs: Vec<DialogPayload>,
        /// Диалоги, исчезнувшие из списка (скрыты или покинуты).
        #[serde(default)]
        removed: Vec<String>,
    },

    /// Пришло новое сообщение (`ONIMV2MESSAGEADD`).
    MessageNew {
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение целиком.
        message: MessagePayload,
        /// Заглушён ли диалог. Фронт обязан молчать при `true`, не заглядывая
        /// в свой список: он видит только загруженную страницу, а источник
        /// истины по mute — сервер.
        #[serde(default)]
        muted: bool,
    },

    /// Сообщение отредактировано.
    ///
    /// Источник — ФОЛБЭК-синхронизация (раз в 60 с), а не поток событий:
    /// `ONIMV2MESSAGEUPDATE` документирован, но живьём не приходит.
    MessageUpdate {
        /// Диалог (строкой).
        dialog_id: String,
        /// Новая версия сообщения.
        message: MessagePayload,
    },

    /// Сообщения удалены (`ONIMV2MESSAGEDELETE`).
    MessageDelete {
        /// Диалог (строкой).
        dialog_id: String,
        /// Идентификаторы удалённых сообщений.
        message_ids: Vec<i64>,
    },

    /// Изменился набор «лайков» у сообщения (`ONIMV2REACTIONCHANGE`).
    ///
    /// Эмодзи-реакций у портала под вебхуком нет — есть ровно один «лайк»
    /// (`im.message.like`), поэтому кадр несёт список пользователей, а не
    /// карту эмодзи. Поле `action` в событии живьём приходит пустым, поэтому
    /// сервер пересчитывает список из `message.params.LIKE`.
    Reaction {
        /// Диалог (строкой).
        dialog_id: String,
        /// Сообщение.
        message_id: i64,
        /// Кто поставил лайк (id пользователей СТРОКАМИ — портал отдаёт их
        /// строками, и приводить к числам незачем).
        likes: Vec<String>,
    },

    /// Собеседник печатает.
    ///
    /// **Сервером не отправляется никогда** (§6 плана): отправка «печатает»
    /// работает, входящих событий портал не даёт. Кадр объявлен, чтобы фронт
    /// не пришлось переписывать, если платформа их однажды добавит.
    Typing {
        /// Диалог (строкой).
        dialog_id: String,
        /// Кто печатает (строкой).
        user_id: String,
    },

    /// Отметка о прочтении.
    ///
    /// **Сервером не отправляется никогда** (§6): «прочитано собеседником»
    /// realtime'ом не приходит, только `readed_list` по запросу.
    Read {
        /// Диалог (строкой).
        dialog_id: String,
        /// Максимальный прочитанный `message_id`.
        message_id: i64,
        /// `in` — прочитали мы, `out` — прочитали нас.
        direction: String,
    },

    /// Три независимых счётчика портала плюс уведомления.
    ///
    /// Отдельный кадр, а не поле в `dialogs_patch`: бейдж на кнопке вкладки
    /// обязан обновляться и тогда, когда список диалогов не менялся.
    Counters {
        /// Общий счётчик для бейджа вкладки.
        total: i64,
        /// Непрочитанное в групповых чатах (`TYPE.CHAT`).
        #[serde(default)]
        chat: i64,
        /// Непрочитанное в личных диалогах (`TYPE.DIALOG`).
        #[serde(default)]
        dialog: i64,
        /// Непрочитанное в чатах задач (`TYPE.TASKS_TASK`).
        #[serde(default)]
        tasks: i64,
        /// Уведомления портала (`TYPE.NOTIFY`). Отдельной директории под них
        /// в v1 нет (Р13), но в общий счётчик они входят.
        #[serde(default)]
        notify: i64,
        /// Просроченных ЗАДАЧ (`tasks.task.counters.get`, Ф10).
        ///
        /// Ниже — ДРУГОЕ семейство счётчиков, и складывать его с полями выше
        /// нельзя: `tasks` считает непрочитанные СООБЩЕНИЯ чатов задач и
        /// обнуляется чтением ленты, а `expired`/`new_comments`/`mentioned` —
        /// СОСТОЯНИЕ самих задач и обнуляются действием над задачей.
        /// Сложенный бейдж не обнулить ни тем, ни другим.
        #[serde(default)]
        expired: i64,
        /// Задач с новыми комментариями.
        #[serde(default)]
        new_comments: i64,
        /// Упоминаний в задачах.
        #[serde(default)]
        mentioned: i64,
    },

    /// Онлайн-статус пользователя.
    ///
    /// **Realtime'а у платформы нет** (§6): кадр приходит только из
    /// фолбэк-синхронизации, то есть с задержкой до 60 секунд.
    Presence {
        /// Пользователь (строкой).
        user_id: String,
        /// `true` ⇒ сейчас в сети.
        online: bool,
        /// Unix-время последнего появления, если портал его отдал.
        #[serde(default)]
        was_online: Option<i64>,
    },

    /// Карточка задачи изменилась (Ф10): сменился статус, срок, ответственный.
    TaskUpdate {
        /// Идентификатор задачи (строкой).
        task_id: String,
        /// Карточка целиком.
        task: TaskPayload,
    },

    /// Прогресс отправки файла (Р8, двухфазный).
    ///
    /// Фаза `client` — браузер → devforge, проценты точные. Фаза `portal` —
    /// devforge → Bitrix24, проценты неизвестны (`indeterminate: true`):
    /// одношаговая base64-заливка не даёт обратной связи о ходе. ФИНАЛЬНОЕ
    /// событие проходит всегда, иначе прогрессбар навсегда замирал бы на 97 %.
    UploadProgress {
        /// Идентификатор upload-сессии.
        upload_id: String,
        /// `client` | `portal`.
        phase: String,
        /// Отправлено байт (в фазе `portal` — размер файла целиком).
        sent: u64,
        /// Всего байт.
        total: u64,
        /// Прогресс неизвестен — рисовать бегущую полосу, а не проценты.
        #[serde(default)]
        indeterminate: bool,
    },

    /// Прогресс скачивания вложения в файловый кэш (Ф8).
    DownloadProgress {
        /// Ключ кэша `<dialog>/<message>/<index>` — тот же, что во фронтовой
        /// карте карточек: без него прогресс второго файла прилетал бы в
        /// карточку первого.
        key: String,
        /// Получено байт.
        received: u64,
        /// Всего байт, если размер известен заранее.
        #[serde(default)]
        total: Option<u64>,
    },

    /// Фоновая операция не удалась.
    ///
    /// REST-ошибки едут своим конвертом ([`crate::routes::error`]); этот кадр
    /// нужен тому, у чего нет запроса-владельца: воркеру событий и
    /// синхронизации. Форма полей совпадает с REST-конвертом намеренно —
    /// фронт разбирает их одной функцией.
    Error {
        /// Машинный код (та же таблица, что у REST).
        code: String,
        /// Человекочитаемое пояснение.
        message: String,
        /// Сколько ждать до повтора.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_secs: Option<u64>,
    },

    // --- «Анализ чата через Claude» ---------------------------------------
    //
    // ПОРЯДОК этих четырёх вариантов заморожен: `bitrix_contract.test.mjs`
    // сверяет теги с `SERVER_FRAMES` в `js/bitrix/ws.js` по составу И порядку,
    // а `server_frame_tags_are_frozen` ниже — по составу и порядку внутри
    // крейта. Вставлены перед `Resync`/`Ping` — там же, где они стоят у VK и
    // Telegram, чтобы три протокола читались одинаково.
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
    /// то есть [`B24ServerMsg::Resync`] и перечитывание всего состояния.
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
    ///
    /// Отдельный кадр, а не [`B24ServerMsg::Error`]: тот несёт КОД из таблицы
    /// REST и адресован панели целиком, а этот привязан к конкретному run'у и
    /// рисуется внутри попапа.
    AnalysisError {
        /// Идентификатор run'а.
        run_id: String,
        /// Диалог (строкой).
        dialog_id: String,
        /// Текст ошибки как есть.
        message: String,
    },

    /// Клиент отстал (`broadcast::error::RecvError::Lagged`) или курсор
    /// событий признан устаревшим (Р18) — перечитать состояние по REST.
    Resync,

    /// Heartbeat. Клиент обязан ответить [`B24ClientMsg::Pong`].
    Ping,
}

/// Мост между ядром анализа и broadcast-каналом плагина
/// ([`forge_chat_analysis::AnalysisSink`]).
///
/// Вся развязка плагина с крейтом — вот эти четыре метода. Ядро не знает ни
/// про [`B24ServerMsg`], ни про broadcast: оно зовёт `started`/`delta`/`done`/
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
pub struct B24AnalysisSink {
    tx: tokio::sync::broadcast::Sender<B24ServerEvent>,
}

impl B24AnalysisSink {
    /// Оборачивает broadcast-канал состояния.
    #[must_use]
    pub fn new(tx: tokio::sync::broadcast::Sender<B24ServerEvent>) -> Self {
        Self { tx }
    }
}

impl forge_chat_analysis::AnalysisSink for B24AnalysisSink {
    fn started(
        &self,
        run_id: &str,
        dialog_id: &str,
        kind: &str,
        messages_used: u32,
        truncated: bool,
    ) {
        let _ = self.tx.send(B24ServerMsg::AnalysisStarted {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            kind: kind.to_string(),
            messages_used,
            truncated,
        });
    }

    fn delta(&self, run_id: &str, dialog_id: &str, text: &str) {
        let _ = self.tx.send(B24ServerMsg::AnalysisDelta {
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
        let _ = self.tx.send(B24ServerMsg::AnalysisDone {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            ok,
            reason: reason.map(str::to_string),
            input_tokens,
            output_tokens,
        });
    }

    fn error(&self, run_id: &str, dialog_id: &str, message: &str) {
        let _ = self.tx.send(B24ServerMsg::AnalysisError {
            run_id: run_id.to_string(),
            dialog_id: dialog_id.to_string(),
            message: message.to_string(),
        });
    }
}

/// Сообщения клиент → сервер.
///
/// Их намеренно ровно один: вся мутация идёт по REST (её проще авторизовать,
/// логировать и ограничивать), а WS — односторонний поток событий с
/// heartbeat'ом.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum B24ClientMsg {
    /// Ответ на [`B24ServerMsg::Ping`]; сбрасывает idle-таймер соединения.
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(msg: &B24ServerMsg) -> String {
        serde_json::to_value(msg).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// ЗАМОРОЖЕННЫЙ КОНТРАКТ: набор тегов дословно совпадает с константой
    /// `SERVER_FRAMES` во фронтовом `js/bitrix/ws.js`. Переименование обязано
    /// валить тест, а не приводить к молчаливому «no handler».
    #[test]
    fn server_frame_tags_are_frozen() {
        let all = [
            B24ServerMsg::LoginState(LoginStateDto::default()),
            B24ServerMsg::DialogsPatch {
                dialogs: vec![],
                removed: vec![],
            },
            B24ServerMsg::MessageNew {
                dialog_id: "chat7".into(),
                message: crate::dto::MessageDto::default(),
                muted: false,
            },
            B24ServerMsg::MessageUpdate {
                dialog_id: "chat7".into(),
                message: crate::dto::MessageDto::default(),
            },
            B24ServerMsg::MessageDelete {
                dialog_id: "chat7".into(),
                message_ids: vec![1],
            },
            B24ServerMsg::Reaction {
                dialog_id: "chat7".into(),
                message_id: 1,
                likes: vec!["1".into()],
            },
            B24ServerMsg::Typing {
                dialog_id: "chat7".into(),
                user_id: "1".into(),
            },
            B24ServerMsg::Read {
                dialog_id: "chat7".into(),
                message_id: 1,
                direction: "in".into(),
            },
            B24ServerMsg::Counters {
                total: 0,
                chat: 0,
                dialog: 0,
                tasks: 0,
                notify: 0,
                expired: 0,
                new_comments: 0,
                mentioned: 0,
            },
            B24ServerMsg::Presence {
                user_id: "1".into(),
                online: true,
                was_online: None,
            },
            B24ServerMsg::TaskUpdate {
                task_id: "3".into(),
                task: crate::tasks::TaskDto {
                    id: "3".into(),
                    ..crate::tasks::TaskDto::default()
                },
            },
            B24ServerMsg::UploadProgress {
                upload_id: "u".into(),
                phase: "client".into(),
                sent: 0,
                total: 1,
                indeterminate: false,
            },
            B24ServerMsg::DownloadProgress {
                key: "chat7/1/0".into(),
                received: 0,
                total: None,
            },
            B24ServerMsg::Error {
                code: "bitrix_error".into(),
                message: "…".into(),
                retry_after_secs: None,
            },
            B24ServerMsg::AnalysisStarted {
                run_id: "r1".into(),
                dialog_id: "chat7".into(),
                kind: "analyze".into(),
                messages_used: 10,
                truncated: false,
            },
            B24ServerMsg::AnalysisDelta {
                run_id: "r1".into(),
                dialog_id: "chat7".into(),
                text: "…".into(),
            },
            B24ServerMsg::AnalysisDone {
                run_id: "r1".into(),
                dialog_id: "chat7".into(),
                ok: true,
                reason: None,
                input_tokens: 1,
                output_tokens: 2,
            },
            B24ServerMsg::AnalysisError {
                run_id: "r1".into(),
                dialog_id: "chat7".into(),
                message: "…".into(),
            },
            B24ServerMsg::Resync,
            B24ServerMsg::Ping,
        ];
        let tags: Vec<String> = all.iter().map(tag).collect();
        assert_eq!(
            tags,
            vec![
                "login_state",
                "dialogs_patch",
                "message_new",
                "message_update",
                "message_delete",
                "reaction",
                "typing",
                "read",
                "counters",
                "presence",
                "task_update",
                "upload_progress",
                "download_progress",
                "error",
                "analysis_started",
                "analysis_delta",
                "analysis_done",
                "analysis_error",
                "resync",
                "ping",
            ]
        );
    }

    #[test]
    fn login_state_is_flat_inside_the_frame() {
        // Newtype-вариант: поля DTO лежат в кадре плоско, без вложенного
        // объекта. Фронт читает `msg.state`, а не `msg.login.state`.
        let v = serde_json::to_value(B24ServerMsg::LoginState(LoginStateDto {
            state: "authorized".into(),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(v["type"], "login_state");
        assert_eq!(v["state"], "authorized");
    }

    #[test]
    fn client_frame_is_only_pong() {
        assert_eq!(
            serde_json::from_str::<B24ClientMsg>(r#"{"type":"pong"}"#).unwrap(),
            B24ClientMsg::Pong
        );
        assert!(serde_json::from_str::<B24ClientMsg>(r#"{"type":"hello"}"#).is_err());
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let v = serde_json::to_value(B24ServerMsg::Error {
            code: "flood_wait".into(),
            message: "wait".into(),
            retry_after_secs: None,
        })
        .unwrap();
        assert!(
            v.get("retry_after_secs").is_none(),
            "необязательное поле не должно сериализоваться: {v}"
        );
    }

    #[test]
    fn frames_round_trip() {
        let msg = B24ServerMsg::Counters {
            total: 5,
            chat: 2,
            dialog: 3,
            tasks: 0,
            notify: 0,
            expired: 2,
            new_comments: 1,
            mentioned: 0,
        };
        let text = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<B24ServerMsg>(&text).unwrap(), msg);
    }
}
