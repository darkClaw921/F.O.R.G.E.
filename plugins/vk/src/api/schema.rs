//! Сырые структуры ответов VK.
//!
//! Слой существует ради одного: превратить недокументированный до конца JSON
//! в типы, НЕ теряя устойчивости. Проекцию для UI ([`crate::dto`]) собирает
//! Ф4 — здесь только форма провода.
//!
//! # Инвариант устойчивости разбора
//!
//! Реальные формы ответов VK документированы неполно (особенно `attachments`,
//! `action`, `keyboard`, `reply_message`), а поля появляются и исчезают между
//! версиями API без объявления. Поэтому:
//!
//! 1. **Все поля объявлены с `#[serde(default)]`, кроме опознавательных**
//!    `id` / `peer_id` / `date` у сущностей, которые без них бессмысленны
//!    ([`MessageRaw`], [`ProfileRaw`], [`GroupRaw`]). Отсутствие `id` у
//!    сообщения — это не «неполные данные», а другой объект, и молча подставить
//!    туда `0` значило бы испортить ключ медиа-кэша.
//! 2. **`deny_unknown_fields` не используется НИГДЕ.** Новое поле в ответе VK
//!    не должно ронять страницу переписки.
//! 3. **Неизвестный `attachment.type` даёт `unsupported`, а не ошибку** —
//!    см. [`AttachmentRaw::normalized_kind`].
//! 4. **Неизвестный `action.type` сохраняется как есть** — [`ActionRaw`] не
//!    перечисляет виды действий вовсе, их набор VK расширяет регулярно.
//!
//! # Почему вложенные сообщения — отдельный тип
//!
//! `fwd_messages` и `reply_message` у VK НЕ несут `id` и `peer_id`: пересланное
//! сообщение живёт в чужой беседе и собственного адреса в текущей не имеет.
//! Требовать у них ту же тройку полей означало бы ронять разбор ЦЕЛОЙ страницы
//! истории из-за одной пересылки. Поэтому вложенные представлены
//! [`NestedMessageRaw`], где обязательных полей нет вовсе.
//!
//! # Почему здесь нет `Serialize`
//!
//! Ни одна структура модуля не выводит `Serialize`. Сырые ответы VK — это
//! слой, ближайший к проводу, и производная сериализация означала бы, что
//! любую из них можно случайно отдать наружу целиком, вместе с полями вроде
//! `access_key` у вложений. Наружу уходит только [`crate::dto`], собранный
//! руками.

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Общие обёртки.
// ---------------------------------------------------------------------------

/// Стандартная страница VK: `{count, items}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemsRaw<T> {
    /// Сколько всего объектов у VK (не сколько приехало).
    #[serde(default)]
    pub count: i64,
    /// Сама страница.
    #[serde(default)]
    pub items: Vec<T>,
}

/// Страница сообщений (`messages.getHistory` / `messages.getById` при
/// `extended=1`).
///
/// Отличается от [`ItemsRaw`] ровно двумя полями — `profiles` и `groups`, — и
/// ради них и существует: при `extended=1` VK кладёт в тот же ответ имена всех
/// упомянутых пользователей и сообществ. Разбирать страницу в `ItemsRaw`
/// означало бы молча выбросить их и потом доплатить за те же имена отдельным
/// `users.get` — то есть отдать половину бюджета 3 rps за уже полученные
/// данные.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessagesPageRaw {
    /// Сколько сообщений всего (у `getHistory` — во всей беседе).
    #[serde(default)]
    pub count: i64,
    /// Сама страница.
    #[serde(default)]
    pub items: Vec<MessageRaw>,
    /// Профили авторов, упомянутых на странице.
    #[serde(default)]
    pub profiles: Vec<ProfileRaw>,
    /// Сообщества, упомянутые на странице.
    #[serde(default)]
    pub groups: Vec<GroupRaw>,
    /// Беседы, упомянутые на странице (приезжают у `getById`).
    #[serde(default)]
    pub conversations: Vec<ConversationRaw>,
}

/// Ответ `messages.getConversations` (`extended=1`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConversationsRaw {
    /// Сколько бесед всего.
    #[serde(default)]
    pub count: i64,
    /// Беседы вместе с последним сообщением.
    #[serde(default)]
    pub items: Vec<ConversationWithMessageRaw>,
    /// Профили участников — VK присылает их одним списком на всю страницу,
    /// поэтому дозапрос имён не нужен (Ф4).
    #[serde(default)]
    pub profiles: Vec<ProfileRaw>,
    /// Сообщества, упомянутые на странице.
    #[serde(default)]
    pub groups: Vec<GroupRaw>,
    /// Сколько бесед с непрочитанными.
    #[serde(default)]
    pub unread_count: i64,
}

/// Беседа + её последнее сообщение.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConversationWithMessageRaw {
    /// Сама беседа.
    #[serde(default)]
    pub conversation: ConversationRaw,
    /// Последнее сообщение; у пустой беседы его нет.
    #[serde(default)]
    pub last_message: Option<MessageRaw>,
}

