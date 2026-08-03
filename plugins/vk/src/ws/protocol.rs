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
}
