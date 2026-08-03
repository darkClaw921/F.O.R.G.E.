//! Проводные структуры REST/WS-поверхности плагина и проекция сырого JSON VK
//! в них.
//!
//! # Что здесь есть
//!
//! 1. ФОРМА структур — их поля, имена и сериализация (заведена в Ф1, потому
//!    что от неё зависит WS-протокол [`crate::ws::protocol`]: кадры
//!    `new_message`/`message_edited` объявлены целиком заранее, чтобы фронтовый
//!    `buildHandlers` писался один раз по готовой таблице).
//! 2. ПРОЕКЦИЯ `api/schema.rs` → DTO: [`dialog_from_raw`], [`message_from_raw`],
//!    [`reply_to_from_nested`], [`flatten_forwards`], [`action_from_raw`].
//!    Вложения проецирует [`crate::attach`], клавиатуру — Ф9.
//! 3. АРИФМЕТИКА ИДЕНТИФИКАТОРОВ: [`dialog_id_string`], [`parse_dialog_id`],
//!    [`peer_kind`] — единственное место, где `peer_id` превращается в строку
//!    и обратно.
//!
//! # Откуда берутся имена
//!
//! Проекция не ходит в сеть и не знает про кэш: источник отображаемых имён
//! приходит параметром — трейт [`PeerNames`] (его реализует
//! [`crate::peers::PeerCache`]). Благодаря этому вся таблица соответствий
//! проверяется юнит-тестами с [`NoNames`], без стенда и без токена.
//!
//! # Инвариант «id — строки»
//!
//! `dialog_id` (у VK это `peer_id`) — СТРОКА: `peer_id` беседы равен
//! `2_000_000_000 + chat_id` и в `i32` не влезает, а JS-число теряет точность
//! на 64-битных значениях. `message_id` и `cmid` — числа (`i32`): у VK они
//! действительно 32-битные. Тот же контракт, что у Telegram, — ради него
//! фронт-модули и остаются близнецами.
//!
//! # Чем форма отличается от телеграмной и почему
//!
//! | поле | решение |
//! |---|---|
//! | `pinned`, `archived`, `folder_id` | СОХРАНЕНЫ константами (`false`/`null`): VK не умеет ни закреплять диалоги, ни архивировать, ни папки. Идентичная форма важнее минимальной — каждое удалённое поле это `if` во фронте |
//! | `attachments: Vec<MediaDto>` | НОВОЕ: у сообщения VK до 10 вложений. `media` (первое) остаётся ради формы, источник истины — массив |
//! | `cmid` | НОВОЕ: `conversation_message_id`, вторая нумерация внутри беседы. Первичным id остаётся `message.id`, `cmid` просто едет рядом (нужен для ответов и будущих edit/delete) |
//! | `fwd`, `action`, `payload` | НОВЫЕ: пересылки у VK рекурсивные, служебные сообщения свои, `payload` возвращается с нажатой кнопки бота |
//! | `important`, `can_write`, `can_write_reason` | НОВЫЕ у диалога: без `can_write` UI не знает, почему поле ввода погашено |
//! | `reply_to` | заполняется БЕСПЛАТНО из инлайненного `reply_message` — подсистемы дозапроса оригиналов (~250 строк у Telegram) в VK-версии нет вовсе |

use serde::{Deserialize, Serialize};

use crate::login::VkUserDto;

/// Диалог (у VK — «беседа», `conversation`) в проекции для UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogDto {
    /// `peer_id` строкой (см. инвариант в шапке модуля).
    pub id: String,
    /// `user` | `group` | `chat`. VK-тип `email` устаревший — такие строки
    /// листинг пропускает целиком.
    pub kind: String,
    /// Отображаемое имя: человек, сообщество или `chat_settings.title`.
    pub title: String,
    /// `screen_name` без «@», если есть.
    pub username: Option<String>,
    /// Собеседник — «бот». У VK нет такого флага: сообщество и есть бот с
    /// точки зрения UI, поэтому `kind == "group"`.
    pub bot: bool,
    /// Собеседник в друзьях (`users.get(fields=is_friend)`) — ближайший
    /// аналог телеграмного «контакта».
    pub contact: bool,
    /// Непрочитанных по данным VK (`conversation.unread_count`).
    pub unread_count: i32,
    /// ВСЕГДА `false`: VK умеет закреплять сообщение ВНУТРИ беседы, но не
    /// саму беседу в списке. Поле сохранено ради идентичности формы.
    pub pinned: bool,
    /// Заглушены ли уведомления (`push_settings.disabled_forever` или
    /// `disabled_until > now`). У VK глушение только per-peer — глобальных
    /// категорийных дефолтов, как в Telegram, не существует.
    pub muted: bool,
    /// ВСЕГДА `false`: архива у VK нет.
    pub archived: bool,
    /// ВСЕГДА `None`: папок у VK нет, их место занимают 4 серверных фильтра
    /// (`all`/`unread`/`important`/`unanswered`).
    pub folder_id: Option<i32>,
    /// Последнее сообщение (`extended=1` отдаёт его вместе с беседой).
    pub last_message: Option<MessageDto>,
    /// Число участников беседы (`chat_settings.members_count`), если известно.
    ///
    /// Новое поле относительно Telegram и вынужденное: статус-строка беседы у
    /// VK — это `[CHAT] N members`. Телеграмный «N msgs» по id последнего
    /// сообщения здесь ВРЁТ (у VK нумерация сообщений сквозная по аккаунту, а
    /// не по беседе), поэтому его место занимает честное число участников — а
    /// взять его больше неоткуда.
    pub members: Option<i32>,
    /// Беседа помечена «важной» (`conversation.important`) — это один из
    /// четырёх серверных фильтров.
    pub important: bool,
    /// Можно ли писать в беседу (`can_write.allowed`).
    pub can_write: bool,
    /// Почему писать нельзя (`can_write.reason`). Без кода UI мог бы только
    /// погасить поле ввода, не объяснив причину.
    pub can_write_reason: Option<i32>,
}

/// Сообщение в проекции для UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDto {
    /// Идентификатор сообщения (у VK 32-битный).
    pub id: i32,
    /// `peer_id` строкой.
    pub dialog_id: String,
    /// `true` ⇒ сообщение исходящее (`out == 1`).
    pub out: bool,
    /// Unix-время отправки, секунды UTC.
    pub date: i64,
    /// Кто написал. Отрицательный `from_id` у VK означает сообщество.
    pub sender: Option<VkUserDto>,
    /// Текст (для медиа — подпись; пусто, если её нет).
    pub text: String,
    /// ПЕРВОЕ вложение — сохранено ради формы Telegram, чтобы свёрнутая
    /// строка ленты рисовалась тем же кодом.
    pub media: Option<MediaDto>,
    /// ВСЕ вложения (до 10), каждое со своим [`MediaDto::index`]. Источник
    /// истины для карточек и для ключа медиа-кэша.
    pub attachments: Vec<MediaDto>,
    /// На какое сообщение это ответ (`reply_message.id`).
    pub reply_to_id: Option<i32>,
    /// Превью цитируемого оригинала. Заполняется из инлайненного
    /// `reply_message` БЕЗ дозапроса — самый выгодный размен во всём плагине.
    pub reply_to: Option<ReplyToDto>,
    /// Unix-время последней правки (`update_time`); `Some` ⇒ UI рисует
    /// «(edited)».
    pub edit_date: Option<i64>,
    /// Клавиатура сообщества (`keyboard`), если она есть.
    pub reply_markup: Option<ReplyMarkupDto>,
    /// `conversation_message_id` — вторая нумерация внутри беседы.
    pub cmid: Option<i32>,
    /// Пересланные сообщения (`fwd_messages`), уже уплощённые и обрезанные
    /// по [`MAX_FWD_DEPTH`]/[`MAX_FWD_PER_LEVEL`].
    pub fwd: Vec<FwdDto>,
    /// Служебное сообщение («N пригласил M», смена фото беседы).
    pub action: Option<ActionDto>,
    /// `payload` нажатой кнопки бота, если сообщение отправлено кнопкой.
    pub payload: Option<String>,
}

/// Максимальная глубина разбора `fwd_messages`.
///
/// У VK цепочка пересылок не ограничена, и наивная рекурсия способна
/// нарисовать экран из одного сообщения (а DTO раздуть на мегабайт).
pub const MAX_FWD_DEPTH: usize = 3;

/// Максимум пересланных на одном уровне; остаток сворачивается в
/// [`FwdDto::omitted`].
pub const MAX_FWD_PER_LEVEL: usize = 10;

/// Длина превью цитаты/пересланного в code point'ах.
pub const REPLY_PREVIEW_CHARS: usize = 140;