/// Беседа (`conversation`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConversationRaw {
    /// Адрес беседы.
    #[serde(default)]
    pub peer: PeerRaw,
    /// Идентификатор последнего прочитанного ВХОДЯЩЕГО.
    #[serde(default)]
    pub in_read: i64,
    /// Идентификатор последнего прочитанного ИСХОДЯЩЕГО.
    #[serde(default)]
    pub out_read: i64,
    /// Непрочитанных сообщений.
    #[serde(default)]
    pub unread_count: i64,
    /// Позиция в серверной сортировке.
    #[serde(default)]
    pub sort_id: SortIdRaw,
    /// Идентификатор последнего сообщения.
    #[serde(default)]
    pub last_message_id: i64,
    /// Можно ли писать (и почему нельзя).
    #[serde(default)]
    pub can_write: CanWriteRaw,
    /// Настройки уведомлений; отсутствуют ⇒ беседа не заглушена.
    #[serde(default)]
    pub push_settings: Option<PushSettingsRaw>,
    /// Параметры чата (только у `peer.type == "chat"`).
    #[serde(default)]
    pub chat_settings: Option<ChatSettingsRaw>,
    /// Помечена «важной» — один из четырёх серверных фильтров.
    #[serde(default)]
    pub important: bool,
    /// Помечена непрочитанной вручную.
    #[serde(default)]
    pub is_marked_unread: bool,
    /// Есть ли непрочитанное упоминание.
    #[serde(default)]
    pub mentions: Vec<i64>,
}

/// Адрес беседы.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeerRaw {
    /// `peer_id`: пользователь — положительный, сообщество — отрицательное,
    /// чат — `2000000000 + local_id`.
    #[serde(default)]
    pub id: i64,
    /// `user` | `chat` | `group` | `email`.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Локальный идентификатор внутри типа.
    #[serde(default)]
    pub local_id: i64,
}

/// Позиция беседы в серверной сортировке.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SortIdRaw {
    /// Старший ключ.
    #[serde(default)]
    pub major_id: i64,
    /// Младший ключ.
    #[serde(default)]
    pub minor_id: i64,
}

/// Можно ли писать в беседу.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CanWriteRaw {
    /// Разрешено ли.
    #[serde(default)]
    pub allowed: bool,
    /// Код причины запрета (VK документирует их отдельным списком).
    #[serde(default)]
    pub reason: i64,
}

/// Настройки уведомлений беседы.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PushSettingsRaw {
    /// Заглушена навсегда.
    #[serde(default)]
    pub disabled_forever: bool,
    /// Заглушена до указанного unix-времени.
    #[serde(default)]
    pub disabled_until: i64,
    /// Без звука.
    #[serde(default)]
    pub no_sound: bool,
    /// Упоминания заглушены.
    #[serde(default)]
    pub disabled_mentions: bool,
}

/// Параметры группового чата.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatSettingsRaw {
    /// Название беседы.
    #[serde(default)]
    pub title: String,
    /// Число участников.
    #[serde(default)]
    pub members_count: i64,
    /// Обложка беседы.
    #[serde(default)]
    pub photo: Option<ChatPhotoRaw>,
    /// `in` | `left` | `kicked`.
    #[serde(default)]
    pub state: String,
    /// Идентификаторы активных участников.
    #[serde(default)]
    pub active_ids: Vec<i64>,
    /// Владелец беседы.
    #[serde(default)]
    pub owner_id: i64,
    /// Канал ли это (у сообществ).
    #[serde(default)]
    pub is_group_channel: bool,
    /// Закреплённое сообщение беседы.
    #[serde(default)]
    pub pinned_message: Option<NestedMessageRaw>,
}

/// Обложка беседы.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatPhotoRaw {
    /// 50×50.
    #[serde(default)]
    pub photo_50: String,
    /// 100×100.
    #[serde(default)]
    pub photo_100: String,
    /// 200×200.
    #[serde(default)]
    pub photo_200: String,
}

// ---------------------------------------------------------------------------
// Сообщения.
// ---------------------------------------------------------------------------

/// Сообщение верхнего уровня.
///
/// `id`, `peer_id` и `date` — ЕДИНСТВЕННЫЕ обязательные поля во всём модуле:
/// без них сообщение не адресуемо (ключ медиа-кэша — это `peer_id` +
/// `message_id` + индекс вложения), и подстановка нулей испортила бы кэш.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessageRaw {
    /// Идентификатор сообщения в аккаунте.
    pub id: i64,
    /// Беседа, которой сообщение принадлежит.
    pub peer_id: i64,
    /// Unix-время отправки.
    pub date: i64,
    /// Автор. Отрицательный ⇒ сообщество.
    #[serde(default)]
    pub from_id: i64,
    /// `1` ⇒ исходящее.
    #[serde(default)]
    pub out: i64,
    /// Текст (для медиа — подпись).
    #[serde(default)]
    pub text: String,
    /// Вторая нумерация — внутри беседы.
    #[serde(default)]
    pub conversation_message_id: i64,
    /// `random_id` отправителя (см. идемпотентность `messages.send`).
    #[serde(default)]
    pub random_id: i64,
    /// Вложения (до 10).
    #[serde(default)]
    pub attachments: Vec<AttachmentRaw>,
    /// Помечено «важным».
    #[serde(default)]
    pub important: bool,
    /// Пересланные сообщения.
    #[serde(default)]
    pub fwd_messages: Vec<NestedMessageRaw>,
    /// Сообщение, на которое отвечают. Приезжает ИНЛАЙНОМ — благодаря этому
    /// подсистемы дозапроса оригиналов в плагине нет вовсе.
    #[serde(default)]
    pub reply_message: Option<Box<NestedMessageRaw>>,
    /// Служебное действие («N пригласил M»).
    #[serde(default)]
    pub action: Option<ActionRaw>,
    /// Клавиатура сообщества.
    #[serde(default)]
    pub keyboard: Option<KeyboardRaw>,
    /// `payload` нажатой кнопки.
    #[serde(default)]
    pub payload: Option<String>,
    /// Unix-время последней правки; `Some` ⇒ «(edited)».
    #[serde(default)]
    pub update_time: i64,
    /// Прослушано ли голосовое.
    #[serde(default)]
    pub was_listened: bool,
    /// Скрыто ли сообщение (сервисные приветствия сообществ).
    #[serde(default)]
    pub is_hidden: bool,
    /// Геометка.
    #[serde(default)]
    pub geo: Option<GeoRaw>,
    /// Автор от имени сообщества-администратора.
    #[serde(default)]
    pub admin_author_id: i64,
}

