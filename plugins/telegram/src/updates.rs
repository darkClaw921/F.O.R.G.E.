//! Постоянный приём апдейтов MTProto и их трансляция в WS-события.
//!
//! ```text
//!   spawn(state)
//!      └─▶ supervisor ──┬─ ждёт Authorized (authorized_notify + тик 2с)
//!                       ├─ берёт updates_rx из TgRuntime (одноразово)
//!                       ├─ run_worker: stream_updates ──▶ broadcast
//!                       └─ упал? backoff 1→2→5→10→30→60с + пересборка рантайма
//! ```
//!
//! # Почему supervisor, а не «просто задача»
//!
//! Приёмник апдейтов ([`grammers_client::Client::stream_updates`]) ПОГЛОЩАЕТ
//! канал, привязанный к конкретному `SenderPool`. Значит, восстановиться
//! после обрыва можно только пересборкой всего рантайма — этим и занимается
//! supervisor: он владеет циклом «жди авторизации → работай → переподключись»,
//! а сам воркер остаётся линейным и без ретраев внутри.
//!
//! # Инвариант: одна ошибка апдейта не роняет поток
//!
//! Неинтересный или неожиданный по форме апдейт просто не даёт события
//! ([`map_raw_update`] возвращает `None`) — потерять одно событие лучше, чем
//! перестать получать все. Ошибка ПОТОКА (`InvocationError`) — другое дело:
//! она означает, что соединения больше нет, и обрабатывается supervisor'ом.
//!
//! # Риск №4 плана: сессию отозвали с другого устройства
//!
//! `AUTH_KEY_UNREGISTERED` / `SESSION_REVOKED` / `USER_DEACTIVATED` — НЕ
//! повод для реконнекта: ключ мёртв, и повторные попытки только шлют мусор в
//! Telegram. Воркер переводит логин в `Disconnected{error}` (UI сам покажет
//! wizard), гасит рантайм и ждёт новой авторизации.
//!
//! # sync_update_state при остановке
//!
//! Состояние апдейтов (`pts`/`qts`/`date`/`seq`) сохраняется в сессию и при
//! штатном shutdown, и перед уходом на реконнект. Без этого `catch_up` после
//! рестарта либо терял сообщения, либо выдавал уже показанные — и то и другое
//! пользователь видит как «переписка поехала». Именно ради этого
//! [`crate::state::TelegramState::shutdown_workers`] даёт воркеру
//! [`crate::state::WORKER_STOP_GRACE`] на самостоятельный выход.
//!
//! # Уведомления: сервер решает, «звенит» ли сообщение
//!
//! Перед стартом потока апдейтов воркер один раз забирает глобальные
//! дефолты уведомлений ([`refresh_notify_defaults`]) — три вызова
//! `account.getNotifySettings`. Без них диалог с УНАСЛЕДОВАННЫМИ настройками
//! (а это большинство: Telegram не дублирует «заглушить все чаты» в каждый
//! peer) выглядел бы незаглушённым, и фронт всплывал бы тостом.
//!
//! Дальше кадр `new_message` несёт готовые `muted`/`archived`
//! ([`DialogFlags`]), а `updateNotifySettings` не только рассылается наружу,
//! но и обновляет серверные кэши ([`CacheEffect`]). Порядок именно такой:
//! кэш пишется ДО broadcast, иначе клиент, дёрнувший REST в ответ на кадр,
//! успел бы прочитать устаревшее состояние.

use std::sync::Arc;
use std::time::Duration;

use grammers_client::client::{UpdateStream, UpdatesConfiguration};
use grammers_client::update::Update;
use grammers_client::Client;
use grammers_mtsender::InvocationError;
use grammers_session::types::PeerId;
use grammers_session::updates::UpdatesLike;
use grammers_tl_types as tl;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::config::TelegramConfig;
use crate::dto::{self, NotifyDefaults};
use crate::login;
use crate::media_cache;
use crate::peers;
use crate::state::TelegramState;
use crate::ws::protocol::TgServerMsg;

/// Лестница backoff'а реконнекта, секунды. Последнее значение — потолок.
pub const BACKOFF_SECS: [u64; 6] = [1, 2, 5, 10, 30, 60];

/// Сколько ждать три вызова `account.getNotifySettings` перед стартом
/// потока апдейтов.
///
/// Ограничение обязательно: дефолты грузятся ДО чтения потока (иначе первые
/// сообщения уедут во фронт с неверным `muted`), и зависший RPC иначе
/// задержал бы realtime насовсем. Апдейты в это время не теряются — они
/// копятся в канале рантайма.
pub const NOTIFY_DEFAULTS_TIMEOUT: Duration = Duration::from_secs(10);

/// Как часто перепроверять состояние логина, пока ждём авторизации.
///
/// `Notify` — основной путь пробуждения; тик нужен как страховка от гонки
/// «авторизовались ровно между проверкой и подпиской» и на случай, если
/// состояние поменяли не через `set_login`.
const AUTH_POLL: Duration = Duration::from_secs(2);

/// Задержка `step`-й попытки реконнекта.
pub fn backoff_secs(step: usize) -> u64 {
    BACKOFF_SECS[step.min(BACKOFF_SECS.len() - 1)]
}

/// Конфигурация потока апдейтов из конфига плагина.
pub fn updates_config(cfg: &TelegramConfig) -> UpdatesConfiguration {
    UpdatesConfiguration {
        catch_up: cfg.catch_up,
        update_queue_limit: Some(cfg.update_queue_limit),
    }
}

/// Означает ли ошибка, что авторизация мертва (риск №4 плана).
///
/// Проверяется И код `401`, И имена: Telegram присылает
/// `SESSION_REVOKED`/`USER_DEACTIVATED` и с другими кодами, а цена ошибки
/// асимметрична — принять «мёртвый ключ» за сетевой сбой значит крутить
/// реконнект в пустоту.
pub fn is_auth_error(err: &InvocationError) -> bool {
    const NAMES: [&str; 5] = [
        "AUTH_KEY_UNREGISTERED",
        "AUTH_KEY_DUPLICATED",
        "SESSION_REVOKED",
        "SESSION_EXPIRED",
        "USER_DEACTIVATED",
    ];
    match err {
        InvocationError::Rpc(rpc) => rpc.code == 401 || NAMES.iter().any(|n| rpc.name.contains(n)),
        _ => false,
    }
}

/// Имя активности для WS-события `typing`.
///
/// Строки — контракт с фронтом (он рисует по ним «печатает…», «пишет
/// голосовое…»), поэтому snake_case и без вариантов «как получилось».
pub fn action_name(action: &tl::enums::SendMessageAction) -> &'static str {
    use tl::enums::SendMessageAction as A;
    match action {
        A::SendMessageTypingAction => "typing",
        A::SendMessageCancelAction => "cancel",
        A::SendMessageRecordVideoAction => "record_video",
        A::SendMessageUploadVideoAction(_) => "upload_video",
        A::SendMessageRecordAudioAction => "record_voice",
        A::SendMessageUploadAudioAction(_) => "upload_audio",
        A::SendMessageUploadPhotoAction(_) => "upload_photo",
        A::SendMessageUploadDocumentAction(_) => "upload_document",
        A::SendMessageGeoLocationAction => "geo",
        A::SendMessageChooseContactAction => "choose_contact",
        A::SendMessageGamePlayAction => "game",
        A::SendMessageRecordRoundAction => "record_round",
        A::SendMessageUploadRoundAction(_) => "upload_round",
        A::SpeakingInGroupCallAction => "speaking",
        A::SendMessageHistoryImportAction(_) => "history_import",
        A::SendMessageChooseStickerAction => "choose_sticker",
        A::SendMessageEmojiInteraction(_) => "emoji",
        A::SendMessageEmojiInteractionSeen(_) => "emoji_seen",
        A::SendMessageTextDraftAction(_)
        | A::InputSendMessageRichMessageDraftAction(_)
        | A::SendMessageRichMessageDraftAction(_) => "draft",
    }
}

/// Событие удаления сообщений.
///
/// `channel_id = None` — удаление в ЛС или малой группе: Telegram НЕ
/// сообщает, из какого диалога удалено. Тогда `dialog_id` уходит пустой
/// строкой, и фронт ищет сообщения по id среди загруженных диалогов. Врать
/// конкретным диалогом здесь нельзя — удалили бы строку не в том чате.
pub fn deleted_event(channel_id: Option<i64>, ids: &[i32]) -> TgServerMsg {
    let dialog_id = channel_id
        .and_then(PeerId::channel)
        .map(dto::dialog_id_string)
        .unwrap_or_default();
    TgServerMsg::MessageDeleted {
        dialog_id,
        message_ids: ids.to_vec(),
    }
}

/// Что означает пришедший `updateNotifySettings`.
///
/// Telegram шлёт ОДИН апдейт и для «заглушил конкретный чат», и для
/// «заглушил все каналы»; отличаются они ровно вариантом `NotifyPeer`
/// (unit-варианты — глобальные, `Peer(..)`/`NotifyForumTopic(..)` —
/// адресные). Раньше глобальные молча выбрасывались — из-за этого mute
/// «всех личных чатов» с телефона был для нас невидим.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyOutcome {
    /// Изменился глобальный дефолт категории; внутри — ВСЕ три категории
    /// после применения апдейта (то есть готовое новое состояние кэша).
    Defaults(NotifyDefaults),
    /// Изменились настройки конкретного диалога; `muted` уже разрешён с
    /// учётом наследования.
    Dialog {
        /// Диалог, к которому относятся настройки.
        peer: PeerId,
        /// Итоговое «заглушён».
        muted: bool,
    },
    /// Апдейт не про диалог и не про категорию — форумный топик.
    ///
    /// Отдельный вариант, а не «просто None»: топики форума наш контракт
    /// пока не адресует, и это осознанное «игнорируем», а не дыра.
    Ignored,
}