/// Одно пересланное сообщение.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FwdDto {
    /// Глубина вложенности (0 — переслано напрямую в это сообщение).
    pub depth: u8,
    /// Автор оригинала.
    pub sender: Option<VkUserDto>,
    /// Unix-время оригинала.
    pub date: i64,
    /// Превью текста, обрезанное по [`REPLY_PREVIEW_CHARS`].
    pub text: Option<String>,
    /// Вид первого вложения оригинала (словарь [`MediaDto::kind`]).
    pub media_kind: Option<String>,
    /// Сколько соседей отброшено границей [`MAX_FWD_PER_LEVEL`] (0 — все
    /// поместились). Показывается как «…ещё N пересланных».
    pub omitted: u32,
}

/// Оригинал, на который отвечает сообщение, в проекции для UI.
///
/// В отличие от Telegram, заполняется ОДНИМ приёмом: VK инлайнит
/// `reply_message` в сам объект сообщения, поэтому страница сообщений стоит
/// один запрос вместо двух.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyToDto {
    /// `id` оригинала в этом же диалоге.
    pub message_id: Option<i32>,
    /// `conversation_message_id` оригинала — ИМЕННО он адресует ответ в
    /// беседах, поэтому едет отдельным полем.
    pub cmid: Option<i32>,
    /// Отображаемое имя автора оригинала.
    pub sender: Option<String>,
    /// Id автора оригинала строкой.
    pub sender_id: Option<String>,
    /// Превью текста, обрезанное по [`REPLY_PREVIEW_CHARS`]. `None` у
    /// сообщений без текста (чистое медиа, служебные).
    pub text: Option<String>,
    /// Вид вложения оригинала (словарь [`MediaDto::kind`]).
    pub media_kind: Option<String>,
    /// Оригинал недоступен (удалён/скрыт настройками приватности).
    pub unavailable: bool,
}

/// Служебное сообщение (`message.action`).
///
/// Неизвестный `kind` сохраняется КАК ЕСТЬ с пустым текстом: набор действий
/// VK расширяется, и падать на новом виде нельзя.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDto {
    /// Сырое имя действия VK (`chat_invite_user`, `chat_photo_update`, …).
    pub kind: String,
    /// Кого касается действие (id строкой), если применимо.
    pub member_id: Option<String>,
    /// Готовый человекочитаемый текст; пусто у неизвестных видов.
    pub text: String,
}

/// Описание вложения — ровно то, по чему UI выбирает вид карточки.
///
/// Байты и адреса ФАЙЛОВ здесь не передаются: URL вложений VK подписанные и
/// протухающие, поэтому они не кэшируются и не покидают бэкенд вовсе (байты
/// отдаёт `GET /api/vk/media/:peer/:mid/:index`).
///
/// # Две ссылки-исключения и почему они не нарушают правило
///
/// [`MediaDto::player_url`] и [`MediaDto::link_url`] — ЕДИНСТВЕННЫЕ поля-URL,
/// и ни одно из них не ведёт на CDN вложений:
///
/// * `player_url` — страница-плеер VK: файла у вложения `video` не существует
///   вовсе, открыть ролик можно только там;
/// * `link_url` — АДРЕС, КОТОРЫЙ ПРИСЛАЛ СОБЕСЕДНИК (вложение `link`). Это не
///   инфраструктура VK, а содержимое переписки: карточка без перехода по такой
///   ссылке бесполезна — пользователь всё равно скопирует адрес глазами.
///
/// Разница принципиальная: утечка CDN-адреса заставила бы браузер сходить на
/// серверы VK с нашим Referer и подписанным токеном в пути, а `link_url` ведёт
/// туда, куда пользователь и собирался. Оба поля проходят через белый список
/// схем (`http`/`https`) — см. [`crate::attach::http_url`] на бэкенде и
/// `js/vk/format.js::safeUrl` на фронте.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDto {
    /// Порядковый номер вложения В СООБЩЕНИИ (0..=9).
    ///
    /// Ключевое отличие от Telegram: без индекса ключ медиа-кэша
    /// `dialog/message.kind` не различал бы два фото одного сообщения, и
    /// второе затирало бы первое.
    pub index: u8,
    /// `photo` | `video` | `audio` | `voice` | `document` | `sticker` |
    /// `graffiti` | `other` | `unsupported`.
    ///
    /// `unsupported` — это НЕ ошибка разбора: неизвестный тип вложения
    /// обязан доехать до UI отключённой карточкой, а не ронять всю страницу.
    pub kind: String,
    /// MIME, если VK его прислал.
    pub mime: Option<String>,
    /// Размер файла в байтах.
    pub size: Option<u64>,
    /// Оригинальное имя файла (у документов).
    pub file_name: Option<String>,
    /// Заголовок (у ссылок, товаров, статей, аудиозаписей).
    pub title: Option<String>,
    /// Ширина в пикселях.
    pub width: Option<i32>,
    /// Высота в пикселях.
    pub height: Option<i32>,
    /// Длительность в секундах (видео/аудио/голосовые).
    pub duration_secs: Option<i32>,
    /// Есть ли превью, которое можно нарисовать до полной загрузки.
    ///
    /// ОБЕЩАНИЕ, ОБЕСПЕЧЕННОЕ ТЕМ ЖЕ КОДОМ, который потом ищет адрес:
    /// вычисляется как `attach::url_for(att, Thumb).is_some()`. Пока это так,
    /// «DTO обещал превью, а `/api/vk/media/…?kind=thumb` ответил `no_media`»
    /// невозможно построением. См. `attach::media_from_attachment`.
    pub has_thumb: bool,
    /// Есть ли сам файл, который можно отдать байтами.
    ///
    /// Как и [`MediaDto::has_thumb`] — это `url_for(att, Full).is_some()`.
    ///
    /// У `video` он `false`: вложение видео VK не содержит воспроизводимого
    /// файла — прямые `files.*` доступны только владельцу ролика. Кэшируем
    /// превью, а само видео открывается по [`MediaDto::player_url`].
    pub has_full: bool,
    /// Страница-плеер (только `video`). Ведёт на СТРАНИЦУ VK, а не на файл.
    pub player_url: Option<String>,
    /// Адрес вложения `link` — тот, что прислал собеседник (`kind: "other"`).
    ///
    /// НЕ адрес файла на CDN VK: см. шапку [`MediaDto`]. Пропущен через белый
    /// список схем ещё на бэкенде ([`crate::attach::http_url`]), поэтому в
    /// поле не бывает ни `javascript:`, ни `data:`. Фронт всё равно проверяет
    /// повторно (`js/vk/format.js::webUrl`) — рубежа должно быть два.
    pub link_url: Option<String>,
    /// Огибающая голосового сообщения (`audio_message.waveform`).
    pub waveform: Vec<i32>,
}

/// Клавиатура сообщения (`message.keyboard` у VK).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMarkupDto {
    /// `inline` | `keyboard` — VK различает эти два вида флагом `inline`.
    pub kind: String,
    /// Ряды кнопок.
    pub rows: Vec<Vec<ReplyButtonDto>>,
    /// Свернуть клавиатуру после первого нажатия (`one_time`).
    pub single_use: bool,
}

/// Кнопка клавиатуры в проекции для UI.
///
/// Сам `payload` в DTO НЕ ездит намеренно: нажатие адресуется координатами
/// `{row, col}` (`POST /api/vk/dialogs/:id/messages/:mid/button`), а `label`
/// и `payload` сервер достаёт из сообщения сам. Так исключается подделка
/// `payload` с фронта — и заодно фронту нечего хранить между рендерами.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyButtonDto {
    /// Номер ряда В КЛАВИАТУРЕ VK (0-based).
    ///
    /// Координаты едут ЯВНО, а не выводятся из позиции в [`ReplyMarkupDto::rows`]:
    /// фронт выбрасывает кнопки без подписи (рисовать пустой прямоугольник
    /// нельзя) и на этом сдвинул бы собственную нумерацию, а сервер ищет
    /// кнопку по координатам в СЫРОЙ клавиатуре, перечитанной из VK.
    pub row: u8,
    /// Номер кнопки в ряду (0-based). См. [`ReplyButtonDto::row`].
    pub col: u8,
    /// Надпись (`action.label`).
    pub text: String,
    /// `text` | `open_link` | `callback` | `vkpay` | `open_app` | `location` |
    /// `other`.
    pub kind: String,
    /// Цвет из VK (`primary`/`secondary`/`negative`/`positive`).
    pub color: Option<String>,
    /// URL для `open_link`.
    pub url: Option<String>,
    /// Можно ли нажать кнопку ПОД ПОЛЬЗОВАТЕЛЬСКИМ ТОКЕНОМ.
    ///
    /// `true` только у `text` (отправляется как обычное сообщение с
    /// `payload` — так работает и официальный клиент) и `open_link` (её
    /// открывает браузер, бэкенд не участвует).
    ///
    /// Для остальных `false`, и это ограничение ПЛАТФОРМЫ, а не нашей
    /// реализации: клиентской половины `message_event` («нажать callback от
    /// имени пользователя») в публичном API VK не существует —
    /// `messages.sendMessageEventAnswer` это сторона сообщества. UI обязан
    /// нарисовать такие кнопки отключёнными с пояснением: невидимая кнопка
    /// «поедет» рядом и будет выглядеть поломкой бота.
    pub supported: bool,
}