/// Вложенное сообщение: пересланное, цитируемое или закреплённое.
///
/// Обязательных полей нет ВООБЩЕ — см. блок «Почему вложенные сообщения —
/// отдельный тип» в шапке модуля.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NestedMessageRaw {
    /// Идентификатор, если VK его прислал (в пересылках его обычно нет).
    #[serde(default)]
    pub id: i64,
    /// Беседа-источник, если известна.
    #[serde(default)]
    pub peer_id: i64,
    /// Unix-время.
    #[serde(default)]
    pub date: i64,
    /// Автор.
    #[serde(default)]
    pub from_id: i64,
    /// Текст.
    #[serde(default)]
    pub text: String,
    /// Вторая нумерация.
    #[serde(default)]
    pub conversation_message_id: i64,
    /// Вложения.
    #[serde(default)]
    pub attachments: Vec<AttachmentRaw>,
    /// Рекурсия: у пересылки бывают свои пересылки. Глубину режет Ф4
    /// ([`crate::dto::MAX_FWD_DEPTH`]), разбор — нет.
    #[serde(default)]
    pub fwd_messages: Vec<NestedMessageRaw>,
    /// Цитата внутри пересылки.
    #[serde(default)]
    pub reply_message: Option<Box<NestedMessageRaw>>,
    /// Служебное действие.
    #[serde(default)]
    pub action: Option<ActionRaw>,
    /// Геометка.
    #[serde(default)]
    pub geo: Option<GeoRaw>,
}

/// Служебное действие сообщения.
///
/// Виды НЕ перечисляются enum'ом намеренно: VK добавляет их регулярно, и
/// незнакомый вид обязан доехать до UI строкой, а не уронить разбор.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActionRaw {
    /// `chat_invite_user`, `chat_photo_update`, `chat_title_update`, …
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Кого касается.
    #[serde(default)]
    pub member_id: i64,
    /// Текст (например, новое название беседы).
    #[serde(default)]
    pub text: String,
    /// Адрес приглашённого по email.
    #[serde(default)]
    pub email: String,
    /// Новая обложка беседы.
    #[serde(default)]
    pub photo: Option<ChatPhotoRaw>,
}

/// Геометка сообщения.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GeoRaw {
    /// `point` и прочие типы VK.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Координаты.
    #[serde(default)]
    pub coordinates: Option<GeoCoordinatesRaw>,
    /// Человекочитаемое место.
    #[serde(default)]
    pub place: Option<GeoPlaceRaw>,
}

/// Координаты геометки.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GeoCoordinatesRaw {
    /// Широта.
    #[serde(default)]
    pub latitude: f64,
    /// Долгота.
    #[serde(default)]
    pub longitude: f64,
}

/// Описание места геометки.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GeoPlaceRaw {
    /// Название.
    #[serde(default)]
    pub title: String,
    /// Страна.
    #[serde(default)]
    pub country: String,
    /// Город.
    #[serde(default)]
    pub city: String,
}

// ---------------------------------------------------------------------------
// Клавиатура.
// ---------------------------------------------------------------------------

/// Клавиатура сообщества.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyboardRaw {
    /// Свернуть после первого нажатия.
    #[serde(default)]
    pub one_time: bool,
    /// Инлайновая (под сообщением) или нижняя.
    #[serde(default)]
    pub inline: bool,
    /// Автор клавиатуры.
    #[serde(default)]
    pub author_id: i64,
    /// Ряды кнопок.
    #[serde(default)]
    pub buttons: Vec<Vec<KeyboardButtonRaw>>,
}

/// Кнопка клавиатуры.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyboardButtonRaw {
    /// `primary` | `secondary` | `negative` | `positive`.
    #[serde(default)]
    pub color: String,
    /// Что кнопка делает.
    #[serde(default)]
    pub action: KeyboardActionRaw,
}

/// Действие кнопки.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyboardActionRaw {
    /// `text` | `open_link` | `callback` | `vkpay` | `open_app` | `location`.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Надпись.
    #[serde(default)]
    pub label: String,
    /// Полезная нагрузка (JSON строкой).
    #[serde(default)]
    pub payload: String,
    /// Ссылка для `open_link`.
    #[serde(default)]
    pub link: String,
    /// Хэш мини-приложения.
    #[serde(default)]
    pub hash: String,
    /// Идентификатор приложения.
    #[serde(default)]
    pub app_id: i64,
    /// Владелец приложения.
    #[serde(default)]
    pub owner_id: i64,
}