/// Разбор `updateNotifySettings` — чистая функция.
///
/// `now` и текущие `defaults` передаются параметрами, чтобы расчёт был
/// детерминированным и проверялся без сети.
pub fn notify_outcome(
    u: &tl::types::UpdateNotifySettings,
    now: i64,
    defaults: NotifyDefaults,
) -> NotifyOutcome {
    let tl::enums::PeerNotifySettings::Settings(settings) = &u.notify_settings;
    match &u.peer {
        tl::enums::NotifyPeer::Peer(p) => {
            let peer = PeerId::from(&p.peer);
            NotifyOutcome::Dialog {
                peer,
                // Пустые настройки peer'а означают «как в категории» — без
                // этой подстановки заглушённый глобально чат вернулся бы
                // во фронт как звенящий.
                //
                // Оговорка: категория здесь берётся по тегу `PeerId`, а он
                // не отличает мегагруппу от broadcast-канала. Промах
                // возможен только у диалога с полностью унаследованными
                // настройками и разными дефолтами `chats`/`broadcasts`;
                // ближайший листинг диалогов перезапишет запись точным
                // значением (там kind известен из индекса peer'ов).
                muted: dto::notify_muted_with_default(
                    settings,
                    now,
                    defaults.for_peer_id(peer),
                ),
            }
        }
        tl::enums::NotifyPeer::NotifyUsers => NotifyOutcome::Defaults(NotifyDefaults {
            users: dto::notify_muted(settings, now),
            ..defaults
        }),
        tl::enums::NotifyPeer::NotifyChats => NotifyOutcome::Defaults(NotifyDefaults {
            chats: dto::notify_muted(settings, now),
            ..defaults
        }),
        tl::enums::NotifyPeer::NotifyBroadcasts => NotifyOutcome::Defaults(NotifyDefaults {
            broadcasts: dto::notify_muted(settings, now),
            ..defaults
        }),
        // Форумные топики: у них свой подскоуп настроек, диалогом нашего
        // контракта они не являются. Молча пропускаем — падать нельзя.
        tl::enums::NotifyPeer::NotifyForumTopic(_) => NotifyOutcome::Ignored,
    }
}

/// WS-событие для `updateNotifySettings` (без побочных эффектов).
pub fn notify_settings_event(
    u: &tl::types::UpdateNotifySettings,
    now: i64,
    defaults: NotifyDefaults,
) -> Option<TgServerMsg> {
    match notify_outcome(u, now, defaults) {
        NotifyOutcome::Defaults(d) => Some(TgServerMsg::NotifyDefaultsChanged {
            users: d.users,
            chats: d.chats,
            broadcasts: d.broadcasts,
        }),
        NotifyOutcome::Dialog { peer, muted } => Some(TgServerMsg::NotifySettings {
            dialog_id: dto::dialog_id_string(peer),
            muted,
        }),
        NotifyOutcome::Ignored => None,
    }
}

/// Флаги ДИАЛОГА, которые уходят в кадр `new_message`.
///
/// Живут отдельно от [`crate::dto::MessageDto`], потому что относятся к
/// чату, а не к сообщению (см. шапку [`crate::ws::protocol`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DialogFlags {
    /// Заглушён ли диалог (с учётом наследования от категории).
    pub muted: bool,
    /// Лежит ли диалог в архиве.
    pub archived: bool,
}

/// Разрешает флаги диалога из данных кэша — чистая часть [`dialog_flags`].
///
/// Ключевое правило: `cached_muted == None` — это «не знаем», и тогда
/// берётся дефолт КАТЕГОРИИ, а не `false`. Слепой `false` на промахе кэша
/// — ровно тот дефект, из-за которого тосты приходили от заглушённых
/// чатов, не попавших в загруженную страницу списка.
pub fn resolve_dialog_flags(
    cached_muted: Option<bool>,
    archived: bool,
    defaults: NotifyDefaults,
    kind: &str,
) -> DialogFlags {
    DialogFlags {
        muted: cached_muted.unwrap_or_else(|| defaults.for_kind(kind)),
        archived,
    }
}

/// Проекция сырого апдейта в WS-событие.
///
/// `None` ⇒ апдейт нас не касается (их десятки: звонки, реакции, темы).
/// Сознательно чистая функция без обращений к состоянию и без текущего
/// времени — вся матрица вариантов тестируется без сети.
///
/// `updateNotifySettings` здесь НЕ обрабатывается намеренно: ему нужны и
/// текущие глобальные дефолты, и запись в серверный кэш, поэтому он
/// разбирается раньше — в [`map_update`] через [`notify_outcome`].
pub fn map_raw_update(update: &tl::enums::Update) -> Option<TgServerMsg> {
    use tl::enums::Update as U;
    let msg = match update {
        U::UserTyping(u) => {
            let who = dto::dialog_id_string(PeerId::user(u.user_id)?);
            TgServerMsg::Typing {
                // В личной переписке диалог и есть собеседник.
                dialog_id: who.clone(),
                user_id: who,
                action: action_name(&u.action).into(),
            }
        }
        U::ChatUserTyping(u) => TgServerMsg::Typing {
            dialog_id: dto::dialog_id_string(PeerId::chat(u.chat_id)?),
            user_id: dto::dialog_id_string(PeerId::from(u.from_id.clone())),
            action: action_name(&u.action).into(),
        },
        U::ChannelUserTyping(u) => TgServerMsg::Typing {
            dialog_id: dto::dialog_id_string(PeerId::channel(u.channel_id)?),
            user_id: dto::dialog_id_string(PeerId::from(u.from_id.clone())),
            action: action_name(&u.action).into(),
        },
        U::UserStatus(u) => {
            let (online, was_online) = match &u.status {
                tl::enums::UserStatus::Online(_) => (true, None),
                tl::enums::UserStatus::Offline(o) => (false, Some(i64::from(o.was_online))),
                // Recently / LastWeek / LastMonth / Empty — точного времени
                // Telegram не даёт (настройки приватности собеседника).
                _ => (false, None),
            };
            TgServerMsg::Online {
                user_id: dto::dialog_id_string(PeerId::user(u.user_id)?),
                online,
                was_online,
            }
        }
        // «Мы прочитали» — в том числе с другого устройства: бейдж обязан
        // погаснуть и здесь.
        U::ReadHistoryInbox(u) => TgServerMsg::Read {
            dialog_id: dto::dialog_id_string(PeerId::from(u.peer.clone())),
            max_id: u.max_id,
            direction: "in".into(),
        },
        // «Нас прочитали» — галочки на исходящих.
        U::ReadHistoryOutbox(u) => TgServerMsg::Read {
            dialog_id: dto::dialog_id_string(PeerId::from(u.peer.clone())),
            max_id: u.max_id,
            direction: "out".into(),
        },
        // Те же два события для каналов/супергрупп приходят отдельными
        // типами — без них бейдж канала не гас бы при чтении с телефона.
        U::ReadChannelInbox(u) => TgServerMsg::Read {
            dialog_id: dto::dialog_id_string(PeerId::channel(u.channel_id)?),
            max_id: u.max_id,
            direction: "in".into(),
        },
        U::ReadChannelOutbox(u) => TgServerMsg::Read {
            dialog_id: dto::dialog_id_string(PeerId::channel(u.channel_id)?),
            max_id: u.max_id,
            direction: "out".into(),
        },
        // Любое изменение папок (создали/удалили/переименовали/переставили)
        // сводится к одному событию: фронт перечитывает `GET /folders`.
        U::DialogFilter(_) | U::DialogFilterOrder(_) | U::DialogFilters => {
            TgServerMsg::FoldersChanged
        }
        _ => return None,
    };
    Some(msg)
}

/// Изменение серверных кэшей, которое требует апдейт.
///
/// Выделено в данные (а не в прямые вызовы состояния из маппера) ровно
/// затем, чтобы разбор апдейта остался чистой функцией: «что запомнить»
/// проверяется в тестах без состояния, а применяет эффекты
/// [`apply_effects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheEffect {
    /// Новые глобальные дефолты уведомлений.
    Defaults(NotifyDefaults),
    /// Разрешённый `muted` конкретного диалога (ключ — Bot API `i64`).
    DialogMuted {
        /// Диалог.
        dialog_id: i64,
        /// Итоговое «заглушён».
        muted: bool,
    },
}

/// Что делать с апдейтом: события в broadcast, unread и правки кэшей.
#[derive(Debug, Default, PartialEq)]
struct Mapped {
    /// События для WS-подписчиков.
    events: Vec<TgServerMsg>,
    /// Диалог, у которого выросло число непрочитанных.
    unread_for: Option<String>,
    /// Что запомнить в состоянии до рассылки событий.
    effects: Vec<CacheEffect>,
}

/// Контекст обработки одного апдейта: всё, что маппер знать обязан, но
/// добыть сам не может (состояние и часы).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct UpdateContext {
    /// Глобальные дефолты уведомлений на момент обработки.
    defaults: NotifyDefaults,
    /// Флаги диалога нового сообщения (для остальных апдейтов не нужны).
    flags: DialogFlags,
    /// Текущее время — расчёт `muted` обязан быть детерминированным.
    now: i64,
}

/// [`Mapped`] для `updateNotifySettings`.
fn notify_mapped(u: &tl::types::UpdateNotifySettings, ctx: UpdateContext) -> Mapped {
    match notify_outcome(u, ctx.now, ctx.defaults) {
        NotifyOutcome::Defaults(d) => Mapped {
            events: vec![TgServerMsg::NotifyDefaultsChanged {
                users: d.users,
                chats: d.chats,
                broadcasts: d.broadcasts,
            }],
            unread_for: None,
            effects: vec![CacheEffect::Defaults(d)],
        },
        NotifyOutcome::Dialog { peer, muted } => Mapped {
            events: vec![TgServerMsg::NotifySettings {
                dialog_id: dto::dialog_id_string(peer),
                muted,
            }],
            unread_for: None,
            // Диалог без Bot API-идентификатора (битый peer) в кэш не
            // кладём: ключ, который невозможно сопоставить с сообщением,
            // хуже отсутствия записи.
            effects: peer
                .bot_api_dialog_id()
                .map(|dialog_id| CacheEffect::DialogMuted { dialog_id, muted })
                .into_iter()
                .collect(),
        },
        NotifyOutcome::Ignored => Mapped::default(),
    }
}

