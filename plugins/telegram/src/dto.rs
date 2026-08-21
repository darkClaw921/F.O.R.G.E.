//! Wire-DTO плагина Telegram (то, что видит фронтенд).
//!
//! # Инвариант №1: 64-битные id — строками
//!
//! Telegram-идентификаторы диалогов/пользователей 64-битные, а `Number` в
//! JS теряет точность после 2^53. Поэтому `dialog_id` / `user_id` /
//! [`TgUserDto::id`] всегда сериализуются СТРОКАМИ. Исключение — `id`
//! сообщения: в Telegram он 32-битный, и остаётся числом ([`i32`]), чтобы
//! фронт мог сортировать и сравнивать его без парсинга.
//!
//! Формат строки — [Bot API dialog ID](https://core.telegram.org/api/bots/ids):
//! тот же вид, что показывают все остальные клиенты (`123456789` для
//! пользователя, `-100…` для канала). Именно он принимается обратно в
//! `/api/telegram/dialogs/:id/*`, поэтому фронт может гонять id как есть.
//!
//! # Инвариант №2: `Option` сериализуется в `null`, а не пропадает
//!
//! Ни одно поле DTO НЕ помечено `skip_serializing_if`. Ключ всегда
//! присутствует в JSON, просто может быть `null`. Так фронт различает «поля
//! нет в контракте» (ошибка версии) и «значения нет» (нормальный случай), а
//! проверки вида `if ('media' in msg)` не зависят от содержимого.
//! Единственное исключение в плагине — `retry_after_secs` в
//! [`crate::routes::error`], там `skip` осмыслен: поле относится только к
//! `flood_wait`.
//!
//! # Что здесь НЕ делается
//!
//! - `text` всегда plain: entities (жирный, ссылки, код) не парсятся —
//!   разметка Telegram в терминальном UI не рисуется, а склейка HTML из
//!   пользовательского текста была бы XSS-риском на фронте.
//! - Медиа только ОПИСЫВАЕТСЯ ([`MediaDto`]), но не скачивается: байты
//!   тянет отдельная подсистема кэша, а `MediaDto` — то, по чему UI решает,
//!   какую карточку рисовать.

use std::collections::HashMap;

use grammers_client::media::Media;
use grammers_client::message::Message;
use grammers_client::peer::{Dialog, Peer};
use grammers_session::types::{PeerAuth, PeerId, PeerKind, PeerRef};
use grammers_tl_types as tl;
use serde::{Deserialize, Serialize};

use crate::login::TgUserDto;

/// Строковое представление идентификатора диалога.
///
/// `PeerId` внутри уже хранится в Bot API-формате, поэтому достаточно
/// распечатать его. Сентинел «сам себе» (`PeerId::self_user`, у которого
/// настоящий id неизвестен) отдаётся как `"0"`: он не может быть валидным
/// dialog_id, и фронт трактует его как «диалог неизвестен».
pub fn dialog_id_string(id: PeerId) -> String {
    match id.bot_api_dialog_id() {
        Some(v) => v.to_string(),
        None => "0".to_string(),
    }
}

/// Обратное преобразование: строка из URL → [`PeerId`].
///
/// `None` ⇒ строка не число или число вне валидных диапазонов Bot API
/// (роут отвечает `404 unknown_dialog`).
pub fn parse_dialog_id(raw: &str) -> Option<PeerId> {
    let n: i64 = raw.trim().parse().ok()?;
    PeerId::from_bot_api_dialog_id(n)
}

/// `user | group | channel` для [`PeerId`] (по встроенному в него тегу).
pub fn kind_of(id: PeerId) -> &'static str {
    match id.kind() {
        PeerKind::User => "user",
        PeerKind::Chat => "group",
        PeerKind::Channel => "channel",
    }
}

/// `user | group | channel` для [`Peer`].
///
/// Отличается от [`kind_of`] на megagroup'ах: у них `PeerKind::Channel`, но
/// grammers уже разложил их в [`Peer::Group`] — для UI это именно группа.
pub fn peer_kind(peer: &Peer) -> &'static str {
    match peer {
        Peer::User(_) => "user",
        Peer::Group(_) => "group",
        Peer::Channel(_) => "channel",
    }
}

/// Заголовок диалога: полное имя пользователя, иначе title группы/канала.
///
/// Пустая строка вместо `None` — сознательно: у удалённых аккаунтов имени
/// нет вовсе, а UI обязан что-то нарисовать, поэтому подстановку заглушки
/// («deleted account» / id) делает фронт, одинаково для всех kind'ов.
pub fn peer_title(peer: &Peer) -> String {
    match peer {
        Peer::User(u) => {
            let full = u.full_name();
            if full.trim().is_empty() {
                u.username().unwrap_or_default().to_string()
            } else {
                full
            }
        }
        other => other.name().unwrap_or_default().to_string(),
    }
}

/// Пара флагов `(bot, contact)` для [`Peer`].
///
/// Оба флага осмысленны только для личных чатов: у группы/канала «бот» и
/// «контакт» не определены, и отдавать там `true` значило бы сломать
/// категорийный фильтр папок (папка «боты» показала бы группы). Контакт —
/// это `contact || mutual_contact`: односторонний контакт (я добавил, меня
/// нет) в официальном клиенте тоже считается контактом.
pub fn peer_bot_contact(peer: &Peer) -> (bool, bool) {
    match peer {
        Peer::User(u) => (u.is_bot(), u.contact() || u.mutual_contact()),
        _ => (false, false),
    }
}

/// Отправитель сообщения в проекции UI.
///
/// [`TgUserDto`] переиспользуется и для не-пользовательских отправителей
/// (пост от имени канала): `first_name` заполняется названием, `phone`
/// остаётся `None`. Отдельный тип не заводится — фронту нужны те же три
/// поля (id / @username / отображаемое имя), а лишний вариант в контракте
/// усложнил бы handlers.
pub fn sender_dto(peer: &Peer) -> TgUserDto {
    match peer {
        Peer::User(u) => TgUserDto::from_user(u),
        other => TgUserDto {
            id: dialog_id_string(other.id()),
            username: other.username().map(str::to_string),
            first_name: Some(peer_title(other)).filter(|s| !s.is_empty()),
            phone: None,
        },
    }
}

/// Диалог (чат) в проекции для UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogDto {
    /// Идентификатор диалога — строкой (см. инвариант №1).
    pub id: String,
    /// `user` | `group` | `channel`.
    pub kind: String,
    /// Отображаемое имя (имя человека или title группы/канала).
    pub title: String,
    /// `@username` без «@», если есть.
    pub username: Option<String>,
    /// Собеседник — бот. Всегда `false` для групп и каналов.
    ///
    /// Нужен фронту для категорийного фильтра папок: в Telegram бот
    /// попадает ТОЛЬКО в категорию «боты», даже если он в контактах, —
    /// без этого флага папка «боты» не отличалась бы от «контактов».
    pub bot: bool,
    /// Собеседник в списке контактов (`contact || mutual_contact`).
    /// Всегда `false` для групп, каналов и ботов-не-контактов.
    pub contact: bool,
    /// Непрочитанных сообщений по данным Telegram.
    pub unread_count: i32,
    /// Закреплён ли диалог (Telegram отдаёт закреплённые первыми).
    pub pinned: bool,
    /// Заглушены ли уведомления диалога (mute до будущего времени,
    /// «навсегда» или silent-режим).
    pub muted: bool,
    /// Лежит ли диалог в архиве (`folder_id == 1`).
    pub archived: bool,
    /// Сырой peer folder Telegram: `None`/`0` — основной список, `1` — архив.
    pub folder_id: Option<i32>,
    /// Последнее сообщение, если Telegram прислал его вместе с диалогом.
    pub last_message: Option<MessageDto>,
}

/// Сообщение в проекции для UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDto {
    /// Идентификатор сообщения внутри диалога (32-битный в Telegram).
    pub id: i32,
    /// Идентификатор диалога — строкой (см. инвариант №1).
    pub dialog_id: String,
    /// `true` ⇒ сообщение исходящее (написано нами).
    pub out: bool,
    /// Unix-время отправки, секунды UTC.
    pub date: i64,
    /// Кто написал. `None` для постов от имени канала и сервисных сообщений.
    pub sender: Option<TgUserDto>,
    /// Текст сообщения (для медиа — подпись; пусто, если её нет).
    pub text: String,
    /// Описание вложения, если оно есть (байты здесь не передаются).
    pub media: Option<MediaDto>,
    /// На какое сообщение это — ответ.
    ///
    /// Сырое число из reply-header'а, как его прислал Telegram. Поле
    /// СТАРШЕ [`MessageDto::reply_to`] и намеренно оставлено без правок:
    /// на него уже опираются фронт и тесты. Из-за этого на форумах оно
    /// расходится с `reply_to` — см. [`reply_to_from_header`].
    pub reply_to_id: Option<i32>,
    /// Что именно процитировано — то, из чего UI рисует «плашку ответа».
    ///
    /// `None` ⇒ сообщение ничему не отвечает. Отличается от
    /// [`MessageDto::reply_to_id`] тем, что несёт превью оригинала
    /// (автор/текст/вид медиа) и честно сообщает `unavailable`, когда
    /// оригинал достать не удалось.
    ///
    /// Два намеренных расхождения со старым полем: на форумах здесь нет
    /// ложной цитаты «на топик», а у сервисных сообщений цитата ЕСТЬ
    /// (grammers `reply_to_message_id()` читает только обычные) — см.
    /// [`reply_to_from_header`].
    pub reply_to: Option<ReplyToDto>,
    /// Unix-время последней правки; `Some` ⇒ UI рисует «(edited)».
    pub edit_date: Option<i64>,
    /// Клавиатура сообщения (кнопки бота), если она есть.
    ///
    /// `None` у обычных сообщений между людьми и у всех сервисных: поле
    /// `reply_markup` существует только у `message#…`, у `messageService`
    /// его нет в схеме вовсе.
    pub reply_markup: Option<ReplyMarkupDto>,
}

/// Оригинал, на который отвечает сообщение, в проекции для UI.
///
/// Всё, что нужно, чтобы нарисовать приглушённую строку-цитату над
/// текстом, НЕ делая отдельного запроса за каждым оригиналом.
///
/// Заполняется в два приёма:
/// 1. [`reply_to_from_header`] — синхронно, из самого сообщения: id
///    оригинала, процитированный фрагмент, а для ответов в чужой чат ещё
///    и автор с видом медиа (их Telegram кладёт прямо в header);
/// 2. [`enrich_reply_previews`] — из уже полученных оригиналов: автор,
///    превью текста, вид медиа, `out`.
///
/// Признак «второй приём ещё не делали» — [`ReplyToDto::out`] `== None`
/// при `message_id.is_some()`; на нём построен [`reply_needs_preview`].
/// Поэтому `out` НЕ должен заполняться первым приёмом, даже когда его
/// можно угадать.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyToDto {
    /// Id оригинала В ЭТОМ ЖЕ диалоге.
    ///
    /// `None` — оригинала в этом диалоге нет и адресовать его нечем:
    /// ответ на сторис либо ответ на сообщение ЧУЖОГО чата. Фронт по
    /// этому полю решает, кликабельна ли цитата (переход к оригиналу
    /// осмыслен только внутри открытого диалога): id чужого чата в
    /// нашем диалоге указывал бы на постороннее сообщение.
    pub message_id: Option<i32>,
    /// Отображаемое имя автора оригинала.
    ///
    /// `None` у постов от имени канала и у НАШИХ сообщений в личке
    /// (Telegram не присылает `from_id` там, где отправитель очевиден) —
    /// подпись в таком случае фронт выводит из [`ReplyToDto::out`].
    pub sender: Option<String>,
    /// Id автора оригинала строкой Bot API-формата (инвариант №1).
    pub sender_id: Option<String>,
    /// Превью текста оригинала: одна строка, не длиннее
    /// [`REPLY_PREVIEW_CHARS`] code point'ов (см. [`reply_preview_text`]).
    /// `None` у сообщений без текста (чистое медиа, сервисные).
    pub text: Option<String>,
    /// Вид вложения оригинала: `photo` | `video` | `document` |
    /// `sticker` | `voice` | `audio` | `other`; `None` — вложения нет.
    /// Словарь совпадает с [`MediaDto::kind`].
    pub media_kind: Option<String>,
    /// Процитированный фрагмент, если отвечающий выделил кусок текста.
    ///
    /// Живёт в header'е САМОГО ответа, а не в оригинале, поэтому
    /// переживает удаление оригинала и остаётся заполненным даже при
    /// `unavailable == true` — ровно как в официальных клиентах.
    /// Нормализуется тем же [`reply_preview_text`], что и `text`.
    pub quote: Option<String>,
    /// Оригинал — наше сообщение. `None` ⇒ ещё не выяснено.
    pub out: Option<bool>,
    /// Оригинал показать нечем: удалён, недоступен или это ответ на
    /// объект без сообщения (сторис). Фронт рисует «сообщение удалено».
    pub unavailable: bool,
}

/// Клавиатура сообщения в проекции для UI.
///
/// Один тип на все четыре TL-конструктора (`replyInlineMarkup`,
/// `replyKeyboardMarkup`, `replyKeyboardHide`, `replyKeyboardForceReply`):
/// фронту нужен один разбор, а различие — в поле [`ReplyMarkupDto::kind`].
/// Поля-подсказки (`resize`/`single_use`/`persistent`/`placeholder`) есть
/// только у reply-клавиатур; у inline-разметки они всегда «выключены»,
/// потому что Telegram их для неё не передаёт (инвариант №2 запрещает
/// пропускать ключи, поэтому отдаются нулевые значения, а не отсутствие).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMarkupDto {
    /// `inline` | `keyboard` | `hide` | `force_reply`.
    pub kind: String,
    /// Ряды кнопок. Пусты у `hide` и `force_reply` — там кнопок нет по схеме.
    pub rows: Vec<Vec<ReplyButtonDto>>,
    /// Подсказка «клавиатуру можно ужать по высоте» (только `keyboard`).
    pub resize: bool,
    /// Свернуть клавиатуру после первого нажатия (`keyboard`/`force_reply`).
    pub single_use: bool,
    /// Не сворачивать клавиатуру самостоятельно (только `keyboard`).
    pub persistent: bool,
    /// Текст-подсказка в поле ввода, пока клавиатура активна.
    pub placeholder: Option<String>,
}

/// Кнопка клавиатуры в проекции для UI.
///
/// `kind` — закрытый словарь контракта; всё, чего фронт не умеет нажимать
/// (включая конструкторы будущих layer'ов), приезжает как `other` С
/// СОХРАНЁННЫМ текстом: кнопка обязана отрисоваться неактивной, а не
/// исчезнуть — иначе ряд «поедет» и пользователь решит, что бот сломан.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyButtonDto {
    /// Надпись на кнопке (для reply-клавиатуры она же — отправляемый текст).
    pub text: String,
    /// `callback` | `url` | `switch_inline` | `switch_inline_current` |
    /// `text` | `request_phone` | `request_location` | `request_poll` |
    /// `web_view` | `buy` | `game` | `login_url` | `user_profile` | `other`.
    pub kind: String,
    /// Payload callback-кнопки в base64 (standard, с padding).
    ///
    /// Сырые байты произвольные (боты кладут туда protobuf, msgpack, что
    /// угодно) и в JSON как строка не проходят — отсюда base64. Фронт
    /// обращается с ней как с непрозрачным токеном и возвращает как есть.
    pub data: Option<String>,
    /// URL для `url` / `login_url` / `web_view` / `user_profile`.
    pub url: Option<String>,
    /// Готовый inline-запрос для `switch_inline*`.
    pub query: Option<String>,
    /// Кнопка требует подтверждения паролем (2FA) перед callback'ом.
    pub requires_password: bool,
}

/// Описание вложения — ровно то, по чему UI выбирает вид карточки.
///
/// Сами байты не передаются: их отдаёт отдельный роут медиа-кэша.
/// [`MediaDto::has_thumb`] говорит, есть ли у вложения превью, которое
/// можно нарисовать до полной загрузки.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDto {
    /// `photo` | `video` | `document` | `sticker` | `voice` | `audio` | `other`.
    pub kind: String,
    /// MIME-тип, если Telegram его прислал.
    pub mime: Option<String>,
    /// Размер файла в байтах.
    pub size: Option<u64>,
    /// Оригинальное имя файла (только у документов).
    pub file_name: Option<String>,
    /// Ширина в пикселях (фото/видео).
    pub width: Option<i32>,
    /// Высота в пикселях (фото/видео).
    pub height: Option<i32>,
    /// Длительность в секундах (видео/аудио/голосовые).
    pub duration_secs: Option<i32>,
    /// Есть ли у вложения превью-миниатюра.
    pub has_thumb: bool,
    /// Звуковая огибающая голосового: столбики `0..=255`, не длиннее
    /// `forge_voice::waveform::MAX_WAVEFORM`.
    ///
    /// # Откуда она берётся
    ///
    /// Из `DocumentAttributeAudio.waveform` — Telegram присылает её сам,
    /// упакованной по пять бит на отсчёт ([`forge_voice::waveform::from_telegram`]).
    /// Атрибут есть только у ГОЛОСОВЫХ; у обычного аудиофайла его нет, и тогда
    /// огибающую считает бэкенд по скачанному файлу
    /// ([`crate::voice::waveform_of`]), а пока не посчитал — поле пусто и фронт
    /// рисует детерминированную заглушку.
    ///
    /// Поле называется и заполняется так же, как `waveform` у VK и Bitrix:
    /// волну во всех трёх панелях рисует один и тот же код
    /// (`static/js/messenger/waveform-core.js`).
    #[serde(default)]
    pub waveform: Vec<i32>,
}

/// Флаги атрибутов документа, по которым определяется его вид.
///
/// Вынесены отдельно, чтобы классификация ([`document_kind`]) была чистой
/// функцией и тестировалась без сети и без TL-типов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DocFlags {
    /// Атрибут `DocumentAttributeAudio` с флагом `voice` — голосовое.
    pub voice: bool,
    /// Атрибут `DocumentAttributeAudio` без `voice` — музыка/подкаст.
    pub audio: bool,
    /// Атрибут `DocumentAttributeVideo` (в т.ч. круглые видео).
    pub video: bool,
    /// Атрибут `DocumentAttributeSticker`.
    pub sticker: bool,
    /// Атрибут `DocumentAttributeAnimated` — GIF/анимация.
    pub animated: bool,
}

/// Вид документа для UI.
///
/// Порядок проверок важен: атрибуты Telegram точнее MIME (например, у
/// голосового сообщения MIME `audio/ogg`, и без флага `voice` оно
/// выглядело бы как обычная музыка), поэтому MIME используется только как
/// запасной вариант.
pub fn document_kind(mime: Option<&str>, flags: DocFlags) -> &'static str {
    if flags.sticker {
        return "sticker";
    }
    if flags.voice {
        return "voice";
    }
    if flags.video {
        return "video";
    }
    if flags.audio {
        return "audio";
    }
    // Анимации (GIF) Telegram шлёт как video/mp4 без звука — для UI это
    // видео-карточка, а не документ.
    if flags.animated {
        return "video";
    }
    match mime {
        Some(m) if m.starts_with("video/") => "video",
        Some(m) if m.starts_with("audio/") => "audio",
        Some(m) if m.starts_with("image/") => "photo",
        _ => "document",
    }
}

/// Собирает [`DocFlags`] из сырых атрибутов документа.
fn doc_flags(attrs: &[tl::enums::DocumentAttribute]) -> DocFlags {
    let mut f = DocFlags::default();
    for attr in attrs {
        match attr {
            tl::enums::DocumentAttribute::Audio(a) => {
                if a.voice {
                    f.voice = true;
                } else {
                    f.audio = true;
                }
            }
            tl::enums::DocumentAttribute::Video(_) => f.video = true,
            tl::enums::DocumentAttribute::Sticker(_)
            | tl::enums::DocumentAttribute::CustomEmoji(_) => f.sticker = true,
            tl::enums::DocumentAttribute::Animated => f.animated = true,
            _ => {}
        }
    }
    f
}

/// Огибающая из атрибутов документа, приведённая к общей шкале.
///
/// `DocumentAttributeAudio.waveform` — единственное место, где Telegram отдаёт
/// НАСТОЯЩУЮ форму записи, и отдаёт он её бесплатно, вместе с сообщением.
/// Поэтому атрибут всегда побеждает расчёт на бэке: тот стоит запуска ffmpeg и
/// требует, чтобы файл уже был скачан.
///
/// Атрибутов `Audio` в документе бывает два (несогласованные данные: музыка и
/// голос разом — см. `doc_flags_handles_custom_emoji_and_mixed_attributes`),
/// поэтому берётся ПЕРВЫЙ НЕПУСТОЙ, а не первый попавшийся: пустое поле у
/// одного из атрибутов не должно скрыть заполненное у другого.
fn audio_waveform(attrs: &[tl::enums::DocumentAttribute]) -> Vec<i32> {
    for attr in attrs {
        if let tl::enums::DocumentAttribute::Audio(a) = attr {
            if let Some(packed) = a.waveform.as_ref().filter(|w| !w.is_empty()) {
                let w = forge_voice::waveform::from_telegram(packed);
                if !w.is_empty() {
                    return w;
                }
            }
        }
    }
    Vec::new()
}

/// Размеры одного превью, если они вообще известны.
///
/// `Stripped`/`Path` пропускаются: это blur-заглушки и векторные контуры без
/// реальных пиксельных размеров.
fn photo_size_dimensions(size: &grammers_client::media::PhotoSize) -> Option<(i32, i32)> {
    use grammers_client::media::PhotoSize as P;
    match size {
        P::Size(s) => Some((s.width, s.height)),
        P::Cached(s) => Some((s.width, s.height)),
        P::Progressive(s) => Some((s.width, s.height)),
        P::Empty(_) | P::Stripped(_) | P::Path(_) => None,
    }
}

/// Самое большое разрешение из набора (по площади).
fn largest_dimensions(dims: impl Iterator<Item = (i32, i32)>) -> Option<(i32, i32)> {
    dims.max_by_key(|(w, h)| i64::from(*w) * i64::from(*h))
}

/// Наибольшее известное разрешение среди превью/размеров фото.
///
/// У фото нет собственных width/height: Telegram отдаёт набор размеров, и
/// «размер фото» — это самый большой из них.
fn largest_resolution(sizes: &[grammers_client::media::PhotoSize]) -> Option<(i32, i32)> {
    largest_dimensions(sizes.iter().filter_map(photo_size_dimensions))
}

/// Проекция вложения в [`MediaDto`].
///
/// `None` для [`Media::WebPage`]: превью ссылки — не вложение, оно уже
/// содержится в тексте сообщения, и рисовать под ним карточку файла
/// неправильно. Остальные виды (в т.ч. опросы, гео, контакты) описываются
/// как `other`, чтобы UI показал честное «вложение, которое я не умею
/// рисовать», а не проглотил сообщение целиком.
pub fn media_dto(media: &Media) -> Option<MediaDto> {
    let dto = match media {
        Media::Photo(p) => {
            let thumbs = p.thumbs();
            let (w, h) = largest_resolution(&thumbs).unzip();
            MediaDto {
                kind: "photo".into(),
                mime: Some("image/jpeg".into()),
                size: p.size().map(|s| s as u64),
                file_name: None,
                width: w,
                height: h,
                duration_secs: None,
                has_thumb: !thumbs.is_empty(),
                waveform: Vec::new(),
            }
        }
        Media::Document(d) => {
            let attrs = match d.raw.document.as_ref() {
                Some(tl::enums::Document::Document(raw)) => raw.attributes.as_slice(),
                _ => &[],
            };
            let flags = doc_flags(attrs);
            let mime = d.mime_type().map(str::to_string);
            let (w, h) = d.resolution().unzip();
            MediaDto {
                kind: document_kind(mime.as_deref(), flags).into(),
                mime,
                size: d.size().map(|s| s as u64),
                file_name: d.name().map(str::to_string),
                width: w,
                height: h,
                // Дробные секунды Telegram использует для видео; UI
                // показывает целые, поэтому округляем вверх — «0:00» у
                // полусекундного кружка выглядело бы поломкой.
                duration_secs: d.duration().map(|s| s.ceil() as i32),
                has_thumb: !d.thumbs().is_empty(),
                waveform: audio_waveform(attrs),
            }
        }
        Media::Sticker(s) => {
            let (w, h) = s.document.resolution().unzip();
            MediaDto {
                kind: "sticker".into(),
                mime: s.document.mime_type().map(str::to_string),
                size: s.document.size().map(|v| v as u64),
                file_name: None,
                width: w,
                height: h,
                duration_secs: None,
                has_thumb: !s.document.thumbs().is_empty(),
                waveform: Vec::new(),
            }
        }
        Media::WebPage(_) => return None,
        _ => MediaDto {
            kind: "other".into(),
            ..MediaDto::default()
        },
    };
    Some(dto)
}