// ---------------------------------------------------------------------------
// Вложения.
// ---------------------------------------------------------------------------

/// Вложение сообщения.
///
/// Форма VK — «тег + одноимённое поле»: `{"type":"photo","photo":{…}}`.
/// Незнакомый тег оставляет все поля пустыми, но НЕ роняет разбор.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttachmentRaw {
    /// Сырой тип из ответа VK.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Фотография.
    #[serde(default)]
    pub photo: Option<PhotoRaw>,
    /// Видеозапись.
    #[serde(default)]
    pub video: Option<VideoRaw>,
    /// Аудиозапись.
    #[serde(default)]
    pub audio: Option<AudioRaw>,
    /// Документ (в том числе гифки и присланные файлы).
    #[serde(default)]
    pub doc: Option<DocRaw>,
    /// Голосовое сообщение.
    #[serde(default)]
    pub audio_message: Option<AudioMessageRaw>,
    /// Стикер.
    #[serde(default)]
    pub sticker: Option<StickerRaw>,
    /// Граффити.
    #[serde(default)]
    pub graffiti: Option<GraffitiRaw>,
    /// Ссылка с превью.
    #[serde(default)]
    pub link: Option<LinkRaw>,
    /// Опрос, товар, запись на стене и прочее, что мы не разбираем: тип
    /// сохраняется, содержимое — нет.
    #[serde(default)]
    pub wall: Option<serde_json::Value>,
}

/// Нормализованные виды вложений — словарь, общий с [`crate::dto::MediaDto`].
pub const KIND_PHOTO: &str = "photo";
/// Видеозапись.
pub const KIND_VIDEO: &str = "video";
/// Аудиозапись.
pub const KIND_AUDIO: &str = "audio";
/// Голосовое сообщение.
pub const KIND_VOICE: &str = "voice";
/// Документ.
pub const KIND_DOCUMENT: &str = "document";
/// Стикер.
pub const KIND_STICKER: &str = "sticker";
/// Граффити.
pub const KIND_GRAFFITI: &str = "graffiti";
/// Известный VK тип, у которого нет файла для показа (ссылка, опрос, товар).
pub const KIND_OTHER: &str = "other";
/// Тип, о котором мы не знаем ничего.
pub const KIND_UNSUPPORTED: &str = "unsupported";

impl AttachmentRaw {
    /// Тип вложения в словаре UI.
    ///
    /// НЕЗНАКОМЫЙ тип даёт [`KIND_UNSUPPORTED`], а не ошибку: карточка
    /// нарисуется отключённой, страница переписки уцелеет. Это и есть
    /// «forward-compatible разбор» из шапки модуля — VK добавляет типы
    /// вложений без предупреждения.
    pub fn normalized_kind(&self) -> &'static str {
        match self.kind.as_str() {
            "photo" => KIND_PHOTO,
            "video" => KIND_VIDEO,
            "audio" => KIND_AUDIO,
            "audio_message" => KIND_VOICE,
            "doc" => KIND_DOCUMENT,
            "sticker" => KIND_STICKER,
            "graffiti" => KIND_GRAFFITI,
            // Известные, но без собственного файла: показать нечего, а вид
            // сообщения UI всё равно обязан отрисовать.
            "link" | "wall" | "wall_reply" | "poll" | "market" | "market_album" | "gift"
            | "story" | "podcast" | "article" | "call" | "money_transfer" | "money_request"
            | "event" | "narrative" | "audio_playlist" | "curator" | "group" | "page"
            | "album" => KIND_OTHER,
            _ => KIND_UNSUPPORTED,
        }
    }

    /// Знаком ли нам тип вложения.
    pub fn is_known(&self) -> bool {
        self.normalized_kind() != KIND_UNSUPPORTED
    }
}

/// Фотография.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PhotoRaw {
    /// Идентификатор.
    #[serde(default)]
    pub id: i64,
    /// Владелец.
    #[serde(default)]
    pub owner_id: i64,
    /// Подпись.
    #[serde(default)]
    pub text: String,
    /// Unix-время загрузки.
    #[serde(default)]
    pub date: i64,
    /// Доступные размеры.
    #[serde(default)]
    pub sizes: Vec<PhotoSizeRaw>,
    /// Ключ доступа для приватных фото.
    #[serde(default)]
    pub access_key: String,
}

/// Один размер фотографии.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PhotoSizeRaw {
    /// Прямая ссылка на файл (подписанная и протухающая — наружу не уезжает).
    #[serde(default)]
    pub url: String,
    /// Ширина.
    #[serde(default)]
    pub width: i64,
    /// Высота.
    #[serde(default)]
    pub height: i64,
    /// Буквенный код размера VK (`s`, `m`, `x`, `y`, `z`, `w`).
    #[serde(default, rename = "type")]
    pub kind: String,
}