// ---------------------------------------------------------------------------
// Клавиатура сообщества.
// ---------------------------------------------------------------------------

/// Потолок рядов клавиатуры.
///
/// У самого VK лимит меньше (10 рядов), но разметка приезжает от ЧУЖОГО
/// сообщества через ответ метода, и доверять её размеру нельзя: «клавиатура»
/// из десяти тысяч рядов — это мегабайт DTO и подвисшая вкладка. Число
/// совпадает с фронтовым `MAX_ROWS` (js/vk/botkb.js), чтобы обрезка случалась
/// на одной стороне, а не на двух по-разному.
pub const MAX_KB_ROWS: usize = 32;

/// Потолок кнопок в одном ряду (фронтовый `MAX_ROW_BUTTONS`).
pub const MAX_KB_ROW_BUTTONS: usize = 12;

/// Потолок длины подписи кнопки, символов.
///
/// Больше фронтового `BUTTON_TEXT_MAX` (48) намеренно: видимую обрезку с
/// многоточием делает UI, здесь стоит лишь граница против подписи на мегабайт.
/// В VK нажатие уезжает ПОЛНОЙ подписью из перечитанного сообщения
/// ([`crate::routes::dialogs::post_button`]), поэтому обрезка тут ни на что,
/// кроме показа, не влияет.
pub const BUTTON_LABEL_CHARS: usize = 64;

/// Потолок длины строки `action.type`, символов (forward-compat: незнакомый
/// вид доезжает до UI как есть, но длиной он ограничен).
pub const BUTTON_KIND_CHARS: usize = 32;

/// Виды кнопок, которые вообще можно нажать под ПОЛЬЗОВАТЕЛЬСКИМ токеном.
///
/// - `text` — VK-клиент отправляет её как обычное сообщение с `payload`, и это
///   ровно то, что умеет [`crate::api::methods::SendBody`];
/// - `open_link` — открывает браузер, API вообще не участвует.
///
/// Всё остальное (`callback`, `vkpay`, `open_app`, `location`) требует
/// официального приложения VK либо ключа доступа СООБЩЕСТВА:
/// `messages.sendMessageEventAnswer` — это метод, которым бот ОТВЕЧАЕТ на уже
/// полученный `message_event`, а обратной, клиентской половины («отправить
/// `message_event` от имени пользователя») в публичном API VK нет вовсе.
/// Прямого аналога телеграмному `messages.getBotCallbackAnswer` не существует.
pub const SUPPORTED_BUTTON_KINDS: [&str; 2] = ["text", "open_link"];

/// Цвета кнопок VK. Незнакомое значение не отдаётся наружу вовсе: цвет уезжает
/// в `data`-атрибут разметки, и словарь тут закрытый.
const BUTTON_COLORS: [&str; 4] = ["primary", "secondary", "negative", "positive"];

/// Можно ли нажать кнопку этого вида под пользовательским токеном.
///
/// ЕДИНСТВЕННОЕ место, где принимается это решение: UI рисует кнопку по флагу
/// [`ReplyButtonDto::supported`] и сам ничего не классифицирует — иначе две
/// таблицы видов разъехались бы, и пользователь получил бы либо активную
/// кнопку, которая всегда отвечает 409, либо погашенную кнопку, которая
/// работает.
#[must_use]
pub fn button_is_supported(kind: &str) -> bool {
    SUPPORTED_BUTTON_KINDS.contains(&kind)
}

/// `message.keyboard` → [`ReplyMarkupDto`].
///
/// Возвращает `None`, когда рисовать нечего (пустая матрица `buttons`):
/// пустая рамка под сообщением выглядела бы как сломанная клавиатура.
///
/// Разбор УСТОЙЧИВ К НЕЗНАКОМЫМ ВИДАМ: `action.type`, которого мы не знаем,
/// доезжает до UI строкой как есть и получает `supported: false` — сообщение с
/// новым видом кнопки обязано остаться читаемым, а не исчезнуть из ленты.
///
/// Кнопки БЕЗ подписи не выбрасываются: их отбросит фронт, а координаты
/// соседей обязаны остаться теми же, что в сыром объекте VK — по ним сервер
/// ищет нажатую кнопку (см. [`ReplyButtonDto::row`]).
#[must_use]
pub fn reply_markup_from_raw(kb: &crate::api::schema::KeyboardRaw) -> Option<ReplyMarkupDto> {
    let mut rows: Vec<Vec<ReplyButtonDto>> = Vec::new();
    for (r, raw_row) in kb.buttons.iter().take(MAX_KB_ROWS).enumerate() {
        let row: Vec<ReplyButtonDto> = raw_row
            .iter()
            .take(MAX_KB_ROW_BUTTONS)
            .enumerate()
            .map(|(c, b)| button_from_raw(b, r, c))
            .collect();
        rows.push(row);
    }
    // Ряды без кнопок отбрасываются ПОСЛЕ нумерации: координаты уже проставлены
    // и от чистки не сдвигаются.
    rows.retain(|row| !row.is_empty());
    if rows.is_empty() {
        return None;
    }
    Some(ReplyMarkupDto {
        // У VK нет ни `hide`, ни `force_reply`: вид клавиатуры — это ровно один
        // флаг `inline`.
        kind: if kb.inline { "inline" } else { "keyboard" }.to_string(),
        rows,
        single_use: kb.one_time,
    })
}

/// Одна кнопка клавиатуры → [`ReplyButtonDto`] с координатами.
fn button_from_raw(
    b: &crate::api::schema::KeyboardButtonRaw,
    row: usize,
    col: usize,
) -> ReplyButtonDto {
    let kind = truncate_chars(b.action.kind.trim(), BUTTON_KIND_CHARS);
    // Пустой `action.type` — это «вид неизвестен», а не отдельный вид: слово
    // `other` фронт и так подставляет незнакомым видам.
    let kind = if kind.is_empty() {
        "other".to_string()
    } else {
        kind
    };
    let color = b.color.trim();
    ReplyButtonDto {
        // Координаты 0..MAX_KB_ROW_BUTTONS — в u8 помещаются с огромным
        // запасом, поэтому `as` здесь не может потерять значение.
        row: row as u8,
        col: col as u8,
        text: truncate_chars(b.action.label.trim(), BUTTON_LABEL_CHARS),
        supported: button_is_supported(&kind),
        // Ссылка значима только у `open_link`; у остальных видов VK кладёт в
        // `link` пустую строку, а у `open_app` — адрес мини-приложения, который
        // открывать в браузере бессмысленно.
        url: (kind == "open_link" && !b.action.link.trim().is_empty())
            .then(|| b.action.link.trim().to_string()),
        color: BUTTON_COLORS.contains(&color).then(|| color.to_string()),
        kind,
    }
}

// ---------------------------------------------------------------------------
// Арифметика идентификаторов.
// ---------------------------------------------------------------------------

/// Нижняя граница `peer_id` групповых бесед: `2_000_000_000 + chat_id`.
///
/// Именно из-за неё `dialog_id` — строка: значение выходит за i32, а через
/// пару лет активной беседы могло бы подобраться и к границам точности
/// JS-числа. Строковый контракт снимает вопрос навсегда и совпадает с
/// телеграмным.
pub const CHAT_PEER_BASE: i64 = 2_000_000_000;

/// `peer_id` → `dialog_id` для провода.
pub fn dialog_id_string(peer_id: i64) -> String {
    peer_id.to_string()
}

/// `dialog_id` из URL → `peer_id`.
///
/// Возвращает `None` на всё, что не является непустым целым: мусор, дробное,
/// переполнение i64, `+1`, emoji, пустая строка. Ноль тоже отвергается — peer
/// с таким идентификатором у VK не существует, а «нулевой» диалог в путях
/// означал бы, что клиент подставил неинициализированную переменную.
///
/// Пробелы по краям срезаются: `dialog_id` попадает в URL из curl и копипасты,
/// и `" 1234 "` — это тот же диалог, а не другой (иначе кэши и Map'ы фронта
/// расходились бы по вариантам записи одного id).
///
/// Ведущий `+` НЕ принимается намеренно, хотя `i64::from_str` его понимает:
/// `+123` и `123` — одна беседа, и два написания одного id развели бы ключи
/// кэша. Проверка делается ДО парсинга, поэтому решение видно в коде, а не
/// прячется в поведении стандартной библиотеки.
pub fn parse_dialog_id(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() || t.starts_with('+') {
        return None;
    }
    let id: i64 = t.parse().ok()?;
    (id != 0).then_some(id)
}