// ----------------------------------------------------------------------
// base64 для callback-payload'а
//
// Почему свой кодер, а не крейт: нужны ровно две операции над короткими
// (≤64 байта по схеме Telegram) блобами, без потоков, SIMD и alphabet'ов.
// `base64 0.22` уже есть в Cargo.lock от tmux-web, то есть выигрыш «крейт
// уже собран» тут нулевой, а плата — лишняя строка в манифесте плагина и
// ещё одна зависимость, которую придётся вести при обновлениях. Двадцать
// строк ниже полностью покрыты тестами (в т.ч. не-UTF8 байтами), поэтому
// цена сопровождения ниже, чем у зависимости.
// ----------------------------------------------------------------------

/// Стандартный алфавит base64 (RFC 4648, §4) — тот же, что у Bot API.
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Кодирует байты в base64 (standard alphabet, с padding).
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(BASE64_ALPHABET[(n >> 12) as usize & 63] as char);
        // Хвост короче трёх байт добивается '=': длина исходных данных
        // обязана восстанавливаться однозначно (Telegram сверяет payload
        // побайтово, лишний нулевой байт сделал бы callback невалидным).
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Декодирует base64 обратно в байты; `None` ⇒ строка не base64.
///
/// На входе намеренно терпимее, чем на выходе: принимаются и URL-safe
/// символы (`-`/`_`), и строка без padding, и переводы строк. Причина —
/// payload возвращается фронтом через JSON-тело и мог быть пропущен через
/// URL-кодирование или перенос строки; ужесточать здесь значит отвечать
/// `400` на данные, которые мы сами же и выдали. Отвергается только то,
/// что восстановить нельзя: чужие символы и обрубленная группа (остаток в
/// один символ несёт меньше байта).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for byte in s.bytes() {
        let v: u8 = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            // Padding и пробельные символы не несут бит: длина считается
            // по накопленным битам, поэтому их достаточно пропустить.
            b'=' | b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = ((acc << 6) | u32::from(v)) & 0x3_FFFF;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits == 6 {
        return None; // обрубок: одного символа на байт не хватает
    }
    Some(out)
}

/// Проекция одной кнопки клавиатуры в [`ReplyButtonDto`].
///
/// Матч намеренно заканчивается `_ =>`: в схеме Telegram уже 18
/// конструкторов кнопок, и каждый layer добавляет новые. Падать сборкой
/// (или, хуже, терять кнопку) из-за незнакомого варианта нельзя — он
/// уезжает в `other` с сохранённым текстом.
pub fn reply_button_dto(button: &tl::enums::KeyboardButton) -> ReplyButtonDto {
    use tl::enums::KeyboardButton as B;
    let mut dto = ReplyButtonDto {
        // `KeyboardButton::text()` знает все варианты схемы — надёжнее,
        // чем повторять извлечение текста в каждой ветке.
        text: button.text(),
        kind: "other".into(),
        ..ReplyButtonDto::default()
    };
    match button {
        B::Button(_) => dto.kind = "text".into(),
        B::Callback(b) => {
            dto.kind = "callback".into();
            dto.data = Some(base64_encode(&b.data));
            dto.requires_password = b.requires_password;
        }
        B::Url(b) => {
            dto.kind = "url".into();
            dto.url = Some(b.url.clone());
        }
        B::UrlAuth(b) => {
            dto.kind = "login_url".into();
            dto.url = Some(b.url.clone());
        }
        B::InputKeyboardButtonUrlAuth(b) => {
            dto.kind = "login_url".into();
            dto.url = Some(b.url.clone());
        }
        B::WebView(b) => {
            dto.kind = "web_view".into();
            dto.url = Some(b.url.clone());
        }
        B::SimpleWebView(b) => {
            dto.kind = "web_view".into();
            dto.url = Some(b.url.clone());
        }
        B::SwitchInline(b) => {
            // same_peer — «подставить запрос в текущий чат», иначе клиент
            // сначала спрашивает, в какой чат переключиться. Для UI это
            // два разных действия, поэтому и kind'а два.
            dto.kind = if b.same_peer {
                "switch_inline_current".into()
            } else {
                "switch_inline".into()
            };
            dto.query = Some(b.query.clone());
        }
        B::RequestPhone(_) => dto.kind = "request_phone".into(),
        B::RequestGeoLocation(_) => dto.kind = "request_location".into(),
        B::RequestPoll(_) => dto.kind = "request_poll".into(),
        B::Buy(_) => dto.kind = "buy".into(),
        B::Game(_) => dto.kind = "game".into(),
        B::UserProfile(b) => {
            dto.kind = "user_profile".into();
            // У кнопки нет URL, есть только user_id. Отдаём канонический
            // deep-link Telegram: фронту не нужен отдельный вид действия,
            // а ссылку он всё равно открывает через своё окно.
            dto.url = Some(format!("tg://user?id={}", b.user_id));
        }
        B::InputKeyboardButtonUserProfile(_) => {
            // Исходящая форма (её отправляет бот): вместо id там InputUser,
            // из которого deep-link не собрать ⇒ url остаётся null.
            dto.kind = "user_profile".into();
        }
        // Сюда попадают keyboardButtonRequestPeer, keyboardButtonCopy и всё,
        // что появится в следующих layer'ах.
        _ => {}
    }
    dto
}

/// Ряды кнопок TL → ряды DTO (пустые ряды сохраняются как пустые).
fn reply_markup_rows(rows: &[tl::enums::KeyboardButtonRow]) -> Vec<Vec<ReplyButtonDto>> {
    rows.iter()
        .map(|row| {
            let tl::enums::KeyboardButtonRow::Row(r) = row;
            r.buttons.iter().map(reply_button_dto).collect()
        })
        .collect()
}

/// Проекция клавиатуры сообщения в [`ReplyMarkupDto`].
///
/// `selective` (показывать клавиатуру только упомянутым) сознательно
/// отбрасывается: в личном чате с ботом он всегда истинен, а в группах наш
/// UI всё равно не знает, кого упомянули в тексте, — фильтровать по нему
/// значило бы прятать рабочие кнопки.
pub fn reply_markup_dto(markup: &tl::enums::ReplyMarkup) -> ReplyMarkupDto {
    use tl::enums::ReplyMarkup as M;
    match markup {
        M::ReplyInlineMarkup(m) => ReplyMarkupDto {
            kind: "inline".into(),
            rows: reply_markup_rows(&m.rows),
            ..ReplyMarkupDto::default()
        },
        M::ReplyKeyboardMarkup(m) => ReplyMarkupDto {
            kind: "keyboard".into(),
            rows: reply_markup_rows(&m.rows),
            resize: m.resize,
            single_use: m.single_use,
            persistent: m.persistent,
            placeholder: m.placeholder.clone(),
        },
        // «Убрать клавиатуру»: кнопок в конструкторе нет по схеме.
        M::ReplyKeyboardHide(_) => ReplyMarkupDto {
            kind: "hide".into(),
            ..ReplyMarkupDto::default()
        },
        // Не клавиатура, а инструкция UI «ответь на это сообщение».
        M::ReplyKeyboardForceReply(m) => ReplyMarkupDto {
            kind: "force_reply".into(),
            single_use: m.single_use,
            placeholder: m.placeholder.clone(),
            ..ReplyMarkupDto::default()
        },
    }
}

// ----------------------------------------------------------------------
// Цитаты (reply_to): разбор header'а и обогащение превью
// ----------------------------------------------------------------------

/// Потолок превью текста оригинала в code point'ах.
///
/// Режем по code point'ам, а не по байтам: `"🦀"` — четыре байта, и
/// побайтовая обрезка разорвала бы его посреди символа. Значение
/// подобрано под одну-две строки цитаты в UI.
pub const REPLY_PREVIEW_CHARS: usize = 140;

/// Сколько оригиналов можно запросить одним `messages.getMessages`.
///
/// Ограничение Telegram; grammers `get_messages_by_id` НЕ режет список
/// сам, поэтому нарезка — забота вызывающего (см. [`reply_ids_to_fetch`]).
pub const REPLY_FETCH_BATCH: usize = 100;

/// Превью текста для цитаты: одна строка, не длиннее
/// [`REPLY_PREVIEW_CHARS`] code point'ов.
///
/// Переводы строк и табы схлопываются в один пробел — цитата рисуется
/// одной строкой, и «лесенка» из исходного сообщения ломала бы вёрстку
/// ленты. Обрезанный текст заканчивается многоточием, которое ВХОДИТ в
/// лимит: длина результата никогда не превышает [`REPLY_PREVIEW_CHARS`].
/// Пустой (или состоящий из одних пробелов) текст даёт `None` — фронт
/// покажет только пометку медиа.
pub fn reply_preview_text(text: &str) -> Option<String> {
    let mut flat = String::with_capacity(text.len().min(REPLY_PREVIEW_CHARS * 4));
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            // Пробелы копятся, но пишутся только перед следующим
            // непробельным символом — так пропадают и хвостовые.
            space = !flat.is_empty();
            continue;
        }
        if space {
            flat.push(' ');
            space = false;
        }
        flat.push(ch);
        // Считаем на лету: +1 символ сверх лимита уже доказывает, что
        // резать придётся, и дальше можно не разбирать весь текст.
        if flat.chars().count() > REPLY_PREVIEW_CHARS {
            let cut: String = flat.chars().take(REPLY_PREVIEW_CHARS - 1).collect();
            return Some(cut + "…");
        }
    }
    Some(flat).filter(|s| !s.is_empty())
}

/// Автор оригинала из `reply_from` (ответ в чужой чат): `(имя, id)`.
///
/// Telegram кладёт сюда `MessageFwdHeader` именно затем, чтобы клиент
/// нарисовал цитату без похода в тот чат. Имя может быть только
/// строкой (`from_name` у скрытых аккаунтов, `post_author` у подписи
/// поста), id — только peer'ом; бывает и то, и другое, и ничего.
fn reply_from_parts(fwd: Option<&tl::enums::MessageFwdHeader>) -> (Option<String>, Option<String>) {
    let Some(tl::enums::MessageFwdHeader::Header(f)) = fwd else {
        return (None, None);
    };
    let name = f
        .from_name
        .clone()
        .or_else(|| f.post_author.clone())
        .filter(|s| !s.trim().is_empty());
    let id = f
        .from_id
        .as_ref()
        .map(|p| dialog_id_string(PeerId::from(p)));
    (name, id)
}

/// Разбор reply-header'а сообщения в [`ReplyToDto`] — синхронно, без сети.
///
/// `None` ⇒ сообщение ничему не отвечает.
///
/// Три случая, которые обязаны разбираться по-разному:
///
/// - **Форум-топик.** Чтобы отправить сообщение в топик, клиент кладёт id
///   топика в `reply_to_msg_id` ([док](https://core.telegram.org/api/forum)),
///   поэтому у ОБЫЧНОГО сообщения форума header есть, а ответа нет.
///   Отличаем по `forum_topic && reply_to_top_id.is_none()` — иначе под
///   каждой строкой супергруппы-форума висела бы цитата «сообщение
///   топика». Старое поле [`MessageDto::reply_to_id`] этой проверки не
///   делает (и не будет — оно часть уже работающего контракта), так что
///   на форумах два поля СОЗНАТЕЛЬНО расходятся.
/// - **Ответ в чужой чат** (`reply_to_peer_id`). Оригинал лежит в другом
///   диалоге: `get_messages_by_id` текущего диалога его не найдёт
///   никогда, зато Telegram сам приложил автора (`reply_from`) и медиа
///   (`reply_media`). Заполняем их локально, а `message_id` оставляем
///   `None` — в нашем диалоге под этим номером лежит ПОСТОРОННЕЕ
///   сообщение, и переход по цитате увёл бы не туда.
/// - **Ответ на сторис.** Сообщения-оригинала не существует вовсе
///   (`messageReplyStoryHeader` несёт только `story_id`), достать текст
///   неоткуда ⇒ `unavailable: true` с пустыми полями.
pub fn reply_to_from_header(header: &tl::enums::MessageReplyHeader) -> Option<ReplyToDto> {
    let h = match header {
        tl::enums::MessageReplyHeader::Header(h) => h,
        tl::enums::MessageReplyHeader::MessageReplyStoryHeader(_) => {
            return Some(ReplyToDto {
                unavailable: true,
                ..ReplyToDto::default()
            });
        }
    };
    if h.forum_topic && h.reply_to_top_id.is_none() {
        return None;
    }
    // Цитата фрагмента приходит вместе с ответом, а не с оригиналом.
    let quote = h
        .quote_text
        .clone()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| reply_preview_text(&s));
    if h.reply_to_peer_id.is_some() {
        let (sender, sender_id) = reply_from_parts(h.reply_from.as_ref());
        let media_kind = h
            .reply_media
            .clone()
            .and_then(Media::from_raw)
            .as_ref()
            .and_then(media_dto)
            .map(|m| m.kind);
        let known = sender.is_some() || sender_id.is_some() || media_kind.is_some();
        return Some(ReplyToDto {
            message_id: None,
            sender,
            sender_id,
            text: None, // текста чужого чата Telegram не присылает
            media_kind,
            quote,
            out: None,
            unavailable: !known,
        });
    }
    Some(ReplyToDto {
        message_id: Some(h.reply_to_msg_id?),
        quote,
        ..ReplyToDto::default()
    })
}

/// Нужно ли ещё достать превью оригинала для этой цитаты.
///
/// `true` ⇒ id оригинала известен, но автор/текст/медиа не заполнены.
/// Признак — `out == None`: его выставляет ТОЛЬКО обогащение, поэтому
/// повторный проход по уже обогащённой странице ничего не запрашивает.
pub fn reply_needs_preview(reply: &ReplyToDto) -> bool {
    reply.message_id.is_some() && !reply.unavailable && reply.out.is_none()
}

/// Id оригиналов, которых на странице нет и которые надо добрать
/// батчем: без повторов, в порядке первого упоминания.
///
/// Оригинал часто лежит на той же странице (ответ соседу) — такие id
/// отсеиваются, иначе каждая страница тянула бы за собой лишний RPC.
/// Результат нарезайте по [`REPLY_FETCH_BATCH`].
pub fn reply_ids_to_fetch(messages: &[MessageDto]) -> Vec<i32> {
    let present: std::collections::HashSet<i32> = messages.iter().map(|m| m.id).collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for msg in messages {
        let Some(reply) = msg.reply_to.as_ref() else {
            continue;
        };
        if !reply_needs_preview(reply) {
            continue;
        }
        let Some(id) = reply.message_id else {
            continue;
        };
        if present.contains(&id) || !seen.insert(id) {
            continue;
        }
        out.push(id);
    }
    out
}

/// Поля превью, вынутые из оригинала (внутренний индекс обогащения).
#[derive(Debug, Clone, Default)]
struct PreviewParts {
    sender: Option<String>,
    sender_id: Option<String>,
    text: Option<String>,
    media_kind: Option<String>,
    out: bool,
}

/// Что показать вместо оригинала: имя автора, id, превью, вид медиа.
fn preview_parts(msg: &MessageDto) -> PreviewParts {
    PreviewParts {
        // Имя, а не username: цитата повторяет подпись строки в ленте.
        sender: msg.sender.as_ref().and_then(|u| {
            u.first_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| u.username.clone())
        }),
        sender_id: msg.sender.as_ref().map(|u| u.id.clone()),
        text: reply_preview_text(&msg.text),
        media_kind: msg.media.as_ref().map(|m| m.kind.clone()),
        out: msg.out,
    }
}

/// Подставляет превью одного оригинала (или помечает недоступность).
fn apply_preview(reply: &mut ReplyToDto, parts: Option<&PreviewParts>) {
    match parts {
        Some(p) => {
            reply.sender = p.sender.clone();
            reply.sender_id = p.sender_id.clone();
            reply.text = p.text.clone();
            reply.media_kind = p.media_kind.clone();
            reply.out = Some(p.out);
            reply.unavailable = false;
        }
        // Оригинал не достался — удалён или недоступен. `quote` при этом
        // НЕ стираем: процитированный фрагмент пришёл с самим ответом и
        // остаётся единственным, что можно показать.
        None => reply.unavailable = true,
    }
}

/// Обогащает цитаты страницы истории — чистая функция, без сети.
///
/// `originals` — оригиналы, добранные батчем по [`reply_ids_to_fetch`]
/// (ключ — запрошенный id). Оригиналы, уже лежащие на самой странице,
/// добавлять не нужно: они берутся из `messages`.
///
/// Финальный проход: всё, чего нет ни на странице, ни в `originals`,
/// помечается `unavailable: true`. Поэтому вызывать её следует ПОСЛЕ
/// батча, а не вместо него.
pub fn enrich_reply_previews(messages: &mut [MessageDto], originals: &HashMap<i32, MessageDto>) {
    // Индекс строится до мутации: иначе не одолжить `messages` дважды.
    let mut index: HashMap<i32, PreviewParts> =
        messages.iter().map(|m| (m.id, preview_parts(m))).collect();
    for (id, original) in originals {
        index.insert(*id, preview_parts(original));
    }
    for msg in messages.iter_mut() {
        let Some(reply) = msg.reply_to.as_mut() else {
            continue;
        };
        if !reply_needs_preview(reply) {
            continue;
        }
        let id = reply.message_id.expect("reply_needs_preview проверил id");
        apply_preview(reply, index.get(&id));
    }
}

/// [`enrich_reply_previews`] для одиночного сообщения.
///
/// `original` — оригинал, если его удалось достать; `None` ⇒
/// `unavailable: true`. Для роута «одно сообщение по id» и для ответа на
/// собственную отправку, где батчить нечего.
pub fn enrich_reply_preview(message: &mut MessageDto, original: Option<&MessageDto>) {
    let Some(reply) = message.reply_to.as_mut() else {
        return;
    };
    if !reply_needs_preview(reply) {
        return;
    }
    // Чужой оригинал (id не совпал) превью не даёт — иначе цитата
    // показала бы посторонний текст.
    let parts = original
        .filter(|o| Some(o.id) == reply.message_id)
        .map(preview_parts);
    apply_preview(reply, parts.as_ref());
}

/// Существует ли сообщение на самом деле.
///
/// `messages.getMessages` отдаёт на удалённый id не пустоту, а
/// `messageEmpty`, и grammers оборачивает его в обычный `Message`:
/// `Option::is_some` тут ничего не доказывает, а `text()` даст `""` и
/// `date()` — эпоху. Без этой проверки вместо «сообщение удалено»
/// нарисуется пустая цитата.
pub fn message_exists(msg: &Message) -> bool {
    matches!(
        msg.raw,
        tl::enums::Message::Message(_) | tl::enums::Message::Service(_)
    )
}

/// Индекс оригиналов для [`enrich_reply_previews`] из ответа
/// `get_messages_by_id` (порядок и длина там совпадают с запросом).
///
/// Дыры (`None`) и `messageEmpty` (см. [`message_exists`]) отсеиваются —
/// именно они и означают «оригинал недоступен».
pub fn reply_originals_index(fetched: &[Option<Message>]) -> HashMap<i32, MessageDto> {
    fetched
        .iter()
        .flatten()
        .filter(|m| message_exists(m))
        .map(|m| (m.id(), from_grammers_message(m)))
        .collect()
}

/// Проекция сообщения grammers в [`MessageDto`].
///
/// Сетевых вызовов не делает: всё берётся из уже полученного объекта,
/// поэтому маппер безопасно вызывать и в hot-path'е updates-воркера.
/// Цитата ([`MessageDto::reply_to`]) заполняется только тем, что есть в
/// самом сообщении; превью оригинала добивает [`enrich_reply_previews`].
pub fn from_grammers_message(msg: &Message) -> MessageDto {
    MessageDto {
        id: msg.id(),
        dialog_id: dialog_id_string(msg.peer_id()),
        out: msg.outgoing(),
        date: msg.date().timestamp(),
        sender: msg.sender().map(sender_dto),
        text: msg.text().to_string(),
        media: msg.media().as_ref().and_then(media_dto),
        reply_to_id: msg.reply_to_message_id(),
        reply_to: msg.reply_header().as_ref().and_then(reply_to_from_header),
        edit_date: msg.edit_date().map(|d| d.timestamp()),
        reply_markup: msg.reply_markup().as_ref().map(reply_markup_dto),
    }
}

/// Текущее unix-время, секунды UTC.
///
/// Часы «до эпохи» (сбитое системное время) дают 0 — тогда любые
/// mute-настройки с будущим `mute_until` честно считаются заглушёнными.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Заглушён ли диалог по настройкам уведомлений на момент `now`.
///
/// Правила Telegram:
/// - `silent == Some(true)` ⇒ заглушён независимо от `mute_until`;
/// - `mute_until > now` ⇒ заглушён; «навсегда» Telegram кодирует очень
///   большим значением (`i32::MAX`), поэтому сравнение идёт в `i64` —
///   переполнения нет по построению;
/// - `mute_until` в прошлом, `0` или отсутствует ⇒ не заглушён.
pub fn notify_muted(settings: &tl::types::PeerNotifySettings, now: i64) -> bool {
    if settings.silent == Some(true) {
        return true;
    }
    match settings.mute_until {
        Some(until) => i64::from(until) > now,
        None => false,
    }
}

/// Глобальные дефолты уведомлений по трём категориям Telegram.
///
/// # Зачем
///
/// Telegram НЕ дублирует настройки в каждый диалог: если пользователь
/// заглушил «все личные чаты» (или «все каналы») с телефона, у самих
/// диалогов `PeerNotifySettings` приезжает пустым, и решение принимается
/// по дефолту категории (`account.getNotifySettings` с
/// `InputNotifyUsers` / `InputNotifyChats` / `InputNotifyBroadcasts`).
/// Без этого дефолта такие диалоги считались бы незаглушёнными — ровно то,
/// из-за чего тосты приходят от чатов, которые пользователь молчал.
///
/// Значения — уже РАЗРЕШЁННЫЕ на момент `now` булевы «заглушено», а не
/// сырые `mute_until`: срок глобального mute проверяется один раз при
/// загрузке дефолтов, а не в каждом диалоге.
///
/// Хранится в состоянии плагина и обновляется по `updateNotifySettings`
/// с категорийным `NotifyPeer` (`NotifyUsers`/`NotifyChats`/`NotifyBroadcasts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotifyDefaults {
    /// Дефолт для личных чатов — включая ботов и Saved Messages.
    pub users: bool,
    /// Дефолт для групп — И малых, И мегагрупп (у Telegram это одна категория).
    pub chats: bool,
    /// Дефолт для broadcast-каналов.
    pub broadcasts: bool,
}

impl NotifyDefaults {
    /// Собирает дефолты из трёх ответов `account.getNotifySettings`.
    ///
    /// Каждый разрешается обычным [`notify_muted`]: у глобальных настроек
    /// наследовать уже не от кого, поэтому пустые настройки означают «не
    /// заглушено».
    pub fn from_settings(
        users: &tl::types::PeerNotifySettings,
        chats: &tl::types::PeerNotifySettings,
        broadcasts: &tl::types::PeerNotifySettings,
        now: i64,
    ) -> Self {
        Self {
            users: notify_muted(users, now),
            chats: notify_muted(chats, now),
            broadcasts: notify_muted(broadcasts, now),
        }
    }