/// Видеозапись.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VideoRaw {
    /// Идентификатор.
    #[serde(default)]
    pub id: i64,
    /// Владелец.
    #[serde(default)]
    pub owner_id: i64,
    /// Название.
    #[serde(default)]
    pub title: String,
    /// Описание.
    #[serde(default)]
    pub description: String,
    /// Длительность, секунды.
    #[serde(default)]
    pub duration: i64,
    /// Превью.
    #[serde(default)]
    pub image: Vec<PhotoSizeRaw>,
    /// Первый кадр.
    #[serde(default)]
    pub first_frame: Vec<PhotoSizeRaw>,
    /// Ширина.
    #[serde(default)]
    pub width: i64,
    /// Высота.
    #[serde(default)]
    pub height: i64,
    /// Страница-плеер: ЕДИНСТВЕННАЯ ссылка VK, которую UI открывает напрямую.
    #[serde(default)]
    pub player: String,
    /// Ключ доступа.
    #[serde(default)]
    pub access_key: String,
}

/// Аудиозапись.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioRaw {
    /// Идентификатор.
    #[serde(default)]
    pub id: i64,
    /// Владелец.
    #[serde(default)]
    pub owner_id: i64,
    /// Исполнитель.
    #[serde(default)]
    pub artist: String,
    /// Название.
    #[serde(default)]
    pub title: String,
    /// Длительность, секунды.
    #[serde(default)]
    pub duration: i64,
    /// Прямая ссылка (протухающая).
    #[serde(default)]
    pub url: String,
}

/// Документ.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocRaw {
    /// Идентификатор.
    #[serde(default)]
    pub id: i64,
    /// Владелец.
    #[serde(default)]
    pub owner_id: i64,
    /// Имя файла.
    #[serde(default)]
    pub title: String,
    /// Размер, байты.
    #[serde(default)]
    pub size: u64,
    /// Расширение.
    #[serde(default)]
    pub ext: String,
    /// Прямая ссылка.
    #[serde(default)]
    pub url: String,
    /// Числовой тип документа VK (3 — гиф, 4 — изображение, …).
    #[serde(default, rename = "type")]
    pub kind: i64,
    /// Превью.
    #[serde(default)]
    pub preview: Option<DocPreviewRaw>,
    /// Ключ доступа.
    #[serde(default)]
    pub access_key: String,
}

/// Превью документа.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocPreviewRaw {
    /// Картиночное превью.
    #[serde(default)]
    pub photo: Option<DocPreviewPhotoRaw>,
    /// Превью граффити.
    #[serde(default)]
    pub graffiti: Option<GraffitiRaw>,
    /// Превью голосового.
    #[serde(default)]
    pub audio_message: Option<AudioMessageRaw>,
}

/// Картиночное превью документа.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocPreviewPhotoRaw {
    /// Размеры превью.
    #[serde(default)]
    pub sizes: Vec<PhotoSizeRaw>,
}

/// Голосовое сообщение.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioMessageRaw {
    /// Идентификатор.
    #[serde(default)]
    pub id: i64,
    /// Владелец.
    #[serde(default)]
    pub owner_id: i64,
    /// Длительность, секунды.
    #[serde(default)]
    pub duration: i64,
    /// Огибающая для отрисовки волны.
    #[serde(default)]
    pub waveform: Vec<i64>,
    /// Ссылка на ogg.
    #[serde(default)]
    pub link_ogg: String,
    /// Ссылка на mp3.
    #[serde(default)]
    pub link_mp3: String,
    /// Расшифровка речи, если VK её сделал.
    #[serde(default)]
    pub transcript: String,
    /// Состояние расшифровки.
    #[serde(default)]
    pub transcript_state: String,
    /// Ключ доступа.
    #[serde(default)]
    pub access_key: String,
}

/// Стикер.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StickerRaw {
    /// Идентификатор набора.
    #[serde(default)]
    pub product_id: i64,
    /// Идентификатор стикера.
    #[serde(default)]
    pub sticker_id: i64,
    /// Изображения на прозрачном фоне.
    #[serde(default)]
    pub images: Vec<StickerImageRaw>,
    /// Изображения с фоном (для тёмной темы).
    #[serde(default)]
    pub images_with_background: Vec<StickerImageRaw>,
    /// Ссылка на анимацию.
    #[serde(default)]
    pub animation_url: String,
}

/// Одно изображение стикера.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StickerImageRaw {
    /// Ссылка.
    #[serde(default)]
    pub url: String,
    /// Ширина.
    #[serde(default)]
    pub width: i64,
    /// Высота.
    #[serde(default)]
    pub height: i64,
}

/// Граффити.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GraffitiRaw {
    /// Идентификатор.
    #[serde(default)]
    pub id: i64,
    /// Владелец.
    #[serde(default)]
    pub owner_id: i64,
    /// Ссылка.
    #[serde(default)]
    pub url: String,
    /// Ширина.
    #[serde(default)]
    pub width: i64,
    /// Высота.
    #[serde(default)]
    pub height: i64,
    /// Ключ доступа.
    #[serde(default)]
    pub access_key: String,
}

/// Ссылка с превью.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LinkRaw {
    /// Адрес.
    #[serde(default)]
    pub url: String,
    /// Заголовок.
    #[serde(default)]
    pub title: String,
    /// Подпись.
    #[serde(default)]
    pub caption: String,
    /// Описание.
    #[serde(default)]
    pub description: String,
    /// Картинка превью.
    #[serde(default)]
    pub photo: Option<PhotoRaw>,
}

// ---------------------------------------------------------------------------
// Профили и сообщества.
// ---------------------------------------------------------------------------