/// Тип peer'а по его идентификатору.
///
/// Диапазоны VK: отрицательный — сообщество, `>= `[`CHAT_PEER_BASE`] —
/// групповая беседа, остальное — пользователь. Функция ЧИСТАЯ и не требует ни
/// сети, ни кэша: это то самое место, где VK удобнее Telegram — тип
/// собеседника выводится из самого id.
pub fn peer_kind(peer_id: i64) -> &'static str {
    if peer_id >= CHAT_PEER_BASE {
        "chat"
    } else if peer_id < 0 {
        "group"
    } else {
        // Ноль сюда попасть не должен (его отсекает `parse_dialog_id`), но
        // паниковать на нём незачем: «пользователь» — безопасный ответ.
        "user"
    }
}

/// Текущее unix-время, секунды. Одна точка на модуль — тесты передают своё.
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Обрезает строку по `max` СИМВОЛАМ (не байтам) и добавляет многоточие.
///
/// По символам, а не по байтам: срез `&s[..140]` на кириллице паникует, а
/// «140 байт» превью — это 70 русских букв, то есть половина обещанного.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Сужает i64 до i32 без паники.
///
/// У VK идентификаторы сообщений 32-битные, но приезжают в JSON числом без
/// объявленной ширины. Насыщение вместо `unwrap` выбрано осознанно: испорченное
/// значение id хуже, чем «сообщение не открылось», но и то и другое лучше
/// падения всей страницы истории.
fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

// ---------------------------------------------------------------------------
// Источник имён.
// ---------------------------------------------------------------------------

/// Откуда проекция берёт отображаемые имена.
///
/// Реализуется [`crate::peers::PeerCache`]; в юнит-тестах используется
/// [`NoNames`]. Трейт СИНХРОННЫЙ: кэш имён живёт в памяти, а любая сеть
/// («узнать имя») обязана случиться ДО проекции — иначе на каждой строке
/// истории мог бы незаметно появиться сетевой вызов, а бюджет у нас 3 rps.
pub trait PeerNames {
    /// Карточка автора по его id (отрицательный — сообщество).
    fn sender(&self, id: i64) -> Option<VkUserDto>;

    /// Собеседник в друзьях (`users.get(fields=is_friend)`).
    fn is_friend(&self, _id: i64) -> bool {
        false
    }

    /// Число участников беседы, если известно (для статус-строки).
    fn members(&self, _id: i64) -> Option<i32> {
        None
    }
}

/// «Имён нет» — проекция отдаёт то, что есть в самом сообщении.
///
/// Не заглушка ради тестов: ровно это состояние возникает в жизни, когда VK
/// прислал сообщение от id, которого не оказалось ни в `profiles`, ни в
/// `groups`. Показать в такой строке числовой id честнее, чем не показать
/// сообщение.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoNames;

impl PeerNames for NoNames {
    fn sender(&self, _id: i64) -> Option<VkUserDto> {
        None
    }
}

/// Автор сообщения: из кэша имён, иначе — голый id.
///
/// `None` только при `from_id == 0` (у VK так приходят системные записи).
fn sender_dto<N: PeerNames + ?Sized>(names: &N, from_id: i64) -> Option<VkUserDto> {
    if from_id == 0 {
        return None;
    }
    Some(names.sender(from_id).unwrap_or(VkUserDto {
        id: from_id.to_string(),
        ..VkUserDto::default()
    }))
}

// ---------------------------------------------------------------------------
// Проекция беседы.
// ---------------------------------------------------------------------------

/// `conversation` + её последнее сообщение → [`DialogDto`].
///
/// Возвращает `None` для бесед, которых в UI быть не должно:
///
/// - `peer.type == "email"` — устаревший вид переписки VK, у него нет ни
///   истории в нашем понимании, ни возможности ответить;
/// - `peer.id == 0` — беседа без адреса (такое приезжает в усечённых ответах);
///   строка с ней всё равно никуда не кликается.
///
/// `now` приходит параметром ради `muted`: «заглушена до» сравнивается с
/// текущим временем, и тест на границу не должен зависеть от системных часов.
pub fn dialog_from_raw<N: PeerNames + ?Sized>(
    conv: &crate::api::schema::ConversationRaw,
    last: Option<&crate::api::schema::MessageRaw>,
    names: &N,
    now: i64,
) -> Option<DialogDto> {
    let id = conv.peer.id;
    if id == 0 || conv.peer.kind == "email" {
        return None;
    }
    // Тип берём из ответа, но незнакомую строку выводим из самого id: набор
    // значений `peer.type` VK менял и раньше, а диапазоны идентификаторов —
    // нет.
    let kind = match conv.peer.kind.as_str() {
        "user" | "chat" | "group" => conv.peer.kind.clone(),
        _ => peer_kind(id).to_string(),
    };

    let chat = conv.chat_settings.as_ref();
    let profile = names.sender(id);
    let title = if kind == "chat" {
        chat.map(|c| c.title.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Беседа {}", id - CHAT_PEER_BASE))
    } else {
        profile
            .as_ref()
            .map(VkUserDto::display_name)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| dialog_id_string(id))
    };

    let muted = conv
        .push_settings
        .as_ref()
        .is_some_and(|p| p.disabled_forever || p.disabled_until > now);

    Some(DialogDto {
        id: dialog_id_string(id),
        // `bot` считается до move `kind` в структуру: у VK нет флага «бот», и
        // сообщество и есть бот с точки зрения UI.
        bot: kind == "group",
        kind,
        title,
        username: profile
            .as_ref()
            .and_then(|p| p.screen_name.clone())
            .filter(|s| !s.is_empty()),
        contact: names.is_friend(id),
        unread_count: clamp_i32(conv.unread_count),
        // ↓ три константы формы Telegram: закрепления диалогов, архива и папок
        //   у VK нет. Удалять поля нельзя — каждое стало бы `if` во фронте.
        pinned: false,
        archived: false,
        folder_id: None,
        muted,
        last_message: last.map(|m| message_from_raw(m, names)),
        members: chat
            .map(|c| c.members_count)
            .filter(|n| *n > 0)
            .map(clamp_i32),
        important: conv.important,
        can_write: conv.can_write.allowed,
        // Код причины интересен UI только когда писать НЕЛЬЗЯ: у разрешённой
        // беседы VK кладёт туда ноль, и отдавать его наружу значило бы
        // предлагать фронту объяснять несуществующий запрет.
        can_write_reason: (!conv.can_write.allowed && conv.can_write.reason != 0)
            .then(|| clamp_i32(conv.can_write.reason)),
    })
}

// ---------------------------------------------------------------------------
// Проекция сообщения.
// ---------------------------------------------------------------------------

/// `message` → [`MessageDto`].
///
/// Ни одного сетевого вызова: всё, что нужно, VK уже прислал в самом объекте
/// (в том числе `reply_message` — см. шапку модуля), а имена приходят из
/// [`PeerNames`].
pub fn message_from_raw<N: PeerNames + ?Sized>(
    msg: &crate::api::schema::MessageRaw,
    names: &N,
) -> MessageDto {
    let attachments = crate::attach::media_list(&msg.attachments);
    MessageDto {
        id: clamp_i32(msg.id),
        dialog_id: dialog_id_string(msg.peer_id),
        out: msg.out == 1,
        date: msg.date,
        sender: sender_dto(names, msg.from_id),
        text: msg.text.clone(),
        // Первое вложение — ради формы Telegram: свёрнутая строка ленты
        // рисуется тем же кодом. Источник истины — `attachments`.
        media: attachments.first().cloned(),
        attachments,
        reply_to_id: msg
            .reply_message
            .as_ref()
            .map(|r| clamp_i32(r.id))
            .filter(|v| *v > 0),
        reply_to: msg
            .reply_message
            .as_ref()
            .map(|r| reply_to_from_nested(r, names)),
        // `update_time` у неправленого сообщения — ноль, а `Some(0)` заставил
        // бы UI нарисовать «(edited)» под каждым сообщением.
        edit_date: (msg.update_time > 0).then_some(msg.update_time),
        // `payload` кнопок наружу НЕ едет (см. [`ReplyButtonDto`]): нажатие
        // адресуется координатами, и подделать полезную нагрузку с фронта
        // нечем.
        reply_markup: msg.keyboard.as_ref().and_then(reply_markup_from_raw),
        cmid: (msg.conversation_message_id > 0).then(|| clamp_i32(msg.conversation_message_id)),
        fwd: flatten_forwards(&msg.fwd_messages, names),
        action: msg.action.as_ref().map(action_from_raw),
        payload: msg.payload.clone().filter(|p| !p.is_empty()),
    }
}