    /// Дефолт для `kind` из [`DialogDto::kind`] (`user`/`group`/`channel`).
    ///
    /// Неизвестный kind даёт `false`: молчать по ошибке хуже, чем показать
    /// лишнее уведомление — «пропавшее» сообщение пользователь не увидит
    /// вовсе, а лишнее он хотя бы заметит.
    pub fn for_kind(&self, kind: &str) -> bool {
        match kind {
            "user" => self.users,
            "group" => self.chats,
            "channel" => self.broadcasts,
            _ => false,
        }
    }

    /// Дефолт по одному лишь [`PeerId`].
    ///
    /// ВНИМАНИЕ: по тегу id мегагруппа неотличима от broadcast-канала (обе
    /// `PeerKind::Channel`), поэтому мегагруппа получит дефолт каналов.
    /// Там, где известен настоящий kind (индекс peer'ов, клиентский
    /// [`Peer`]), используйте [`NotifyDefaults::for_kind`].
    pub fn for_peer_id(&self, id: PeerId) -> bool {
        self.for_kind(kind_of(id))
    }
}

/// [`notify_muted`] с наследованием глобального дефолта категории.
///
/// Приоритеты (порядок обязателен):
/// 1. явный `silent == Some(true)` ⇒ заглушён;
/// 2. явный `mute_until > now` ⇒ заглушён;
/// 3. явный `mute_until` в прошлом или `0` ⇒ НЕ заглушён — точечный unmute
///    перебивает даже «заглушить все чаты» (иначе диалог, размьюченный
///    вручную, продолжал бы молчать);
/// 4. настройки пусты целиком (`silent` и `mute_until` — оба `None`) ⇒
///    берётся `default_muted`.
///
/// Проверка «пусто целиком» намеренно смотрит на ОБА поля: сервер шлёт
/// объект с выставленным `silent` и унаследованным `mute_until`, и такой
/// peer по контракту считается настроенным явно.
pub fn notify_muted_with_default(
    settings: &tl::types::PeerNotifySettings,
    now: i64,
    default_muted: bool,
) -> bool {
    if settings.silent.is_none() && settings.mute_until.is_none() {
        return default_muted;
    }
    notify_muted(settings, now)
}

/// [`notify_muted`] для сырого `tl::enums::Dialog`.
///
/// У `Dialog::Folder` (строка «Архив» в основном списке) настроек
/// уведомлений нет — он никогда не «заглушён».
pub fn dialog_muted(raw: &tl::enums::Dialog, now: i64) -> bool {
    match raw {
        tl::enums::Dialog::Dialog(d) => {
            let tl::enums::PeerNotifySettings::Settings(s) = &d.notify_settings;
            notify_muted(s, now)
        }
        tl::enums::Dialog::Folder(_) => false,
    }
}

/// [`dialog_muted`] с наследованием глобальных дефолтов.
///
/// Категория берётся по тегу `PeerId` — см. оговорку про мегагруппы в
/// [`NotifyDefaults::for_peer_id`]. Когда настоящий kind известен (есть
/// индекс peer'ов), зовите [`dialog_muted_with_defaults_for_kind`].
pub fn dialog_muted_with_defaults(
    raw: &tl::enums::Dialog,
    now: i64,
    defaults: NotifyDefaults,
) -> bool {
    dialog_muted_with_defaults_for_kind(raw, now, defaults, kind_of(raw_dialog_peer(raw)))
}

/// [`dialog_muted_with_defaults`] с явно заданной категорией диалога.
pub fn dialog_muted_with_defaults_for_kind(
    raw: &tl::enums::Dialog,
    now: i64,
    defaults: NotifyDefaults,
    kind: &str,
) -> bool {
    match raw {
        tl::enums::Dialog::Dialog(d) => {
            let tl::enums::PeerNotifySettings::Settings(s) = &d.notify_settings;
            notify_muted_with_default(s, now, defaults.for_kind(kind))
        }
        tl::enums::Dialog::Folder(_) => false,
    }
}

/// `folder_id` сырого диалога (`None` у `Dialog::Folder`).
pub fn dialog_folder_id(raw: &tl::enums::Dialog) -> Option<i32> {
    match raw {
        tl::enums::Dialog::Dialog(d) => d.folder_id,
        tl::enums::Dialog::Folder(_) => None,
    }
}

/// Лежит ли диалог в архиве. У Telegram ровно две peer-папки: `0` —
/// основной список (и отсутствие поля значит то же), `1` — архив.
pub fn is_archived(folder_id: Option<i32>) -> bool {
    folder_id == Some(1)
}

/// Служебная строка «Архив» (`dialogFolder`), а не диалог.
///
/// `messages.getDialogs` подмешивает её в ОСНОВНОЙ список — это та самая
/// строка «Archived chats» в шапке чатов официальных клиентов. Диалогом она
/// не является, и её `peer` — не «папка», а peer ВЕРХНЕГО СООБЩЕНИЯ папки,
/// то есть настоящий чат, лежащий в архиве.
///
/// Поэтому строку обязан отбрасывать КАЖДЫЙ, кто строит из сырого ответа
/// список диалогов: пропущенная дальше, она даёт [`DialogDto`] с чужим
/// `id`, `unread_count = 0` и — главное — `archived = false` / `muted =
/// false` (у `dialogFolder` нет ни `folder_id`, ни `notify_settings`).
/// Такой DTO не только рисуется во фронте призрачным дубликатом архивного
/// чата в `~/inbox`, но и ЗАТИРАЕТ в серверном кэше флагов правду о нём
/// ([`crate::state::TelegramState::remember_dialogs`]) — после чего
/// заглушённый архивный чат снова начинает «звенеть» тостом.
pub fn is_folder_row(raw: &tl::enums::Dialog) -> bool {
    matches!(raw, tl::enums::Dialog::Folder(_))
}

/// [`PeerId`] сырого диалога.
pub fn raw_dialog_peer(raw: &tl::enums::Dialog) -> PeerId {
    match raw {
        tl::enums::Dialog::Dialog(d) => PeerId::from(&d.peer),
        tl::enums::Dialog::Folder(d) => PeerId::from(&d.peer),
    }
}

/// [`PeerId`] сырого сообщения (`None` у `MessageEmpty` без peer'а).
fn raw_message_peer(msg: &tl::enums::Message) -> Option<PeerId> {
    match msg {
        tl::enums::Message::Message(m) => Some(PeerId::from(&m.peer_id)),
        tl::enums::Message::Service(m) => Some(PeerId::from(&m.peer_id)),
        tl::enums::Message::Empty(m) => m.peer_id.as_ref().map(PeerId::from),
    }
}

/// Ищет верхнее сообщение диалога в общем списке сообщений сырого ответа
/// `messages.getDialogs`: совпасть должны и id, и диалог (id сообщений
/// уникальны только внутри канала, между диалогами они пересекаются).
pub fn find_top_message<'a>(
    raw: &tl::enums::Dialog,
    messages: &'a [tl::enums::Message],
) -> Option<&'a tl::enums::Message> {
    let top = match raw {
        tl::enums::Dialog::Dialog(d) => d.top_message,
        tl::enums::Dialog::Folder(d) => d.top_message,
    };
    if top <= 0 {
        return None; // пустой диалог — «верхнего» сообщения нет
    }
    let peer = raw_dialog_peer(raw);
    messages
        .iter()
        .find(|m| m.id() == top && raw_message_peer(m) == Some(peer))
}

/// Что известно об одном peer'е из сырых списков `users`/`chats`.
#[derive(Debug, Clone)]
struct RawPeerEntry {
    /// `user` | `group` | `channel` (мегагруппы — `group`, как в [`peer_kind`]).
    kind: &'static str,
    /// Отображаемое имя (пустая строка — заглушку рисует фронт).
    title: String,
    /// `@username` без «@», если есть.
    username: Option<String>,
    /// `access_hash`, если Telegram его прислал (у малых групп его нет).
    access_hash: Option<i64>,
    /// Peer — бот (только у users, см. [`DialogDto::bot`]).
    bot: bool,
    /// Peer в контактах (`contact || mutual_contact`, только у users).
    contact: bool,
    /// Готовый DTO отправителя — только у пользователей.
    user: Option<TgUserDto>,
}

/// Поля [`DialogDto`], которые берутся из индекса peer'ов.
///
/// Отдельная структура вместо кортежа: полей уже пять, из них три —
/// `String`/`Option<String>` и два `bool`, и перепутать их местами в
/// кортеже было бы вопросом времени.
struct PeerParts {
    /// `user` | `group` | `channel`.
    kind: String,
    /// Отображаемое имя.
    title: String,
    /// `@username` без «@».
    username: Option<String>,
    /// Флаг бота.
    bot: bool,
    /// Флаг контакта.
    contact: bool,
}

/// Индекс peer'ов сырого ответа Telegram (`users` + `chats`).
///
/// Нужен для маппинга «сырых» диалогов и сообщений БЕЗ клиентских типов
/// grammers: у `Dialog::new`/`build_peer_map` видимость `pub(crate)`, и
/// собрать их вокруг ответа `messages.getDialogs`, вызванного напрямую
/// (архив, см. `?folder=` в роуте диалогов), невозможно. Семантика kind'ов
/// и имён повторяет клиентский путь ([`peer_kind`] / [`peer_title`]).
pub struct RawPeerIndex {
    entries: HashMap<PeerId, RawPeerEntry>,
}

impl RawPeerIndex {
    /// Строит индекс по сырым спискам из ответа Telegram.
    pub fn new(users: &[tl::enums::User], chats: &[tl::enums::Chat]) -> Self {
        let mut entries = HashMap::new();
        for user in users {
            let tl::enums::User::User(u) = user else {
                continue; // UserEmpty: ни имени, ни хеша — индексу нечего дать
            };
            let Some(id) = PeerId::user(u.id) else {
                continue;
            };
            // Полное имя — как у grammers `User::full_name()`: имя + фамилия,
            // пустая фамилия не оставляет висячий пробел.
            let mut title = u.first_name.clone().unwrap_or_default();
            if let Some(last) = u.last_name.as_deref().filter(|s| !s.is_empty()) {
                if !title.is_empty() {
                    title.push(' ');
                }
                title.push_str(last);
            }
            if title.trim().is_empty() {
                title = u.username.clone().unwrap_or_default();
            }
            entries.insert(
                id,
                RawPeerEntry {
                    kind: "user",
                    title,
                    username: u.username.clone(),
                    access_hash: u.access_hash,
                    bot: u.bot,
                    // Односторонний контакт тоже контакт: в официальном
                    // клиенте папка «контакты» ловит и его.
                    contact: u.contact || u.mutual_contact,
                    user: Some(TgUserDto {
                        id: dialog_id_string(id),
                        username: u.username.clone(),
                        first_name: u.first_name.clone(),
                        phone: u.phone.clone(),
                    }),
                },
            );
        }
        for chat in chats {
            use tl::enums::Chat as C;
            // Классификация повторяет grammers `Peer::from_raw`: канал —
            // только broadcast, мегагруппы для UI остаются группами.
            let entry = match chat {
                C::Empty(c) => PeerId::chat(c.id).map(|id| (id, "group", String::new(), None, None)),
                C::Chat(c) => {
                    PeerId::chat(c.id).map(|id| (id, "group", c.title.clone(), None, None))
                }
                C::Forbidden(c) => {
                    PeerId::chat(c.id).map(|id| (id, "group", c.title.clone(), None, None))
                }
                C::Channel(c) => PeerId::channel(c.id).map(|id| {
                    let kind = if c.broadcast { "channel" } else { "group" };
                    (id, kind, c.title.clone(), c.username.clone(), c.access_hash)
                }),
                C::ChannelForbidden(c) => PeerId::channel(c.id).map(|id| {
                    let kind = if c.broadcast { "channel" } else { "group" };
                    (id, kind, c.title.clone(), None, Some(c.access_hash))
                }),
            };
            if let Some((id, kind, title, username, access_hash)) = entry {
                entries.insert(
                    id,
                    RawPeerEntry {
                        kind,
                        title,
                        username,
                        access_hash,
                        // Ни «бот», ни «контакт» к группам и каналам не
                        // применимы — см. [`peer_bot_contact`].
                        bot: false,
                        contact: false,
                        user: None,
                    },
                );
            }
        }
        Self { entries }
    }

    /// Поля DTO диалога, известные по peer'у. Промах индекса (Telegram не
    /// прислал peer) деградирует до kind из тега id, пустого имени и
    /// «не бот, не контакт»: врать про бота опаснее, чем недосказать —
    /// человек в папке «боты» заметнее, чем бот в общем списке.
    fn dto_parts(&self, id: PeerId) -> PeerParts {
        match self.entries.get(&id) {
            Some(e) => PeerParts {
                kind: e.kind.to_string(),
                title: e.title.clone(),
                username: e.username.clone(),
                bot: e.bot,
                contact: e.contact,
            },
            None => PeerParts {
                kind: kind_of(id).to_string(),
                title: String::new(),
                username: None,
                bot: false,
                contact: false,
            },
        }
    }

    /// DTO отправителя для `from_id` сырого сообщения (зеркало [`sender_dto`]).
    fn sender(&self, peer: &tl::enums::Peer) -> Option<TgUserDto> {
        let id = PeerId::from(peer);
        let entry = self.entries.get(&id)?;
        if let Some(user) = &entry.user {
            return Some(user.clone());
        }
        Some(TgUserDto {
            id: dialog_id_string(id),
            username: entry.username.clone(),
            first_name: Some(entry.title.clone()).filter(|s| !s.is_empty()),
            phone: None,
        })
    }

    /// `PeerRef`'ы для пополнения кэша peer'ов: users/каналы — с
    /// `access_hash`, малые группы — с ambient-авторити (хеш им не нужен).
    /// «min»-peer'ы без хеша пропускаются: кэшировать нечего.
    pub fn peer_refs(&self) -> Vec<PeerRef> {
        self.entries
            .iter()
            .filter_map(|(id, e)| match (id.kind(), e.access_hash) {
                (PeerKind::Chat, _) => Some(id.to_ambient_ref()),
                (_, Some(hash)) => Some(PeerRef {
                    id: *id,
                    auth: PeerAuth::from_hash(hash),
                }),
                (_, None) => None,
            })
            .collect()
    }

    /// `InputPeer` для пагинации сырого `messages.getDialogs`.
    pub fn input_peer(&self, id: PeerId) -> tl::enums::InputPeer {
        let hash = self.entries.get(&id).and_then(|e| e.access_hash);
        let Some(bare) = id.bare_id() else {
            return tl::enums::InputPeer::Empty;
        };
        match id.kind() {
            PeerKind::User => tl::enums::InputPeer::User(tl::types::InputPeerUser {
                user_id: bare,
                access_hash: hash.unwrap_or_default(),
            }),
            PeerKind::Chat => tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id: bare }),
            PeerKind::Channel => tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
                channel_id: bare,
                access_hash: hash.unwrap_or_default(),
            }),
        }
    }
}

/// Проекция СЫРОГО сообщения в [`MessageDto`] (зеркало
/// [`from_grammers_message`] для ответов, полученных мимо клиентских
/// типов grammers).
///
/// - `Message::Service` даёт строку с пустым текстом (как и клиентский
///   путь: у сервисных сообщений нет ни текста, ни медиа);
/// - `Message::Empty` — дыра истории, маппить нечего ⇒ `None`.
pub fn from_raw_message(msg: &tl::enums::Message, index: &RawPeerIndex) -> Option<MessageDto> {
    match msg {
        tl::enums::Message::Message(m) => Some(MessageDto {
            id: m.id,
            dialog_id: dialog_id_string(PeerId::from(&m.peer_id)),
            out: m.out,
            date: i64::from(m.date),
            sender: m.from_id.as_ref().and_then(|p| index.sender(p)),
            text: m.message.clone(),
            media: m
                .media
                .clone()
                .and_then(Media::from_raw)
                .as_ref()
                .and_then(media_dto),
            reply_to_id: match &m.reply_to {
                Some(tl::enums::MessageReplyHeader::Header(h)) => h.reply_to_msg_id,
                _ => None,
            },
            reply_to: m.reply_to.as_ref().and_then(reply_to_from_header),
            edit_date: m.edit_date.map(i64::from),
            reply_markup: m.reply_markup.as_ref().map(reply_markup_dto),
        }),
        tl::enums::Message::Service(m) => Some(MessageDto {
            id: m.id,
            dialog_id: dialog_id_string(PeerId::from(&m.peer_id)),
            out: m.out,
            date: i64::from(m.date),
            sender: m.from_id.as_ref().and_then(|p| index.sender(p)),
            text: String::new(),
            media: None,
            reply_to_id: match &m.reply_to {
                Some(tl::enums::MessageReplyHeader::Header(h)) => h.reply_to_msg_id,
                _ => None,
            },
            reply_to: m.reply_to.as_ref().and_then(reply_to_from_header),
            edit_date: None,
            // У `messageService` поля `reply_markup` нет в схеме вовсе.
            reply_markup: None,
        }),
        tl::enums::Message::Empty(_) => None,
    }
}

/// Проекция СЫРОГО диалога в [`DialogDto`] (зеркало
/// [`from_grammers_dialog`] для прямых вызовов `messages.getDialogs`).
///
/// `top` — верхнее сообщение диалога, найденное [`find_top_message`];
/// `now` — unix-время для расчёта `muted` (см. [`notify_muted`]).
///
/// Глобальные дефолты уведомлений НЕ учитываются (эквивалент пустых
/// [`NotifyDefaults`]) — вариант для вызовов, где их ещё нет; когда они
/// загружены, зовите [`from_raw_dialog_with_defaults`].
pub fn from_raw_dialog(
    raw: &tl::enums::Dialog,
    top: Option<&tl::enums::Message>,
    index: &RawPeerIndex,
    now: i64,
) -> DialogDto {
    from_raw_dialog_with_defaults(raw, top, index, now, NotifyDefaults::default())
}

/// [`from_raw_dialog`] с наследованием глобальных настроек уведомлений.
///
/// Категория для дефолта берётся из индекса peer'ов, а не из тега id:
/// только там мегагруппа отличается от broadcast-канала (`group` против
/// `channel`), а у Telegram это разные глобальные дефолты.
pub fn from_raw_dialog_with_defaults(
    raw: &tl::enums::Dialog,
    top: Option<&tl::enums::Message>,
    index: &RawPeerIndex,
    now: i64,
    defaults: NotifyDefaults,
) -> DialogDto {
    let (unread_count, pinned) = match raw {
        tl::enums::Dialog::Dialog(d) => (d.unread_count, d.pinned),
        tl::enums::Dialog::Folder(d) => (0, d.pinned),
    };
    let peer_id = raw_dialog_peer(raw);
    let parts = index.dto_parts(peer_id);
    let folder_id = dialog_folder_id(raw);
    DialogDto {
        id: dialog_id_string(peer_id),
        muted: dialog_muted_with_defaults_for_kind(raw, now, defaults, &parts.kind),
        kind: parts.kind,
        title: parts.title,
        username: parts.username,
        bot: parts.bot,
        contact: parts.contact,
        unread_count,
        pinned,
        archived: is_archived(folder_id),
        folder_id,
        last_message: top.and_then(|m| from_raw_message(m, index)),
    }
}

/// Папка (dialog filter) Telegram в проекции для UI.
///
/// Все id peer'ов — строки Bot API-формата (инвариант №1), ровно те же,
/// что в [`DialogDto::id`]: фронт сопоставляет их без преобразований.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderDto {
    /// Идентификатор папки у Telegram (2..255; 0/1 — служебные).
    pub id: i32,
    /// Название папки, заданное пользователем.
    pub title: String,
    /// Эмодзи-иконка папки, если выбрана.
    pub emoticon: Option<String>,
    /// Диалоги, закреплённые внутри папки.
    pub pinned_ids: Vec<String>,
    /// Диалоги, добавленные вручную.
    pub include_ids: Vec<String>,
    /// Диалоги, исключённые вручную.
    pub exclude_ids: Vec<String>,
    /// Автовключение: личные чаты контактов.
    pub contacts: bool,
    /// Автовключение: личные чаты не-контактов.
    pub non_contacts: bool,
    /// Автовключение: группы.
    pub groups: bool,
    /// Автовключение: каналы.
    pub broadcasts: bool,
    /// Автовключение: боты.
    pub bots: bool,
    /// Прятать заглушённые диалоги.
    pub exclude_muted: bool,
    /// Прятать прочитанные диалоги.
    pub exclude_read: bool,
    /// Прятать архив.
    pub exclude_archived: bool,
}

/// `InputPeer` папки → строковый dialog_id нашего контракта.
///
/// `None` для `Empty`/`PeerSelf` («сам себе» не адресуется, см.
/// [`dialog_id_string`]) и для id вне валидных диапазонов Bot API.
pub fn input_peer_dialog_id(peer: &tl::enums::InputPeer) -> Option<String> {
    use tl::enums::InputPeer as P;
    let id = match peer {
        P::User(p) => PeerId::user(p.user_id)?,
        P::Chat(p) => PeerId::chat(p.chat_id)?,
        P::Channel(p) => PeerId::channel(p.channel_id)?,
        P::UserFromMessage(p) => PeerId::user(p.user_id)?,
        P::ChannelFromMessage(p) => PeerId::channel(p.channel_id)?,
        P::Empty | P::PeerSelf => return None,
    };
    Some(dialog_id_string(id))
}

/// Маппинг списка peer'ов папки в строки dialog_id.
fn input_peer_ids(peers: &[tl::enums::InputPeer]) -> Vec<String> {
    peers.iter().filter_map(input_peer_dialog_id).collect()
}

/// Проекция сырого `DialogFilter` в [`FolderDto`].
///
/// - `Default` — основной список «Все чаты», НЕ папка ⇒ `None`;
/// - `Filter` — обычная папка, маппится целиком;
/// - `Chatlist` — общая папка-ссылка: у неё нет ни include-флагов, ни
///   исключений — соответствующие поля уходят `false`/пустыми.
pub fn folder_dto(filter: &tl::enums::DialogFilter) -> Option<FolderDto> {
    use tl::enums::DialogFilter as F;
    let dto = match filter {
        F::Default => return None,
        F::Filter(f) => {
            let tl::enums::TextWithEntities::Entities(title) = &f.title;
            FolderDto {
                id: f.id,
                title: title.text.clone(),
                emoticon: f.emoticon.clone(),
                pinned_ids: input_peer_ids(&f.pinned_peers),
                include_ids: input_peer_ids(&f.include_peers),
                exclude_ids: input_peer_ids(&f.exclude_peers),
                contacts: f.contacts,
                non_contacts: f.non_contacts,
                groups: f.groups,
                broadcasts: f.broadcasts,
                bots: f.bots,
                exclude_muted: f.exclude_muted,
                exclude_read: f.exclude_read,
                exclude_archived: f.exclude_archived,
            }
        }
        F::Chatlist(f) => {
            let tl::enums::TextWithEntities::Entities(title) = &f.title;
            FolderDto {
                id: f.id,
                title: title.text.clone(),
                emoticon: f.emoticon.clone(),
                pinned_ids: input_peer_ids(&f.pinned_peers),
                include_ids: input_peer_ids(&f.include_peers),
                ..FolderDto::default()
            }
        }
    };
    Some(dto)
}

/// Проекция диалога grammers в [`DialogDto`].
///
/// `unread_count` и `pinned` берутся из сырого `tl::enums::Dialog`: у
/// grammers для них нет геттеров. Папки (`Dialog::Folder`) в списке
/// пользователя тоже встречаются — у них нет ни счётчика, ни последнего
/// сообщения, поэтому оба поля остаются нулевыми.
pub fn from_grammers_dialog(dialog: &Dialog) -> DialogDto {
    from_grammers_dialog_with_defaults(dialog, now_unix(), NotifyDefaults::default())
}