/// Пользователь (`users.get` и блок `profiles` расширенных ответов).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileRaw {
    /// Идентификатор — обязателен: профиль без него не с чем связать.
    pub id: i64,
    /// Имя.
    #[serde(default)]
    pub first_name: String,
    /// Фамилия.
    #[serde(default)]
    pub last_name: String,
    /// Короткое имя без «@».
    #[serde(default)]
    pub screen_name: String,
    /// Аватар 50×50.
    #[serde(default)]
    pub photo_50: String,
    /// Аватар 100×100.
    #[serde(default)]
    pub photo_100: String,
    /// `1` ⇒ онлайн.
    #[serde(default)]
    pub online: i64,
    /// Когда был в сети.
    #[serde(default)]
    pub last_seen: Option<LastSeenRaw>,
    /// `1` ⇒ в друзьях (ближайший аналог телеграмного «контакта»).
    #[serde(default)]
    pub is_friend: i64,
    /// `deleted` | `banned`, если аккаунт недоступен.
    #[serde(default)]
    pub deactivated: String,
}

/// Когда пользователь был в сети.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LastSeenRaw {
    /// Unix-время.
    #[serde(default)]
    pub time: i64,
    /// Платформа VK (1 — мобильная версия, 7 — веб, …).
    #[serde(default)]
    pub platform: i64,
}

/// Сообщество (`groups.getById` и блок `groups` расширенных ответов).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GroupRaw {
    /// Идентификатор БЕЗ знака: в `peer_id` он приходит отрицательным.
    pub id: i64,
    /// Название.
    #[serde(default)]
    pub name: String,
    /// Короткое имя.
    #[serde(default)]
    pub screen_name: String,
    /// Аватар 50×50.
    #[serde(default)]
    pub photo_50: String,
    /// Аватар 100×100.
    #[serde(default)]
    pub photo_100: String,
    /// `group` | `page` | `event`.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// `deleted` | `banned`, если сообщество недоступно.
    #[serde(default)]
    pub deactivated: String,
}

// ---------------------------------------------------------------------------
// LongPoll (наполняется в Ф5).
// ---------------------------------------------------------------------------

/// Ответ `messages.getLongPollServer`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LongPollServerRaw {
    /// Ключ доступа (протухает — это `failed:2`).
    #[serde(default)]
    pub key: String,
    /// Хост LongPoll-сервера БЕЗ схемы (`lp.vk.com/wh123456`).
    #[serde(default)]
    pub server: String,
    /// Позиция во времени событий.
    #[serde(default)]
    pub ts: u64,
    /// Позиция в истории сообщений; приходит только при `need_pts=1`.
    #[serde(default)]
    pub pts: u64,
}

/// Ответ `messages.getLongPollHistory`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LongPollHistoryRaw {
    /// Компактные события `[код, …]`.
    ///
    /// Тип элемента — `Value`, а НЕ число: события LongPoll разнородны внутри
    /// одного массива (`[4, msg_id, flags, peer_id, ts, "текст", {…}]`), и
    /// строгий `Vec<Vec<i64>>` ронял бы разбор на первом же новом сообщении.
    /// Разбор позиций — работа Ф5 (`longpoll.rs`).
    #[serde(default)]
    pub history: Vec<Vec<serde_json::Value>>,
    /// Полные сообщения из этих событий.
    #[serde(default)]
    pub messages: ItemsRaw<MessageRaw>,
    /// Профили упомянутых пользователей.
    #[serde(default)]
    pub profiles: Vec<ProfileRaw>,
    /// Упомянутые сообщества.
    #[serde(default)]
    pub groups: Vec<GroupRaw>,
    /// Беседы, затронутые событиями.
    #[serde(default)]
    pub conversations: Vec<ConversationRaw>,
    /// Новое значение `pts`.
    #[serde(default)]
    pub new_pts: u64,
    /// `1` ⇒ история не поместилась, нужен ещё запрос.
    #[serde(default)]
    pub more: i64,
}

// ---------------------------------------------------------------------------
// Загрузка файлов (наполняется в Ф8).
// ---------------------------------------------------------------------------

/// Ответ `*.getMessagesUploadServer`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UploadServerRaw {
    /// Куда лить файл (ЧУЖОЙ хост, не троттлится).
    #[serde(default)]
    pub upload_url: String,
    /// Альбом (у фото).
    #[serde(default)]
    pub album_id: i64,
    /// Сообщество (у фото).
    #[serde(default)]
    pub group_id: i64,
}