/// Инлайненный `reply_message` → [`ReplyToDto`], БЕЗ дозапроса.
///
/// Главный размен всего плагина: у Telegram оригинал цитаты приходится
/// добирать вторым RPC (~250 строк обогащения в его `dto.rs`), у VK он уже
/// лежит внутри сообщения. Страница истории стоит один запрос вместо двух.
///
/// Вглубь не рекурсируем: у цитаты бывают свои цитаты и пересылки, но в
/// превью нужен один уровень — остальное пользователь увидит, перейдя к
/// оригиналу.
pub fn reply_to_from_nested<N: PeerNames + ?Sized>(
    m: &crate::api::schema::NestedMessageRaw,
    names: &N,
) -> ReplyToDto {
    let text = m.text.trim();
    let media_kind = crate::attach::first_kind(&m.attachments);
    ReplyToDto {
        message_id: (m.id > 0).then(|| clamp_i32(m.id)),
        // Именно `cmid` адресует ответ в беседах — поэтому он едет отдельным
        // полем, а не «вместо» message_id.
        cmid: (m.conversation_message_id > 0).then(|| clamp_i32(m.conversation_message_id)),
        sender: sender_dto(names, m.from_id).map(|s| s.display_name()),
        sender_id: (m.from_id != 0).then(|| m.from_id.to_string()),
        text: (!text.is_empty()).then(|| truncate_chars(text, REPLY_PREVIEW_CHARS)),
        media_kind,
        // Пустой во всех отношениях объект — это «оригинал недоступен»
        // (удалён либо скрыт приватностью), а не «сообщение без текста».
        unavailable: m.date == 0
            && text.is_empty()
            && m.attachments.is_empty()
            && m.conversation_message_id == 0,
    }
}

/// `fwd_messages` → плоский список с глубиной, обрезанный по границам.
///
/// У VK пересылки РЕКУРСИВНЫ и по документации ничем не ограничены: цепочка
/// «переслал пересланное» может уходить сколь угодно глубоко и быть сколь
/// угодно широкой на каждом уровне. Наивный разбор превращает один DTO в
/// мегабайт, а один экран ленты — в бесконечную простыню, поэтому границы
/// [`MAX_FWD_DEPTH`] и [`MAX_FWD_PER_LEVEL`] — не оптимизация, а условие
/// работоспособности.
///
/// Отброшенный остаток уровня записывается в [`FwdDto::omitted`] ПОСЛЕДНЕГО
/// уместившегося соседа: отдельная запись-«хвост» без автора и даты выглядела
/// бы в ленте как ещё одно пересланное сообщение.
pub fn flatten_forwards<N: PeerNames + ?Sized>(
    list: &[crate::api::schema::NestedMessageRaw],
    names: &N,
) -> Vec<FwdDto> {
    let mut out = Vec::new();
    push_fwd_level(&mut out, list, 0, names);
    out
}

/// Один уровень пересылок; рекурсия ограничена [`MAX_FWD_DEPTH`].
fn push_fwd_level<N: PeerNames + ?Sized>(
    out: &mut Vec<FwdDto>,
    list: &[crate::api::schema::NestedMessageRaw],
    depth: u8,
    names: &N,
) {
    if usize::from(depth) >= MAX_FWD_DEPTH || list.is_empty() {
        return;
    }
    let taken = list.len().min(MAX_FWD_PER_LEVEL);
    let omitted = (list.len() - taken) as u32;
    for (i, m) in list.iter().take(taken).enumerate() {
        let text = m.text.trim();
        out.push(FwdDto {
            depth,
            sender: sender_dto(names, m.from_id),
            date: m.date,
            text: (!text.is_empty()).then(|| truncate_chars(text, REPLY_PREVIEW_CHARS)),
            media_kind: crate::attach::first_kind(&m.attachments),
            omitted: if i + 1 == taken { omitted } else { 0 },
        });
        push_fwd_level(out, &m.fwd_messages, depth + 1, names);
    }
}