/// [`from_grammers_dialog`] с наследованием глобальных настроек уведомлений
/// и явным `now` (последнее — ради тестируемости вызывающего кода).
pub fn from_grammers_dialog_with_defaults(
    dialog: &Dialog,
    now: i64,
    defaults: NotifyDefaults,
) -> DialogDto {
    let (unread_count, pinned) = match &dialog.raw {
        tl::enums::Dialog::Dialog(d) => (d.unread_count, d.pinned),
        tl::enums::Dialog::Folder(d) => (0, d.pinned),
    };
    let folder_id = dialog_folder_id(&dialog.raw);
    let peer = dialog.peer();
    let kind = peer_kind(peer);
    let (bot, contact) = peer_bot_contact(peer);
    DialogDto {
        id: dialog_id_string(dialog.peer_id()),
        kind: kind.into(),
        title: peer_title(peer),
        username: peer.username().map(str::to_string),
        bot,
        contact,
        unread_count,
        pinned,
        muted: dialog_muted_with_defaults_for_kind(&dialog.raw, now, defaults, kind),
        archived: is_archived(folder_id),
        folder_id,
        last_message: dialog.last_message.as_ref().map(from_grammers_message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_message() -> MessageDto {
        MessageDto {
            id: 42,
            dialog_id: "-1001234567890123".into(),
            out: true,
            date: 1_800_000_000,
            sender: Some(TgUserDto {
                id: "9876543210987".into(),
                username: Some("forge".into()),
                first_name: Some("F.O.R.G.E.".into()),
                phone: None,
            }),
            text: "hi".into(),
            media: None,
            reply_to_id: Some(41),
            reply_to: Some(ReplyToDto {
                message_id: Some(41),
                sender: Some("Иван".into()),
                sender_id: Some("777000".into()),
                text: Some("оригинал".into()),
                media_kind: None,
                quote: None,
                out: Some(false),
                unavailable: false,
            }),
            edit_date: Some(1_800_000_100),
            reply_markup: None,
        }
    }

    #[test]
    fn message_dto_round_trips_and_keeps_dialog_id_as_string() {
        let msg = big_message();
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains("\"dialog_id\":\"-1001234567890123\""),
            "dialog_id обязан быть строкой: {json}"
        );
        assert!(json.contains("\"id\":42"), "id сообщения — число: {json}");
        assert!(
            json.contains("\"id\":\"9876543210987\""),
            "user_id обязан быть строкой: {json}"
        );
        let back: MessageDto = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    /// Идентификатор канала не влезает в `Number` JS без потери точности —
    /// проверяем, что через JSON он проходит байт-в-байт.
    #[test]
    fn huge_ids_survive_a_json_round_trip() {
        for raw in [
            -1_001_234_567_890_123_i64,
            -1_997_852_516_352,
            1_099_511_627_775,
            777_000,
        ] {
            let dto = DialogDto {
                id: raw.to_string(),
                kind: "channel".into(),
                ..DialogDto::default()
            };
            let json = serde_json::to_string(&dto).unwrap();
            assert!(json.contains(&format!("\"id\":\"{raw}\"")), "{json}");
            let back: DialogDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back.id, raw.to_string());
            assert_eq!(back.id.parse::<i64>().unwrap(), raw);
        }
    }

    /// Инвариант №2: ключ есть всегда, значение может быть `null`. Фронт
    /// опирается на присутствие полей.
    #[test]
    fn optional_fields_are_serialized_as_explicit_null() {
        let json = serde_json::to_value(MessageDto::default()).unwrap();
        for key in [
            "sender",
            "media",
            "reply_to_id",
            "reply_to",
            "edit_date",
            "reply_markup",
        ] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
            assert!(json[key].is_null(), "{key} ожидался null: {json}");
        }
        let json = serde_json::to_value(DialogDto::default()).unwrap();
        for key in ["username", "last_message"] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
            assert!(json[key].is_null(), "{key} ожидался null: {json}");
        }
        let json = serde_json::to_value(MediaDto::default()).unwrap();
        for key in ["mime", "size", "file_name", "width", "height", "duration_secs"] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
            assert!(json[key].is_null(), "{key} ожидался null: {json}");
        }
    }

    #[test]
    fn dialog_dto_nests_last_message() {
        let dto = DialogDto {
            id: "777000".into(),
            kind: "user".into(),
            title: "Telegram".into(),
            username: Some("telegram".into()),
            bot: true,
            contact: false,
            unread_count: 3,
            pinned: true,
            muted: true,
            archived: true,
            folder_id: Some(1),
            last_message: Some(big_message()),
        };
        let json = serde_json::to_string(&dto).unwrap();
        let back: DialogDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
        assert_eq!(back.last_message.unwrap().id, 42);
    }

    #[test]
    fn dialog_id_string_round_trips_through_peer_id() {
        // Пользователь, малая группа и канал — все три формата Bot API
        // (`-100` + bare id канала, ровно как в остальных клиентах).
        for raw in [777_000_i64, -999_999_999_999, -1_001_234_567_890] {
            let id = PeerId::from_bot_api_dialog_id(raw).expect("валидный dialog_id");
            let s = dialog_id_string(id);
            assert_eq!(s, raw.to_string());
            assert_eq!(parse_dialog_id(&s), Some(id));
        }
    }

    #[test]
    fn parse_dialog_id_rejects_garbage() {
        for raw in ["", "  ", "abc", "12abc", "1e10", "999999999999999999999", "0"] {
            assert!(
                parse_dialog_id(raw).is_none(),
                "{raw:?} не должен парситься в dialog_id"
            );
        }
        // Пробелы по краям — терпим (curl/копипаста), значение то же.
        assert_eq!(
            parse_dialog_id(" 777000 "),
            PeerId::from_bot_api_dialog_id(777_000)
        );
    }

    #[test]
    fn kind_of_reads_the_tag_embedded_in_peer_id() {
        assert_eq!(kind_of(PeerId::user(777_000).unwrap()), "user");
        assert_eq!(kind_of(PeerId::chat(123).unwrap()), "group");
        assert_eq!(kind_of(PeerId::channel(123).unwrap()), "channel");
    }

    #[test]
    fn document_kind_prefers_attributes_over_mime() {
        // Голосовое: MIME как у музыки, различает только флаг voice.
        assert_eq!(
            document_kind(
                Some("audio/ogg"),
                DocFlags {
                    voice: true,
                    ..DocFlags::default()
                }
            ),
            "voice"
        );
        assert_eq!(
            document_kind(
                Some("audio/ogg"),
                DocFlags {
                    audio: true,
                    ..DocFlags::default()
                }
            ),
            "audio"
        );
        // Стикер важнее всего остального (у него бывает и video-атрибут).
        assert_eq!(
            document_kind(
                Some("video/webm"),
                DocFlags {
                    sticker: true,
                    video: true,
                    ..DocFlags::default()
                }
            ),
            "sticker"
        );
        // GIF: video/mp4 + animated ⇒ видео-карточка.
        assert_eq!(
            document_kind(
                Some("video/mp4"),
                DocFlags {
                    animated: true,
                    ..DocFlags::default()
                }
            ),
            "video"
        );
    }

    #[test]
    fn document_kind_falls_back_to_mime_then_to_document() {
        let none = DocFlags::default();
        assert_eq!(document_kind(Some("video/mp4"), none), "video");
        assert_eq!(document_kind(Some("audio/mpeg"), none), "audio");
        assert_eq!(document_kind(Some("image/png"), none), "photo");
        assert_eq!(document_kind(Some("application/pdf"), none), "document");
        assert_eq!(document_kind(None, none), "document");
    }

    /// Из набора превью берётся самое большое по площади (конструкторы
    /// `PhotoSize`-вариантов приватны у grammers, поэтому проверяется
    /// выборка размеров — единственное, что здесь может сломаться).
    #[test]
    fn largest_dimensions_picks_the_biggest_by_area() {
        assert_eq!(
            largest_dimensions([(90, 90), (1280, 720), (320, 320)].into_iter()),
            Some((1280, 720))
        );
        // Площадь, а не ширина: панорама уже, но пикселей в ней больше.
        assert_eq!(
            largest_dimensions([(1000, 100), (400, 400)].into_iter()),
            Some((400, 400))
        );
        assert_eq!(largest_dimensions(std::iter::empty()), None);
        assert_eq!(largest_resolution(&[]), None);
    }

    /// Точные границы Bot API dialog id (<https://core.telegram.org/api/bots/ids>):
    /// значения на границах валидны и проходят round-trip, значения сразу за
    /// ними и «дыры» между диапазонами — нет. Реальный случай: новые аккаунты
    /// уже получают id вблизи максимума диапазона, а монофорумы живут в
    /// отдельном «дальнем» диапазоне.
    #[test]
    fn parse_dialog_id_honours_exact_bot_api_boundaries() {
        for (raw, kind) in [
            ("1", "user"),
            ("1099511627775", "user"), // максимальный user id (2^40 - 1)
            ("-1", "group"),
            ("-999999999999", "group"), // минимальный chat dialog id
            ("-1000000000001", "channel"), // максимальный channel dialog id
            ("-1997852516352", "channel"), // минимальный channel dialog id
            ("-2002147483649", "channel"), // монофорумы: верх диапазона
            ("-4000000000000", "channel"), // монофорумы: низ диапазона
        ] {
            let id = parse_dialog_id(raw).unwrap_or_else(|| panic!("{raw} обязан парситься"));
            assert_eq!(kind_of(id), kind, "{raw}");
            assert_eq!(dialog_id_string(id), raw, "round-trip {raw}");
        }
        for raw in [
            "1099511627776",  // max user id + 1 — внутренний сентинел self-user
            "-1000000000000", // дыра между chat- и channel-диапазонами
            "-1997852516353", // сразу за нижней границей каналов
            "-2002147483648", // дыра между каналами и монофорумами
            "-4000000000001", // сразу за нижней границей монофорумов
        ] {
            assert!(parse_dialog_id(raw).is_none(), "{raw} не должен парситься");
        }
        assert!(parse_dialog_id(&i64::MAX.to_string()).is_none());
        assert!(parse_dialog_id(&i64::MIN.to_string()).is_none());
    }

    /// Сентинел «сам себе» отдаётся фронту как `"0"` и НИКОГДА не парсится
    /// обратно: echo этой строки в URL обязан дать 404, а не панику в
    /// `kind()` или резолв случайного диалога.
    #[test]
    fn self_user_sentinel_prints_as_zero_and_never_round_trips() {
        let sentinel = PeerId::self_user();
        assert_eq!(dialog_id_string(sentinel), "0");
        // kind_of на сентинеле не должен паниковать (kind() знает self-user).
        assert_eq!(kind_of(sentinel), "user");
        assert_eq!(parse_dialog_id("0"), None);
        assert_eq!(parse_dialog_id("-0"), None, "\"-0\" парсится в 0 — тоже мимо");
    }

    /// Копипаста id из RTL-чатов и «красивых» источников приносит невидимые
    /// направляющие символы и не-ASCII цифры. Такие строки обязаны давать
    /// `None` (⇒ 404), а не панику или резолв другого диалога.
    #[test]
    fn parse_dialog_id_rejects_invisible_marks_and_non_ascii_digits() {
        for raw in [
            "\u{200e}777000",  // LEFT-TO-RIGHT MARK перед id (копипаста из RTL)
            "777000\u{200f}",  // RIGHT-TO-LEFT MARK после id
            "٧٧٧٠٠٠",          // арабо-индийские цифры
            "７７７０００",     // fullwidth-цифры
            "77\u{200b}7000",  // ZERO WIDTH SPACE внутри числа
            "0x777000",        // hex-запись
        ] {
            assert!(parse_dialog_id(raw).is_none(), "{raw:?} не должен парситься");
        }
    }

    /// MIME от сторонних клиентов бывает пустым, без подтипа или в верхнем
    /// регистре — классификация обязана падать в безопасный `document`, а не
    /// угадывать.
    #[test]
    fn document_kind_treats_malformed_mime_as_document() {
        let none = DocFlags::default();
        assert_eq!(document_kind(Some(""), none), "document");
        assert_eq!(document_kind(Some("video"), none), "document"); // без «/»
        assert_eq!(document_kind(Some("VIDEO/MP4"), none), "document"); // регистр не угадываем
        assert_eq!(document_kind(Some(" video/mp4"), none), "document"); // мусорный пробел
    }

    /// Пограничные, но реальные MIME без атрибутов: webp/gif/svg — картинки
    /// (photo-карточка), octet-stream — честный документ.
    #[test]
    fn document_kind_classifies_common_edge_mimes_by_prefix() {
        let none = DocFlags::default();
        assert_eq!(document_kind(Some("image/webp"), none), "photo");
        assert_eq!(document_kind(Some("image/gif"), none), "photo");
        assert_eq!(document_kind(Some("image/svg+xml"), none), "photo");
        assert_eq!(document_kind(Some("application/octet-stream"), none), "document");
    }

    /// GIF, загруженный именно как `image/gif` (а не перекодированный в mp4),
    /// всё равно анимация: атрибут Animated важнее image/*-эвристики. И у
    /// голосового атрибут побеждает даже бессмысленный видео-MIME.
    #[test]
    fn document_kind_attributes_beat_contradicting_mime() {
        assert_eq!(
            document_kind(
                Some("image/gif"),
                DocFlags {
                    animated: true,
                    ..DocFlags::default()
                }
            ),
            "video"
        );
        assert_eq!(
            document_kind(
                Some("video/mp4"),
                DocFlags {
                    voice: true,
                    ..DocFlags::default()
                }
            ),
            "voice"
        );
    }

    /// Кастомные эмодзи для UI — стикеры; Filename/ImageSize/HasStickers не
    /// должны влиять на флаги; битые данные с двумя Audio-атрибутами (музыка
    /// и голос разом) не должны терять voice.
    #[test]
    fn doc_flags_handles_custom_emoji_and_mixed_attributes() {
        let attrs = vec![
            tl::enums::DocumentAttribute::CustomEmoji(tl::types::DocumentAttributeCustomEmoji {
                free: true,
                text_color: false,
                alt: "🔥".into(),
                stickerset: tl::enums::InputStickerSet::Empty,
            }),
            tl::enums::DocumentAttribute::Filename(tl::types::DocumentAttributeFilename {
                file_name: "emoji.webp".into(),
            }),
        ];
        let f = doc_flags(&attrs);
        assert!(f.sticker, "CustomEmoji обязан классифицироваться стикером");
        assert!(!f.video && !f.audio && !f.voice && !f.animated);

        // Голос + музыка в одном документе (несогласованные атрибуты) —
        // voice не теряется и побеждает в классификации.
        let attrs = vec![
            tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
                voice: false,
                duration: 60,
                title: Some("t".into()),
                performer: None,
                waveform: None,
            }),
            tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
                voice: true,
                duration: 7,
                title: None,
                performer: None,
                waveform: None,
            }),
        ];
        let f = doc_flags(&attrs);
        assert!(f.voice && f.audio);
        assert_eq!(document_kind(Some("audio/ogg"), f), "voice");

        // Unit-варианты: Animated поднимает флаг, HasStickers и ImageSize —
        // нет (HasStickers бывает у видео с прикреплёнными стикерами).
        let attrs = vec![
            tl::enums::DocumentAttribute::Animated,
            tl::enums::DocumentAttribute::HasStickers,
            tl::enums::DocumentAttribute::ImageSize(tl::types::DocumentAttributeImageSize {
                w: 100,
                h: 100,
            }),
        ];
        let f = doc_flags(&attrs);
        assert!(f.animated);
        assert!(!f.sticker, "HasStickers — не стикер");
    }

    /// Битые размеры от сервера (i32::MAX × i32::MAX) не должны переполнять
    /// вычисление площади: счёт обязан идти в i64 (в debug переполнение i32
    /// уронило бы процесс паникой).
    #[test]
    fn largest_dimensions_survives_extreme_values_without_overflow() {
        assert_eq!(
            largest_dimensions([(1, 1), (i32::MAX, i32::MAX)].into_iter()),
            Some((i32::MAX, i32::MAX))
        );
        // Вырожденное превью 0×0 — валидный, хоть и бесполезный ответ.
        assert_eq!(largest_dimensions([(0, 0)].into_iter()), Some((0, 0)));
    }

    /// Атрибут голосового кладётся в `MediaDto.waveform` — в той же шкале и с
    /// тем же потолком, что у VK.
    #[test]
    fn the_voice_attribute_becomes_a_real_waveform() {
        let audio = |voice: bool, w: Option<Vec<u8>>| {
            tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
                voice,
                duration: 7,
                title: None,
                performer: None,
                waveform: w,
            })
        };

        // `0b11111_00001_00000` → отсчёты [0, 1, 31] → общая шкала 0..255.
        let w = audio_waveform(&[audio(true, Some(vec![0x20, 0x7c]))]);
        assert_eq!(w, vec![0, 8, forge_voice::waveform::WAVEFORM_MAX_VALUE]);

        // Атрибута нет / он пуст — огибающей нет, но и паники нет: такую
        // запись досчитает бэкенд по скачанному файлу.
        assert!(audio_waveform(&[audio(true, None)]).is_empty());
        assert!(audio_waveform(&[audio(true, Some(Vec::new()))]).is_empty());
        assert!(audio_waveform(&[]).is_empty());

        // Два атрибута Audio (несогласованные данные): побеждает НЕПУСТОЙ, а
        // не первый попавшийся.
        let w = audio_waveform(&[audio(false, None), audio(true, Some(vec![0xff; 5]))]);
        assert_eq!(w.len(), 8);
        assert!(
            w.iter().all(|v| *v == forge_voice::waveform::WAVEFORM_MAX_VALUE),
            "{w:?}"
        );

        // Длинная запись укладывается в общий потолок столбиков.
        let long = audio_waveform(&[audio(true, Some(vec![0xa5; 1_000]))]);
        assert!(long.len() <= forge_voice::waveform::MAX_WAVEFORM, "{}", long.len());
        assert!(long
            .iter()
            .all(|v| (0..=forge_voice::waveform::WAVEFORM_MAX_VALUE).contains(v)));
    }

    #[test]
    fn doc_flags_reads_voice_and_video_attributes() {
        let attrs = vec![
            tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
                voice: true,
                duration: 7,
                title: None,
                performer: None,
                waveform: None,
            }),
        ];
        let f = doc_flags(&attrs);
        assert!(f.voice, "флаг voice обязан подниматься");
        assert!(!f.audio, "voice и audio взаимоисключающи");

        let attrs = vec![tl::enums::DocumentAttribute::Video(
            tl::types::DocumentAttributeVideo {
                round_message: false,
                supports_streaming: true,
                nosound: false,
                duration: 12.5,
                w: 640,
                h: 480,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            },
        )];
        assert!(doc_flags(&attrs).video);
    }

    // ------------------------------------------------------------------
    // Фикстуры сырых TL-типов (конструкторы у них полные, без Default).
    // ------------------------------------------------------------------

    fn raw_notify(mute_until: Option<i32>, silent: Option<bool>) -> tl::types::PeerNotifySettings {
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
        }
    }

    fn raw_dialog(
        peer: tl::enums::Peer,
        top_message: i32,
        unread_count: i32,
        pinned: bool,
        folder_id: Option<i32>,
        settings: tl::types::PeerNotifySettings,
    ) -> tl::enums::Dialog {
        tl::enums::Dialog::Dialog(tl::types::Dialog {
            pinned,
            unread_mark: false,
            view_forum_as_messages: false,
            peer,
            top_message,
            read_inbox_max_id: 0,
            read_outbox_max_id: 0,
            unread_count,
            unread_mentions_count: 0,
            unread_reactions_count: 0,
            unread_poll_votes_count: 0,
            notify_settings: tl::enums::PeerNotifySettings::Settings(settings),
            pts: None,
            draft: None,
            folder_id,
            ttl_period: None,
        })
    }

    fn raw_user(
        id: i64,
        first_name: Option<&str>,
        last_name: Option<&str>,
        username: Option<&str>,
        access_hash: Option<i64>,
    ) -> tl::enums::User {
        tl::enums::User::User(tl::types::User {
            is_self: false,
            contact: false,
            mutual_contact: false,
            deleted: false,
            bot: false,
            bot_chat_history: false,
            bot_nochats: false,
            verified: false,
            restricted: false,
            min: false,
            bot_inline_geo: false,
            support: false,
            scam: false,
            apply_min_photo: false,
            fake: false,
            bot_attach_menu: false,
            premium: false,
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: false,
            stories_hidden: false,
            stories_unavailable: false,
            contact_require_premium: false,
            bot_business: false,
            bot_has_main_app: false,
            bot_forum_view: false,
            bot_forum_can_manage_topics: false,
            bot_can_manage_bots: false,
            bot_guestchat: false,
            bot_guard: false,
            id,
            access_hash,
            first_name: first_name.map(str::to_string),
            last_name: last_name.map(str::to_string),
            username: username.map(str::to_string),
            phone: None,
            photo: None,
            status: None,
            bot_info_version: None,
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: None,
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
        })
    }

    fn raw_channel(
        id: i64,
        title: &str,
        username: Option<&str>,
        broadcast: bool,
        access_hash: Option<i64>,
    ) -> tl::enums::Chat {
        tl::enums::Chat::Channel(tl::types::Channel {
            creator: false,
            left: false,
            broadcast,
            verified: false,
            megagroup: !broadcast,
            restricted: false,
            signatures: false,
            min: false,
            scam: false,
            has_link: false,
            has_geo: false,
            slowmode_enabled: false,
            call_active: false,
            call_not_empty: false,
            fake: false,
            gigagroup: false,
            noforwards: false,
            join_to_send: false,
            join_request: false,
            forum: false,
            stories_hidden: false,
            stories_hidden_min: false,
            stories_unavailable: false,
            signature_profiles: false,
            autotranslation: false,
            broadcast_messages_allowed: false,
            monoforum: false,
            forum_tabs: false,
            id,
            access_hash,
            title: title.to_string(),
            username: username.map(str::to_string),
            photo: tl::enums::ChatPhoto::Empty,
            date: 0,
            restriction_reason: None,
            admin_rights: None,
            banned_rights: None,
            default_banned_rights: None,
            participants_count: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            emoji_status: None,
            level: None,
            subscription_until_date: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_monoforum_id: None,
        })
    }

    fn raw_small_chat(id: i64, title: &str) -> tl::enums::Chat {
        tl::enums::Chat::Chat(tl::types::Chat {
            creator: false,
            left: false,
            deactivated: false,
            call_active: false,
            call_not_empty: false,
            noforwards: false,
            id,
            title: title.to_string(),
            photo: tl::enums::ChatPhoto::Empty,
            participants_count: 2,
            date: 0,
            version: 1,
            migrated_to: None,
            admin_rights: None,
            default_banned_rights: None,
        })
    }

    fn raw_message(
        id: i32,
        peer_id: tl::enums::Peer,
        from_id: Option<tl::enums::Peer>,
        text: &str,
        date: i32,
    ) -> tl::enums::Message {
        tl::enums::Message::Message(tl::types::Message {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            post: false,
            from_scheduled: false,
            legacy: false,
            edit_hide: false,
            pinned: false,
            noforwards: false,
            invert_media: false,
            offline: false,
            video_processing_pending: false,
            paid_suggested_post_stars: false,
            paid_suggested_post_ton: false,
            id,
            from_id,
            from_boosts_applied: None,
            from_rank: None,
            peer_id,
            saved_peer_id: None,
            fwd_from: None,
            via_bot_id: None,
            via_business_bot_id: None,
            guestchat_via_from: None,
            reply_to: None,
            date,
            message: text.to_string(),
            media: None,
            reply_markup: None,
            entities: None,
            views: None,
            forwards: None,
            replies: None,
            edit_date: None,
            post_author: None,
            grouped_id: None,
            reactions: None,
            restriction_reason: None,
            ttl_period: None,
            quick_reply_shortcut_id: None,
            effect: None,
            factcheck: None,
            report_delivery_until_date: None,
            paid_message_stars: None,
            suggested_post: None,
            schedule_repeat_period: None,
            summary_from_language: None,
            rich_message: None,
        })
    }

    fn peer_user(id: i64) -> tl::enums::Peer {
        tl::enums::Peer::User(tl::types::PeerUser { user_id: id })
    }

    fn peer_chat(id: i64) -> tl::enums::Peer {
        tl::enums::Peer::Chat(tl::types::PeerChat { chat_id: id })
    }

    fn peer_channel(id: i64) -> tl::enums::Peer {
        tl::enums::Peer::Channel(tl::types::PeerChannel { channel_id: id })
    }

    /// Строка «Архив» в основном списке диалогов (`Dialog::Folder`).
    fn raw_folder_row() -> tl::enums::Dialog {
        tl::enums::Dialog::Folder(tl::types::DialogFolder {
            pinned: true,
            folder: tl::enums::Folder::Folder(tl::types::Folder {
                autofill_new_broadcasts: false,
                autofill_public_groups: false,
                autofill_new_correspondents: false,
                id: 1,
                title: "Archive".into(),
                photo: None,
            }),
            peer: peer_user(777_000),
            top_message: 5,
            unread_muted_peers_count: 0,
            unread_unmuted_peers_count: 0,
            unread_muted_messages_count: 0,
            unread_unmuted_messages_count: 0,
        })
    }

    /// [`raw_user`] с флагами `bot`/`contact`/`mutual_contact`.
    fn raw_user_flags(
        id: i64,
        name: &str,
        bot: bool,
        contact: bool,
        mutual_contact: bool,
    ) -> tl::enums::User {
        let mut user = raw_user(id, Some(name), None, None, Some(id));
        if let tl::enums::User::User(u) = &mut user {
            u.bot = bot;
            u.contact = contact;
            u.mutual_contact = mutual_contact;
        }
        user
    }

    /// Матрица `notify_muted`: прошлое/будущее/«навсегда»/ноль/None и
    /// silent-флаг. `i32::MAX` («навсегда») не должен переполняться.
    #[test]
    fn notify_muted_covers_past_future_forever_and_silent() {
        let now = 1_800_000_000_i64;
        // Нет настроек — не заглушён.
        assert!(!notify_muted(&raw_notify(None, None), now));
        // mute_until в прошлом или ровно сейчас — уже отзвенело.
        assert!(!notify_muted(&raw_notify(Some(1), None), now));
        assert!(!notify_muted(&raw_notify(Some(1_800_000_000), None), now));
        // Ноль — распространённый «не заглушён» от старых клиентов.
        assert!(!notify_muted(&raw_notify(Some(0), None), now));
        // Будущее — заглушён.
        assert!(notify_muted(&raw_notify(Some(1_800_000_001), None), now));
        // «Навсегда» = i32::MAX: сравнение обязано идти в i64 без паники,
        // даже если now само близко к i64-границам.
        assert!(notify_muted(&raw_notify(Some(i32::MAX), None), now));
        assert!(!notify_muted(&raw_notify(Some(i32::MAX), None), i64::MAX));
        // silent глушит независимо от mute_until.
        assert!(notify_muted(&raw_notify(None, Some(true)), now));
        assert!(notify_muted(&raw_notify(Some(0), Some(true)), now));
        assert!(!notify_muted(&raw_notify(None, Some(false)), now));
    }

    /// `archived`/`folder_id`: 0 и None — основной список, 1 — архив;
    /// строка-папка (`Dialog::Folder`) не бывает ни muted, ни archived.
    #[test]
    fn dialog_folder_and_archive_flags() {
        let now = 1_800_000_000_i64;
        for (folder_id, want_archived) in [(None, false), (Some(0), false), (Some(1), true)] {
            let raw = raw_dialog(peer_user(777_000), 1, 0, false, folder_id, raw_notify(None, None));
            assert_eq!(dialog_folder_id(&raw), folder_id);
            assert_eq!(is_archived(dialog_folder_id(&raw)), want_archived);
        }
        let muted = raw_dialog(
            peer_user(777_000),
            1,
            0,
            false,
            Some(1),
            raw_notify(Some(i32::MAX), None),
        );
        assert!(dialog_muted(&muted, now));

        let folder_row = raw_folder_row();
        assert!(!dialog_muted(&folder_row, now));
        assert_eq!(dialog_folder_id(&folder_row), None);
        assert!(!is_archived(dialog_folder_id(&folder_row)));
    }

    /// Новые поля DialogDto: ключи присутствуют всегда, `folder_id` —
    /// явный `null` (инвариант №2), значения переживают round-trip.
    #[test]
    fn dialog_dto_serializes_muted_archived_and_folder_id() {
        let json = serde_json::to_value(DialogDto::default()).unwrap();
        for key in ["muted", "archived", "folder_id"] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
        }
        assert_eq!(json["muted"], false);
        assert_eq!(json["archived"], false);
        assert!(json["folder_id"].is_null(), "{json}");

        let dto = DialogDto {
            id: "777000".into(),
            muted: true,
            archived: true,
            folder_id: Some(1),
            ..DialogDto::default()
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"muted\":true"), "{json}");
        assert!(json.contains("\"archived\":true"), "{json}");
        assert!(json.contains("\"folder_id\":1"), "{json}");
        let back: DialogDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }

    /// Индекс сырых peer'ов повторяет семантику клиентского пути: user —
    /// полное имя, мегагруппа — group, broadcast — channel, промах — kind
    /// из тега id и пустое имя.
    #[test]
    fn raw_peer_index_mirrors_client_kinds_and_titles() {
        let users = vec![raw_user(777_000, Some("Иван"), Some("Петров"), Some("ivan"), Some(7))];
        let chats = vec![
            raw_small_chat(123, "Семья"),
            raw_channel(1_234_567_890, "Новости", Some("news"), true, Some(42)),
            raw_channel(1_234_567_891, "Чат проекта", None, false, Some(43)),
        ];
        let index = RawPeerIndex::new(&users, &chats);

        let p = index.dto_parts(PeerId::user(777_000).unwrap());
        assert_eq!((p.kind.as_str(), p.title.as_str()), ("user", "Иван Петров"));
        assert_eq!(p.username.as_deref(), Some("ivan"));

        let p = index.dto_parts(PeerId::chat(123).unwrap());
        assert_eq!((p.kind.as_str(), p.title.as_str()), ("group", "Семья"));
        assert!(!p.bot && !p.contact, "у группы флагов бота/контакта нет");

        let p = index.dto_parts(PeerId::channel(1_234_567_890).unwrap());
        assert_eq!(p.kind, "channel");
        assert_eq!(p.username.as_deref(), Some("news"));

        // Мегагруппа (не broadcast) — для UI группа, как в peer_kind.
        let p = index.dto_parts(PeerId::channel(1_234_567_891).unwrap());
        assert_eq!(p.kind, "group");

        // Промах индекса: kind из тега id, имя пустое (заглушка — на фронте).
        let p = index.dto_parts(PeerId::user(999).unwrap());
        assert_eq!((p.kind.as_str(), p.title.as_str()), ("user", ""));
        assert!(p.username.is_none());
        assert!(!p.bot && !p.contact, "промах не должен выдумывать флаги");
    }

    /// Пользователь без имени в индексе деградирует до username (как
    /// [`peer_title`]); `UserEmpty` в индекс не попадает.
    #[test]
    fn raw_peer_index_falls_back_to_username_and_skips_empty_users() {
        let users = vec![
            raw_user(1, None, None, Some("ghost"), None),
            tl::enums::User::Empty(tl::types::UserEmpty { id: 2 }),
        ];
        let index = RawPeerIndex::new(&users, &[]);
        assert_eq!(index.dto_parts(PeerId::user(1).unwrap()).title, "ghost");
        assert_eq!(
            index.dto_parts(PeerId::user(2).unwrap()).title,
            "",
            "UserEmpty не должен давать имени"
        );
    }

    /// `peer_refs`: у кого есть hash — с hash'ем, малые группы — с
    /// ambient-авторити, min-user без hash'а пропускается.
    #[test]
    fn raw_peer_index_builds_cacheable_refs() {
        let users = vec![
            raw_user(10, Some("A"), None, None, Some(111)),
            raw_user(11, Some("B"), None, None, None), // min: кэшировать нечего
        ];
        let chats = vec![
            raw_small_chat(20, "G"),
            raw_channel(30, "C", None, true, Some(333)),
        ];
        let index = RawPeerIndex::new(&users, &chats);
        let refs = index.peer_refs();
        let by_id: HashMap<PeerId, PeerRef> = refs.into_iter().map(|r| (r.id, r)).collect();
        assert_eq!(by_id.len(), 3, "min-user обязан быть пропущен");
        assert_eq!(by_id[&PeerId::user(10).unwrap()].auth.hash(), 111);
        assert_eq!(by_id[&PeerId::chat(20).unwrap()].auth.hash(), 0, "ambient");
        assert_eq!(by_id[&PeerId::channel(30).unwrap()].auth.hash(), 333);
        assert!(!by_id.contains_key(&PeerId::user(11).unwrap()));
    }

    /// `input_peer` для пагинации: правильный вариант по kind'у, промах
    /// хеша — ambient 0 (Telegram примет, если сочтёт возможным).
    #[test]
    fn raw_peer_index_makes_input_peers_for_pagination() {
        let users = vec![raw_user(10, Some("A"), None, None, Some(111))];
        let index = RawPeerIndex::new(&users, &[raw_small_chat(20, "G")]);
        assert_eq!(
            index.input_peer(PeerId::user(10).unwrap()),
            tl::enums::InputPeer::User(tl::types::InputPeerUser {
                user_id: 10,
                access_hash: 111,
            })
        );
        assert_eq!(
            index.input_peer(PeerId::chat(20).unwrap()),
            tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id: 20 })
        );
        assert_eq!(
            index.input_peer(PeerId::channel(77).unwrap()),
            tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
                channel_id: 77,
                access_hash: 0,
            })
        );
        assert_eq!(
            index.input_peer(PeerId::self_user()),
            tl::enums::InputPeer::Empty,
            "сентинел без bare id не должен паниковать"
        );
    }

    /// Сырое сообщение маппится с отправителем из индекса; сервисное — с
    /// пустым текстом; дыра истории (`MessageEmpty`) — `None`.
    #[test]
    fn from_raw_message_mirrors_the_client_projection() {
        let users = vec![raw_user(777_000, Some("Иван"), None, Some("ivan"), Some(7))];
        let index = RawPeerIndex::new(&users, &[]);

        let mut msg = raw_message(42, peer_user(777_000), Some(peer_user(777_000)), "привет", 1_800_000_000);
        if let tl::enums::Message::Message(m) = &mut msg {
            m.edit_date = Some(1_800_000_100);
            m.reply_to = Some(tl::enums::MessageReplyHeader::Header(
                tl::types::MessageReplyHeader {
                    reply_to_scheduled: false,
                    forum_topic: false,
                    quote: false,
                    reply_to_ephemeral: false,
                    reply_to_msg_id: Some(41),
                    reply_to_peer_id: None,
                    reply_from: None,
                    reply_media: None,
                    reply_to_top_id: None,
                    quote_text: None,
                    quote_entities: None,
                    quote_offset: None,
                    todo_item_id: None,
                    poll_option: None,
                },
            ));
        }
        let dto = from_raw_message(&msg, &index).expect("обычное сообщение маппится");
        assert_eq!(dto.id, 42);
        assert_eq!(dto.dialog_id, "777000");
        assert_eq!(dto.text, "привет");
        assert_eq!(dto.date, 1_800_000_000);
        assert_eq!(dto.reply_to_id, Some(41));
        assert_eq!(dto.edit_date, Some(1_800_000_100));
        let sender = dto.sender.expect("отправитель есть в индексе");
        assert_eq!(sender.id, "777000");
        assert_eq!(sender.username.as_deref(), Some("ivan"));
        assert_eq!(sender.first_name.as_deref(), Some("Иван"));

        let service = tl::enums::Message::Service(tl::types::MessageService {
            out: false,
            mentioned: false,
            media_unread: false,
            reactions_are_possible: false,
            silent: false,
            post: false,
            legacy: false,
            id: 43,
            from_id: None,
            peer_id: peer_user(777_000),
            saved_peer_id: None,
            reply_to: None,
            date: 1_800_000_200,
            action: tl::enums::MessageAction::HistoryClear,
            reactions: None,
            ttl_period: None,
        });
        let dto = from_raw_message(&service, &index).expect("сервисное сообщение маппится");
        assert_eq!(dto.id, 43);
        assert!(dto.text.is_empty(), "у сервисного сообщения нет текста");
        assert!(dto.media.is_none());

        let empty = tl::enums::Message::Empty(tl::types::MessageEmpty {
            id: 44,
            peer_id: None,
        });
        assert!(from_raw_message(&empty, &index).is_none());
    }

    /// Сырой диалог из архива: верхнее сообщение находится по паре
    /// (peer, id) — id из ЧУЖОГО диалога с тем же номером не подхватывается.
    #[test]
    fn from_raw_dialog_maps_archive_rows_with_their_top_message() {
        let now = 1_800_000_000_i64;
        let users = vec![raw_user(777_000, Some("Иван"), None, None, Some(7))];
        let chats = vec![raw_channel(1_234_567_890, "Новости", None, true, Some(42))];
        let index = RawPeerIndex::new(&users, &chats);
        let channel_peer = tl::enums::Peer::Channel(tl::types::PeerChannel {
            channel_id: 1_234_567_890,
        });

        // Два сообщения с ОДИНАКОВЫМ id в разных диалогах.
        let messages = vec![
            raw_message(5, peer_user(777_000), None, "не то", 1),
            raw_message(5, channel_peer.clone(), None, "то самое", 2),
        ];
        let raw = raw_dialog(
            channel_peer,
            5,
            3,
            true,
            Some(1),
            raw_notify(Some(i32::MAX), None),
        );
        let top = find_top_message(&raw, &messages);
        let dto = from_raw_dialog(&raw, top, &index, now);
        assert_eq!(dto.id, "-1001234567890");
        assert_eq!(dto.kind, "channel");
        assert_eq!(dto.title, "Новости");
        assert_eq!(dto.unread_count, 3);
        assert!(dto.pinned);
        assert!(dto.muted, "mute «навсегда» обязан дать muted");
        assert!(dto.archived);
        assert_eq!(dto.folder_id, Some(1));
        let last = dto.last_message.expect("верхнее сообщение найдено");
        assert_eq!(last.text, "то самое", "матчинг обязан учитывать peer");

        // Пустой диалог (top_message = 0) — без последнего сообщения.
        let empty = raw_dialog(peer_user(777_000), 0, 0, false, None, raw_notify(None, None));
        assert!(find_top_message(&empty, &messages).is_none());
    }

    /// `input_peer_dialog_id`: все адресуемые варианты дают Bot API-строку,
    /// `Empty`/`PeerSelf` — `None`.
    #[test]
    fn input_peer_dialog_id_covers_all_variants() {
        use tl::enums::InputPeer as P;
        let cases: Vec<(P, Option<&str>)> = vec![
            (
                P::User(tl::types::InputPeerUser {
                    user_id: 777_000,
                    access_hash: 1,
                }),
                Some("777000"),
            ),
            (P::Chat(tl::types::InputPeerChat { chat_id: 123 }), Some("-123")),
            (
                P::Channel(tl::types::InputPeerChannel {
                    channel_id: 1_234_567_890,
                    access_hash: 1,
                }),
                Some("-1001234567890"),
            ),
            (
                P::UserFromMessage(Box::new(tl::types::InputPeerUserFromMessage {
                    peer: P::Empty,
                    msg_id: 1,
                    user_id: 42,
                })),
                Some("42"),
            ),
            (
                P::ChannelFromMessage(Box::new(tl::types::InputPeerChannelFromMessage {
                    peer: P::Empty,
                    msg_id: 1,
                    channel_id: 99,
                })),
                // Bot API-формат канала: «-100» + bare id с нулевым паддингом.
                Some("-1000000000099"),
            ),
            (P::Empty, None),
            (P::PeerSelf, None),
        ];
        for (peer, want) in cases {
            assert_eq!(
                input_peer_dialog_id(&peer).as_deref(),
                want,
                "{peer:?}"
            );
        }
    }

    fn folder_title(text: &str) -> tl::enums::TextWithEntities {
        tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
            text: text.to_string(),
            entities: vec![],
        })
    }

    /// Маппинг всех вариантов DialogFilter: обычная папка — целиком,
    /// Default (основной список) — пропускается, Chatlist — без
    /// include-флагов и исключений.
    #[test]
    fn folder_dto_maps_every_dialog_filter_variant() {
        assert!(folder_dto(&tl::enums::DialogFilter::Default).is_none());

        let filter = tl::enums::DialogFilter::Filter(tl::types::DialogFilter {
            contacts: true,
            non_contacts: false,
            groups: true,
            broadcasts: false,
            bots: true,
            exclude_muted: true,
            exclude_read: false,
            exclude_archived: true,
            title_noanimate: false,
            id: 2,
            title: folder_title("Работа"),
            emoticon: Some("💼".into()),
            color: None,
            pinned_peers: vec![tl::enums::InputPeer::User(tl::types::InputPeerUser {
                user_id: 777_000,
                access_hash: 1,
            })],
            include_peers: vec![
                tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
                    channel_id: 1_234_567_890,
                    access_hash: 1,
                }),
                tl::enums::InputPeer::Empty, // мусор от сервера — молча пропускается
            ],
            exclude_peers: vec![tl::enums::InputPeer::Chat(tl::types::InputPeerChat {
                chat_id: 123,
            })],
        });
        let dto = folder_dto(&filter).expect("обычная папка маппится");
        assert_eq!(dto.id, 2);
        assert_eq!(dto.title, "Работа");
        assert_eq!(dto.emoticon.as_deref(), Some("💼"));
        assert_eq!(dto.pinned_ids, vec!["777000"]);
        assert_eq!(dto.include_ids, vec!["-1001234567890"]);
        assert_eq!(dto.exclude_ids, vec!["-123"]);
        assert!(dto.contacts && dto.groups && dto.bots);
        assert!(!dto.non_contacts && !dto.broadcasts);
        assert!(dto.exclude_muted && dto.exclude_archived);
        assert!(!dto.exclude_read);

        let chatlist = tl::enums::DialogFilter::Chatlist(tl::types::DialogFilterChatlist {
            has_my_invites: false,
            title_noanimate: false,
            id: 3,
            title: folder_title("Общая"),
            emoticon: None,
            color: None,
            pinned_peers: vec![],
            include_peers: vec![tl::enums::InputPeer::User(tl::types::InputPeerUser {
                user_id: 42,
                access_hash: 1,
            })],
        });
        let dto = folder_dto(&chatlist).expect("chatlist маппится");
        assert_eq!(dto.id, 3);
        assert_eq!(dto.title, "Общая");
        assert!(dto.emoticon.is_none());
        assert_eq!(dto.include_ids, vec!["42"]);
        assert!(dto.pinned_ids.is_empty() && dto.exclude_ids.is_empty());
        // У общей папки-ссылки нет include-флагов — все false.
        assert!(
            !dto.contacts
                && !dto.non_contacts
                && !dto.groups
                && !dto.broadcasts
                && !dto.bots
                && !dto.exclude_muted
                && !dto.exclude_read
                && !dto.exclude_archived
        );
    }

    /// FolderDto: Option-поле — явный null (инвариант №2), round-trip.
    #[test]
    fn folder_dto_serialization_follows_module_invariants() {
        let json = serde_json::to_value(FolderDto::default()).unwrap();
        assert!(json.get("emoticon").is_some());
        assert!(json["emoticon"].is_null());
        for key in [
            "id",
            "title",
            "pinned_ids",
            "include_ids",
            "exclude_ids",
            "contacts",
            "non_contacts",
            "groups",
            "broadcasts",
            "bots",
            "exclude_muted",
            "exclude_read",
            "exclude_archived",
        ] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
        }
        let dto = FolderDto {
            id: 7,
            title: "Личное".into(),
            emoticon: Some("🏠".into()),
            pinned_ids: vec!["777000".into()],
            ..FolderDto::default()
        };
        let back: FolderDto =
            serde_json::from_str(&serde_json::to_string(&dto).unwrap()).unwrap();
        assert_eq!(dto, back);
    }

    // ------------------------------------------------------------------
    // Наследование глобальных настроек уведомлений
    // ------------------------------------------------------------------

    /// Полная матрица наследования: пустые настройки берут дефолт категории,
    /// ЛЮБОЕ явное значение его перебивает — в обе стороны. Реальные случаи:
    /// «заглушить все личные чаты» с телефона (наследование) и точечный
    /// unmute одного чата поверх этого (перебивание).
    #[test]
    fn notify_muted_with_default_inherits_only_fully_empty_settings() {
        let now = 1_800_000_000_i64;
        let past = Some(1_700_000_000);
        let future = Some(1_900_000_000);
        /// Строка матрицы: mute_until, silent, глобальный дефолт, ожидание, метка.
        type MuteCase = (Option<i32>, Option<bool>, bool, bool, &'static str);
        let cases: [MuteCase; 14] = [
            (None, None, false, false, "пусто + дефолт «не глушить»"),
            (None, None, true, true, "пусто + глобальный mute ⇒ наследуем"),
            (past, None, true, false, "точечный unmute бьёт глобальный mute"),
            (Some(0), None, true, false, "mute_until=0 — тот же явный unmute"),
            (Some(0), None, false, false, "0 без дефолта — просто не заглушён"),
            (future, None, false, true, "явный mute при звонком дефолте"),
            (future, None, true, true, "явный mute поверх глобального"),
            (Some(i32::MAX), None, false, true, "«навсегда» без переполнения"),
            (Some(i32::MAX), None, true, true, "«навсегда» поверх глобального"),
            (None, Some(true), false, true, "silent=true глушит сам по себе"),
            (None, Some(true), true, true, "silent=true при глобальном mute"),
            (None, Some(false), true, false, "явный silent=false — уже не «пусто»"),
            (past, Some(true), false, true, "silent важнее протухшего mute_until"),
            (future, Some(false), true, true, "будущий mute_until глушит при silent=false"),
        ];
        for (mute_until, silent, default_muted, want, label) in cases {
            let settings = raw_notify(mute_until, silent);
            assert_eq!(
                notify_muted_with_default(&settings, now, default_muted),
                want,
                "{label} (mute_until={mute_until:?}, silent={silent:?}, default={default_muted})"
            );
        }
        // Старый маппер обязан остаться прежним: «оба None» — не заглушено.
        assert!(!notify_muted(&raw_notify(None, None), now));
    }

    /// Категории Telegram: user (в т.ч. бот) → users, группа И мегагруппа →
    /// chats, broadcast → broadcasts; неизвестный kind не глушим.
    #[test]
    fn notify_defaults_pick_the_category_of_the_peer() {
        let d = NotifyDefaults {
            users: true,
            chats: false,
            broadcasts: true,
        };
        assert!(d.for_kind("user"));
        assert!(!d.for_kind("group"));
        assert!(d.for_kind("channel"));
        assert!(!d.for_kind("forum_topic"), "неизвестный kind не глушим");

        assert!(d.for_peer_id(PeerId::user(777_000).unwrap()));
        assert!(!d.for_peer_id(PeerId::chat(123).unwrap()));
        // Задокументированное ограничение: по тегу id мегагруппа неотличима
        // от канала и получает дефолт broadcasts. Из-за этого from_raw_dialog
        // берёт kind из индекса peer'ов, а не из id.
        assert!(d.for_peer_id(PeerId::channel(123).unwrap()));

        let now = 1_800_000_000_i64;
        assert_eq!(
            NotifyDefaults::from_settings(
                &raw_notify(Some(i32::MAX), None), // все личные чаты заглушены
                &raw_notify(None, None),           // группы звонкие
                &raw_notify(Some(1), None),        // у каналов mute истёк
                now,
            ),
            NotifyDefaults {
                users: true,
                chats: false,
                broadcasts: false,
            }
        );
        assert_eq!(NotifyDefaults::default(), NotifyDefaults { users: false, chats: false, broadcasts: false });
    }

    /// Диалог с пустыми настройками наследует дефолт СВОЕЙ категории;
    /// строка-папка «Архив» не заглушается никогда.
    #[test]
    fn dialog_muted_with_defaults_resolves_per_category() {
        let now = 1_800_000_000_i64;
        let empty = || raw_notify(None, None);
        let users_only = NotifyDefaults {
            users: true,
            ..NotifyDefaults::default()
        };
        let chats_only = NotifyDefaults {
            chats: true,
            ..NotifyDefaults::default()
        };

        let user = raw_dialog(peer_user(777_000), 1, 0, false, None, empty());
        assert!(!dialog_muted(&user, now), "старый маппер дефолтов не знает");
        assert!(dialog_muted_with_defaults(&user, now, users_only));
        assert!(!dialog_muted_with_defaults(&user, now, chats_only));

        let group = raw_dialog(peer_chat(123), 1, 0, false, None, empty());
        assert!(dialog_muted_with_defaults(&group, now, chats_only));
        assert!(!dialog_muted_with_defaults(&group, now, users_only));

        // Мегагруппа: по тегу id — «канал» (дефолт broadcasts), по индексу —
        // «группа» (дефолт chats). Расхождение лечится явным kind'ом.
        let mega = raw_dialog(peer_channel(1_234_567_891), 1, 0, false, None, empty());
        assert!(!dialog_muted_with_defaults(&mega, now, chats_only));
        assert!(dialog_muted_with_defaults_for_kind(&mega, now, chats_only, "group"));

        // Точечный unmute переживает «заглушить всё».
        let all = NotifyDefaults {
            users: true,
            chats: true,
            broadcasts: true,
        };
        let unmuted = raw_dialog(peer_user(777_000), 1, 0, false, None, raw_notify(Some(0), None));
        assert!(!dialog_muted_with_defaults(&unmuted, now, all));

        assert!(
            !dialog_muted_with_defaults(&raw_folder_row(), now, all),
            "у строки-папки настроек уведомлений нет"
        );
    }

    /// `from_raw_dialog_with_defaults` берёт категорию из индекса: мегагруппа
    /// наследует дефолт групп, а не каналов.
    #[test]
    fn from_raw_dialog_inherits_defaults_by_indexed_kind() {
        let now = 1_800_000_000_i64;
        let index = RawPeerIndex::new(
            &[raw_user(777_000, Some("Иван"), None, None, Some(7))],
            &[
                raw_channel(1_234_567_890, "Новости", None, true, Some(42)),
                raw_channel(1_234_567_891, "Чат проекта", None, false, Some(43)),
            ],
        );
        let chats_only = NotifyDefaults {
            chats: true,
            ..NotifyDefaults::default()
        };
        let empty = || raw_notify(None, None);

        let mega = raw_dialog(peer_channel(1_234_567_891), 0, 0, false, None, empty());
        let dto = from_raw_dialog_with_defaults(&mega, None, &index, now, chats_only);
        assert_eq!(dto.kind, "group");
        assert!(dto.muted, "мегагруппа обязана наследовать дефолт групп");

        let broadcast = raw_dialog(peer_channel(1_234_567_890), 0, 0, false, None, empty());
        let dto = from_raw_dialog_with_defaults(&broadcast, None, &index, now, chats_only);
        assert_eq!(dto.kind, "channel");
        assert!(!dto.muted, "канал не должен ловить дефолт групп");

        // Без дефолтов поведение прежнее — регрессия существующих вызовов.
        let user = raw_dialog(peer_user(777_000), 0, 0, false, None, empty());
        assert!(!from_raw_dialog(&user, None, &index, now).muted);
        assert!(
            from_raw_dialog_with_defaults(
                &user,
                None,
                &index,
                now,
                NotifyDefaults {
                    users: true,
                    ..NotifyDefaults::default()
                }
            )
            .muted
        );
    }

    // ------------------------------------------------------------------
    // bot / contact
    // ------------------------------------------------------------------

    /// Флаги пользователя доезжают из сырого индекса в DTO; бот в контактах
    /// несёт ОБА флага — приоритет «бот важнее контакта» отдан фронту, здесь
    /// данные не искажаются.
    #[test]
    fn raw_peer_index_carries_bot_and_contact_into_dialog_dto() {
        let now = 1_800_000_000_i64;
        let users = vec![
            raw_user_flags(1001, "Бот", true, false, false),
            raw_user_flags(1002, "Друг", false, true, false),
            raw_user_flags(1003, "Взаимный", false, false, true),
            raw_user_flags(1004, "Незнакомец", false, false, false),
            raw_user_flags(1005, "Бот в контактах", true, true, false),
        ];
        let chats = vec![raw_small_chat(20, "Семья")];
        let index = RawPeerIndex::new(&users, &chats);

        let dialog_of = |peer| raw_dialog(peer, 0, 0, false, None, raw_notify(None, None));
        let dto = |peer| from_raw_dialog(&dialog_of(peer), None, &index, now);

        let bot = dto(peer_user(1001));
        assert!(bot.bot && !bot.contact);
        let friend = dto(peer_user(1002));
        assert!(!friend.bot && friend.contact);
        let mutual = dto(peer_user(1003));
        assert!(
            mutual.contact,
            "mutual_contact без contact — тоже контакт (односторонний список)"
        );
        let stranger = dto(peer_user(1004));
        assert!(!stranger.bot && !stranger.contact);
        let bot_contact = dto(peer_user(1005));
        assert!(bot_contact.bot && bot_contact.contact);

        let group = dto(peer_chat(20));
        assert!(!group.bot && !group.contact, "у группы флагов быть не может");

        // Peer, которого Telegram не прислал: флаги не выдумываются.
        let unknown = dto(peer_user(9999));
        assert!(!unknown.bot && !unknown.contact);
    }

    /// Контракт: ключи `bot`/`contact` присутствуют всегда и переживают
    /// round-trip (фронт читает их без проверки на undefined).
    #[test]
    fn dialog_dto_serializes_bot_and_contact() {
        let json = serde_json::to_value(DialogDto::default()).unwrap();
        for key in ["bot", "contact"] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
            assert_eq!(json[key], false, "{key} по умолчанию false");
        }
        let dto = DialogDto {
            id: "1001".into(),
            kind: "user".into(),
            bot: true,
            contact: true,
            ..DialogDto::default()
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"bot\":true"), "{json}");
        assert!(json.contains("\"contact\":true"), "{json}");
        let back: DialogDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }

    // ------------------------------------------------------------------
    // base64 для callback-payload'а
    // ------------------------------------------------------------------

    /// Векторы RFC 4648 фиксируют алфавит и padding, полный байтовый
    /// диапазон — устойчивость к не-UTF8 payload'ам (боты кладут в
    /// callback-data сырые структуры, а не текст).
    #[test]
    fn base64_round_trips_arbitrary_bytes_including_non_utf8() {
        for (raw, want) in [
            (&b""[..], ""),
            (&b"f"[..], "Zg=="),
            (&b"fo"[..], "Zm8="),
            (&b"foo"[..], "Zm9v"),
            (&b"foob"[..], "Zm9vYg=="),
            (&b"fooba"[..], "Zm9vYmE="),
            (&b"foobar"[..], "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(raw), want, "encode {raw:?}");
            assert_eq!(base64_decode(want).as_deref(), Some(raw), "decode {want}");
        }

        let all_bytes: Vec<u8> = (0..=255u8).collect();
        assert!(
            String::from_utf8(all_bytes.clone()).is_err(),
            "фикстура обязана быть не-UTF8"
        );
        let encoded = base64_encode(&all_bytes);
        assert!(encoded.is_ascii(), "результат обязан быть ASCII: {encoded}");
        assert!(
            encoded.contains('+') && encoded.contains('/') && encoded.ends_with('='),
            "standard alphabet с padding: {encoded}"
        );
        assert_eq!(base64_decode(&encoded), Some(all_bytes));

        // Терпимость входа: URL-safe символы, отсутствующий padding, переносы.
        assert_eq!(base64_decode("-_8="), base64_decode("+/8="));
        assert_eq!(base64_decode("Zm9vYmE").as_deref(), Some(&b"fooba"[..]));
        assert_eq!(base64_decode("Zm9v\nYmFy").as_deref(), Some(&b"foobar"[..]));

        // Мусор и обрубленная группа (остаток в один символ) — отказ.
        assert!(base64_decode("!!!").is_none());
        assert!(base64_decode("Zg~=").is_none());
        assert!(base64_decode("Z").is_none());
        assert!(base64_decode("Zm9vY").is_none());
    }

    // ------------------------------------------------------------------
    // Клавиатуры ботов
    // ------------------------------------------------------------------

    fn kb_row(buttons: Vec<tl::enums::KeyboardButton>) -> tl::enums::KeyboardButtonRow {
        tl::enums::KeyboardButtonRow::Row(tl::types::KeyboardButtonRow { buttons })
    }

    /// Все 18 конструкторов кнопок layer 227 + обе формы switch_inline.
    /// Порядок ветвей маппера — часть контракта: фронт рисует действие по
    /// `kind`, и «уехавшая» кнопка выглядит как сломанный бот.
    #[test]
    fn reply_button_dto_maps_every_known_constructor() {
        use tl::enums::KeyboardButton as B;
        let style = None;
        let buttons = vec![
            B::Button(tl::types::KeyboardButton {
                style: style.clone(),
                text: "простая".into(),
            }),
            B::Url(tl::types::KeyboardButtonUrl {
                style: style.clone(),
                text: "сайт".into(),
                url: "https://example.org".into(),
            }),
            B::Callback(tl::types::KeyboardButtonCallback {
                requires_password: true,
                style: style.clone(),
                text: "нажми".into(),
                data: vec![0x00, 0xff, 0x10],
            }),
            B::RequestPhone(tl::types::KeyboardButtonRequestPhone {
                style: style.clone(),
                text: "телефон".into(),
            }),
            B::RequestGeoLocation(tl::types::KeyboardButtonRequestGeoLocation {
                style: style.clone(),
                text: "гео".into(),
            }),
            B::SwitchInline(tl::types::KeyboardButtonSwitchInline {
                same_peer: false,
                style: style.clone(),
                text: "в другой чат".into(),
                query: "запрос".into(),
                peer_types: None,
            }),
            B::SwitchInline(tl::types::KeyboardButtonSwitchInline {
                same_peer: true,
                style: style.clone(),
                text: "сюда же".into(),
                query: "тот же запрос".into(),
                peer_types: None,
            }),
            B::Game(tl::types::KeyboardButtonGame {
                style: style.clone(),
                text: "играть".into(),
            }),
            B::Buy(tl::types::KeyboardButtonBuy {
                style: style.clone(),
                text: "купить".into(),
            }),
            B::UrlAuth(tl::types::KeyboardButtonUrlAuth {
                style: style.clone(),
                text: "войти".into(),
                fwd_text: None,
                url: "https://auth.example.org".into(),
                button_id: 1,
            }),
            B::InputKeyboardButtonUrlAuth(tl::types::InputKeyboardButtonUrlAuth {
                request_write_access: false,
                style: style.clone(),
                text: "войти (input)".into(),
                fwd_text: None,
                url: "https://auth2.example.org".into(),
                bot: tl::enums::InputUser::UserSelf,
            }),
            B::RequestPoll(tl::types::KeyboardButtonRequestPoll {
                style: style.clone(),
                quiz: Some(true),
                text: "опрос".into(),
            }),
            B::InputKeyboardButtonUserProfile(tl::types::InputKeyboardButtonUserProfile {
                style: style.clone(),
                text: "профиль (input)".into(),
                user_id: tl::enums::InputUser::UserSelf,
            }),
            B::UserProfile(tl::types::KeyboardButtonUserProfile {
                style: style.clone(),
                text: "профиль".into(),
                user_id: 777_000,
            }),
            B::WebView(tl::types::KeyboardButtonWebView {
                style: style.clone(),
                text: "приложение".into(),
                url: "https://app.example.org".into(),
            }),
            B::SimpleWebView(tl::types::KeyboardButtonSimpleWebView {
                style: style.clone(),
                text: "простое приложение".into(),
                url: "https://simple.example.org".into(),
            }),
            // Конструкторы, которых нет в словаре контракта: обязаны стать
            // "other" С сохранённым текстом, а не пропасть из ряда.
            B::RequestPeer(tl::types::KeyboardButtonRequestPeer {
                style: style.clone(),
                text: "выбрать чат".into(),
                button_id: 2,
                peer_type: tl::enums::RequestPeerType::User(tl::types::RequestPeerTypeUser {
                    bot: None,
                    premium: None,
                }),
                max_quantity: 1,
            }),
            B::InputKeyboardButtonRequestPeer(tl::types::InputKeyboardButtonRequestPeer {
                name_requested: false,
                username_requested: false,
                photo_requested: false,
                style: style.clone(),
                text: "выбрать чат (input)".into(),
                button_id: 3,
                peer_type: tl::enums::RequestPeerType::Chat(tl::types::RequestPeerTypeChat {
                    creator: false,
                    bot_participant: false,
                    has_username: None,
                    forum: None,
                    user_admin_rights: None,
                    bot_admin_rights: None,
                }),
                max_quantity: 1,
            }),
            B::Copy(tl::types::KeyboardButtonCopy {
                style,
                text: "скопировать".into(),
                copy_text: "секрет".into(),
            }),
        ];

        let markup = tl::enums::ReplyMarkup::ReplyInlineMarkup(tl::types::ReplyInlineMarkup {
            rows: vec![kb_row(buttons)],
        });
        let dto = reply_markup_dto(&markup);
        assert_eq!(dto.kind, "inline");
        assert_eq!(dto.rows.len(), 1);
        let row = &dto.rows[0];

        let kinds: Vec<&str> = row.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "text",
                "url",
                "callback",
                "request_phone",
                "request_location",
                "switch_inline",
                "switch_inline_current",
                "game",
                "buy",
                "login_url",
                "login_url",
                "request_poll",
                "user_profile",
                "user_profile",
                "web_view",
                "web_view",
                "other",
                "other",
                "other",
            ]
        );
        assert!(
            row.iter().all(|b| !b.text.is_empty()),
            "текст обязан сохраняться у ВСЕХ кнопок, включая other"
        );

        // Детали, на которые опирается фронт.
        let by_text = |text: &str| {
            row.iter()
                .find(|b| b.text == text)
                .unwrap_or_else(|| panic!("кнопка {text} потерялась"))
                .clone()
        };
        let cb = by_text("нажми");
        assert_eq!(cb.data.as_deref(), Some("AP8Q"), "callback-data в base64");
        assert_eq!(base64_decode(cb.data.as_deref().unwrap()), Some(vec![0x00, 0xff, 0x10]));
        assert!(cb.requires_password);
        assert!(cb.url.is_none() && cb.query.is_none());

        assert_eq!(by_text("сайт").url.as_deref(), Some("https://example.org"));
        assert_eq!(
            by_text("войти").url.as_deref(),
            Some("https://auth.example.org")
        );
        assert_eq!(
            by_text("приложение").url.as_deref(),
            Some("https://app.example.org")
        );
        assert_eq!(by_text("в другой чат").query.as_deref(), Some("запрос"));
        assert_eq!(by_text("сюда же").query.as_deref(), Some("тот же запрос"));
        assert_eq!(
            by_text("профиль").url.as_deref(),
            Some("tg://user?id=777000"),
            "user_profile отдаётся deep-link'ом"
        );
        assert!(
            by_text("профиль (input)").url.is_none(),
            "у входной формы нет id, из которого собрать ссылку"
        );
        // У неподдерживаемых кнопок нет ни payload'а, ни ссылки — фронт
        // рисует их неактивными.
        for text in ["выбрать чат", "скопировать"] {
            let b = by_text(text);
            assert!(b.data.is_none() && b.url.is_none() && b.query.is_none());
            assert!(!b.requires_password);
        }
    }

    /// Четыре контейнера разметки: флаги ставятся только там, где они есть в
    /// схеме, а `hide`/`force_reply` не несут кнопок вовсе.
    #[test]
    fn reply_markup_dto_maps_all_four_containers() {
        use tl::enums::ReplyMarkup as M;
        let button = |text: &str| {
            tl::enums::KeyboardButton::Button(tl::types::KeyboardButton {
                style: None,
                text: text.into(),
            })
        };

        let inline = reply_markup_dto(&M::ReplyInlineMarkup(tl::types::ReplyInlineMarkup {
            rows: vec![],
        }));
        assert_eq!(inline.kind, "inline");
        assert!(inline.rows.is_empty(), "пустая клавиатура — пустые ряды");
        assert!(!inline.resize && !inline.single_use && !inline.persistent);
        assert!(inline.placeholder.is_none(), "у inline placeholder'а нет");

        let keyboard = reply_markup_dto(&M::ReplyKeyboardMarkup(tl::types::ReplyKeyboardMarkup {
            resize: true,
            single_use: true,
            selective: true, // сознательно не переносится в DTO
            persistent: true,
            rows: vec![
                kb_row(vec![button("да"), button("нет")]),
                kb_row(vec![]), // пустой ряд от кривого бота — сохраняем форму
            ],
            placeholder: Some("выберите".into()),
        }));
        assert_eq!(keyboard.kind, "keyboard");
        assert!(keyboard.resize && keyboard.single_use && keyboard.persistent);
        assert_eq!(keyboard.placeholder.as_deref(), Some("выберите"));
        assert_eq!(keyboard.rows.len(), 2);
        assert_eq!(keyboard.rows[0].len(), 2);
        assert_eq!(keyboard.rows[0][1].kind, "text");
        assert!(keyboard.rows[1].is_empty());

        let hide = reply_markup_dto(&M::ReplyKeyboardHide(tl::types::ReplyKeyboardHide {
            selective: true,
        }));
        assert_eq!(hide.kind, "hide");
        assert!(hide.rows.is_empty());
        assert!(!hide.resize && !hide.single_use && !hide.persistent);
        assert!(hide.placeholder.is_none());

        let force = reply_markup_dto(&M::ReplyKeyboardForceReply(
            tl::types::ReplyKeyboardForceReply {
                single_use: true,
                selective: false,
                placeholder: Some("ответьте".into()),
            },
        ));
        assert_eq!(force.kind, "force_reply");
        assert!(force.rows.is_empty(), "force_reply не несёт кнопок");
        assert!(force.single_use);
        assert!(!force.resize && !force.persistent);
        assert_eq!(force.placeholder.as_deref(), Some("ответьте"));
    }

    /// `reply_markup` доезжает до `MessageDto` из сырого сообщения; у
    /// сервисных сообщений поля нет по схеме; форма JSON — по контракту.
    #[test]
    fn message_dto_carries_reply_markup_and_keeps_contract_shape() {
        let index = RawPeerIndex::new(&[raw_user_flags(1001, "Бот", true, false, false)], &[]);
        let mut msg = raw_message(7, peer_user(1001), Some(peer_user(1001)), "жми", 1_800_000_000);
        if let tl::enums::Message::Message(m) = &mut msg {
            m.reply_markup = Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(
                tl::types::ReplyInlineMarkup {
                    rows: vec![kb_row(vec![tl::enums::KeyboardButton::Callback(
                        tl::types::KeyboardButtonCallback {
                            requires_password: false,
                            style: None,
                            text: "ок".into(),
                            data: vec![b'o', b'k'],
                        },
                    )])],
                },
            ));
        }
        let dto = from_raw_message(&msg, &index).expect("сообщение маппится");
        let markup = dto.reply_markup.clone().expect("клавиатура обязана доехать");
        assert_eq!(markup.kind, "inline");
        assert_eq!(markup.rows[0][0].data.as_deref(), Some("b2s="));

        // Сообщение без разметки — явный null, а не отсутствующий ключ.
        let plain = raw_message(8, peer_user(1001), None, "текст", 1_800_000_001);
        let plain = from_raw_message(&plain, &index).unwrap();
        assert!(plain.reply_markup.is_none());
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("reply_markup").is_some() && json["reply_markup"].is_null());

        // Форма JSON клавиатуры и кнопки — дословно по контракту.
        let json = serde_json::to_value(&dto).unwrap();
        let markup_json = &json["reply_markup"];
        for key in ["kind", "rows", "resize", "single_use", "persistent", "placeholder"] {
            assert!(markup_json.get(key).is_some(), "ключ {key} обязан быть: {json}");
        }
        assert!(markup_json["placeholder"].is_null());
        let button_json = &markup_json["rows"][0][0];
        for key in ["text", "kind", "data", "url", "query", "requires_password"] {
            assert!(button_json.get(key).is_some(), "ключ {key} обязан быть: {json}");
        }
        assert_eq!(button_json["kind"], "callback");
        assert!(button_json["url"].is_null() && button_json["query"].is_null());

        // Round-trip: DTO не теряет клавиатуру на границе JSON.
        let back: MessageDto = serde_json::from_str(&serde_json::to_string(&dto).unwrap()).unwrap();
        assert_eq!(back, dto);

        // Сервисное сообщение: reply_markup отсутствует в схеме ⇒ null.
        let service = tl::enums::Message::Service(tl::types::MessageService {
            out: false,
            mentioned: false,
            media_unread: false,
            reactions_are_possible: false,
            silent: false,
            post: false,
            legacy: false,
            id: 9,
            from_id: None,
            peer_id: peer_user(1001),
            saved_peer_id: None,
            reply_to: None,
            date: 1_800_000_200,
            action: tl::enums::MessageAction::HistoryClear,
            reactions: None,
            ttl_period: None,
        });
        assert!(from_raw_message(&service, &index).unwrap().reply_markup.is_none());
    }

    // ------------------------------------------------------------------
    // Краевые случаи: наследование mute, флаги бота, клавиатуры-бомбы
    // ------------------------------------------------------------------

    /// Независимый «оракул» ожидаемого `muted`.
    ///
    /// Написан отдельно от продуктового кода СПЕЦИАЛЬНО: матрица ниже
    /// иначе сверяла бы реализацию сама с собой. Это дословная запись
    /// приоритетов контракта: «пусто целиком ⇒ дефолт категории; silent
    /// ⇒ глушим; иначе решает mute_until».
    fn expected_muted(
        mute_until: Option<i32>,
        silent: Option<bool>,
        now: i64,
        category_default: bool,
    ) -> bool {
        if silent.is_none() && mute_until.is_none() {
            return category_default;
        }
        if silent == Some(true) {
            return true;
        }
        match mute_until {
            Some(until) => i64::from(until) > now,
            None => false,
        }
    }

    /// ПОЛНАЯ матрица наследования: 8 комбинаций глобальных дефолтов × 4
    /// типа peer'а (ЛС, малая группа, мегагруппа, broadcast) × 6 состояний
    /// per-peer настроек — и всё через продуктовый путь
    /// [`from_raw_dialog_with_defaults`], а не через отдельные хелперы.
    ///
    /// Смысл матрицы: жалоба пользователя («тосты от заглушённых чатов»)
    /// возникает ровно тогда, когда категория для дефолта выбрана неверно.
    /// Особенно мегагруппа: по тегу `PeerId` она неотличима от канала, и
    /// категорию обязан давать индекс peer'ов.
    #[test]
    fn notify_inheritance_matrix_covers_every_kind_and_default() {
        let now = 1_800_000_000_i64;
        let index = RawPeerIndex::new(
            &[raw_user_flags(1001, "Бот", true, false, false)],
            &[
                raw_small_chat(20, "Семья"),
                // Мегагруппа: PeerKind::Channel, но категория Telegram — chats.
                raw_channel(1_234_567_891, "Чат проекта", None, false, Some(43)),
                raw_channel(1_234_567_890, "Новости", None, true, Some(42)),
            ],
        );
        let peers: [(tl::enums::Peer, &str); 4] = [
            (peer_user(1001), "user"),
            (peer_chat(20), "group"),
            (peer_channel(1_234_567_891), "group"),
            (peer_channel(1_234_567_890), "channel"),
        ];
        /// (mute_until, silent, метка) — все осмысленные состояния peer'а.
        type Row = (Option<i32>, Option<bool>, &'static str);
        let settings: [Row; 6] = [
            (None, None, "пусто ⇒ наследование"),
            (Some(0), None, "точечный unmute (0)"),
            (Some(1_700_000_000), None, "mute истёк"),
            (Some(1_900_000_000), None, "mute до будущего"),
            (Some(i32::MAX), None, "mute навсегда"),
            (None, Some(true), "silent"),
        ];

        for mask in 0..8_u8 {
            let defaults = NotifyDefaults {
                users: mask & 1 != 0,
                chats: mask & 2 != 0,
                broadcasts: mask & 4 != 0,
            };
            for (peer, want_kind) in &peers {
                for (mute_until, silent, label) in settings {
                    let raw = raw_dialog(
                        peer.clone(),
                        0,
                        0,
                        false,
                        None,
                        raw_notify(mute_until, silent),
                    );
                    let dto = from_raw_dialog_with_defaults(&raw, None, &index, now, defaults);
                    assert_eq!(&dto.kind, want_kind, "категория peer'а {peer:?}");
                    assert_eq!(
                        dto.muted,
                        expected_muted(mute_until, silent, now, defaults.for_kind(want_kind)),
                        "{label}: kind={want_kind} defaults={defaults:?}"
                    );
                }
            }
        }

        // Отдельно и явно: «заглушить все группы» обязано накрыть мегагруппу
        // и НЕ накрыть broadcast-канал. Через тег id всё было бы наоборот.
        let chats_only = NotifyDefaults {
            chats: true,
            ..NotifyDefaults::default()
        };
        let empty = || raw_notify(None, None);
        let mega = raw_dialog(peer_channel(1_234_567_891), 0, 0, false, None, empty());
        let cast = raw_dialog(peer_channel(1_234_567_890), 0, 0, false, None, empty());
        assert!(from_raw_dialog_with_defaults(&mega, None, &index, now, chats_only).muted);
        assert!(!from_raw_dialog_with_defaults(&cast, None, &index, now, chats_only).muted);
    }

    /// Часы и `mute_until` на границах типов: «навсегда» — это `i32::MAX`,
    /// а `now` бывает каким угодно (сбитые системные часы дают 0, тесты —
    /// границы `i64`). Ни одно сравнение не имеет права переполниться.
    #[test]
    fn mute_math_survives_extreme_clocks() {
        let clocks = [
            i64::MIN,
            i64::MIN + 1,
            -1,
            0,
            1_800_000_000,
            i64::from(i32::MAX) - 1,
            i64::from(i32::MAX),
            i64::MAX - 1,
            i64::MAX,
        ];
        let untils = [None, Some(i32::MIN), Some(-1), Some(0), Some(1), Some(i32::MAX)];
        for now in clocks {
            for until in untils {
                let settings = raw_notify(until, None);
                let want = match until {
                    Some(u) => i64::from(u) > now,
                    None => false,
                };
                assert_eq!(notify_muted(&settings, now), want, "now={now} until={until:?}");
                // Наследование арифметику не трогает: дефолт берётся ТОЛЬКО
                // при полностью пустых настройках.
                assert_eq!(
                    notify_muted_with_default(&settings, now, true),
                    if until.is_none() { true } else { want },
                    "дефолт=true, now={now} until={until:?}"
                );
                assert_eq!(
                    notify_muted_with_default(&settings, now, false),
                    want,
                    "дефолт=false, now={now} until={until:?}"
                );
                // Тот же расчёт через сырой диалог — и по тегу id, и по kind.
                let raw = raw_dialog(peer_user(777_000), 0, 0, false, None, raw_notify(until, None));
                assert_eq!(
                    dialog_muted_with_defaults(&raw, now, NotifyDefaults::default()),
                    want
                );
                assert_eq!(
                    dialog_muted_with_defaults_for_kind(
                        &raw,
                        now,
                        NotifyDefaults::default(),
                        "channel"
                    ),
                    want
                );
            }
        }

        // «Навсегда» перестаёт быть «навсегда» только за 2038-м годом — это
        // свойство самого Telegram (поле 32-битное), а не наша ошибка.
        assert!(notify_muted(&raw_notify(Some(i32::MAX), None), i64::from(i32::MAX) - 1));
        assert!(!notify_muted(&raw_notify(Some(i32::MAX), None), i64::MAX));

        // Глобальные дефолты считаются тем же сравнением ⇒ тоже без паники.
        let forever = raw_notify(Some(i32::MAX), None);
        assert_eq!(
            NotifyDefaults::from_settings(&forever, &forever, &forever, i64::MIN),
            NotifyDefaults {
                users: true,
                chats: true,
                broadcasts: true,
            }
        );
        assert_eq!(
            NotifyDefaults::from_settings(&forever, &forever, &forever, i64::MAX),
            NotifyDefaults::default()
        );
        // Отрицательные часы (система «до эпохи») не делают silent-настройку
        // менее заглушённой: silent вообще не зависит от времени.
        assert!(notify_muted(&raw_notify(None, Some(true)), i64::MAX));
    }

    /// «Бот» и «контакт» — свойства ПОЛЬЗОВАТЕЛЯ. Голые id у пользователей,
    /// малых групп и каналов лежат в независимых пространствах и спокойно
    /// совпадают — совпадение не имеет права «перекрасить» группу в бота
    /// (иначе папка «Боты» показала бы чат, а папка «Группы» — потеряла его).
    #[test]
    fn bot_and_contact_never_cross_peer_kinds() {
        let now = 1_800_000_000_i64;
        // Один и тот же голый id занят пользователем-ботом, малой группой и
        // каналом одновременно.
        let shared = 1001_i64;
        let index = RawPeerIndex::new(
            &[raw_user_flags(shared, "Бот", true, true, true)],
            &[
                raw_small_chat(shared, "Группа"),
                raw_channel(shared, "Канал", None, true, Some(9)),
            ],
        );
        let dto_of = |peer| {
            from_raw_dialog(
                &raw_dialog(peer, 0, 0, false, None, raw_notify(None, None)),
                None,
                &index,
                now,
            )
        };

        let user = dto_of(peer_user(shared));
        assert_eq!(user.kind, "user");
        assert!(user.bot && user.contact, "бот в контактах несёт оба флага");

        for peer in [peer_chat(shared), peer_channel(shared)] {
            let d = dto_of(peer);
            assert!(
                !d.bot && !d.contact,
                "{} с тем же голым id не может быть ботом",
                d.kind
            );
        }

        // Строка-папка указывает на ЧУЖОЙ peer, которого в индексе нет:
        // флаги не выдумываются даже под давлением (промах ⇒ false/false).
        let folder = from_raw_dialog(&raw_folder_row(), None, &index, now);
        assert!(!folder.bot && !folder.contact);

        // Абсурдное сочетание (канал, помеченный ботом) в природе не
        // встречается, но прилететь из чужого DTO может: расчёт категории
        // обязан идти по kind и не паниковать.
        let weird = DialogDto {
            id: "-1001234567890".into(),
            kind: "channel".into(),
            bot: true,
            contact: true,
            ..DialogDto::default()
        };
        let users_only = NotifyDefaults {
            users: true,
            ..NotifyDefaults::default()
        };
        let casts_only = NotifyDefaults {
            broadcasts: true,
            ..NotifyDefaults::default()
        };
        assert!(
            !users_only.for_kind(&weird.kind),
            "флаг бота не переводит канал в категорию личных чатов"
        );
        assert!(casts_only.for_kind(&weird.kind));
        // И такой DTO обязан пережить сериализацию (фронт разберётся сам).
        let back: DialogDto =
            serde_json::from_str(&serde_json::to_string(&weird).unwrap()).unwrap();
        assert_eq!(back, weird);
    }

    /// Callback-кнопка с заданным payload'ом.
    fn cb_button(text: &str, data: &[u8]) -> tl::enums::KeyboardButton {
        tl::enums::KeyboardButton::Callback(tl::types::KeyboardButtonCallback {
            requires_password: false,
            style: None,
            text: text.into(),
            data: data.to_vec(),
        })
    }

    /// Клавиатура-бомба и вырожденные кнопки.
    ///
    /// Маппер обязан сохранять ФОРМУ разметки один в один: фронт сам решает,
    /// что рисовать и что обрезать (у него на это есть `MAX_ROWS` и
    /// `MAX_ROW_BUTTONS`), а «съеденная» на сервере кнопка выглядит как
    /// сломанный бот и отладке не поддаётся.
    #[test]
    fn reply_markup_survives_hostile_keyboards() {
        // 100 кнопок в одном ряду.
        let wide = kb_row(
            (0..100_u8)
                .map(|i| cb_button(&format!("к{i}"), &[i]))
                .collect(),
        );
        let markup = reply_markup_dto(&tl::enums::ReplyMarkup::ReplyInlineMarkup(
            tl::types::ReplyInlineMarkup {
                rows: vec![wide],
            },
        ));
        assert_eq!(markup.rows.len(), 1);
        assert_eq!(markup.rows[0].len(), 100, "ни одна кнопка не потеряна");
        assert!(markup.rows[0].iter().all(|b| b.kind == "callback"));
        // Payload'ы не перепутаны местами.
        for (i, button) in markup.rows[0].iter().enumerate() {
            assert_eq!(
                base64_decode(button.data.as_deref().unwrap()),
                Some(vec![i as u8]),
                "кнопка {i}"
            );
        }

        // 100 рядов по кнопке + пустые ряды между ними: пустой ряд сохраняет
        // форму (бот прислал его осознанно — это вертикальный отступ).
        let mut rows = Vec::new();
        for i in 0..100_u8 {
            rows.push(kb_row(vec![cb_button(&format!("р{i}"), &[i])]));
            rows.push(kb_row(vec![]));
        }
        let tall = reply_markup_dto(&tl::enums::ReplyMarkup::ReplyInlineMarkup(
            tl::types::ReplyInlineMarkup { rows },
        ));
        assert_eq!(tall.rows.len(), 200);
        assert_eq!(tall.rows.iter().filter(|r| r.is_empty()).count(), 100);

        // Пустая и пробельная подпись: сервер её НЕ чинит и НЕ выбрасывает —
        // решение «кнопку без подписи не рисуем» принимает фронт
        // (botkb.js::normalizeButton), и данные для этого решения нужны целые.
        let degenerate = reply_markup_dto(&tl::enums::ReplyMarkup::ReplyKeyboardMarkup(
            tl::types::ReplyKeyboardMarkup {
                resize: false,
                single_use: false,
                selective: false,
                persistent: false,
                rows: vec![kb_row(vec![
                    tl::enums::KeyboardButton::Button(tl::types::KeyboardButton {
                        style: None,
                        text: String::new(),
                    }),
                    tl::enums::KeyboardButton::Button(tl::types::KeyboardButton {
                        style: None,
                        text: "   ".into(),
                    }),
                    // Кнопка без payload'а: `data` — обязательное поле схемы,
                    // но ничто не мешает боту прислать ноль байт.
                    cb_button("пусто", &[]),
                ])],
                placeholder: None,
            },
        ));
        let row = &degenerate.rows[0];
        assert_eq!(row.len(), 3);
        assert_eq!(row[0].text, "");
        assert_eq!(row[1].text, "   ");
        assert_eq!(
            row[2].data.as_deref(),
            Some(""),
            "ноль байт кодируются в пустую строку, а не в null"
        );

        // Payload на пределе Telegram: 64 не-UTF8 байта обязаны пережить
        // «Telegram → base64 → JSON» без искажения и без обрезки.
        let payload: Vec<u8> = (0..64_u8).map(|i| i.wrapping_mul(7).wrapping_add(200)).collect();
        assert!(String::from_utf8(payload.clone()).is_err(), "фикстура не-UTF8");
        let limit = reply_markup_dto(&tl::enums::ReplyMarkup::ReplyInlineMarkup(
            tl::types::ReplyInlineMarkup {
                rows: vec![kb_row(vec![cb_button("предел", &payload)])],
            },
        ));
        let encoded = limit.rows[0][0].data.clone().unwrap();
        assert!(encoded.is_ascii(), "base64 обязан быть ASCII: {encoded}");
        assert_eq!(base64_decode(&encoded), Some(payload));

        // Неизвестный конструктор в ОДНОМ ряду с рабочими: он не имеет права
        // ни «съесть» соседей, ни сдвинуть их — ряд рисуется целиком.
        let mixed = reply_markup_dto(&tl::enums::ReplyMarkup::ReplyInlineMarkup(
            tl::types::ReplyInlineMarkup {
                rows: vec![kb_row(vec![
                    cb_button("до", b"1"),
                    tl::enums::KeyboardButton::Copy(tl::types::KeyboardButtonCopy {
                        style: None,
                        text: "скопировать".into(),
                        copy_text: "секрет".into(),
                    }),
                    cb_button("после", b"2"),
                ])],
            },
        ));
        let kinds: Vec<&str> = mixed.rows[0].iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds, vec!["callback", "other", "callback"]);
        assert_eq!(mixed.rows[0][1].text, "скопировать");
        assert!(
            mixed.rows[0][1].data.is_none(),
            "у неподдержанной кнопки нет payload'а — нажимать нечего"
        );
    }

    /// Строка «Архив» (`dialogFolder`) прикидывается чужим диалогом.
    ///
    /// Это и есть мина, ради которой появился [`is_folder_row`]: её `peer` —
    /// верхнее сообщение архива, то есть НАСТОЯЩИЙ, лежащий в архиве и
    /// (обычно) заглушённый чат. Смапленная как диалог, строка отдаёт про
    /// него ровно противоположные флаги: `archived = false`, `muted = false`.
    /// Дальше эти флаги уезжают в серверный кэш и возвращают исходный дефект
    /// («тосты от чатов, которые пользователь спрятал»).
    #[test]
    fn folder_row_impersonates_the_dialog_it_points_at() {
        let now = 1_800_000_000_i64;
        let index = RawPeerIndex::new(
            &[raw_user_flags(777_000, "Telegram", false, false, false)],
            &[],
        );
        let folder = raw_folder_row();
        assert!(is_folder_row(&folder), "строку-папку обязан ловить предикат");

        // Настоящий диалог того же peer'а: он в архиве и заглушён навсегда.
        let real = raw_dialog(
            peer_user(777_000),
            5,
            3,
            false,
            Some(1),
            raw_notify(Some(i32::MAX), None),
        );
        assert!(!is_folder_row(&real));
        let real_dto = from_raw_dialog(&real, None, &index, now);
        assert!(real_dto.muted && real_dto.archived);

        // А строка-папка выдаёт про НЕГО ЖЕ противоположное.
        let ghost = from_raw_dialog(&folder, None, &index, now);
        assert_eq!(ghost.id, real_dto.id, "тот же dialog_id — отсюда и подмена");
        assert!(!ghost.muted, "у dialogFolder нет notify_settings");
        assert!(!ghost.archived, "у dialogFolder нет folder_id");
        assert_eq!(ghost.unread_count, 0);
    }

    // ------------------------------------------------------------------
    // Цитаты (reply_to)
    // ------------------------------------------------------------------

    /// Header ответа на сообщение того же диалога; всё остальное —
    /// «выключено», как приходит в самом частом случае.
    fn reply_header(reply_to_msg_id: Option<i32>) -> tl::types::MessageReplyHeader {
        tl::types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: false,
            quote: false,
            reply_to_ephemeral: false,
            reply_to_msg_id,
            reply_to_peer_id: None,
            reply_from: None,
            reply_media: None,
            reply_to_top_id: None,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            todo_item_id: None,
            poll_option: None,
        }
    }

    /// Сообщение диалога в проекции DTO — источник превью для обогащения.
    fn dto_msg(id: i32, out: bool, text: &str, sender: Option<(&str, &str)>) -> MessageDto {
        MessageDto {
            id,
            dialog_id: "777000".into(),
            out,
            date: 1_800_000_000,
            sender: sender.map(|(uid, name)| TgUserDto {
                id: uid.into(),
                username: None,
                first_name: Some(name.into()),
                phone: None,
            }),
            text: text.into(),
            ..MessageDto::default()
        }
    }

    /// Сообщение-ответ с уже разобранным header'ом (превью не добито).
    fn dto_reply(id: i32, reply_to_msg_id: i32) -> MessageDto {
        let mut msg = dto_msg(id, false, "ответ", Some(("777000", "Иван")));
        msg.reply_to_id = Some(reply_to_msg_id);
        msg.reply_to = reply_to_from_header(&tl::enums::MessageReplyHeader::Header(reply_header(
            Some(reply_to_msg_id),
        )));
        msg
    }

    /// Сообщение без header'а не получает цитату вовсе — ни в клиентской
    /// проекции, ни в сырой.
    #[test]
    fn message_without_reply_header_has_no_quote() {
        let index = RawPeerIndex::new(&[], &[]);
        let msg = raw_message(42, peer_user(777_000), None, "привет", 1_800_000_000);
        let dto = from_raw_message(&msg, &index).unwrap();
        assert_eq!(dto.reply_to_id, None);
        assert_eq!(dto.reply_to, None, "цитаты нет — поле null");

        // И пустой header без id тоже: показывать нечего.
        let empty =
            reply_to_from_header(&tl::enums::MessageReplyHeader::Header(reply_header(None)));
        assert_eq!(empty, None);
    }

    /// Разбор header'а даёт только id: автор и текст оригинала приезжают
    /// отдельным проходом, и до него цитата помечена «нужно превью».
    #[test]
    fn reply_header_yields_id_and_defers_the_preview() {
        let index = RawPeerIndex::new(&[], &[]);
        let mut msg = raw_message(42, peer_user(777_000), None, "ответ", 1_800_000_000);
        if let tl::enums::Message::Message(m) = &mut msg {
            m.reply_to = Some(tl::enums::MessageReplyHeader::Header(reply_header(Some(
                41,
            ))));
        }
        let dto = from_raw_message(&msg, &index).unwrap();
        let reply = dto.reply_to.clone().expect("цитата разобрана");
        assert_eq!(reply.message_id, Some(41));
        assert_eq!(reply.out, None, "out выставляет только обогащение");
        assert!(!reply.unavailable);
        assert_eq!(reply.sender, None);
        assert_eq!(reply.text, None);
        assert!(reply_needs_preview(&reply));
        assert_eq!(
            dto.reply_to_id,
            Some(41),
            "старое поле осталось прежним — контракт фронта"
        );
    }

    /// Оригинал не достался (удалён) — цитата честно помечена, а не
    /// нарисована пустой строкой.
    #[test]
    fn missing_original_is_marked_unavailable() {
        let mut page = vec![dto_reply(42, 41)];
        enrich_reply_previews(&mut page, &HashMap::new());
        let reply = page[0].reply_to.clone().unwrap();
        assert!(reply.unavailable, "оригинала нет — фронт рисует «удалено»");
        assert_eq!(reply.message_id, Some(41), "id нужен фронту как есть");
        assert_eq!(reply.text, None);
        assert_eq!(reply.sender, None);
        assert!(
            !reply_needs_preview(&reply),
            "повторный проход не должен снова идти в сеть"
        );

        // Одиночная форма ведёт себя так же.
        let mut single = dto_reply(42, 41);
        enrich_reply_preview(&mut single, None);
        assert!(single.reply_to.unwrap().unavailable);
    }

    /// Текстовый оригинал: имя автора, превью и `out` приезжают из него.
    #[test]
    fn text_original_fills_author_and_preview() {
        let mut page = vec![
            dto_msg(41, true, "оригинал\nв две строки", None),
            dto_reply(42, 41),
        ];
        enrich_reply_previews(&mut page, &HashMap::new());
        let reply = page[1].reply_to.clone().unwrap();
        assert_eq!(
            reply.text.as_deref(),
            Some("оригинал в две строки"),
            "перевод строки схлопывается в пробел"
        );
        assert_eq!(reply.out, Some(true), "оригинал — наше сообщение");
        assert!(!reply.unavailable);
        assert_eq!(
            reply.sender, None,
            "у своих сообщений в личке Telegram не присылает from_id"
        );

        // Оригинал, добранный батчем, а не найденный на странице.
        let mut page = vec![dto_reply(42, 41)];
        let originals =
            HashMap::from([(41, dto_msg(41, false, "оригинал", Some(("777000", "Иван"))))]);
        enrich_reply_previews(&mut page, &originals);
        let reply = page[0].reply_to.clone().unwrap();
        assert_eq!(reply.sender.as_deref(), Some("Иван"));
        assert_eq!(reply.sender_id.as_deref(), Some("777000"));
        assert_eq!(reply.text.as_deref(), Some("оригинал"));
        assert_eq!(reply.out, Some(false));
    }

    /// Оригинал без текста, но с медиа: `text` остаётся null, вид
    /// вложения заполнен — фронту хватит на пометку «[photo]».
    #[test]
    fn media_original_without_text_keeps_text_null() {
        let mut original = dto_msg(41, false, "", Some(("777000", "Иван")));
        original.media = Some(MediaDto {
            kind: "photo".into(),
            ..MediaDto::default()
        });
        let mut page = vec![dto_reply(42, 41)];
        enrich_reply_previews(&mut page, &HashMap::from([(41, original)]));
        let reply = page[0].reply_to.clone().unwrap();
        assert_eq!(
            reply.text, None,
            "пустой текст — именно null, не пустая строка"
        );
        assert_eq!(reply.media_kind.as_deref(), Some("photo"));
        assert!(!reply.unavailable);
    }

    /// Длинный текст режется по code point'ам: ровно
    /// [`REPLY_PREVIEW_CHARS`] символов вместе с многоточием, и ни один
    /// эмодзи не разрублен пополам.
    #[test]
    fn long_preview_is_cut_by_code_points() {
        let short = "a".repeat(REPLY_PREVIEW_CHARS);
        let cut = reply_preview_text(&short).unwrap();
        assert_eq!(cut, short, "ровно по лимиту — не режем");

        let long = "a".repeat(REPLY_PREVIEW_CHARS + 1);
        let cut = reply_preview_text(&long).unwrap();
        assert_eq!(cut.chars().count(), REPLY_PREVIEW_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(
            &cut[..REPLY_PREVIEW_CHARS - 1],
            &"a".repeat(REPLY_PREVIEW_CHARS - 1)
        );

        // Каждый эмодзи — один code point, но четыре байта: побайтовая
        // обрезка развалила бы его на мусор.
        let emoji = "🦀".repeat(REPLY_PREVIEW_CHARS + 10);
        let cut = reply_preview_text(&emoji).unwrap();
        assert_eq!(cut.chars().count(), REPLY_PREVIEW_CHARS);
        assert_eq!(
            cut,
            format!("{}…", "🦀".repeat(REPLY_PREVIEW_CHARS - 1)),
            "суррогатные пары целы"
        );
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());

        // Граница: 139 «a» + эмодзи — ровно лимит, обрезки нет.
        let edge = format!("{}🦀", "a".repeat(REPLY_PREVIEW_CHARS - 1));
        assert_eq!(reply_preview_text(&edge).as_deref(), Some(edge.as_str()));

        // Пробельный мусор не превращается в «пустую» цитату.
        assert_eq!(reply_preview_text("   \n\t "), None);
        assert_eq!(reply_preview_text(""), None);
        assert_eq!(
            reply_preview_text(" один   два \n"),
            Some("один два".into())
        );
    }

    /// Процитированный фрагмент едет из header'а ответа и переживает
    /// удаление оригинала.
    #[test]
    fn quote_text_survives_a_deleted_original() {
        let mut header = reply_header(Some(41));
        header.quote = true;
        header.quote_text = Some("важный кусок".into());
        let reply = reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header)).unwrap();
        assert_eq!(reply.quote.as_deref(), Some("важный кусок"));
        assert_eq!(reply.message_id, Some(41));

        let mut msg = dto_msg(42, false, "ответ", None);
        msg.reply_to = Some(reply);
        enrich_reply_preview(&mut msg, None);
        let reply = msg.reply_to.unwrap();
        assert!(reply.unavailable);
        assert_eq!(
            reply.quote.as_deref(),
            Some("важный кусок"),
            "фрагмент пришёл с ответом, а не с оригиналом — стирать его нечем"
        );
    }

    /// Ответ на сторис: сообщения-оригинала не существует в природе.
    #[test]
    fn story_reply_has_no_message_and_is_unavailable() {
        let header = tl::enums::MessageReplyHeader::MessageReplyStoryHeader(
            tl::types::MessageReplyStoryHeader {
                peer: peer_user(777_000),
                story_id: 7,
            },
        );
        let reply = reply_to_from_header(&header).expect("ответ на сторис — всё же ответ");
        assert_eq!(reply.message_id, None, "story_id не адресует сообщение");
        assert!(reply.unavailable);
        assert_eq!(reply.text, None);
        assert!(
            !reply_needs_preview(&reply),
            "запрашивать нечего — id сообщения нет"
        );
    }

    /// Сообщение форум-топика НЕ ответ, хотя header у него есть: id топика
    /// Telegram кладёт в то же поле. Старое `reply_to_id` этого не знает —
    /// расхождение полей осознанное.
    #[test]
    fn forum_topic_marker_is_not_a_reply() {
        let mut header = reply_header(Some(100));
        header.forum_topic = true;
        assert_eq!(
            reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header.clone())),
            None,
            "id топика — не цитата"
        );

        // Настоящий ответ ВНУТРИ топика отличается наличием top_id.
        header.reply_to_top_id = Some(100);
        header.reply_to_msg_id = Some(142);
        let reply = reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header)).unwrap();
        assert_eq!(reply.message_id, Some(142));
    }

    /// Ответ на сообщение ЧУЖОГО чата: превью собирается из header'а, а
    /// `message_id` не отдаётся — в нашем диалоге под этим номером лежит
    /// постороннее сообщение.
    #[test]
    fn cross_chat_reply_uses_header_data_and_hides_message_id() {
        let mut header = reply_header(Some(41));
        header.reply_to_peer_id = Some(peer_channel(1_234));
        header.reply_from = Some(tl::enums::MessageFwdHeader::Header(
            tl::types::MessageFwdHeader {
                imported: false,
                saved_out: false,
                from_id: Some(peer_user(777_000)),
                from_name: Some("Иван".into()),
                date: 1_800_000_000,
                channel_post: None,
                post_author: None,
                saved_from_peer: None,
                saved_from_msg_id: None,
                saved_from_id: None,
                saved_from_name: None,
                saved_date: None,
                psa_type: None,
            },
        ));
        header.reply_media = Some(tl::enums::MessageMedia::Photo(
            tl::types::MessageMediaPhoto {
                spoiler: false,
                live_photo: false,
                photo: Some(tl::enums::Photo::Empty(tl::types::PhotoEmpty { id: 1 })),
                ttl_seconds: None,
                video: None,
            },
        ));
        let reply =
            reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header.clone())).unwrap();
        assert_eq!(
            reply.message_id, None,
            "чужой id в нашем диалоге — чужое сообщение"
        );
        assert_eq!(reply.sender.as_deref(), Some("Иван"));
        assert_eq!(reply.sender_id.as_deref(), Some("777000"));
        assert_eq!(reply.media_kind.as_deref(), Some("photo"));
        assert_eq!(reply.text, None, "текст чужого чата Telegram не присылает");
        assert!(!reply.unavailable);
        assert!(
            !reply_needs_preview(&reply),
            "батчить нечего: оригинал в другом диалоге"
        );

        // Ни автора, ни медиа — показывать нечего, честно говорим об этом.
        let mut bare = header;
        bare.reply_from = None;
        bare.reply_media = None;
        let reply = reply_to_from_header(&tl::enums::MessageReplyHeader::Header(bare)).unwrap();
        assert!(reply.unavailable);
    }

    /// В батч уходят только те оригиналы, которых нет на странице, по
    /// одному разу и в порядке первого упоминания.
    #[test]
    fn fetch_list_is_deduped_and_skips_the_page_itself() {
        let mut page = vec![
            dto_msg(40, false, "старое", None),
            dto_reply(41, 40), // оригинал прямо на странице
            dto_reply(42, 7),
            dto_reply(43, 7), // тот же оригинал
            dto_reply(44, 5),
            dto_msg(45, false, "без цитаты", None),
        ];
        assert_eq!(reply_ids_to_fetch(&page), vec![7, 5]);

        // После обогащения список пуст — второй прогон не ходит в сеть.
        let originals = HashMap::from([(7, dto_msg(7, false, "семь", None))]);
        enrich_reply_previews(&mut page, &originals);
        assert_eq!(reply_ids_to_fetch(&page), Vec::<i32>::new());
        assert_eq!(
            page[1].reply_to.as_ref().unwrap().text.as_deref(),
            Some("старое"),
            "оригинал со страницы подставлен без запроса"
        );
        assert!(page[4].reply_to.as_ref().unwrap().unavailable);
    }

    /// Чужой оригинал (id не совпал) не подставляется: цитата показала бы
    /// посторонний текст.
    #[test]
    fn single_enrichment_rejects_a_mismatched_original() {
        let mut msg = dto_reply(42, 41);
        let other = dto_msg(40, false, "не тот", None);
        enrich_reply_preview(&mut msg, Some(&other));
        let reply = msg.reply_to.unwrap();
        assert!(reply.unavailable);
        assert_eq!(reply.text, None);
    }

    /// Инвариант №2 для цитаты: порядок ключей — контракт, все `Option`
    /// сериализуются явным `null`.
    #[test]
    fn reply_to_serializes_every_field_explicitly() {
        let json = serde_json::to_string(&ReplyToDto::default()).unwrap();
        assert_eq!(
            json,
            r#"{"message_id":null,"sender":null,"sender_id":null,"text":null,"media_kind":null,"quote":null,"out":null,"unavailable":false}"#
        );

        let full = ReplyToDto {
            message_id: Some(41),
            sender: Some("Иван".into()),
            sender_id: Some("777000".into()),
            text: Some("оригинал".into()),
            media_kind: Some("photo".into()),
            quote: Some("кусок".into()),
            out: Some(true),
            unavailable: false,
        };
        let json = serde_json::to_value(&full).unwrap();
        for key in [
            "message_id",
            "sender",
            "sender_id",
            "text",
            "media_kind",
            "quote",
            "out",
            "unavailable",
        ] {
            assert!(json.get(key).is_some(), "ключ {key} обязан присутствовать");
        }
        assert_eq!(json["sender_id"], "777000", "id автора — строка");
        let back: ReplyToDto = serde_json::from_value(json).unwrap();
        assert_eq!(back, full);
    }

    // ------------------------------------------------------------------
    // Цитаты: краевые случаи оригинала
    // ------------------------------------------------------------------

    /// Оригинал с вложением: вид медиа доезжает до цитаты как есть, а
    /// подпись под ним работает превью-текстом.
    ///
    /// Словарь `media_kind` в контракте закрыт (photo|video|document|
    /// sticker|voice|audio|other) ровно потому, что фронт выбирает по нему
    /// пометку в цитате: любое «своё» значение нарисуется пустым местом.
    /// Поэтому обогащение обязано ПЕРЕКЛАДЫВАТЬ `kind` оригинала, а не
    /// придумывать собственную классификацию.
    #[test]
    fn every_media_kind_and_its_caption_reach_the_quote() {
        for kind in [
            "photo", "video", "document", "sticker", "voice", "audio", "other",
        ] {
            // Только вложение, подписи нет: фронту хватит пометки.
            let mut bare = dto_msg(41, false, "", Some(("777000", "Иван")));
            bare.media = Some(MediaDto {
                kind: kind.into(),
                ..MediaDto::default()
            });
            let mut page = vec![dto_reply(42, 41)];
            enrich_reply_previews(&mut page, &HashMap::from([(41, bare)]));
            let reply = page[0].reply_to.clone().unwrap();
            assert_eq!(reply.media_kind.as_deref(), Some(kind));
            assert_eq!(reply.text, None, "{kind}: подписи не было");
            assert!(!reply.unavailable, "{kind}");

            // Вложение с подписью: нужны ОБА поля — пометка и текст.
            let mut captioned = dto_msg(41, false, "подпись\nпод файлом", Some(("777000", "Иван")));
            captioned.media = Some(MediaDto {
                kind: kind.into(),
                ..MediaDto::default()
            });
            let mut page = vec![dto_reply(42, 41)];
            enrich_reply_previews(&mut page, &HashMap::from([(41, captioned)]));
            let reply = page[0].reply_to.clone().unwrap();
            assert_eq!(reply.media_kind.as_deref(), Some(kind), "{kind}");
            assert_eq!(
                reply.text.as_deref(),
                Some("подпись под файлом"),
                "{kind}: подпись — обычное превью, с теми же правилами"
            );
            assert_eq!(reply.sender.as_deref(), Some("Иван"), "{kind}");
        }
    }

    /// Сырой документ с заданными атрибутами — вход настоящего маппера медиа.
    fn raw_document_media(
        mime: &str,
        attrs: Vec<tl::enums::DocumentAttribute>,
    ) -> tl::enums::MessageMedia {
        tl::enums::MessageMedia::Document(tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: false,
            round: false,
            voice: false,
            document: Some(tl::enums::Document::Document(tl::types::Document {
                id: 1,
                access_hash: 2,
                file_reference: Vec::new(),
                date: 1_800_000_000,
                mime_type: mime.into(),
                size: 1024,
                thumbs: None,
                video_thumbs: None,
                dc_id: 2,
                attributes: attrs,
            })),
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        })
    }

    /// Стикер и голосовое как оригинал цитаты — через НАСТОЯЩИЙ маппер
    /// медиа, а не через руками собранный [`MediaDto`].
    ///
    /// У обоих MIME врёт: у голосового `audio/ogg` (без флага `voice` это
    /// «музыка»), у стикера `image/webp` (без атрибута sticker — «фото»).
    /// Пометку в цитате даёт цепочка `Media::from_raw` → [`media_dto`] →
    /// разбор превью, и тест сторожит именно её: подмена любого звена
    /// покажет в цитате «🎵» вместо «🎤».
    #[test]
    fn voice_and_sticker_originals_keep_the_kind_of_the_real_mapper() {
        let index = RawPeerIndex::new(&[], &[]);
        let voice = tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
            voice: true,
            duration: 7,
            title: None,
            performer: None,
            waveform: None,
        });
        let music = tl::enums::DocumentAttribute::Audio(tl::types::DocumentAttributeAudio {
            voice: false,
            duration: 180,
            title: Some("трек".into()),
            performer: None,
            waveform: None,
        });
        let sticker = tl::enums::DocumentAttribute::Sticker(tl::types::DocumentAttributeSticker {
            mask: false,
            alt: "🐸".into(),
            stickerset: tl::enums::InputStickerSet::Empty,
            mask_coords: None,
        });
        let video = tl::enums::DocumentAttribute::Video(tl::types::DocumentAttributeVideo {
            round_message: true,
            supports_streaming: false,
            nosound: false,
            duration: 3.5,
            w: 384,
            h: 384,
            preload_prefix_size: None,
            video_start_ts: None,
            video_codec: None,
        });
        let file = tl::enums::DocumentAttribute::Filename(tl::types::DocumentAttributeFilename {
            file_name: "смета.pdf".into(),
        });

        for (attr, mime, expected) in [
            (voice, "audio/ogg", "voice"),
            (music, "audio/mpeg", "audio"),
            (sticker, "image/webp", "sticker"),
            (video, "video/mp4", "video"),
            (file, "application/pdf", "document"),
        ] {
            let mut raw = raw_message(41, peer_user(777_000), None, "", 1_800_000_000);
            if let tl::enums::Message::Message(m) = &mut raw {
                m.media = Some(raw_document_media(mime, vec![attr]));
            }
            let original = from_raw_message(&raw, &index).expect("оригинал существует");
            assert_eq!(
                original.media.as_ref().map(|m| m.kind.as_str()),
                Some(expected)
            );

            let mut page = vec![dto_reply(42, 41)];
            enrich_reply_previews(&mut page, &HashMap::from([(41, original)]));
            let reply = page[0].reply_to.clone().unwrap();
            assert_eq!(
                reply.media_kind.as_deref(),
                Some(expected),
                "цитата обязана повторять пометку строки оригинала"
            );
            assert_eq!(reply.text, None, "{expected}: подписи не было");
            assert!(!reply.unavailable, "{expected}");
        }
    }

    /// Удалённый оригинал не превращается в пустую цитату.
    ///
    /// На удалённый id Telegram отвечает не пустотой, а `messageEmpty`:
    /// объект есть, но текста и автора у него нет. Пропусти его — и вместо
    /// «сообщение удалено» фронт нарисует безымянную пустую строку, по
    /// которой ещё и предложит перейти. Сырой мостик обязан отсеивать его
    /// так же, как [`message_exists`] отсеивает клиентский.
    #[test]
    fn deleted_original_never_becomes_an_empty_preview() {
        let index = RawPeerIndex::new(&[], &[]);
        let empty = tl::enums::Message::Empty(tl::types::MessageEmpty {
            id: 41,
            peer_id: Some(peer_user(777_000)),
        });
        assert_eq!(
            from_raw_message(&empty, &index),
            None,
            "messageEmpty — дыра истории, а не сообщение"
        );

        // Значит, в карту оригиналов он не попадёт ни при каком раскладе,
        // и цитата честно помечается недоступной.
        let mut page = vec![dto_reply(42, 41)];
        enrich_reply_previews(&mut page, &HashMap::new());
        let reply = page[0].reply_to.clone().unwrap();
        assert!(reply.unavailable);
        assert_eq!(
            (
                reply.sender.clone(),
                reply.text.clone(),
                reply.media_kind.clone(),
                reply.out
            ),
            (None, None, None, None),
            "ни одного поля из «пустого» оригинала"
        );
        assert_eq!(
            reply.message_id,
            Some(41),
            "id остаётся: строка «удалено» рисуется на своём месте"
        );
    }

    /// Сервисное сообщение («вошёл в группу», «сменил фото») как оригинал:
    /// показывать нечего, но это НЕ «недоступно».
    ///
    /// Разница видна пользователю: у `unavailable` фронт пишет «сообщение
    /// удалено» и гасит переход, а тут сообщение живо и клик по цитате
    /// обязан к нему прокрутить.
    #[test]
    fn service_original_is_empty_but_not_unavailable() {
        let index = RawPeerIndex::new(&[], &[]);
        let service = tl::enums::Message::Service(tl::types::MessageService {
            out: false,
            mentioned: false,
            media_unread: false,
            reactions_are_possible: false,
            silent: false,
            post: false,
            legacy: false,
            id: 41,
            from_id: None,
            peer_id: peer_user(777_000),
            saved_peer_id: None,
            reply_to: None,
            date: 1_800_000_000,
            action: tl::enums::MessageAction::ChatDeletePhoto,
            reactions: None,
            ttl_period: None,
        });
        let original = from_raw_message(&service, &index).expect("сервисное — тоже строка ленты");
        assert_eq!(original.text, "");

        let mut page = vec![dto_reply(42, 41)];
        enrich_reply_previews(&mut page, &HashMap::from([(41, original)]));
        let reply = page[0].reply_to.clone().unwrap();
        assert!(
            !reply.unavailable,
            "сообщение существует — переход работает"
        );
        assert_eq!(reply.text, None, "текста у сервисного нет — именно null");
        assert_eq!(reply.media_kind, None);
        assert_eq!(reply.out, Some(false), "направление известно");
        assert!(
            !reply_needs_preview(&reply),
            "второй раз за пустотой в сеть не ходим"
        );
    }

    /// `out` у цитаты — ТРИ состояния, и фронт различает их все: `true` —
    /// отвечают нам, `false` — собеседнику, `null` — превью ещё не добито
    /// (строка из потока апдейтов; фронт дозапросит роутом одного
    /// сообщения). Схлопывание `null` в `false` стёрло бы этот сигнал:
    /// свои сообщения перестали бы подсвечиваться в цитате, а фронт
    /// перестал бы догружать превью.
    #[test]
    fn out_of_the_quote_is_a_tri_state() {
        let mut page = vec![
            dto_msg(41, true, "моё", None),
            dto_msg(40, false, "чужое", Some(("777000", "Иван"))),
            dto_reply(42, 41),
            dto_reply(43, 40),
            dto_reply(44, 39), // оригинала нет ни на странице, ни в батче
        ];
        assert!(
            page[2..]
                .iter()
                .all(|m| m.reply_to.as_ref().unwrap().out.is_none()),
            "до обогащения направление неизвестно"
        );

        enrich_reply_previews(&mut page, &HashMap::new());
        assert_eq!(page[2].reply_to.as_ref().unwrap().out, Some(true));
        assert_eq!(page[3].reply_to.as_ref().unwrap().out, Some(false));
        let gone = page[4].reply_to.clone().unwrap();
        assert_eq!(
            gone.out, None,
            "у недоступного оригинала направление не выдумывается"
        );
        assert!(gone.unavailable);

        // `null` уезжает на фронт явным ключом — иначе «нет превью» и
        // «чужое сообщение» слились бы в одно.
        let json = serde_json::to_value(&page[4]).unwrap();
        assert!(json["reply_to"]["out"].is_null());
        assert_eq!(json["reply_to"]["unavailable"], true);
    }

    /// Процитированный фрагмент живёт по правилам превью и НЕ зависит от
    /// оригинала: он приходит с самим ответом, поэтому спокойно бывает
    /// длиннее текста оригинала (цитировали более раннюю редакцию) и
    /// режется тем же лимитом.
    #[test]
    fn quote_is_normalized_independently_of_the_original() {
        // Пустой и пробельный фрагмент — не «цитата из пустоты», а null:
        // иначе фронт нарисовал бы пустую полоску над текстом.
        for empty in ["", "   ", "\n\t", "\u{a0}"] {
            let mut header = reply_header(Some(41));
            header.quote = true;
            header.quote_text = Some(empty.into());
            let reply =
                reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header)).unwrap();
            assert_eq!(reply.quote, None, "{empty:?}");
        }

        // Многострочный фрагмент схлопывается в одну строку.
        let mut header = reply_header(Some(41));
        header.quote = true;
        header.quote_text = Some("первая\nвторая".into());
        let reply = reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header)).unwrap();
        assert_eq!(reply.quote.as_deref(), Some("первая вторая"));

        // Фрагмент длиннее лимита режется, как любое превью.
        let long = "ц".repeat(REPLY_PREVIEW_CHARS + 40);
        let mut header = reply_header(Some(41));
        header.quote = true;
        header.quote_text = Some(long);
        let reply = reply_to_from_header(&tl::enums::MessageReplyHeader::Header(header)).unwrap();
        let quote = reply.quote.clone().unwrap();
        assert_eq!(quote.chars().count(), REPLY_PREVIEW_CHARS);
        assert!(quote.ends_with('…'));

        // И остаётся длиннее текста оригинала после обогащения — сверять
        // их не с чем и незачем.
        let mut reply_msg = dto_msg(42, false, "ответ", None);
        reply_msg.reply_to = Some(reply);
        let mut page = vec![dto_msg(41, false, "кратко", None), reply_msg];
        enrich_reply_previews(&mut page, &HashMap::new());
        let reply = page[1].reply_to.clone().unwrap();
        assert_eq!(reply.text.as_deref(), Some("кратко"));
        assert_eq!(
            reply.quote.as_deref().map(|q| q.chars().count()),
            Some(REPLY_PREVIEW_CHARS)
        );
        assert!(!reply.unavailable);
    }

    /// Обрезка считает code point'ы, а не графемы: суррогатная пара не
    /// рвётся никогда (иначе на месте эмодзи оказался бы «мусорный» байт
    /// и невалидный UTF-8 в JSON), а ZWJ-склейка распасться может — это
    /// осознанная плата за дешёвый и предсказуемый лимит.
    #[test]
    fn preview_cut_never_breaks_a_code_point() {
        // Семья из ZWJ-склейки: одна «буква» на экране — семь code point'ов.
        let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        assert_eq!(family.chars().count(), 7);
        let cut = reply_preview_text(&family.repeat(30)).unwrap();
        assert_eq!(cut.chars().count(), REPLY_PREVIEW_CHARS);
        assert!(cut.ends_with('…'));
        assert!(
            std::str::from_utf8(cut.as_bytes()).is_ok(),
            "строка осталась валидным UTF-8"
        );
        // 19 целых семей (133 code point'а) + хвост оборванной двадцатой:
        // рвётся склейка, но не символ.
        assert!(cut.starts_with(&family.repeat(19)));

        // Комбинирующая метка — отдельный code point, и она тоже считается.
        let accented = "е\u{301}".repeat(REPLY_PREVIEW_CHARS);
        let cut = reply_preview_text(&accented).unwrap();
        assert_eq!(cut.chars().count(), REPLY_PREVIEW_CHARS);

        // Астральные символы вне эмодзи ведут себя ровно так же.
        let math = "𝕏".repeat(REPLY_PREVIEW_CHARS + 1);
        assert_eq!(
            reply_preview_text(&math).unwrap(),
            format!("{}…", "𝕏".repeat(REPLY_PREVIEW_CHARS - 1))
        );

        // Схлопывание пробелов происходит ДО подсчёта лимита: иначе цитата
        // из отформатированного «лесенкой» сообщения обрезалась бы вдвое
        // раньше, чем нужно.
        let laddered = "слово\n\n\n".repeat(40);
        let cut = reply_preview_text(&laddered).unwrap();
        assert!(!cut.contains('\n'));
        assert_eq!(cut.chars().count(), REPLY_PREVIEW_CHARS);
        assert!(cut.starts_with("слово слово слово"));
    }

    /// Цепочка ответов на одной странице (A→B→C): каждая цитата показывает
    /// СВОЙ оригинал и ни одна не стоит запроса.
    ///
    /// Так выглядит любое обсуждение «в нитку», и здесь легче всего
    /// перепутать звенья: цитата к A обязана показать текст B, а не текст
    /// цитаты, которую несёт сам B (то есть C). Заодно проверяется, что
    /// страница со всей цепочкой внутри не порождает ни одного запроса.
    #[test]
    fn reply_chain_on_one_page_resolves_each_link_separately() {
        let mut b = dto_reply(42, 41);
        b.text = "ответ B".into();
        let mut a = dto_reply(43, 42);
        a.text = "ответ A".into();
        let mut page = vec![
            dto_msg(41, false, "самое первое", Some(("111", "Аня"))),
            b,
            a,
        ];

        assert!(
            reply_ids_to_fetch(&page).is_empty(),
            "вся цепочка на странице — сеть не нужна"
        );
        enrich_reply_previews(&mut page, &HashMap::new());
        assert_eq!(
            page[1].reply_to.as_ref().unwrap().text.as_deref(),
            Some("самое первое")
        );
        assert_eq!(
            page[2].reply_to.as_ref().unwrap().text.as_deref(),
            Some("ответ B"),
            "цитата к B — текст B, а не текст его собственной цитаты"
        );

        // Повторный проход идемпотентен: страницу обогащают и после
        // догрузки соседней, второй раз она меняться не должна.
        let before = page.clone();
        enrich_reply_previews(&mut page, &HashMap::new());
        assert_eq!(page, before);
    }

    /// Сто ответов на одно и то же сообщение стоят ОДНОГО id в батче.
    ///
    /// Ровно так выглядит живой чат вокруг одного объявления; без дедупа
    /// страница уносила бы сотню одинаковых id в `messages.getMessages` и
    /// упиралась бы в лимит на пустом месте.
    #[test]
    fn a_crowd_replying_to_one_message_costs_one_id() {
        let popular = 7;
        let mut page: Vec<MessageDto> = (0..REPLY_FETCH_BATCH as i32 + 20)
            .map(|i| dto_reply(1_000 + i, popular))
            .collect();
        assert_eq!(
            reply_ids_to_fetch(&page),
            vec![popular],
            "дедуп идёт по id оригинала, а не по числу ответов"
        );

        // Разные оригиналы не склеиваются и здесь НЕ режутся: нарезка под
        // лимит Telegram — забота вызывающего (`reply_fetch_plan` в routes),
        // иначе список молча терял бы хвост ещё до планировщика.
        let wide: Vec<MessageDto> = (0..REPLY_FETCH_BATCH as i32 + 20)
            .map(|i| dto_reply(2_000 + i, i + 1))
            .collect();
        assert_eq!(reply_ids_to_fetch(&wide).len(), REPLY_FETCH_BATCH + 20);

        // Один добранный оригинал гасит всю толпу разом.
        let originals = HashMap::from([(popular, dto_msg(popular, false, "популярное", None))]);
        enrich_reply_previews(&mut page, &originals);
        assert!(
            page.iter()
                .all(|m| m.reply_to.as_ref().unwrap().text.as_deref() == Some("популярное")),
            "превью получили все ответы, а не первый"
        );
        assert!(reply_ids_to_fetch(&page).is_empty());
    }
}