/// Проекция апдейта grammers в события. Сети и состояния не касается —
/// всё нужное уже разрешено в [`UpdateContext`].
///
/// # Почему цитаты не добиваются здесь
///
/// `MessageDto.reply_to` у сообщения из потока заполняется только тем, что
/// прислал сам Telegram: `message_id` цитируемого сообщения (плюс автор и
/// медиа, если оригинал в чужом чате — их Telegram кладёт прямо в header).
/// Автора и текст оригинала из НАШЕГО диалога пришлось бы тянуть отдельным
/// `messages.getMessages`, и делать это здесь нельзя:
///
/// - маппер синхронный и стоит в hot-path'е воркера — один общий поток
///   апдейтов на весь аккаунт. Сетевой вызов (даже под таймаутом) задержал
///   бы ВСЕ остальные события: соседние сообщения, typing, прочтения;
/// - при пачке ответов (в чате идёт активная переписка) это ещё и по RPC
///   на сообщение — прямой путь к `FLOOD_WAIT` на ровном месте;
/// - в самом частом случае платить нечего: оригинал уже загружен в ленте у
///   фронта, и превью он строит локально.
///
/// Когда оригинала в ленте нет, фронт дотягивает его одним запросом
/// `GET /dialogs/:id/messages/:mid` — по клику пользователя, а не веерно на
/// каждое входящее. Отсюда и требование контракта: `reply_to` из потока
/// имеет заполненный `message_id` и `unavailable: false` — «превью пока
/// нет», а не «оригинал недоступен».
fn map_update(update: &Update, ctx: UpdateContext) -> Mapped {
    match update {
        Update::NewMessage(msg) => {
            let dto = dto::from_grammers_message(msg);
            // «Активность» диалога — понятие фронта: сервер не знает, какая
            // вкладка что открыла, поэтому считает непрочитанным любое
            // входящее. Обнуляет счётчик `POST /read`, который фронт зовёт
            // при открытии диалога.
            let unread_for = (!dto.out).then(|| dto.dialog_id.clone());
            Mapped {
                events: vec![TgServerMsg::NewMessage {
                    message: dto,
                    muted: ctx.flags.muted,
                    archived: ctx.flags.archived,
                }],
                unread_for,
                effects: Vec::new(),
            }
        }
        Update::MessageEdited(msg) => Mapped {
            events: vec![TgServerMsg::MessageEdited {
                message: dto::from_grammers_message(msg),
            }],
            ..Mapped::default()
        },
        Update::MessageDeleted(del) => Mapped {
            events: vec![deleted_event(del.channel_id(), del.messages())],
            ..Mapped::default()
        },
        // Mute/unmute — и диалога, и целой категории (в т.ч. сделанный с
        // телефона): разбирается до общего маппера, потому что требует
        // дефолтов и записи в кэш.
        Update::Raw(raw) => match &raw.raw {
            tl::enums::Update::NotifySettings(u) => notify_mapped(u, ctx),
            other => Mapped {
                events: map_raw_update(other).into_iter().collect(),
                ..Mapped::default()
            },
        },
        // CallbackQuery / InlineQuery / InlineSend — только для ботов.
        _ => Mapped::default(),
    }
}

/// Флаги диалога по его ключу и категории.
///
/// `dialog_id == None` ⇒ Bot API-идентификатор для peer'а не выводится
/// (битый апдейт): кэш спросить нечем, остаётся дефолт категории.
async fn dialog_flags(
    state: &Arc<TelegramState>,
    dialog_id: Option<i64>,
    kind: &str,
    defaults: NotifyDefaults,
) -> DialogFlags {
    let Some(key) = dialog_id else {
        return resolve_dialog_flags(None, false, defaults, kind);
    };
    let cached = state.is_dialog_muted(key).await;
    let archived = state.is_archived(key).await;
    resolve_dialog_flags(cached, archived, defaults, kind)
}

/// Флаги диалога, из которого пришло сообщение.
///
/// Категория берётся из `Peer` (там мегагруппа — это `group`), а не из тега
/// `PeerId`: иначе мегагруппа получала бы дефолт broadcast-каналов.
async fn message_flags(
    state: &Arc<TelegramState>,
    msg: &grammers_client::message::Message,
    defaults: NotifyDefaults,
) -> DialogFlags {
    let peer_id = msg.peer_id();
    let kind = msg.peer().map_or_else(|| dto::kind_of(peer_id), dto::peer_kind);
    dialog_flags(state, peer_id.bot_api_dialog_id(), kind, defaults).await
}

/// Собирает контекст обработки: снимок дефолтов, флаги диалога, время.
async fn update_context(state: &Arc<TelegramState>, update: &Update) -> UpdateContext {
    let defaults = state.notify_defaults().await;
    let flags = match update {
        Update::NewMessage(msg) => message_flags(state, msg, defaults).await,
        _ => DialogFlags::default(),
    };
    UpdateContext {
        defaults,
        flags,
        now: dto::now_unix(),
    }
}

/// Применяет правки кэшей к состоянию.
///
/// Смена глобальных дефолтов логируется (это редкое и заметное событие —
/// «заглушил все чаты с телефона»), per-peer запись — нет: их бывают сотни
/// при синхронизации после долгого офлайна.
async fn apply_effects(state: &Arc<TelegramState>, effects: &[CacheEffect]) {
    for effect in effects {
        match effect {
            CacheEffect::Defaults(d) => {
                if state.set_notify_defaults(*d).await {
                    tracing::info!(
                        target: "forge_telegram",
                        users = d.users,
                        chats = d.chats,
                        broadcasts = d.broadcasts,
                        "forge-telegram: global notification defaults changed"
                    );
                }
            }
            CacheEffect::DialogMuted { dialog_id, muted } => {
                state.set_dialog_muted(*dialog_id, *muted).await;
            }
        }
    }
}

/// Пополняет кэш peer'ов данными из апдейта.
///
/// Апдейты — самый дешёвый источник свежих `access_hash`'ей: они приходят
/// вместе с сообщением, и запомнить их сейчас дешевле, чем потом сканировать
/// диалоги в [`peers::resolve_peer`].
async fn cache_peers_from(state: &Arc<TelegramState>, update: &Update) {
    let msg = match update {
        Update::NewMessage(m) | Update::MessageEdited(m) => m,
        _ => return,
    };
    for peer in [msg.peer(), msg.sender()].into_iter().flatten() {
        if let Ok(Some(r)) = peer.to_ref().await {
            peers::cache_peer(state, r).await;
        }
    }
}

/// Какие записи медиа-кэша обесценил апдейт.
///
/// Ответ роута медиа помечен `Cache-Control: … immutable`, поэтому запись
/// ОБЯЗАНА исчезнуть с диска в тот же момент, когда сообщение изменилось:
/// иначе отредактированное фото осталось бы старым и в кэше сервера, и в
/// кэше браузера — навсегда.
///
/// Считается по уже собранным событиям (а не по сырому апдейту), чтобы
/// `dialog_id` был ровно тем, что уходит во фронт: у удалений в ЛС Telegram
/// диалог не сообщает, и там строка пустая — [`media_cache::invalidate`]
/// понимает это как «искать во всех каталогах».
///
/// Чистая функция: матрица событий проверяется без сети и без ФС.
pub fn invalidations(events: &[TgServerMsg]) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    for ev in events {
        match ev {
            TgServerMsg::MessageEdited { message } => {
                out.push((message.dialog_id.clone(), message.id));
            }
            TgServerMsg::MessageDeleted {
                dialog_id,
                message_ids,
            } => {
                for id in message_ids {
                    out.push((dialog_id.clone(), *id));
                }
            }
            // Новое сообщение кэшировать было нечем: его id ещё не видели.
            _ => {}
        }
    }
    out
}

/// Обрабатывает один апдейт: кэш peer'ов, кэш уведомлений, медиа-кэш,
/// unread, broadcast.
async fn handle_update(state: &Arc<TelegramState>, update: &Update) {
    cache_peers_from(state, update).await;
    let ctx = update_context(state, update).await;
    let Mapped {
        events,
        unread_for,
        effects,
    } = map_update(update, ctx);

    // Кэши — ДО broadcast: клиент, дёрнувший REST в ответ на кадр, обязан
    // увидеть уже новое состояние, а не то, из которого кадр родился.
    apply_effects(state, &effects).await;

    // Инвалидация ДО broadcast: фронт, получив `message_edited`, сразу же
    // перезапросит медиа — к этому моменту старого файла быть не должно.
    for (dialog_id, message_id) in invalidations(&events) {
        let removed = media_cache::invalidate(&state.config, &dialog_id, message_id).await;
        if removed > 0 {
            tracing::debug!(
                target: "forge_telegram",
                dialog_id = %dialog_id,
                message_id,
                removed,
                "forge-telegram: media cache invalidated"
            );
        }
    }

    if let Some(dialog_id) = unread_for {
        let total = state.bump_unread(&dialog_id).await;
        tracing::debug!(
            target: "forge_telegram",
            dialog_id = %dialog_id,
            unread = total,
            "forge-telegram: incoming message bumped unread"
        );
    }
    for ev in events {
        state.broadcast_event(ev);
    }
}