/// Служебное действие → [`ActionDto`].
///
/// Незнакомый вид сохраняется КАК ЕСТЬ с пустым текстом: набор действий VK
/// расширяет регулярно, и «сообщение исчезло из ленты» — худшая реакция на
/// новый вид, чем «строка без пояснения».
pub fn action_from_raw(a: &crate::api::schema::ActionRaw) -> ActionDto {
    let subject = a.text.trim();
    let text = match a.kind.as_str() {
        "chat_photo_update" => "обновил фотографию беседы".to_string(),
        "chat_photo_remove" => "удалил фотографию беседы".to_string(),
        "chat_create" if !subject.is_empty() => format!("создал беседу «{subject}»"),
        "chat_create" => "создал беседу".to_string(),
        "chat_title_update" if !subject.is_empty() => {
            format!("изменил название беседы на «{subject}»")
        }
        "chat_title_update" => "изменил название беседы".to_string(),
        "chat_invite_user" => "пригласил участника".to_string(),
        "chat_invite_user_by_link" => "присоединился по ссылке".to_string(),
        "chat_invite_user_by_call" | "chat_invite_user_by_call_join_link" => {
            "присоединился к звонку".to_string()
        }
        "chat_kick_user" => "исключил участника".to_string(),
        "chat_pin_message" => "закрепил сообщение".to_string(),
        "chat_unpin_message" => "открепил сообщение".to_string(),
        "chat_screenshot" => "сделал скриншот беседы".to_string(),
        "conversation_style_update" | "conversation_style_update_action" => {
            "изменил оформление беседы".to_string()
        }
        // ↓ forward-compat: вид доезжает до UI строкой, текста у нас для него
        //   нет — и выдумывать его нельзя.
        _ => String::new(),
    };
    ActionDto {
        kind: a.kind.clone(),
        member_id: (a.member_id != 0).then(|| a.member_id.to_string()),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        ActionRaw, CanWriteRaw, ChatSettingsRaw, ConversationRaw, ItemsRaw, KeyboardActionRaw,
        KeyboardButtonRaw, KeyboardRaw, MessageRaw, NestedMessageRaw, PeerRaw, PushSettingsRaw,
    };

    /// Формы структур — часть контракта с фронтом: он читает поля по именам.
    /// Round-trip ловит опечатку в `rename`/`serde(default)` раньше, чем её
    /// поймает вкладка.
    #[test]
    fn wire_structs_round_trip() {
        let msg = MessageDto {
            id: 42,
            dialog_id: "2000000001".into(),
            out: true,
            date: 1_800_000_000,
            text: "привет".into(),
            attachments: vec![MediaDto {
                index: 1,
                kind: "photo".into(),
                has_thumb: true,
                has_full: true,
                ..MediaDto::default()
            }],
            cmid: Some(7),
            fwd: vec![FwdDto {
                depth: 1,
                omitted: 3,
                ..FwdDto::default()
            }],
            action: Some(ActionDto {
                kind: "chat_invite_user".into(),
                member_id: Some("-42".into()),
                text: String::new(),
            }),
            reply_to: Some(ReplyToDto {
                message_id: Some(41),
                cmid: Some(6),
                ..ReplyToDto::default()
            }),
            reply_markup: Some(ReplyMarkupDto {
                kind: "inline".into(),
                rows: vec![vec![ReplyButtonDto {
                    text: "Да".into(),
                    kind: "text".into(),
                    supported: true,
                    ..ReplyButtonDto::default()
                }]],
                single_use: false,
            }),
            ..MessageDto::default()
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: MessageDto = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back, "round-trip сломался на {json}");

        let dialog = DialogDto {
            id: "-1234".into(),
            kind: "group".into(),
            title: "Сообщество".into(),
            can_write: true,
            last_message: Some(msg),
            ..DialogDto::default()
        };
        let json = serde_json::to_string(&dialog).unwrap();
        let back: DialogDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dialog, back);
    }

    /// Поля, которых у VK нет, обязаны остаться в форме с константными
    /// значениями: их удаление превратило бы каждый фронт-модуль в форк.
    #[test]
    fn telegram_shaped_constants_are_present_and_default_to_absent() {
        let d = DialogDto::default();
        assert!(!d.pinned, "VK не закрепляет диалоги");
        assert!(!d.archived, "архива у VK нет");
        assert!(d.folder_id.is_none(), "папок у VK нет");

        let json = serde_json::to_value(&d).unwrap();
        for key in ["pinned", "archived", "folder_id"] {
            assert!(json.get(key).is_some(), "{key} обязан присутствовать в JSON");
        }
    }

    /// Индекс вложения — часть контракта: без него ключ медиа-кэша не
    /// различает два фото одного сообщения.
    #[test]
    fn media_dto_carries_the_attachment_index() {
        let json = serde_json::to_value(MediaDto {
            index: 9,
            kind: "photo".into(),
            ..MediaDto::default()
        })
        .unwrap();
        assert_eq!(json["index"], 9);
    }

    // -----------------------------------------------------------------
    // Арифметика идентификаторов.
    // -----------------------------------------------------------------

    /// Границы диапазонов VK. `2_000_000_000` — ПЕРВЫЙ беседный peer, а
    /// `1_999_999_999` — ещё пользователь: ошибка на единицу здесь означала бы,
    /// что одна беседа в аккаунте рисуется как личка.
    #[test]
    fn peer_kind_boundaries() {
        let table = [
            (1_999_999_999_i64, "user"),
            (CHAT_PEER_BASE, "chat"),
            (CHAT_PEER_BASE + 7, "chat"),
            (-1, "group"),
            (-55, "group"),
            (1, "user"),
            (0, "user"),
            (i64::MAX, "chat"),
            (i64::MIN, "group"),
        ];
        for (id, expected) in table {
            assert_eq!(peer_kind(id), expected, "peer_id {id}");
        }
    }

    /// `dialog_id` из URL: разбор обязан быть строгим и молчаливым.
    #[test]
    fn parse_dialog_id_accepts_only_real_peers() {
        assert_eq!(parse_dialog_id("1234"), Some(1234));
        assert_eq!(parse_dialog_id("-55"), Some(-55));
        assert_eq!(parse_dialog_id("2000000007"), Some(2_000_000_007));
        assert_eq!(parse_dialog_id("  1234  "), Some(1234), "копипаста из curl");

        for garbage in [
            "",
            "   ",
            "0",
            "-0",
            "+123",
            "12.5",
            "1e3",
            "abc",
            "null",
            "../../etc/passwd",
            "😀",
            "99999999999999999999999999",
            "1234;DROP TABLE",
        ] {
            assert_eq!(parse_dialog_id(garbage), None, "мусор {garbage:?}");
        }

        // Круговой ход: строка → i64 → строка.
        for id in [1_i64, -55, 2_000_000_007, i64::MAX, i64::MIN + 1] {
            assert_eq!(parse_dialog_id(&dialog_id_string(id)), Some(id), "{id}");
        }
    }

    /// Превью режется по СИМВОЛАМ: срез по байтам паниковал бы на кириллице,
    /// а «140 байт» — это 70 русских букв вместо обещанных 140.
    #[test]
    fn truncate_chars_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("привет", 10), "привет");
        assert_eq!(truncate_chars("привет", 3), "при…");
        assert_eq!(truncate_chars("", 3), "");
        let long: String = "я".repeat(500);
        let cut = truncate_chars(&long, REPLY_PREVIEW_CHARS);
        assert_eq!(cut.chars().count(), REPLY_PREVIEW_CHARS + 1, "+ многоточие");
        // Эмодзи из суррогатных пар не должны рваться пополам.
        assert_eq!(truncate_chars("😀😀😀", 2), "😀😀…");
    }

    // -----------------------------------------------------------------
    // Проекция беседы.
    // -----------------------------------------------------------------

    fn conv(id: i64, kind: &str) -> ConversationRaw {
        ConversationRaw {
            peer: PeerRaw {
                id,
                kind: kind.to_string(),
                local_id: id,
            },
            can_write: CanWriteRaw {
                allowed: true,
                reason: 0,
            },
            ..ConversationRaw::default()
        }
    }

    /// Имена «как в жизни»: пользователь 1234, сообщество -55.
    struct Names;
    impl PeerNames for Names {
        fn sender(&self, id: i64) -> Option<VkUserDto> {
            match id {
                1234 => Some(VkUserDto {
                    id: "1234".into(),
                    first_name: Some("Павел".into()),
                    last_name: Some("Дуров".into()),
                    screen_name: Some("durov".into()),
                }),
                -55 => Some(VkUserDto {
                    id: "-55".into(),
                    first_name: Some("Тестовое сообщество".into()),
                    screen_name: Some("test_club".into()),
                    ..VkUserDto::default()
                }),
                _ => None,
            }
        }
        fn is_friend(&self, id: i64) -> bool {
            id == 1234
        }
    }

    #[test]
    fn dialog_from_raw_maps_a_private_chat() {
        let d = dialog_from_raw(&conv(1234, "user"), None, &Names, 1_800_000_000).unwrap();
        assert_eq!(d.id, "1234");
        assert_eq!(d.kind, "user");
        assert_eq!(d.title, "Павел Дуров");
        assert_eq!(d.username.as_deref(), Some("durov"));
        assert!(!d.bot);
        assert!(d.contact, "is_friend ⇒ «контакт»");
        assert!(d.can_write);
        assert!(d.can_write_reason.is_none(), "у разрешённой беседы причины нет");
    }

    /// У сообщества `bot == true`: флага «бот» у VK нет вовсе, и сообщество
    /// и есть бот с точки зрения UI.
    #[test]
    fn dialog_from_raw_marks_communities_as_bots() {
        let mut c = conv(-55, "group");
        c.can_write = CanWriteRaw {
            allowed: false,
            reason: 18,
        };
        let d = dialog_from_raw(&c, None, &Names, 0).unwrap();
        assert_eq!(d.kind, "group");
        assert!(d.bot);
        assert_eq!(d.title, "Тестовое сообщество");
        assert!(!d.can_write);
        assert_eq!(d.can_write_reason, Some(18), "UI обязан объяснить запрет");
    }

    #[test]
    fn dialog_from_raw_uses_chat_settings_for_group_chats() {
        let mut c = conv(2_000_000_007, "chat");
        c.chat_settings = Some(ChatSettingsRaw {
            title: "  Проектная  ".into(),
            members_count: 4,
            ..ChatSettingsRaw::default()
        });
        let d = dialog_from_raw(&c, None, &Names, 0).unwrap();
        assert_eq!(d.kind, "chat");
        assert_eq!(d.title, "Проектная");
        assert!(!d.bot);
        assert_eq!(d.members, Some(4), "статус-строка беседы — «[CHAT] N members»");

        // Беседа без названия всё равно обязана быть подписана.
        let mut c = conv(2_000_000_007, "chat");
        c.chat_settings = Some(ChatSettingsRaw::default());
        assert_eq!(dialog_from_raw(&c, None, &Names, 0).unwrap().title, "Беседа 7");
    }

    /// Глушение: `disabled_forever` и «до момента» с обеих сторон границы.
    #[test]
    fn muted_reads_push_settings_against_the_given_now() {
        let now = 1_800_000_000;
        let mut c = conv(1234, "user");
        assert!(!dialog_from_raw(&c, None, &Names, now).unwrap().muted);

        c.push_settings = Some(PushSettingsRaw {
            disabled_forever: true,
            ..PushSettingsRaw::default()
        });
        assert!(dialog_from_raw(&c, None, &Names, now).unwrap().muted);

        c.push_settings = Some(PushSettingsRaw {
            disabled_until: now + 60,
            ..PushSettingsRaw::default()
        });
        assert!(dialog_from_raw(&c, None, &Names, now).unwrap().muted);

        c.push_settings = Some(PushSettingsRaw {
            disabled_until: now - 1,
            ..PushSettingsRaw::default()
        });
        assert!(
            !dialog_from_raw(&c, None, &Names, now).unwrap().muted,
            "истёкшее глушение больше не глушит"
        );
    }

    /// Беседы, которых в UI быть не должно: устаревший `email` и беседа без
    /// адреса. Пропускается СТРОКА целиком — рисовать её некуда.
    #[test]
    fn email_and_addressless_conversations_are_skipped() {
        assert!(dialog_from_raw(&conv(-9, "email"), None, &Names, 0).is_none());
        assert!(dialog_from_raw(&conv(0, "user"), None, &Names, 0).is_none());
        // Незнакомый тип не выбрасывается — он выводится из самого id.
        let d = dialog_from_raw(&conv(2_000_000_001, "quantum"), None, &Names, 0).unwrap();
        assert_eq!(d.kind, "chat");
    }

    /// Три поля формы Telegram, которых у VK нет, обязаны приезжать
    /// константами — иначе каждое стало бы `if` во фронте.
    #[test]
    fn telegram_shaped_constants_stay_constant_in_projection() {
        let mut c = conv(2_000_000_007, "chat");
        c.important = true;
        c.unread_count = 42;
        let d = dialog_from_raw(&c, None, &Names, 0).unwrap();
        assert!(!d.pinned, "закрепления диалогов у VK нет");
        assert!(!d.archived, "архива у VK нет");
        assert!(d.folder_id.is_none(), "папок у VK нет");
        assert!(d.members.is_none(), "у беседы без chat_settings числа участников нет");
        assert!(d.important, "«важное» — наоборот, есть и это фильтр");
        assert_eq!(d.unread_count, 42);
    }

    // -----------------------------------------------------------------
    // Проекция сообщения.
    // -----------------------------------------------------------------

    fn history() -> ItemsRaw<MessageRaw> {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/get_history.json")).unwrap();
        serde_json::from_value(v["response"].clone()).unwrap()
    }

    #[test]
    fn message_from_raw_maps_the_fixture_page() {
        let page = history();
        let dtos: Vec<MessageDto> = page
            .items
            .iter()
            .map(|m| message_from_raw(m, &Names))
            .collect();
        assert_eq!(dtos.len(), 5);

        let first = &dtos[0];
        assert_eq!(first.id, 7001);
        assert_eq!(first.dialog_id, "1234");
        assert!(!first.out);
        assert_eq!(first.text, "простое входящее");
        assert_eq!(first.cmid, Some(512));
        assert_eq!(
            first.sender.as_ref().unwrap().display_name(),
            "Павел Дуров",
            "имя приходит из кэша, а не из сообщения"
        );
        assert!(first.edit_date.is_none());
        assert!(first.media.is_none() && first.attachments.is_empty());

        // Исходящее от неизвестного нам id: карточка автора всё равно есть.
        assert!(dtos[1].out);
        assert_eq!(dtos[1].sender.as_ref().unwrap().id, "999");

        // Правленое сообщение с двумя вложениями и пустым текстом.
        let media = &dtos[3];
        assert_eq!(media.text, "");
        assert_eq!(media.attachments.len(), 2);
        assert_eq!(media.media.as_ref().unwrap().kind, "photo");
        assert_eq!(media.attachments[1].kind, "voice");
        assert_eq!(media.edit_date, Some(1_700_000_140));

        // Служебное сообщение из беседы.
        let action = dtos[4].action.as_ref().unwrap();
        assert_eq!(action.kind, "chat_invite_user");
        assert_eq!(action.member_id.as_deref(), Some("4321"));
        assert_eq!(dtos[4].dialog_id, "2000000007");
    }

    /// Ответ приезжает ИНЛАЙНОМ: подсистемы дозапроса оригиналов в плагине
    /// нет вовсе, и превью обязано собираться прямо здесь.
    #[test]
    fn reply_preview_is_built_without_any_extra_request() {
        let page = history();
        let raw = page.items.iter().find(|m| m.reply_message.is_some()).unwrap();
        let dto = message_from_raw(raw, &Names);
        let reply = dto.reply_to.as_ref().unwrap();
        assert_eq!(reply.text.as_deref(), Some("оригинал"));
        assert_eq!(reply.cmid, Some(513), "цель ответа адресуется через cmid");
        assert_eq!(reply.sender_id.as_deref(), Some("999"));
        assert!(!reply.unavailable);
        // У оригинала в фикстуре собственного id нет — и это норма.
        assert!(reply.message_id.is_none());
        assert!(dto.reply_to_id.is_none());
    }

    /// У VK текст цитаты бывает пустым при непустых вложениях — тогда превью
    /// несёт вид вложения, иначе строка ответа выглядела бы пустой.
    #[test]
    fn reply_preview_falls_back_to_the_attachment_kind() {
        let nested = NestedMessageRaw {
            conversation_message_id: 9,
            from_id: 1234,
            date: 1_700_000_000,
            attachments: vec![crate::api::schema::AttachmentRaw {
                kind: "audio_message".into(),
                ..Default::default()
            }],
            ..NestedMessageRaw::default()
        };
        let r = reply_to_from_nested(&nested, &Names);
        assert!(r.text.is_none());
        assert_eq!(r.media_kind.as_deref(), Some("voice"));
        assert_eq!(r.sender.as_deref(), Some("Павел Дуров"));
        assert!(!r.unavailable);

        // Совсем пустой объект — это «оригинал недоступен».
        let r = reply_to_from_nested(&NestedMessageRaw::default(), &NoNames);
        assert!(r.unavailable);
        assert!(r.text.is_none() && r.sender.is_none());
    }

    /// Превью режется по CODE POINT'ам, а не по байтам.
    ///
    /// Проверка не про экономию трафика: у кириллицы два байта на символ, у
    /// эмодзи — четыре, и обрезка по байтам разрубила бы символ пополам,
    /// оставив в JSON битую последовательность. Ровно `REPLY_PREVIEW_CHARS`
    /// символов исходника плюс многоточие — весь контракт.
    #[test]
    fn reply_preview_is_cut_by_code_points_not_bytes() {
        // 200 символов кириллицы: 400 байт, то есть байтовая обрезка по 140
        // пришлась бы ровно на середину символа.
        let long = "я".repeat(200);
        let nested = NestedMessageRaw {
            conversation_message_id: 9,
            from_id: 1234,
            date: 1_700_000_000,
            text: long,
            ..NestedMessageRaw::default()
        };
        let r = reply_to_from_nested(&nested, &Names);
        let text = r.text.unwrap();
        assert_eq!(
            text.chars().count(),
            REPLY_PREVIEW_CHARS + 1,
            "140 символов исходника плюс многоточие"
        );
        assert!(text.ends_with('…'));
        assert!(text.is_char_boundary(text.len()), "строка не разрублена");

        // Эмодзи вне BMP: одна суррогатная пара в UTF-16, один code point в
        // Rust. Ни одного «половинного» символа на границе быть не должно.
        let emoji = "🙂".repeat(200);
        let nested = NestedMessageRaw {
            conversation_message_id: 9,
            date: 1_700_000_000,
            text: emoji,
            ..NestedMessageRaw::default()
        };
        let text = reply_to_from_nested(&nested, &NoNames).text.unwrap();
        assert_eq!(text.chars().filter(|c| *c == '🙂').count(), REPLY_PREVIEW_CHARS);

        // Ровно на границе — обрезки нет вовсе, многоточие не приписывается.
        let exact = "a".repeat(REPLY_PREVIEW_CHARS);
        let nested = NestedMessageRaw {
            conversation_message_id: 9,
            date: 1_700_000_000,
            text: exact.clone(),
            ..NestedMessageRaw::default()
        };
        assert_eq!(
            reply_to_from_nested(&nested, &NoNames).text.as_deref(),
            Some(exact.as_str())
        );
    }

    // -----------------------------------------------------------------
    // Пересылки.
    // -----------------------------------------------------------------

    /// Рекурсивное дерево пересылок: глубина 5, ширина 30 на каждом уровне.
    /// Без границ такой DTO раздувается неограниченно — это и есть причина
    /// существования MAX_FWD_DEPTH/MAX_FWD_PER_LEVEL.
    #[test]
    fn deep_and_wide_forwards_are_capped_and_terminate() {
        fn tree(depth: usize, width: usize) -> Vec<NestedMessageRaw> {
            if depth == 0 {
                return Vec::new();
            }
            (0..width)
                .map(|i| NestedMessageRaw {
                    text: format!("d{depth}-{i}"),
                    date: 1_700_000_000,
                    from_id: 1234,
                    fwd_messages: tree(depth - 1, width),
                    ..NestedMessageRaw::default()
                })
                .collect()
        }

        let flat = flatten_forwards(&tree(5, 30), &Names);
        // Уровней ровно MAX_FWD_DEPTH, на каждом не больше MAX_FWD_PER_LEVEL
        // соседей у одного родителя.
        let max_depth = flat.iter().map(|f| f.depth).max().unwrap();
        assert_eq!(usize::from(max_depth), MAX_FWD_DEPTH - 1);
        let top: Vec<&FwdDto> = flat.iter().filter(|f| f.depth == 0).collect();
        assert_eq!(top.len(), MAX_FWD_PER_LEVEL);
        assert_eq!(
            top.last().unwrap().omitted,
            20,
            "отброшенный остаток уровня обязан доехать числом"
        );
        assert_eq!(top[0].omitted, 0, "у не-последних соседей хвоста нет");
        // 10 + 10*10 + 10*10*10 — и ни строкой больше.
        assert_eq!(flat.len(), 10 + 100 + 1000);
        assert_eq!(flat[0].sender.as_ref().unwrap().display_name(), "Павел Дуров");
    }

    #[test]
    fn forwards_from_the_fixture_keep_order_and_depth() {
        let page = history();
        let raw = page
            .items
            .iter()
            .find(|m| !m.fwd_messages.is_empty())
            .unwrap();
        let dto = message_from_raw(raw, &Names);
        assert_eq!(dto.fwd.len(), 3, "две пересылки + одна вложенная");
        assert_eq!(dto.fwd[0].depth, 0);
        assert_eq!(dto.fwd[0].text.as_deref(), Some("пересланное раз"));
        assert_eq!(dto.fwd[1].depth, 0);
        assert_eq!(dto.fwd[2].depth, 1, "вложенная пересылка идёт следом");
        assert_eq!(dto.fwd[2].text.as_deref(), Some("вложенная пересылка"));
        assert!(dto.fwd.iter().all(|f| f.omitted == 0));
    }

    // -----------------------------------------------------------------
    // Служебные сообщения.
    // -----------------------------------------------------------------

    /// Известные виды получают человеческий текст, НЕИЗВЕСТНЫЙ доезжает как
    /// есть с пустым текстом — выдумывать формулировку за VK нельзя.
    #[test]
    fn action_table_and_forward_compatibility() {
        let cases = [
            ("chat_photo_update", "", "обновил фотографию беседы"),
            ("chat_title_update", "Новая", "изменил название беседы на «Новая»"),
            ("chat_title_update", "", "изменил название беседы"),
            ("chat_create", "Проектная", "создал беседу «Проектная»"),
            ("chat_invite_user", "", "пригласил участника"),
            ("chat_kick_user", "", "исключил участника"),
            ("chat_pin_message", "", "закрепил сообщение"),
            ("chat_teleport_update", "", ""),
        ];
        for (kind, text, expected) in cases {
            let a = action_from_raw(&ActionRaw {
                kind: kind.into(),
                text: text.into(),
                member_id: 7,
                ..ActionRaw::default()
            });
            assert_eq!(a.kind, kind, "вид сохраняется дословно");
            assert_eq!(a.text, expected, "{kind}/{text}");
            assert_eq!(a.member_id.as_deref(), Some("7"));
        }
        // Действие без участника не должно отдавать «0».
        assert!(action_from_raw(&ActionRaw::default()).member_id.is_none());
    }

    /// Неподдерживаемая кнопка приезжает С ТЕКСТОМ и с `supported:false` —
    /// не исчезает.
    #[test]
    fn unsupported_button_keeps_its_label() {
        let b = ReplyButtonDto {
            text: "Оплатить".into(),
            kind: "vkpay".into(),
            supported: false,
            ..ReplyButtonDto::default()
        };
        let json = serde_json::to_value(&b).unwrap();
        assert_eq!(json["text"], "Оплатить");
        assert_eq!(json["supported"], false);
    }

    // -----------------------------------------------------------------
    // Клавиатуры сообществ (Ф9).
    // -----------------------------------------------------------------

    /// Кнопка стенда: вид, подпись, полезная нагрузка и ссылка.
    fn kb_button(kind: &str, label: &str, payload: &str, link: &str) -> KeyboardButtonRaw {
        KeyboardButtonRaw {
            color: "primary".into(),
            action: KeyboardActionRaw {
                kind: kind.into(),
                label: label.into(),
                payload: payload.into(),
                link: link.into(),
                ..KeyboardActionRaw::default()
            },
        }
    }

    /// Клавиатура из одного ряда.
    fn kb_of(buttons: Vec<KeyboardButtonRaw>) -> KeyboardRaw {
        KeyboardRaw {
            buttons: vec![buttons],
            ..KeyboardRaw::default()
        }
    }

    /// Табличный разбор ВСЕХ видов кнопок VK плюс незнакомого.
    ///
    /// Мутация «пометить `callback` как `supported: true`» краснеет ровно
    /// здесь: это единственное место, где решение о поддержке принимается.
    #[test]
    fn button_kinds_map_to_the_support_table() {
        let cases = [
            // (action.type, supported, есть ли url)
            ("text", true, false),
            ("open_link", true, true),
            ("callback", false, false),
            ("vkpay", false, false),
            ("open_app", false, false),
            ("location", false, false),
            ("intent_subscribe", false, false),
            ("", false, false),
        ];
        for (kind, supported, has_url) in cases {
            let kb = kb_of(vec![kb_button(kind, "Кнопка", "{\"a\":1}", "https://vk.com/x")]);
            let m = reply_markup_from_raw(&kb).unwrap();
            let b = &m.rows[0][0];
            // Незнакомый вид доезжает СТРОКОЙ как есть (forward-compat), а
            // пустой превращается в `other`: «вида нет» — это не вид.
            let expected_kind = if kind.is_empty() { "other" } else { kind };
            assert_eq!(b.kind, expected_kind, "{kind}");
            assert_eq!(b.supported, supported, "{kind}");
            assert_eq!(b.url.is_some(), has_url, "url только у open_link: {kind}");
            assert_eq!(b.text, "Кнопка", "{kind}");
            assert_eq!((b.row, b.col), (0, 0), "{kind}");
            assert_eq!(b.color.as_deref(), Some("primary"), "{kind}");
        }
        assert!(button_is_supported("text") && button_is_supported("open_link"));
        for kind in ["callback", "vkpay", "open_app", "location", "other", ""] {
            assert!(!button_is_supported(kind), "{kind}");
        }
    }

    /// Вид клавиатуры — это ровно флаг `inline`, а `one_time` — `single_use`.
    #[test]
    fn keyboard_kind_follows_the_inline_flag() {
        let mut kb = kb_of(vec![kb_button("text", "Да", "", "")]);
        assert_eq!(reply_markup_from_raw(&kb).unwrap().kind, "keyboard");
        assert!(!reply_markup_from_raw(&kb).unwrap().single_use);
        kb.inline = true;
        kb.one_time = true;
        let m = reply_markup_from_raw(&kb).unwrap();
        assert_eq!(m.kind, "inline");
        assert!(m.single_use);
    }

    /// Клавиатура 20×20 обрезается по обеим границам, а координаты остаются
    /// координатами СЫРОЙ матрицы — по ним сервер ищет нажатую кнопку.
    #[test]
    fn oversized_keyboard_is_capped_on_both_axes() {
        let kb = KeyboardRaw {
            buttons: (0..40)
                .map(|r| {
                    (0..20)
                        .map(|c| kb_button("text", &format!("{r}:{c}"), "", ""))
                        .collect()
                })
                .collect(),
            ..KeyboardRaw::default()
        };
        let m = reply_markup_from_raw(&kb).unwrap();
        assert_eq!(m.rows.len(), MAX_KB_ROWS);
        assert!(m.rows.iter().all(|r| r.len() == MAX_KB_ROW_BUTTONS));
        let last = m.rows.last().unwrap().last().unwrap();
        assert_eq!(
            (last.row as usize, last.col as usize),
            (MAX_KB_ROWS - 1, MAX_KB_ROW_BUTTONS - 1)
        );
        assert_eq!(last.text, "31:11", "координата совпадает с подписью кнопки");
    }

    /// Подпись обрезается по СИМВОЛАМ, кнопка без подписи остаётся на месте
    /// (её выбросит фронт), а координаты соседей не сдвигаются.
    #[test]
    fn labels_are_capped_and_empty_ones_keep_their_slot() {
        let long = "я".repeat(BUTTON_LABEL_CHARS + 40);
        let kb = kb_of(vec![
            kb_button("text", "", "", ""),
            kb_button("text", &long, "", ""),
        ]);
        let m = reply_markup_from_raw(&kb).unwrap();
        assert_eq!(m.rows[0][0].text, "");
        assert_eq!((m.rows[0][1].row, m.rows[0][1].col), (0, 1));
        assert_eq!(
            m.rows[0][1].text.chars().count(),
            BUTTON_LABEL_CHARS + 1,
            "обрезка добавляет многоточие"
        );
    }

    /// Клавиатуры нет вовсе / она пуста ⇒ разметки нет: пустая рамка под
    /// сообщением читалась бы как сломанная клавиатура.
    #[test]
    fn absent_or_empty_keyboards_give_no_markup() {
        assert!(reply_markup_from_raw(&KeyboardRaw::default()).is_none());
        assert!(reply_markup_from_raw(&KeyboardRaw {
            buttons: vec![vec![], vec![]],
            ..KeyboardRaw::default()
        })
        .is_none());
        let msg = MessageRaw {
            id: 5,
            peer_id: -70,
            date: 1,
            ..MessageRaw::default()
        };
        assert!(message_from_raw(&msg, &NoNames).reply_markup.is_none());
        // Незнакомый цвет наружу не едет: значение уходит в data-атрибут.
        let kb = kb_of(vec![KeyboardButtonRaw {
            color: "rainbow".into(),
            action: KeyboardActionRaw {
                kind: "text".into(),
                label: "Х".into(),
                ..KeyboardActionRaw::default()
            },
        }]);
        assert!(reply_markup_from_raw(&kb).unwrap().rows[0][0].color.is_none());
    }

    /// `payload` кнопки НЕ покидает бэкенд.
    ///
    /// Мутация «отдать payload в DTO» краснеет здесь: нажатие адресуется
    /// координатами именно для того, чтобы подделать полезную нагрузку было
    /// нечем.
    #[test]
    fn button_payload_never_reaches_the_wire() {
        let msg = MessageRaw {
            id: 91,
            peer_id: -70,
            date: 1_700_000_000,
            keyboard: Some(kb_of(vec![
                kb_button("text", "Да", "{\"cmd\":\"secret\"}", ""),
                kb_button("callback", "Нет", "{\"cmd\":\"other\"}", ""),
            ])),
            ..MessageRaw::default()
        };
        let dto = message_from_raw(&msg, &NoNames);
        let markup = dto.reply_markup.clone().unwrap();
        assert_eq!(markup.rows[0].len(), 2);
        assert!(markup.rows[0][0].supported && !markup.rows[0][1].supported);

        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("secret"), "payload кнопки уехал на провод: {json}");
        assert!(!json.contains("cmd"), "payload кнопки уехал на провод: {json}");
        let markup_json = serde_json::to_value(&markup).unwrap();
        for b in markup_json["rows"][0].as_array().unwrap() {
            assert!(b.get("payload").is_none(), "поле payload у кнопки: {b}");
            // Координаты, наоборот, обязаны быть: без них нажимать нечем.
            assert!(b.get("row").is_some() && b.get("col").is_some());
        }
    }
}