/// Ответ `docs.save`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocSaveRaw {
    /// `doc` | `audio_message` | `graffiti`.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Сохранённый документ.
    #[serde(default)]
    pub doc: Option<DocRaw>,
    /// Сохранённое голосовое.
    #[serde(default)]
    pub audio_message: Option<AudioMessageRaw>,
    /// Сохранённое граффити.
    #[serde(default)]
    pub graffiti: Option<GraffitiRaw>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каждая фикстура — сохранённая форма реального ответа VK. Разбор обязан
    /// проходить целиком, без единого `unwrap` в проде.
    const CONVERSATIONS: &str = include_str!("../../tests/fixtures/get_conversations.json");
    const HISTORY: &str = include_str!("../../tests/fixtures/get_history.json");
    const BY_ID: &str = include_str!("../../tests/fixtures/get_by_id.json");
    const LP_SERVER: &str = include_str!("../../tests/fixtures/get_longpoll_server.json");
    const LP_HISTORY: &str = include_str!("../../tests/fixtures/get_longpoll_history.json");

    fn response_of(fixture: &str) -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(fixture).unwrap();
        v["response"].clone()
    }

    #[test]
    fn conversations_fixture_parses_whole() {
        let raw: ConversationsRaw = serde_json::from_value(response_of(CONVERSATIONS)).unwrap();
        assert_eq!(raw.count, 3);
        assert_eq!(raw.items.len(), 3);

        // Личка.
        let user = &raw.items[0];
        assert_eq!(user.conversation.peer.kind, "user");
        assert_eq!(user.conversation.peer.id, 1234);
        assert_eq!(user.conversation.unread_count, 2);
        assert!(user.conversation.can_write.allowed);
        assert_eq!(user.last_message.as_ref().unwrap().text, "привет");

        // Групповой чат: заглушен и с обложкой.
        let chat = &raw.items[1];
        assert_eq!(chat.conversation.peer.kind, "chat");
        let settings = chat.conversation.chat_settings.as_ref().unwrap();
        assert_eq!(settings.title, "Проектная");
        assert_eq!(settings.members_count, 4);
        assert!(chat.conversation.push_settings.as_ref().unwrap().disabled_forever);

        // Сообщество: писать нельзя, и причина известна.
        let group = &raw.items[2];
        assert_eq!(group.conversation.peer.id, -55);
        assert!(!group.conversation.can_write.allowed);
        assert_eq!(group.conversation.can_write.reason, 18);

        assert_eq!(raw.profiles.len(), 2);
        assert_eq!(raw.profiles[0].screen_name, "durov");
        assert_eq!(raw.groups.len(), 1);
        assert_eq!(raw.groups[0].name, "Тестовое сообщество");
    }

    #[test]
    fn history_fixture_parses_whole() {
        let raw: ItemsRaw<MessageRaw> = serde_json::from_value(response_of(HISTORY)).unwrap();
        assert_eq!(raw.items.len(), 5);

        // Пересылка БЕЗ id/peer_id — ровно та форма, из-за которой существует
        // NestedMessageRaw.
        let fwd = raw.items.iter().find(|m| !m.fwd_messages.is_empty()).unwrap();
        assert_eq!(fwd.fwd_messages.len(), 2);
        assert_eq!(fwd.fwd_messages[0].id, 0, "у пересылки нет собственного id");
        assert_eq!(fwd.fwd_messages[0].text, "пересланное раз");
        assert_eq!(fwd.fwd_messages[1].fwd_messages.len(), 1, "рекурсия жива");

        // Ответ приезжает инлайном — дозапрос оригинала не нужен.
        let reply = raw.items.iter().find(|m| m.reply_message.is_some()).unwrap();
        assert_eq!(reply.reply_message.as_ref().unwrap().text, "оригинал");

        // Вложения: фото с размерами и голосовое с огибающей.
        let media = raw.items.iter().find(|m| !m.attachments.is_empty()).unwrap();
        assert_eq!(media.attachments.len(), 2);
        assert_eq!(media.attachments[0].normalized_kind(), KIND_PHOTO);
        assert_eq!(media.attachments[0].photo.as_ref().unwrap().sizes.len(), 3);
        assert_eq!(media.attachments[1].normalized_kind(), KIND_VOICE);
        assert_eq!(
            media.attachments[1].audio_message.as_ref().unwrap().waveform.len(),
            5
        );

        // Служебное сообщение.
        let action = raw.items.iter().find(|m| m.action.is_some()).unwrap();
        assert_eq!(action.action.as_ref().unwrap().kind, "chat_invite_user");
    }

    #[test]
    fn by_id_and_longpoll_fixtures_parse() {
        let raw: ItemsRaw<MessageRaw> = serde_json::from_value(response_of(BY_ID)).unwrap();
        assert_eq!(raw.count, 1);
        assert_eq!(raw.items[0].id, 7001);
        assert!(raw.items[0].keyboard.is_some(), "клавиатура сообщества");
        assert_eq!(
            raw.items[0].keyboard.as_ref().unwrap().buttons[0][0].action.kind,
            "text"
        );

        let lp: LongPollServerRaw = serde_json::from_value(response_of(LP_SERVER)).unwrap();
        assert_eq!(lp.server, "lp.vk.com/wh123456");
        assert!(lp.pts > 0, "need_pts=1 обязателен для getLongPollHistory");

        let hist: LongPollHistoryRaw = serde_json::from_value(response_of(LP_HISTORY)).unwrap();
        assert_eq!(hist.history.len(), 2);
        assert_eq!(hist.messages.items.len(), 1);
        assert_eq!(hist.new_pts, 2049);
    }

    /// ГЛАВНЫЙ тест устойчивости: страница из 50 сообщений, одно из которых
    /// несёт незнакомый тип вложения и незнакомые поля верхнего уровня,
    /// разбирается ЦЕЛИКОМ. Один странный объект не имеет права стоить
    /// пользователю всей переписки.
    #[test]
    fn a_page_of_fifty_survives_an_unknown_attachment() {
        let mut items = Vec::new();
        for i in 0..50 {
            items.push(serde_json::json!({
                "id": 1000 + i,
                "peer_id": 1234,
                "date": 1_700_000_000 + i,
                "from_id": 1234,
                "text": format!("сообщение {i}"),
                "attachments": []
            }));
        }
        // 25-е — с типом вложения из будущего и полями, которых мы не знаем.
        items[25] = serde_json::json!({
            "id": 1025,
            "peer_id": 1234,
            "date": 1_700_000_025_i64,
            "text": "",
            "totally_new_field": {"nested": [1, 2, 3]},
            "attachments": [
                {"type": "hologram", "hologram": {"anything": true}},
                {"type": "photo", "photo": {"id": 1, "sizes": [{"url": "u", "type": "x"}]}}
            ]
        });
        let page = serde_json::json!({"count": 50, "items": items});

        let raw: ItemsRaw<MessageRaw> = serde_json::from_value(page).unwrap();
        assert_eq!(raw.items.len(), 50, "страница разобрана целиком");
        assert_eq!(raw.items[25].attachments[0].normalized_kind(), KIND_UNSUPPORTED);
        assert!(!raw.items[25].attachments[0].is_known());
        // Соседнее вложение того же сообщения при этом разобрано нормально.
        assert_eq!(raw.items[25].attachments[1].normalized_kind(), KIND_PHOTO);
        assert_eq!(raw.items[49].text, "сообщение 49");
    }

    /// Ответ без единого необязательного поля обязан разбираться в дефолты.
    #[test]
    fn bare_minimum_message_falls_back_to_defaults() {
        let m: MessageRaw =
            serde_json::from_str(r#"{"id":1,"peer_id":2,"date":3}"#).unwrap();
        assert_eq!((m.id, m.peer_id, m.date), (1, 2, 3));
        assert_eq!(m.text, "");
        assert_eq!(m.out, 0);
        assert!(m.attachments.is_empty());
        assert!(m.fwd_messages.is_empty());
        assert!(m.reply_message.is_none());
        assert!(m.action.is_none());
        assert!(m.keyboard.is_none());

        // …и наоборот: без опознавательных полей разбор ОБЯЗАН отказать, а не
        // подставить нули — иначе ключ медиа-кэша указывал бы в пустоту.
        assert!(serde_json::from_str::<MessageRaw>(r#"{"peer_id":2,"date":3}"#).is_err());
        assert!(serde_json::from_str::<MessageRaw>(r#"{"id":1,"date":3}"#).is_err());
        assert!(serde_json::from_str::<MessageRaw>(r#"{"id":1,"peer_id":2}"#).is_err());
        assert!(serde_json::from_str::<ProfileRaw>(r#"{"first_name":"x"}"#).is_err());
        assert!(serde_json::from_str::<GroupRaw>(r#"{"name":"x"}"#).is_err());

        // Вложенное сообщение — наоборот: обязательных полей нет вовсе.
        let n: NestedMessageRaw = serde_json::from_str("{}").unwrap();
        assert_eq!(n.id, 0);
        assert_eq!(n.text, "");
    }

    /// Незнакомый вид служебного действия сохраняется как есть.
    #[test]
    fn unknown_action_kind_is_preserved_verbatim() {
        let m: MessageRaw = serde_json::from_str(
            r#"{"id":1,"peer_id":2,"date":3,
                "action":{"type":"chat_teleport_update","member_id":7,"quantum":true}}"#,
        )
        .unwrap();
        let a = m.action.unwrap();
        assert_eq!(a.kind, "chat_teleport_update");
        assert_eq!(a.member_id, 7);
        assert_eq!(a.text, "");
    }

    /// Таблица нормализации типов вложений целиком.
    #[test]
    fn attachment_kind_table() {
        let table = [
            ("photo", KIND_PHOTO),
            ("video", KIND_VIDEO),
            ("audio", KIND_AUDIO),
            ("audio_message", KIND_VOICE),
            ("doc", KIND_DOCUMENT),
            ("sticker", KIND_STICKER),
            ("graffiti", KIND_GRAFFITI),
            ("link", KIND_OTHER),
            ("wall", KIND_OTHER),
            ("poll", KIND_OTHER),
            ("market", KIND_OTHER),
            ("story", KIND_OTHER),
            ("call", KIND_OTHER),
            ("", KIND_UNSUPPORTED),
            ("hologram", KIND_UNSUPPORTED),
            ("PHOTO", KIND_UNSUPPORTED),
        ];
        for (raw, expected) in table {
            let a = AttachmentRaw {
                kind: raw.to_string(),
                ..AttachmentRaw::default()
            };
            assert_eq!(a.normalized_kind(), expected, "тип {raw:?}");
        }
    }

    /// Числовые поля VK приходят то числом, то строкой в разных методах.
    /// Строгий разбор ронял бы страницу — проверяем, что мы переживаем
    /// отсутствие поля (самый частый случай) и не требуем точного типа там,
    /// где это не опознавательное поле.
    #[test]
    fn optional_numeric_blocks_tolerate_absence() {
        let c: ConversationRaw = serde_json::from_str(r#"{"peer":{"id":5}}"#).unwrap();
        assert_eq!(c.peer.id, 5);
        assert_eq!(c.peer.kind, "");
        assert!(!c.can_write.allowed);
        assert!(c.push_settings.is_none());
        assert!(c.chat_settings.is_none());

        let s: LongPollServerRaw = serde_json::from_str("{}").unwrap();
        assert_eq!(s.ts, 0);
        assert_eq!(s.pts, 0);
    }
}