/// Один вызов `account.getNotifySettings` для категории.
async fn notify_scope(
    client: &Client,
    peer: tl::enums::InputNotifyPeer,
) -> Result<tl::types::PeerNotifySettings, InvocationError> {
    let answer = client
        .invoke(&tl::functions::account::GetNotifySettings { peer })
        .await?;
    let tl::enums::PeerNotifySettings::Settings(settings) = answer;
    Ok(settings)
}

/// Забирает глобальные дефолты уведомлений тремя RPC.
///
/// `None` ⇒ хотя бы один вызов не удался. Частичный результат сознательно
/// не возвращается: «две категории из трёх» дали бы кэш, в котором одна
/// категория молча считается незаглушённой — хуже, чем честно оставить
/// прежнее значение и повторить на следующем реконнекте.
///
/// Ошибки не фатальны и не влияют на поток апдейтов; в лог уходит только
/// имя категории и текст ошибки (кредов в нём нет).
pub async fn load_notify_defaults(client: &Client, now: i64) -> Option<NotifyDefaults> {
    let scopes = [
        ("users", tl::enums::InputNotifyPeer::InputNotifyUsers),
        ("chats", tl::enums::InputNotifyPeer::InputNotifyChats),
        ("broadcasts", tl::enums::InputNotifyPeer::InputNotifyBroadcasts),
    ];
    let mut settings = Vec::with_capacity(scopes.len());
    for (name, peer) in scopes {
        match notify_scope(client, peer).await {
            Ok(s) => settings.push(s),
            Err(e) => {
                tracing::warn!(
                    target: "forge_telegram",
                    scope = name,
                    error = %e,
                    "forge-telegram: could not read global notification settings"
                );
                return None;
            }
        }
    }
    Some(NotifyDefaults::from_settings(
        &settings[0],
        &settings[1],
        &settings[2],
        now,
    ))
}

/// Перечитывает глобальные дефолты и, если они изменились, рассылает
/// `notify_defaults_changed`.
///
/// Вызывается перед стартом потока апдейтов (в т.ч. на каждом реконнекте):
/// пока плагина не было в сети, пользователь мог заглушить всё с телефона.
/// Кадр шлётся ТОЛЬКО при реальном изменении — иначе каждый реконнект
/// заставлял бы фронт перерисовывать список без причины.
pub async fn refresh_notify_defaults(state: &Arc<TelegramState>, client: &Client) {
    let fresh = tokio::time::timeout(
        NOTIFY_DEFAULTS_TIMEOUT,
        load_notify_defaults(client, dto::now_unix()),
    )
    .await;
    let fresh = match fresh {
        Ok(Some(d)) => d,
        Ok(None) => return,
        Err(_) => {
            tracing::warn!(
                target: "forge_telegram",
                timeout_secs = NOTIFY_DEFAULTS_TIMEOUT.as_secs(),
                "forge-telegram: global notification settings timed out, keeping the previous ones"
            );
            return;
        }
    };
    if state.set_notify_defaults(fresh).await {
        tracing::info!(
            target: "forge_telegram",
            users = fresh.users,
            chats = fresh.chats,
            broadcasts = fresh.broadcasts,
            "forge-telegram: global notification defaults loaded"
        );
        state.broadcast_event(TgServerMsg::NotifyDefaultsChanged {
            users: fresh.users,
            chats: fresh.chats,
            broadcasts: fresh.broadcasts,
        });
    }
}

/// Причина выхода воркера.
#[derive(Debug)]
enum WorkerExit {
    /// Штатная остановка по `shutdown`-токену.
    Shutdown,
    /// Авторизация мертва — реконнект бессмысленен (риск №4).
    AuthInvalid(String),
    /// Соединение потеряно — можно пробовать снова с backoff'ом.
    Failed(String),
}

/// Линейный воркер: читает поток апдейтов, пока может.
async fn run_worker(
    state: &Arc<TelegramState>,
    client: Client,
    rx: UnboundedReceiver<UpdatesLike>,
) -> WorkerExit {
    let cfg = updates_config(&state.config);
    let mut stream = match client.stream_updates(rx, cfg).await {
        Ok(s) => s,
        Err(e) => return WorkerExit::Failed(format!("could not start update stream: {e}")),
    };
    tracing::info!(
        target: "forge_telegram",
        catch_up = state.config.catch_up,
        queue_limit = state.config.update_queue_limit,
        "forge-telegram: updates worker started"
    );

    loop {
        // `biased` — сначала shutdown: на остановке важнее успеть сохранить
        // состояние апдейтов, чем разобрать ещё один кадр.
        let next = tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                sync_state(&stream, "shutdown").await;
                return WorkerExit::Shutdown;
            }
            next = stream.next() => next,
        };
        match next {
            Ok(update) => handle_update(state, &update).await,
            Err(e) => {
                // Сохраняем pts ДО ухода: иначе реконнект с catch_up
                // пере-выкачает уже показанные сообщения.
                sync_state(&stream, "stream error").await;
                return if is_auth_error(&e) {
                    WorkerExit::AuthInvalid(format!("Telegram rejected the session: {e}"))
                } else {
                    WorkerExit::Failed(e.to_string())
                };
            }
        }
    }
}

/// Сохраняет состояние апдейтов в сессию, не заваливая лог при ошибке.
async fn sync_state(stream: &UpdateStream, why: &str) {
    match stream.sync_update_state().await {
        Ok(()) => tracing::debug!(
            target: "forge_telegram",
            reason = why,
            "forge-telegram: update state persisted"
        ),
        Err(e) => tracing::warn!(
            target: "forge_telegram",
            reason = why,
            error = %e,
            "forge-telegram: could not persist update state"
        ),
    }
}

/// Ждёт состояния `Authorized`. `false` ⇒ пришёл shutdown.
async fn wait_authorized(state: &Arc<TelegramState>) -> bool {
    loop {
        if state.shutdown.is_cancelled() {
            return false;
        }
        // Подписка ДО проверки: `notify_waiters` не оставляет «permit», и
        // авторизация, случившаяся между проверкой и await'ом, была бы
        // потеряна — воркер проспал бы до следующего тика.
        let notified = state.authorized_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        if state.is_authorized().await {
            return true;
        }
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => return false,
            _ = notified => {}
            _ = tokio::time::sleep(AUTH_POLL) => {}
        }
    }
}

/// Забирает клиент и (одноразовый) приёмник апдейтов из рантайма.
async fn take_stream(
    state: &Arc<TelegramState>,
) -> Option<(Client, UnboundedReceiver<UpdatesLike>)> {
    let mut guard = state.runtime.write().await;
    let rt = guard.as_mut()?;
    let rx = rt.take_updates()?;
    Some((rt.client.clone(), rx))
}

/// Пауза backoff'а, прерываемая shutdown'ом. `false` ⇒ пора выходить.
async fn backoff_sleep(state: &Arc<TelegramState>, step: usize) -> bool {
    let secs = backoff_secs(step);
    tracing::info!(
        target: "forge_telegram",
        delay_secs = secs,
        "forge-telegram: updates worker will reconnect after backoff"
    );
    tokio::select! {
        _ = state.shutdown.cancelled() => false,
        _ = tokio::time::sleep(Duration::from_secs(secs)) => true,
    }
}

/// Пересобирает подключение: пауза → гасим старый рантайм → восстановление.
///
/// Пересборка (а не «переподписка») обязательна: приёмник апдейтов
/// одноразовый и принадлежит конкретному `SenderPool`.
async fn reconnect(state: &Arc<TelegramState>, step: &mut usize) -> bool {
    if !backoff_sleep(state, *step).await {
        return false;
    }
    *step = (*step + 1).min(BACKOFF_SECS.len() - 1);
    state.clear_runtime().await;
    crate::restore_session(state).await;
    true
}

/// Supervisor: единственная долгоживущая задача плагина.
pub async fn supervisor(state: Arc<TelegramState>) {
    let mut step = 0usize;
    loop {
        if !wait_authorized(&state).await {
            break;
        }
        let Some((client, rx)) = take_stream(&state).await else {
            // Авторизация есть, а рабочего приёмника нет: либо рантайм успели
            // погасить, либо его приёмник уже израсходован прошлым воркером.
            // И то и другое лечится пересборкой соединения.
            tracing::debug!(
                target: "forge_telegram",
                "forge-telegram: no updates receiver available, rebuilding the connection"
            );
            if !reconnect(&state, &mut step).await {
                break;
            }
            continue;
        };
        // Дожили до рабочего потока ⇒ прошлые неудачи больше не в счёт.
        step = 0;

        // Дефолты уведомлений — ДО первого разобранного апдейта: иначе
        // сообщение из глобально заглушённой категории уйдёт во фронт как
        // звенящее. Апдейты за это время не теряются: они копятся в канале
        // рантайма, который мы только что забрали, но ещё не читаем.
        refresh_notify_defaults(&state, &client).await;

        match run_worker(&state, client, rx).await {
            WorkerExit::Shutdown => break,
            WorkerExit::AuthInvalid(reason) => {
                // Реконнекта НЕ делаем: ключ мёртв. UI покажет wizard, и
                // следующий виток начнётся только после нового логина.
                login::on_auth_invalidated(&state, reason).await;
                state.clear_runtime().await;
                step = 0;
            }
            WorkerExit::Failed(reason) => {
                tracing::warn!(
                    target: "forge_telegram",
                    error = %reason,
                    "forge-telegram: updates worker stopped, will reconnect"
                );
                if !reconnect(&state, &mut step).await {
                    break;
                }
            }
        }
    }
    tracing::info!(target: "forge_telegram", "forge-telegram: updates supervisor stopped");
}

/// Запускает supervisor и регистрирует его для graceful shutdown.
///
/// Вызывается ВСЕГДА (даже без кредов): supervisor сам дождётся авторизации,
/// поэтому вход через wizard уже в работающем процессе поднимает realtime без
/// рестарта devforge.
pub async fn spawn(state: &Arc<TelegramState>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { supervisor(st).await });
    state.register_worker(handle).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login::{LoginFlow, TgUserDto};

    fn state() -> Arc<TelegramState> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            ..TelegramConfig::default()
        };
        // Каталог нарочно не удаляется до конца процесса: тесты этого модуля
        // файлов не создают, а `TempDir` в возвращаемом типе только мешал бы.
        std::mem::forget(dir);
        TelegramState::new(cfg)
    }

    fn rpc(code: i32, message: &str) -> InvocationError {
        InvocationError::Rpc(grammers_mtsender::RpcError::from(tl::types::RpcError {
            error_code: code,
            error_message: message.to_string(),
        }))
    }

    fn typing_action() -> tl::enums::SendMessageAction {
        tl::enums::SendMessageAction::SendMessageTypingAction
    }

    fn authorized_user() -> LoginFlow {
        LoginFlow::Authorized {
            user: TgUserDto {
                id: "1".into(),
                username: None,
                first_name: None,
                phone: None,
            },
        }
    }

    #[test]
    fn backoff_climbs_and_saturates_at_a_minute() {
        assert_eq!(backoff_secs(0), 1);
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(2), 5);
        assert_eq!(backoff_secs(3), 10);
        assert_eq!(backoff_secs(4), 30);
        assert_eq!(backoff_secs(5), 60);
        assert_eq!(backoff_secs(99), 60, "потолок обязан быть минутой");
    }

    #[test]
    fn updates_config_follows_the_plugin_config() {
        let cfg = TelegramConfig {
            catch_up: false,
            update_queue_limit: 7,
            ..TelegramConfig::default()
        };
        let uc = updates_config(&cfg);
        assert!(!uc.catch_up);
        assert_eq!(uc.update_queue_limit, Some(7));
    }

    #[test]
    fn auth_errors_are_told_apart_from_network_errors() {
        for name in [
            "AUTH_KEY_UNREGISTERED",
            "AUTH_KEY_DUPLICATED",
            "SESSION_REVOKED",
            "SESSION_EXPIRED",
            "USER_DEACTIVATED",
        ] {
            assert!(is_auth_error(&rpc(400, name)), "{name} — мёртвая сессия");
        }
        assert!(
            is_auth_error(&rpc(401, "SOMETHING_ELSE")),
            "401 = не авторизован"
        );

        assert!(!is_auth_error(&rpc(420, "FLOOD_WAIT_5")));
        assert!(!is_auth_error(&rpc(500, "INTERNAL")));
        assert!(!is_auth_error(&rpc(400, "PEER_ID_INVALID")));
        assert!(!is_auth_error(&InvocationError::Dropped));
    }

    #[test]
    fn action_names_are_snake_case_contract() {
        use tl::enums::SendMessageAction as A;
        let cases: Vec<(A, &str)> = vec![
            (A::SendMessageTypingAction, "typing"),
            (A::SendMessageCancelAction, "cancel"),
            (A::SendMessageRecordAudioAction, "record_voice"),
            (A::SendMessageRecordVideoAction, "record_video"),
            (A::SendMessageChooseStickerAction, "choose_sticker"),
            (
                A::SendMessageUploadPhotoAction(tl::types::SendMessageUploadPhotoAction {
                    progress: 0,
                }),
                "upload_photo",
            ),
            (
                A::SendMessageUploadDocumentAction(tl::types::SendMessageUploadDocumentAction {
                    progress: 50,
                }),
                "upload_document",
            ),
        ];
        for (action, expected) in cases {
            assert_eq!(action_name(&action), expected);
        }
    }

    #[test]
    fn user_typing_maps_dialog_to_the_sender_itself() {
        let ev = map_raw_update(&tl::enums::Update::UserTyping(
            tl::types::UpdateUserTyping {
                user_id: 777_000,
                top_msg_id: None,
                action: typing_action(),
            },
        ))
        .expect("typing обязан маппиться");
        match ev {
            TgServerMsg::Typing {
                dialog_id,
                user_id,
                action,
            } => {
                assert_eq!(dialog_id, "777000");
                assert_eq!(user_id, "777000", "в ЛС диалог и есть собеседник");
                assert_eq!(action, "typing");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn chat_and_channel_typing_carry_both_ids() {
        let ev = map_raw_update(&tl::enums::Update::ChatUserTyping(
            tl::types::UpdateChatUserTyping {
                chat_id: 123,
                from_id: tl::enums::Peer::User(tl::types::PeerUser { user_id: 777_000 }),
                action: typing_action(),
            },
        ))
        .unwrap();
        match ev {
            TgServerMsg::Typing {
                dialog_id, user_id, ..
            } => {
                assert_eq!(dialog_id, "-123", "малая группа — отрицательный id");
                assert_eq!(user_id, "777000");
            }
            other => panic!("unexpected: {other:?}"),
        }

        let ev = map_raw_update(&tl::enums::Update::ChannelUserTyping(
            tl::types::UpdateChannelUserTyping {
                channel_id: 1_234_567_890,
                top_msg_id: None,
                from_id: tl::enums::Peer::User(tl::types::PeerUser { user_id: 777_000 }),
                action: typing_action(),
            },
        ))
        .unwrap();
        match ev {
            TgServerMsg::Typing { dialog_id, .. } => {
                assert_eq!(dialog_id, "-1001234567890", "канал — префикс -100");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn user_status_maps_online_and_last_seen() {
        let ev = map_raw_update(&tl::enums::Update::UserStatus(
            tl::types::UpdateUserStatus {
                user_id: 777_000,
                status: tl::enums::UserStatus::Online(tl::types::UserStatusOnline {
                    expires: 1_800_000_300,
                }),
            },
        ))
        .unwrap();
        assert_eq!(
            ev,
            TgServerMsg::Online {
                user_id: "777000".into(),
                online: true,
                was_online: None,
            }
        );

        let ev = map_raw_update(&tl::enums::Update::UserStatus(
            tl::types::UpdateUserStatus {
                user_id: 777_000,
                status: tl::enums::UserStatus::Offline(tl::types::UserStatusOffline {
                    was_online: 1_800_000_000,
                }),
            },
        ))
        .unwrap();
        assert_eq!(
            ev,
            TgServerMsg::Online {
                user_id: "777000".into(),
                online: false,
                was_online: Some(1_800_000_000),
            }
        );

        // «Был недавно» — точного времени приватность не отдаёт.
        let ev = map_raw_update(&tl::enums::Update::UserStatus(
            tl::types::UpdateUserStatus {
                user_id: 777_000,
                status: tl::enums::UserStatus::Recently(tl::types::UserStatusRecently {
                    by_me: false,
                }),
            },
        ))
        .unwrap();
        match ev {
            TgServerMsg::Online {
                online, was_online, ..
            } => {
                assert!(!online);
                assert!(was_online.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_updates_carry_direction_for_dialogs_and_channels() {
        let cases: Vec<(tl::enums::Update, &str, &str, i32)> = vec![
            (
                tl::enums::Update::ReadHistoryInbox(tl::types::UpdateReadHistoryInbox {
                    folder_id: None,
                    peer: tl::enums::Peer::User(tl::types::PeerUser { user_id: 777_000 }),
                    top_msg_id: None,
                    max_id: 42,
                    still_unread_count: 0,
                    pts: 1,
                    pts_count: 1,
                }),
                "777000",
                "in",
                42,
            ),
            (
                tl::enums::Update::ReadHistoryOutbox(tl::types::UpdateReadHistoryOutbox {
                    peer: tl::enums::Peer::User(tl::types::PeerUser { user_id: 777_000 }),
                    max_id: 43,
                    pts: 1,
                    pts_count: 1,
                }),
                "777000",
                "out",
                43,
            ),
            (
                tl::enums::Update::ReadChannelInbox(tl::types::UpdateReadChannelInbox {
                    folder_id: None,
                    channel_id: 1_234_567_890,
                    max_id: 7,
                    still_unread_count: 0,
                    pts: 1,
                }),
                "-1001234567890",
                "in",
                7,
            ),
            (
                tl::enums::Update::ReadChannelOutbox(tl::types::UpdateReadChannelOutbox {
                    channel_id: 1_234_567_890,
                    max_id: 8,
                }),
                "-1001234567890",
                "out",
                8,
            ),
        ];
        for (update, want_dialog, want_dir, want_max) in cases {
            let ev = map_raw_update(&update).expect("read-апдейт обязан маппиться");
            match ev {
                TgServerMsg::Read {
                    dialog_id,
                    max_id,
                    direction,
                } => {
                    assert_eq!(dialog_id, want_dialog);
                    assert_eq!(direction, want_dir);
                    assert_eq!(max_id, want_max);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn irrelevant_raw_updates_are_ignored() {
        // Апдейт о непрочитанных реакциях нас не касается, но и падать
        // из-за него нельзя.
        let update = tl::enums::Update::ReadChannelDiscussionInbox(
            tl::types::UpdateReadChannelDiscussionInbox {
                channel_id: 1,
                top_msg_id: 2,
                read_max_id: 3,
                broadcast_id: None,
                broadcast_post: None,
            },
        );
        assert!(map_raw_update(&update).is_none());
    }

    /// Удаление в канале адресуется точно, в ЛС — пустым `dialog_id`
    /// (Telegram не говорит, какой это диалог; фронт ищет id у себя).
    #[test]
    fn deleted_event_addresses_channels_and_leaves_dm_unaddressed() {
        let ev = deleted_event(Some(1_234_567_890), &[7, 8]);
        assert_eq!(
            ev,
            TgServerMsg::MessageDeleted {
                dialog_id: "-1001234567890".into(),
                message_ids: vec![7, 8],
            }
        );

        let ev = deleted_event(None, &[9]);
        match ev {
            TgServerMsg::MessageDeleted {
                dialog_id,
                message_ids,
            } => {
                assert!(dialog_id.is_empty(), "врать конкретным диалогом нельзя");
                assert_eq!(message_ids, vec![9]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Правка и удаление обесценивают медиа-кэш; всё остальное — нет.
    #[test]
    fn invalidations_cover_edits_and_deletions_only() {
        let edited = TgServerMsg::MessageEdited {
            message: dto::MessageDto {
                id: 42,
                dialog_id: "-1001234567890".into(),
                ..dto::MessageDto::default()
            },
        };
        assert_eq!(
            invalidations(&[edited]),
            vec![("-1001234567890".to_string(), 42)]
        );

        let deleted = TgServerMsg::MessageDeleted {
            dialog_id: "777000".into(),
            message_ids: vec![7, 8, 9],
        };
        assert_eq!(
            invalidations(&[deleted]),
            vec![
                ("777000".to_string(), 7),
                ("777000".to_string(), 8),
                ("777000".to_string(), 9),
            ]
        );

        // Удаление в ЛС приходит без диалога — строка пустая, и это НЕ повод
        // пропускать инвалидацию (media_cache обойдёт все каталоги).
        let dm = TgServerMsg::MessageDeleted {
            dialog_id: String::new(),
            message_ids: vec![5],
        };
        assert_eq!(invalidations(&dm_events(dm)), vec![(String::new(), 5)]);

        // Новое сообщение, typing, read, прогресс — кэшу безразличны.
        let untouched = vec![
            TgServerMsg::NewMessage {
                message: dto::MessageDto {
                    id: 1,
                    dialog_id: "777000".into(),
                    ..dto::MessageDto::default()
                },
                muted: false,
                archived: false,
            },
            TgServerMsg::Typing {
                dialog_id: "777000".into(),
                user_id: "777000".into(),
                action: "typing".into(),
            },
            TgServerMsg::Read {
                dialog_id: "777000".into(),
                max_id: 3,
                direction: "in".into(),
            },
            TgServerMsg::Resync,
        ];
        assert!(invalidations(&untouched).is_empty());
    }

    fn dm_events(ev: TgServerMsg) -> Vec<TgServerMsg> {
        vec![ev]
    }

    /// Полный цикл: закэшированное медиа исчезает с диска, как только
    /// сообщение отредактировали (иначе `Cache-Control: immutable` навсегда
    /// зафиксировал бы старую картинку).
    #[tokio::test]
    async fn editing_a_message_evicts_its_cached_media() {
        use crate::media_cache::{self, CacheKey, MediaKind, MediaMeta};

        let st = state();
        for kind in MediaKind::all() {
            let key = CacheKey {
                dialog_id: -1001234567890,
                message_id: 42,
                kind,
            };
            media_cache::put_atomic(
                &st.config,
                &key,
                b"old-picture",
                &MediaMeta::now(Some("image/jpeg".into()), 11, None),
            )
            .await
            .unwrap();
        }
        let full = CacheKey {
            dialog_id: -1001234567890,
            message_id: 42,
            kind: MediaKind::Full,
        };
        assert!(media_cache::get(&st.config, &full).await.is_some());

        // То же, что делает handle_update для события правки.
        for (dialog_id, message_id) in invalidations(&[TgServerMsg::MessageEdited {
            message: dto::MessageDto {
                id: 42,
                dialog_id: "-1001234567890".into(),
                ..dto::MessageDto::default()
            },
        }]) {
            media_cache::invalidate(&st.config, &dialog_id, message_id).await;
        }

        assert!(
            media_cache::get(&st.config, &full).await.is_none(),
            "после правки старый файл обязан исчезнуть"
        );
        let thumb = CacheKey {
            kind: MediaKind::Thumb,
            ..full
        };
        assert!(media_cache::get(&st.config, &thumb).await.is_none());
    }

    /// Supervisor обязан завершаться по shutdown-токену, даже если
    /// авторизации так и не случилось (иначе процесс не выйдет).
    #[tokio::test]
    async fn supervisor_exits_on_shutdown_while_waiting_for_auth() {
        let st = state();
        spawn(&st).await;
        // Дать задаче дойти до ожидания авторизации.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(st.workers.lock().await.len(), 1);

        st.shutdown.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(3), st.shutdown_workers()).await;
        assert!(stopped.is_ok(), "supervisor должен выйти по токену");
        assert!(st.workers.lock().await.is_empty());
    }

    /// Авторизация без рантайма: supervisor не крутится в busy-loop (ушёл в
    /// backoff-пересборку) и всё равно завершается по shutdown.
    #[tokio::test]
    async fn supervisor_survives_authorized_without_runtime() {
        let st = state();
        spawn(&st).await;
        st.set_login(authorized_user()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Никакой паники; состояние логина воркером не сброшено.
        assert!(st.is_authorized().await);

        st.shutdown.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), st.shutdown_workers())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn wait_authorized_returns_false_after_cancel() {
        let st = state();
        st.shutdown.cancel();
        assert!(!wait_authorized(&st).await);
    }

    /// Авторизация, случившаяся во время ожидания, будит воркер через Notify.
    #[tokio::test]
    async fn wait_authorized_wakes_up_on_login() {
        let st = state();
        let waiter = {
            let st = st.clone();
            tokio::spawn(async move { wait_authorized(&st).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        st.set_login(authorized_user()).await;
        let woke = tokio::time::timeout(Duration::from_secs(3), waiter)
            .await
            .expect("ожидание не должно висеть")
            .unwrap();
        assert!(woke, "переход в Authorized обязан разбудить воркер");
    }

    /// Без рантайма приёмник апдейтов взять нельзя — и это не паника.
    #[tokio::test]
    async fn take_stream_without_runtime_is_none() {
        let st = state();
        assert!(take_stream(&st).await.is_none());
    }

    /// Входящее сообщение поднимает unread, `read` его обнуляет.
    #[tokio::test]
    async fn unread_counter_reacts_to_incoming_only() {
        let st = state();
        assert_eq!(st.bump_unread("777000").await, 1);
        assert_eq!(st.unread_of("777000").await, 1);
        st.set_unread("777000", 0).await;
        assert_eq!(st.unread_of("777000").await, 0);
    }

    fn notify_update(
        peer: tl::enums::NotifyPeer,
        mute_until: Option<i32>,
        silent: Option<bool>,
    ) -> tl::types::UpdateNotifySettings {
        tl::types::UpdateNotifySettings {
            peer,
            notify_settings: tl::enums::PeerNotifySettings::Settings(
                tl::types::PeerNotifySettings {
                    show_previews: None,
                    silent,
                    mute_until,
                    ios_sound: None,
                    android_sound: None,
                    other_sound: None,
                    stories_muted: None,
                    stories_hide_sender: None,
                    stories_ios_sound: None,
                    stories_android_sound: None,
                    stories_other_sound: None,
                },
            ),
        }
    }

    fn dialog_notify_peer(user_id: i64) -> tl::enums::NotifyPeer {
        tl::enums::NotifyPeer::Peer(tl::types::NotifyPeer {
            peer: tl::enums::Peer::User(tl::types::PeerUser { user_id }),
        })
    }

    fn channel_notify_peer(channel_id: i64) -> tl::enums::NotifyPeer {
        tl::enums::NotifyPeer::Peer(tl::types::NotifyPeer {
            peer: tl::enums::Peer::Channel(tl::types::PeerChannel { channel_id }),
        })
    }

    fn muted_defaults() -> NotifyDefaults {
        NotifyDefaults {
            users: true,
            chats: true,
            broadcasts: true,
        }
    }

    /// Mute/unmute диалога: `muted` считается по фиксированному `now`
    /// (mute в будущем/«навсегда» — true; прошлое/None — false), а пустые
    /// настройки НАСЛЕДУЮТ дефолт категории.
    #[test]
    fn notify_settings_event_resolves_dialog_mute_with_inheritance() {
        let now = 1_800_000_000_i64;
        // (mute_until, silent, дефолты, ожидаемый muted)
        let cases: Vec<(Option<i32>, Option<bool>, NotifyDefaults, bool)> = vec![
            (Some(i32::MAX), None, NotifyDefaults::default(), true), // «навсегда»
            (Some(1_800_000_001), None, NotifyDefaults::default(), true),
            (Some(1), None, NotifyDefaults::default(), false),
            (None, None, NotifyDefaults::default(), false),
            (None, Some(true), NotifyDefaults::default(), true),
            // Пусто целиком ⇒ дефолт категории «users».
            (None, None, muted_defaults(), true),
            // Точечный unmute перебивает «заглушить все чаты».
            (Some(1), None, muted_defaults(), false),
            // Дефолт ДРУГОЙ категории на ЛС не влияет.
            (
                None,
                None,
                NotifyDefaults {
                    chats: true,
                    broadcasts: true,
                    ..NotifyDefaults::default()
                },
                false,
            ),
        ];
        for (mute_until, silent, defaults, want_muted) in cases {
            let u = notify_update(dialog_notify_peer(777_000), mute_until, silent);
            let ev = notify_settings_event(&u, now, defaults)
                .expect("настройки диалога дают событие");
            assert_eq!(
                ev,
                TgServerMsg::NotifySettings {
                    dialog_id: "777000".into(),
                    muted: want_muted,
                },
                "mute_until={mute_until:?} silent={silent:?} defaults={defaults:?}"
            );
        }
    }

    /// Глобальный `updateNotifySettings` меняет ровно СВОЮ категорию и
    /// даёт кадр `notify_defaults_changed` со всеми тремя значениями.
    #[test]
    fn global_notify_updates_touch_only_their_own_category() {
        let now = 1_800_000_000_i64;
        let base = NotifyDefaults {
            users: false,
            chats: true,
            broadcasts: false,
        };
        let cases = vec![
            (
                tl::enums::NotifyPeer::NotifyUsers,
                NotifyDefaults {
                    users: true,
                    ..base
                },
            ),
            (
                tl::enums::NotifyPeer::NotifyChats,
                NotifyDefaults {
                    chats: true,
                    ..base
                },
            ),
            (
                tl::enums::NotifyPeer::NotifyBroadcasts,
                NotifyDefaults {
                    broadcasts: true,
                    ..base
                },
            ),
        ];
        for (peer, want) in cases {
            let u = notify_update(peer.clone(), Some(i32::MAX), None);
            assert_eq!(
                notify_outcome(&u, now, base),
                NotifyOutcome::Defaults(want),
                "{peer:?}"
            );
            assert_eq!(
                notify_settings_event(&u, now, base),
                Some(TgServerMsg::NotifyDefaultsChanged {
                    users: want.users,
                    chats: want.chats,
                    broadcasts: want.broadcasts,
                })
            );
        }

        // Снятие глобального mute («все каналы» размьючены) тоже доезжает.
        let u = notify_update(tl::enums::NotifyPeer::NotifyBroadcasts, Some(0), None);
        assert_eq!(
            notify_outcome(&u, now, muted_defaults()),
            NotifyOutcome::Defaults(NotifyDefaults {
                users: true,
                chats: true,
                broadcasts: false,
            })
        );
    }

    /// Форумный топик — не диалог нашего контракта: ни события, ни паники,
    /// ни правки кэшей.
    #[test]
    fn forum_topic_notify_update_is_ignored_without_panic() {
        let now = 1_800_000_000_i64;
        let u = tl::types::UpdateNotifySettings {
            peer: tl::enums::NotifyPeer::NotifyForumTopic(tl::types::NotifyForumTopic {
                peer: tl::enums::Peer::Channel(tl::types::PeerChannel {
                    channel_id: 1_234_567_890,
                }),
                top_msg_id: 42,
            }),
            notify_settings: notify_update(
                tl::enums::NotifyPeer::NotifyUsers,
                Some(i32::MAX),
                None,
            )
            .notify_settings,
        };
        assert_eq!(notify_outcome(&u, now, muted_defaults()), NotifyOutcome::Ignored);
        assert!(notify_settings_event(&u, now, muted_defaults()).is_none());
        assert_eq!(
            notify_mapped(
                &u,
                UpdateContext {
                    defaults: muted_defaults(),
                    now,
                    ..UpdateContext::default()
                }
            ),
            Mapped::default()
        );
    }

    /// Разбор `updateNotifySettings` не только вещает наружу, но и говорит,
    /// что запомнить: глобальные дефолты целиком, per-peer — ключом `i64`.
    #[test]
    fn notify_mapped_produces_cache_effects() {
        let now = 1_800_000_000_i64;
        let ctx = UpdateContext {
            defaults: NotifyDefaults::default(),
            now,
            ..UpdateContext::default()
        };

        let per_peer = notify_mapped(
            &notify_update(channel_notify_peer(1_234_567_890), Some(i32::MAX), None),
            ctx,
        );
        assert_eq!(
            per_peer.events,
            vec![TgServerMsg::NotifySettings {
                dialog_id: "-1001234567890".into(),
                muted: true,
            }]
        );
        assert_eq!(
            per_peer.effects,
            vec![CacheEffect::DialogMuted {
                dialog_id: -1_001_234_567_890,
                muted: true,
            }]
        );
        assert!(per_peer.unread_for.is_none());

        let global = notify_mapped(
            &notify_update(tl::enums::NotifyPeer::NotifyChats, Some(i32::MAX), None),
            ctx,
        );
        assert_eq!(
            global.effects,
            vec![CacheEffect::Defaults(NotifyDefaults {
                chats: true,
                ..NotifyDefaults::default()
            })]
        );
    }

    /// `map_raw_update` знать про уведомления не обязан (их разбирает
    /// `map_update` с дефолтами), а любые изменения папок сводит к
    /// `folders_changed`.
    #[test]
    fn raw_updates_cover_folder_changes_but_not_notify_settings() {
        assert!(
            map_raw_update(&tl::enums::Update::NotifySettings(notify_update(
                dialog_notify_peer(777_000),
                None,
                Some(true),
            )))
            .is_none(),
            "уведомления разбираются отдельно — с дефолтами и записью в кэш"
        );

        let folder_updates = vec![
            tl::enums::Update::DialogFilter(tl::types::UpdateDialogFilter {
                id: 2,
                filter: None,
            }),
            tl::enums::Update::DialogFilterOrder(tl::types::UpdateDialogFilterOrder {
                order: vec![2, 3],
            }),
            tl::enums::Update::DialogFilters,
        ];
        for update in folder_updates {
            assert_eq!(
                map_raw_update(&update),
                Some(TgServerMsg::FoldersChanged),
                "{update:?}"
            );
        }
    }

    /// Промах кэша mute'а обязан брать дефолт КАТЕГОРИИ, а не `false`:
    /// именно из-за слепого `false` тосты приходили от чатов, которых нет
    /// в загруженной странице списка диалогов.
    #[test]
    fn cache_miss_falls_back_to_the_category_default() {
        let defaults = NotifyDefaults {
            users: true,
            chats: false,
            broadcasts: true,
        };
        for (kind, want) in [("user", true), ("group", false), ("channel", true)] {
            assert_eq!(
                resolve_dialog_flags(None, false, defaults, kind),
                DialogFlags {
                    muted: want,
                    archived: false,
                },
                "промах кэша для {kind}"
            );
        }
        // Неизвестная категория — не паника и не «молчим».
        assert!(!resolve_dialog_flags(None, false, muted_defaults(), "wat").muted);

        // Запись в кэше сильнее дефолта в ОБЕ стороны.
        assert!(!resolve_dialog_flags(Some(false), false, muted_defaults(), "user").muted);
        assert!(
            resolve_dialog_flags(Some(true), false, NotifyDefaults::default(), "user").muted
        );

        // Архив прокидывается как есть.
        assert!(resolve_dialog_flags(Some(false), true, NotifyDefaults::default(), "user").archived);
    }

    /// Тот же путь, но через настоящее состояние: кэш → дефолт → архив.
    #[tokio::test]
    async fn dialog_flags_read_the_state_caches() {
        let st = state();
        let defaults = muted_defaults();

        // Ничего не знаем ⇒ дефолт категории.
        assert_eq!(
            dialog_flags(&st, Some(777_000), "user", defaults).await,
            DialogFlags {
                muted: true,
                archived: false,
            }
        );

        // Знаем, что размьючен точечно ⇒ дефолт не применяется.
        st.set_dialog_muted(777_000, false).await;
        st.set_archived(777_000, true).await;
        assert_eq!(
            dialog_flags(&st, Some(777_000), "user", defaults).await,
            DialogFlags {
                muted: false,
                archived: true,
            }
        );

        // Битый peer (нет Bot API-идентификатора) — тоже дефолт, без паники.
        assert_eq!(
            dialog_flags(&st, None, "channel", defaults).await,
            DialogFlags {
                muted: true,
                archived: false,
            }
        );
    }

    /// Эффекты применяются к состоянию: дефолты и per-peer запись.
    #[tokio::test]
    async fn apply_effects_updates_both_caches() {
        let st = state();
        apply_effects(
            &st,
            &[
                CacheEffect::Defaults(muted_defaults()),
                CacheEffect::DialogMuted {
                    dialog_id: -1_001_234_567_890,
                    muted: false,
                },
            ],
        )
        .await;

        assert_eq!(st.notify_defaults().await, muted_defaults());
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(false));
        // Диалог, о котором эффектов не было, так и остаётся неизвестным.
        assert_eq!(st.is_dialog_muted(777_000).await, None);
    }

    /// Кадр `new_message` берёт флаги из контекста — фронту гадать не по
    /// чему. Проверяется на всех трёх категориях сразу с промахом кэша.
    #[tokio::test]
    async fn new_message_frame_carries_resolved_flags() {
        let st = state();
        st.set_notify_defaults(NotifyDefaults {
            users: true,
            chats: false,
            broadcasts: true,
        })
        .await;
        st.set_archived(-1_001_234_567_890, true).await;
        let defaults = st.notify_defaults().await;

        for (dialog_id, kind, want_muted, want_archived) in [
            (777_000_i64, "user", true, false),
            (-999_999_999_999_i64, "group", false, false),
            (-1_001_234_567_890_i64, "channel", true, true),
        ] {
            let flags = dialog_flags(&st, Some(dialog_id), kind, defaults).await;
            assert_eq!(
                flags,
                DialogFlags {
                    muted: want_muted,
                    archived: want_archived,
                },
                "{kind}"
            );

            // Ровно эти значения уезжают в кадр (map_update кладёт их из ctx).
            let frame = TgServerMsg::NewMessage {
                message: dto::MessageDto {
                    id: 1,
                    dialog_id: dialog_id.to_string(),
                    ..dto::MessageDto::default()
                },
                muted: flags.muted,
                archived: flags.archived,
            };
            let json = serde_json::to_string(&frame).unwrap();
            assert!(
                json.contains(&format!(r#""muted":{want_muted},"archived":{want_archived}"#)),
                "{json}"
            );
        }
    }

    // ------------------------------------------------------------------
    // Краевые случаи уведомлений
    // ------------------------------------------------------------------

    fn chat_notify_peer(chat_id: i64) -> tl::enums::NotifyPeer {
        tl::enums::NotifyPeer::Peer(tl::types::NotifyPeer {
            peer: tl::enums::Peer::Chat(tl::types::PeerChat { chat_id }),
        })
    }

    /// Матрица per-peer уведомлений: 8 комбинаций глобальных дефолтов × три
    /// типа peer'а × все состояния настроек.
    ///
    /// Отдельно фиксируется задокументированное ОГРАНИЧЕНИЕ: категорию тут
    /// приходится выводить из тега `PeerId`, а он не отличает мегагруппу от
    /// broadcast-канала. Если кто-то однажды «починит» это без нового
    /// источника kind'а, тест обязан упасть, а не молча поменять поведение.
    #[test]
    fn per_peer_notify_matrix_covers_defaults_and_peer_tags() {
        let now = 1_800_000_000_i64;
        // (peer, dialog_id, категория, по которой берётся дефолт)
        let peers: [(tl::enums::NotifyPeer, &str, &str); 3] = [
            (dialog_notify_peer(777_000), "777000", "user"),
            (chat_notify_peer(555), "-555", "group"),
            (
                channel_notify_peer(1_234_567_890),
                "-1001234567890",
                "channel",
            ),
        ];
        let settings: [(Option<i32>, Option<bool>); 6] = [
            (None, None),
            (Some(0), None),
            (Some(1_700_000_000), None),
            (Some(1_900_000_000), None),
            (Some(i32::MAX), None),
            (None, Some(true)),
        ];

        for mask in 0..8_u8 {
            let defaults = NotifyDefaults {
                users: mask & 1 != 0,
                chats: mask & 2 != 0,
                broadcasts: mask & 4 != 0,
            };
            for (peer, dialog_id, category) in &peers {
                for (mute_until, silent) in settings {
                    let u = notify_update(peer.clone(), mute_until, silent);
                    let want = if silent.is_none() && mute_until.is_none() {
                        defaults.for_kind(category)
                    } else if silent == Some(true) {
                        true
                    } else {
                        mute_until.is_some_and(|v| i64::from(v) > now)
                    };
                    assert_eq!(
                        notify_outcome(&u, now, defaults),
                        NotifyOutcome::Dialog {
                            peer: dto::parse_dialog_id(dialog_id).unwrap(),
                            muted: want,
                        },
                        "{dialog_id} defaults={defaults:?} until={mute_until:?} silent={silent:?}"
                    );
                }
            }
        }

        // Ограничение по тегу id: мегагруппа приходит как Channel и получает
        // дефолт broadcasts, а не chats. Ошибка возможна ТОЛЬКО у диалога с
        // полностью унаследованными настройками; ближайший листинг перепишет
        // запись точным значением (там kind известен из индекса peer'ов).
        let chats_only = NotifyDefaults {
            chats: true,
            ..NotifyDefaults::default()
        };
        assert_eq!(
            notify_outcome(
                &notify_update(channel_notify_peer(1_234_567_891), None, None),
                now,
                chats_only,
            ),
            NotifyOutcome::Dialog {
                peer: dto::parse_dialog_id("-1001234567891").unwrap(),
                muted: false,
            },
            "мегагруппу от канала по тегу id не отличить — это известная цена"
        );
    }

    /// Границы часов и «навсегда» в разборе апдейта: ни `i64::MAX`, ни
    /// `i64::MIN` не должны ни паниковать, ни превращать mute в его
    /// противоположность внутри своей ветки.
    #[test]
    fn notify_outcome_survives_extreme_clocks() {
        let defaults = muted_defaults();
        for now in [i64::MIN, 0, i64::MAX] {
            // Явный mute «навсегда»: при now = i64::MAX даже он уже истёк —
            // это свойство 32-битного поля Telegram, а не переполнение.
            let forever = notify_update(dialog_notify_peer(777_000), Some(i32::MAX), None);
            assert_eq!(
                notify_outcome(&forever, now, defaults),
                NotifyOutcome::Dialog {
                    peer: dto::parse_dialog_id("777000").unwrap(),
                    muted: now < i64::from(i32::MAX),
                }
            );
            // Пустые настройки от времени не зависят вовсе — только от дефолта.
            let inherited = notify_update(dialog_notify_peer(777_000), None, None);
            assert_eq!(
                notify_outcome(&inherited, now, defaults),
                NotifyOutcome::Dialog {
                    peer: dto::parse_dialog_id("777000").unwrap(),
                    muted: true,
                }
            );
            // Глобальный апдейт на тех же часах — тоже без паники.
            assert_eq!(
                notify_outcome(
                    &notify_update(tl::enums::NotifyPeer::NotifyUsers, Some(i32::MAX), None),
                    now,
                    NotifyDefaults::default(),
                ),
                NotifyOutcome::Defaults(NotifyDefaults {
                    users: now < i64::from(i32::MAX),
                    ..NotifyDefaults::default()
                })
            );
        }
    }

    /// Сообщение из диалога, которого нет НИ В ОДНОМ кэше, хотя кэши уже
    /// заполнены соседями.
    ///
    /// Это исходная жалоба в чистом виде: список диалогов отдаёт верхнюю
    /// сотню, а сообщение может прийти из любого чата аккаунта. Промах
    /// обязан давать дефолт КАТЕГОРИИ; заодно проверяется, что запись
    /// соседнего диалога не «протекает» на незнакомца.
    #[tokio::test]
    async fn unknown_dialog_is_not_confused_with_its_neighbours() {
        let st = state();
        let defaults = NotifyDefaults {
            users: true,
            chats: false,
            broadcasts: true,
        };
        st.set_notify_defaults(defaults).await;
        // Кэши НЕ пусты: в них лежат другие диалоги всех трёх категорий.
        st.remember_dialog_flags(111_222, false, true).await;
        st.remember_dialog_flags(-777, true, false).await;
        st.remember_dialog_flags(-1_009_999_999_999, false, false).await;

        for (dialog_id, kind, want_muted) in [
            (999_888_777_i64, "user", true),
            (-424_242_i64, "group", false),
            (-1_001_111_111_111_i64, "channel", true),
        ] {
            assert_eq!(
                st.is_dialog_muted(dialog_id).await,
                None,
                "диалог обязан быть незнакомым"
            );
            assert_eq!(
                dialog_flags(&st, Some(dialog_id), kind, defaults).await,
                DialogFlags {
                    muted: want_muted,
                    archived: false,
                },
                "{kind} мимо кэша"
            );
        }

        // Соседи при этом остались нетронутыми — промах ничего не записал.
        assert_eq!(st.is_dialog_muted(111_222).await, Some(false));
        assert!(st.is_archived(111_222).await);
        assert_eq!(st.notify_overrides.read().await.len(), 3);
    }

    /// Гонка «настройки раньше списка»: `updateNotifySettings` про диалог,
    /// которого мы ещё ни разу не листали, обязан работать сразу и НЕ
    /// теряться от последующих листингов чужих страниц.
    ///
    /// Порядок в реальности именно такой: WS поднимается вместе с сессией, а
    /// список диалогов фронт запрашивает уже после — и заглушённый чат вполне
    /// может прислать сообщение в этом промежутке.
    #[tokio::test]
    async fn notify_settings_before_the_first_dialog_list_is_not_lost() {
        let st = state();
        let defaults = NotifyDefaults::default(); // ничего не заглушено глобально
        st.set_notify_defaults(defaults).await;
        let now = 1_800_000_000_i64;

        // 1. Пользователь глушит чат с телефона — списка мы ещё не видели.
        let effects = notify_mapped(
            &notify_update(channel_notify_peer(1_234_567_890), Some(i32::MAX), None),
            UpdateContext {
                defaults,
                now,
                ..UpdateContext::default()
            },
        );
        apply_effects(&st, &effects.effects).await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(true));

        // 2. Сообщение из него уже не «звенит», хотя ни одного листинга не было.
        assert_eq!(
            dialog_flags(&st, Some(-1_001_234_567_890), "channel", defaults).await,
            DialogFlags {
                muted: true,
                archived: false,
            }
        );

        // 3. Приезжает страница списка БЕЗ этого диалога — знание обязано
        //    уцелеть (кэши дополняются, а не пересеваются).
        st.remember_dialogs(&[dto::DialogDto {
            id: "777000".into(),
            muted: false,
            ..dto::DialogDto::default()
        }])
        .await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(true));

        // 4. И только страница, где диалог ЕСТЬ, перебивает значение:
        //    Telegram — источник истины, апдейт мог устареть.
        st.remember_dialogs(&[dto::DialogDto {
            id: "-1001234567890".into(),
            muted: false,
            archived: true,
            ..dto::DialogDto::default()
        }])
        .await;
        assert_eq!(
            dialog_flags(&st, Some(-1_001_234_567_890), "channel", defaults).await,
            DialogFlags {
                muted: false,
                archived: true,
            }
        );

        // 5. Обратный порядок (сначала список, потом апдейт) даёт то же
        //    итоговое состояние — иначе результат зависел бы от гонки.
        let unmute = notify_mapped(
            &notify_update(channel_notify_peer(1_234_567_890), Some(1), None),
            UpdateContext {
                defaults,
                now,
                ..UpdateContext::default()
            },
        );
        apply_effects(&st, &unmute.effects).await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(false));
        assert!(
            st.is_archived(-1_001_234_567_890).await,
            "апдейт про уведомления не трогает архив"
        );
    }

    /// Смена глобального дефолта не имеет права молча «размьютить» диалоги,
    /// у которых есть точечная запись: кадр `notify_defaults_changed` меняет
    /// ТОЛЬКО дефолт, а разрешённые значения остаются в силе.
    #[tokio::test]
    async fn changing_global_defaults_keeps_per_dialog_decisions() {
        let st = state();
        st.set_dialog_muted(777_000, false).await; // точечно размьючен
        st.set_dialog_muted(-555, true).await; // точечно заглушён

        let all_muted = muted_defaults();
        apply_effects(&st, &[CacheEffect::Defaults(all_muted)]).await;
        assert_eq!(st.notify_defaults().await, all_muted);

        assert_eq!(
            dialog_flags(&st, Some(777_000), "user", all_muted).await,
            DialogFlags::default(),
            "точечный unmute переживает «заглушить все чаты»"
        );
        assert!(dialog_flags(&st, Some(-555), "group", all_muted).await.muted);
        // А незнакомый диалог теперь молчит по дефолту категории.
        assert!(
            dialog_flags(&st, Some(-1_001_234_567_890), "channel", all_muted)
                .await
                .muted
        );
    }
}
