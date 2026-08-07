//! Проводные DTO панели и ЧИСТАЯ проекция сырья портала в них.
//!
//! # Что здесь есть
//!
//! 1. ФОРМА структур — то, что уезжает в HTTP-ответ и в WS-кадры
//!    ([`crate::ws::protocol`]). Форма — контракт с фронтом, и менять её
//!    можно только вместе с `static/js/bitrix/*`.
//! 2. ПРОЕКЦИЯ `api/schema.rs` → DTO ([`dialog_from_raw`]). Проекция
//!    **не делает сетевых вызовов вообще**: имена приходят через трейт
//!    [`PeerNames`], поэтому вся таблица соответствий проверяется юнит-тестами
//!    на живых фикстурах, без стенда и без вебхука.
//! 3. АРИФМЕТИКА ИДЕНТИФИКАТОРОВ — [`dialog_id_string`], [`parse_dialog_id`],
//!    [`dialog_kind`]: единственное место, где `DIALOG_ID` разбирается и
//!    собирается.
//!
//! # `dialog_id` и `chat_id` — РАЗНЫЕ поля
//!
//! Путать их дороже всего в этом плагине, потому что оба числовые на вид:
//!
//! | | что это | примеры |
//! |---|---|---|
//! | `dialog_id` | адрес диалога в методах `im.*` | `"chat7"`, `"sg12"`, `"19"` |
//! | `chat_id` | номер чата во внутренней таблице портала | `7`, `21`, `3` |
//!
//! У ЛИЧНОГО диалога `dialog_id` — это `user_id` собеседника (`"19"`), а
//! `chat_id` — совсем другое число (`21`). Методы сообщений адресуются
//! `DIALOG_ID`, методы чата (`im.chat.*`, `im.disk.folder.get`) — `CHAT_ID`.
//!
//! # Ловушка: `id` в `im.recent.get` ПОЛИМОРФЕН
//!
//! У группового чата это строка `"chat7"`, у личного — **число** `19`.
//! Разбирается [`crate::api::schema::StrOrNum`], а наружу уходит ВСЕГДА строка:
//! единая форма — единственный способ не разветвлять фронт (там же лежат ключи
//! `Map` и `data-`атрибуты DOM).
//!
//! # Ловушка: `role` приходит в РАЗНОМ РЕГИСТРЕ
//!
//! `im.recent.get` отдаёт `"OWNER"`, `im.dialog.get` — `"owner"`. Это один и
//! тот же портал в одном и том же ответе на соседние вызовы. Нормализация в
//! нижний регистр ([`normalize_role`]) делается ЗДЕСЬ, один раз: сравнение
//! `role == "owner"` во фронте не должно зависеть от того, каким методом
//! приехала строка.
//!
//! # Поля-константы (`archived`, `folder_id`)
//!
//! У Битрикс24 нет ни архива, ни папок. Поля всё равно есть и всегда равны
//! `false`/`null` — ради ИДЕНТИЧНОСТИ ФОРМЫ с VK и Telegram: каждое удалённое
//! поле превратилось бы в `if` во фронте, а построчный дифф трёх панелей —
//! главный инструмент их сопровождения.
//!
//! # Аватары (Р6)
//!
//! В DTO уезжает ПУТЬ ПРОКСИ `/api/bitrix/media/avatar/:user_id`, а не
//! CDN-адрес портала — хотя тот и публичен. Инвариант «внешних URL в DOM нет»
//! держится единообразно с соседними панелями: одно исключение означало бы, что
//! проверять придётся каждое поле каждого DTO.

use serde::{Deserialize, Serialize};

use crate::api::schema::RecentItemRaw;
use crate::bbcode;

/// Потолок длины сообщения портала, символов (Р12).
///
/// Платформа принимает более длинный текст, отвечает успехом и МОЛЧА режет его
/// (живой замер: 25 000 символов превратились в 20 006 с хвостом `(...)`).
/// Поэтому длину проверяет плагин и отвечает `400 invalid_input` (Ф6):
/// «успешно отправленный» обрубок хуже честного отказа — отправитель уверен,
/// что собеседник прочитал всё.
pub const MAX_MESSAGE_CHARS: usize = 20_000;

/// Длина превью последнего сообщения, символов.
///
/// Считается в code point'ах, а не в байтах: обрезка кириллицы по байтам дала
/// бы битый UTF-8.
pub const PREVIEW_CHARS: usize = 160;

/// Префикс прокси аватаров (Р6). Полный путь — `AVATAR_PATH_PREFIX` + id.
pub const AVATAR_PATH_PREFIX: &str = "/api/bitrix/media/avatar/";

/// Вид диалога — закрытый словарь, дословно совпадающий с фронтом.
pub mod kind {
    /// Личная переписка с сотрудником.
    pub const USER: &str = "user";
    /// Обычный групповой чат.
    pub const CHAT: &str = "chat";
    /// Чат задачи (`entity_type == "TASKS_TASK"`) — директория `~/tasks`.
    pub const TASK: &str = "task";
    /// Чат рабочей группы/проекта (`dialog_id` вида `sg12`).
    pub const GROUP: &str = "sg";
    /// Открытая линия (обращение клиента).
    pub const OPENLINE: &str = "openline";
    /// Чат с копилотом.
    pub const COPILOT: &str = "copilot";
    /// Коллаба.
    pub const COLLAB: &str = "collab";
}

/// Сущность портала, означающая чат задачи.
///
/// Значение критично и проверяется тестом: `"TASKS"` (без `_TASK`) —
/// НЕ то же самое, на нём `im.chat.get` молча отдаёт `{"result":null}` (Ф10).
pub const ENTITY_TASK: &str = "TASKS_TASK";

/// Источник отображаемых имён и присутствия для проекции.
///
/// Существует ради ЧИСТОТЫ проекции: [`dialog_from_raw`] не знает ни про кэш,
/// ни про клиент портала и потому тестируется на живых фикстурах без сети.
/// Реализация — [`crate::peers::PeerCache`].
pub trait PeerNames {
    /// Отображаемое имя сотрудника.
    fn display_name(&self, id: i64) -> Option<String>;
    /// Цвет заглушки аватара.
    fn color(&self, id: i64) -> Option<String>;
    /// Присутствие: «онлайн сейчас» и «когда был активен».
    fn presence(&self, id: i64) -> Option<(bool, Option<String>)>;
    /// Это бот.
    fn is_bot(&self, id: i64) -> bool;
    /// Есть ли у сотрудника настоящее фото (Ф8, Р6).
    ///
    /// Проекция знает только «да/нет»: САМ CDN-адрес наружу не уходит никогда,
    /// в DTO уезжает путь прокси [`AVATAR_PATH_PREFIX`]`<id>`. Адрес остаётся в
    /// [`crate::peers::PeerCache`] и виден одному потребителю — роуту аватаров.
    fn has_avatar(&self, id: i64) -> bool;
}

/// Пустой источник имён — для тестов проекции и для веток, где кэш недоступен.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoNames;

impl PeerNames for NoNames {
    fn display_name(&self, _id: i64) -> Option<String> {
        None
    }
    fn color(&self, _id: i64) -> Option<String> {
        None
    }
    fn presence(&self, _id: i64) -> Option<(bool, Option<String>)> {
        None
    }
    fn is_bot(&self, _id: i64) -> bool {
        false
    }
    fn has_avatar(&self, _id: i64) -> bool {
        false
    }
}

/// Превью последнего сообщения в строке списка.
///
/// Отдельный тип, а не `MessageDto` (Ф5), потому что портал отдаёт здесь
/// ДРУГОЙ объект: у `recent[].message` нет ни `params`, ни вложений, ни
/// клавиатур — только то, что нужно строке списка. Выдать его за полное
/// сообщение значило бы соврать фронту о доступных полях.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastMessageDto {
    /// Идентификатор сообщения.
    pub id: i64,
    /// Текст ПОСЛЕ разбора BB-кодов: в списке нужна строка, а не разметка.
    pub text: String,
    /// Автор (строкой; `"0"` — система).
    pub author_id: String,
    /// Имя автора, если оно известно кэшу.
    pub author_name: Option<String>,
    /// Дата в формате ATOM, как её отдал портал.
    pub date: Option<String>,
    /// Своё сообщение (`author_id == self_id`).
    pub out: bool,
    /// Системное сообщение (`author_id == 0`): приглашения, смена названия,
    /// уведомления о задачах.
    pub system: bool,
    /// К сообщению приложен файл.
    pub has_file: bool,
}

/// Строка списка диалогов.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogDto {
    /// `DIALOG_ID` СТРОКОЙ: `"chat7"`, `"sg12"`, `"19"`.
    pub id: String,
    /// `chat_id` — ДРУГОЕ поле (см. шапку модуля). У части личных диалогов
    /// портал его не присылает вовсе.
    pub chat_id: Option<i64>,
    /// Вид (см. [`kind`]) — по нему работают директории колонки (Р2).
    pub kind: String,
    /// Заголовок строки.
    pub title: String,
    /// Путь ПРОКСИ аватара (Р6), никогда не CDN-адрес портала.
    pub avatar_url: Option<String>,
    /// Цвет заглушки — им красится кружок с инициалами, когда аватара нет.
    pub color: Option<String>,
    /// Непрочитанных в этом диалоге.
    pub unread_count: i64,
    /// Последнее сообщение (превью).
    pub last_message: Option<LastMessageDto>,
    /// Дата последнего изменения строки (ATOM) — по ней сортируется список.
    pub date_update: Option<String>,
    /// Диалог закреплён пользователем (`im.recent.pin`).
    pub pinned: bool,
    /// Уведомления заглушены (свой id в `chat.mute_list`).
    pub muted: bool,
    /// Можно ли писать: `text_field_enabled` И `restrictions.send`.
    pub can_write: bool,
    /// Число участников чата, если портал его прислал.
    pub members: Option<i64>,
    /// Сущность чата: `TASKS_TASK`, `CRM`, `GENERAL`, …
    pub entity_type: Option<String>,
    /// Идентификатор сущности строкой (у сделки — `DEAL|1`).
    pub entity_id: Option<String>,
    /// Идентификатор задачи — заполнен ТОЛЬКО когда `entity_type ==
    /// "TASKS_TASK"`. Отдельным полем, чтобы фронт не разбирал `entity_id`
    /// сам.
    pub task_id: Option<String>,
    /// Относительный путь сущности на портале (`/company/personal/user/1/
    /// tasks/task/view/1/`). **Не ссылка**: хоста портала в DTO нет (он часть
    /// секрета), поэтому фронт использует поле как признак «сущность есть», а
    /// не как `href`.
    pub entity_link_url: Option<String>,
    /// Роль в чате, НОРМАЛИЗОВАННАЯ в нижний регистр: `owner` / `manager` /
    /// `member` / `guest`.
    pub role: Option<String>,
    /// Собеседник онлайн (только у личных диалогов).
    pub online: bool,
    /// Когда собеседник был активен (ATOM).
    pub was_online: Option<String>,
    /// ВСЕГДА `false`: архива у портала нет. Поле сохранено ради формы.
    pub archived: bool,
    /// ВСЕГДА `None`: папок у портала нет, их место занимают четыре локальные
    /// директории колонки (Р2).
    pub folder_id: Option<i64>,
}

/// Счётчики непрочитанного портала в проводной форме.
///
/// Поля дословно совпадают с WS-кадром `counters`
/// ([`crate::ws::protocol::B24ServerMsg::Counters`]): фронт применяет обе
/// формы одной функцией, и расхождение имён означало бы, что бейдж после
/// перезагрузки страницы показывает одно число, а после WS-события — другое.
///
/// # Почему счётчиков несколько
///
/// Портал считает НЕЗАВИСИМО личные диалоги (`DIALOG`), групповые чаты
/// (`CHAT`), чаты задач (`TASKS_TASK`) и уведомления (`NOTIFY`). Общий
/// `total` — не их сумма, а собственное число портала (`TYPE.ALL`), и
/// подменять его сложением нельзя: у портала свои правила про заглушённые
/// чаты.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountersDto {
    /// Общий счётчик для бейджа вкладки (`TYPE.ALL`, включает уведомления).
    pub total: i64,
    /// Непрочитанное в групповых чатах.
    pub chat: i64,
    /// Непрочитанное в личных диалогах.
    pub dialog: i64,
    /// Непрочитанное в чатах задач.
    pub tasks: i64,
    /// Уведомления портала. Отдельной директории под них в v1 нет (Р13), но в
    /// общий счётчик они входят.
    pub notify: i64,
}

impl CountersDto {
    /// Проекция сырого `im.counters.get`.
    pub fn from_raw(raw: &crate::api::schema::CountersRaw) -> Self {
        Self {
            total: raw.types.all.max(0),
            chat: raw.types.chat.max(0),
            dialog: raw.types.dialog.max(0),
            tasks: raw.types.tasks_task.max(0),
            notify: raw.types.notify.max(0),
        }
    }

    /// Непрочитанное в ДИАЛОГАХ — без уведомлений портала.
    ///
    /// Именно это число показывает директория `~/unread`: уведомления
    /// («вам поставили задачу», «сегодня день рождения у…») живут в самом
    /// портале, открыть их в панели нельзя, и включать их в счётчик
    /// директории значило бы обещать строки, которых там не появится.
    pub fn dialogs_unread(&self) -> i64 {
        self.chat + self.dialog + self.tasks
    }
}

/// Unix-время «сейчас».
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Обрезает строку по code point'ам и вешает многоточие.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// Каноническая строка `DIALOG_ID` из полиморфного `id` портала.
pub fn dialog_id_string(raw: &crate::api::schema::StrOrNum) -> String {
    raw.as_string().trim().to_string()
}

/// Проверяет `DIALOG_ID`, пришедший ИЗВНЕ (из URL), и возвращает канонический.
///
/// Принимаются ровно три формы платформы:
///
/// - `19` — личный диалог (это `user_id`);
/// - `chat7` — групповой чат;
/// - `sg12` — чат рабочей группы.
///
/// Всё остальное — `None`, и роут обязан ответить `404 unknown_dialog`, а не
/// `400`: для фронта «такого диалога нет» и «идентификатор не разбирается» —
/// одна и та же ветка.
///
/// Ведущий `+` отвергается намеренно: `+19` и `19` — один диалог портала, но
/// два разных ключа кэша во фронте.
pub fn parse_dialog_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.len() > 32 {
        return None;
    }
    let digits = |d: &str| -> bool { !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) };

    if let Some(rest) = s.strip_prefix("chat") {
        return digits(rest).then(|| format!("chat{rest}"));
    }
    if let Some(rest) = s.strip_prefix("sg") {
        return digits(rest).then(|| format!("sg{rest}"));
    }
    digits(s).then(|| s.to_string())
}

/// Вид диалога по строке списка.
///
/// Порядок проверок значим: сначала ЗАДАЧА (её признак — `entity_type`, а не
/// тип чата), потом префикс `sg`, потом тип чата портала, и только затем тип
/// самой строки. Иначе чат задачи, у которого `type: "tasksTask"`, попал бы в
/// обычные чаты — то есть исчез бы из директории `~/tasks`.
pub fn dialog_kind(id: &str, item_type: Option<&str>, chat_kind: Option<&str>, entity: Option<&str>) -> &'static str {
    if entity.is_some_and(|e| e.eq_ignore_ascii_case(ENTITY_TASK)) {
        return kind::TASK;
    }
    if id.starts_with("sg") {
        return kind::GROUP;
    }
    match chat_kind.unwrap_or_default().to_ascii_lowercase().as_str() {
        "taskstask" | "tasks" => kind::TASK,
        "copilot" => kind::COPILOT,
        "collab" => kind::COLLAB,
        "lines" | "open" | "openline" | "open_line" | "imopenlines" => kind::OPENLINE,
        "sonetgroup" | "sonet_group" | "group" => kind::GROUP,
        _ => {
            if item_type.is_some_and(|t| t.eq_ignore_ascii_case("user")) {
                kind::USER
            } else {
                kind::CHAT
            }
        }
    }
}

/// Приводит роль к нижнему регистру.
///
/// `im.recent.get` отдаёт `"OWNER"`, `im.dialog.get` — `"owner"`. Пустая строка
/// превращается в `None`.
pub fn normalize_role(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
}

/// Есть ли у пользователя настоящий аватар.
///
/// Правило одно на весь плагин и живёт в [`crate::peers::real_avatar_url`]:
/// проекция DTO решает «рисовать ли `<img>`», а прокси (Ф8) — «есть ли что
/// скачивать», и разъехаться эти два ответа не имеют права.
fn has_real_avatar(raw: &crate::api::schema::StrOrBool) -> bool {
    crate::peers::real_avatar_url(raw).is_some()
}

/// Путь прокси аватара (Р6) либо `None`.
///
/// Только для ЛИЧНЫХ диалогов: прокси адресуется `user_id`, а аватара чата
/// в таблице путей нет вовсе.
fn avatar_path(user_id: i64, has_avatar: bool) -> Option<String> {
    (has_avatar && user_id > 0).then(|| format!("{AVATAR_PATH_PREFIX}{user_id}"))
}

/// Проекция строки `im.recent.get` / `im.recent.list.items` в [`DialogDto`].
///
/// Чистая: ни сети, ни кэша, ни времени. Имена и присутствие приходят через
/// [`PeerNames`], «своё сообщение» определяется по `self_id`.
///
/// Возвращает `None`, если у строки нет идентификатора: рисовать её всё равно
/// нечем, а пустой `id` во фронте стал бы ключом-дублем для всех таких строк.
pub fn dialog_from_raw(item: &RecentItemRaw, self_id: i64, peers: &dyn PeerNames) -> Option<DialogDto> {
    let id = dialog_id_string(&item.id);
    if id.is_empty() {
        return None;
    }

    let chat = &item.chat;
    let entity_type = chat
        .entity_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let dialog_kind = dialog_kind(
        &id,
        item.kind.as_deref(),
        chat.kind.as_deref(),
        entity_type.as_deref(),
    );

    // У личного диалога `dialog_id` — это `user_id` собеседника; у группового
    // блок `user` заполнен заглушкой `{"id":0}`.
    let peer_id = item.user.id.as_i64().filter(|v| *v > 0).unwrap_or_else(|| {
        if dialog_kind == kind::USER {
            id.parse().unwrap_or_default()
        } else {
            0
        }
    });

    let title = item
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| chat.name.clone().filter(|s| !s.trim().is_empty()))
        .or_else(|| (peer_id > 0).then(|| peers.display_name(peer_id)).flatten())
        .unwrap_or_else(|| id.clone());

    let (online, was_online) = if dialog_kind == kind::USER && peer_id > 0 {
        peers.presence(peer_id).unwrap_or((false, None))
    } else {
        (false, None)
    };

    let entity_id = {
        let v = item.chat.entity_id.as_string();
        (!v.trim().is_empty()).then_some(v)
    };
    let task_id = entity_type
        .as_deref()
        .filter(|e| e.eq_ignore_ascii_case(ENTITY_TASK))
        .and(entity_id.clone());

    let has_avatar = has_real_avatar(&item.avatar.url) || has_real_avatar(&item.user.avatar);

    Some(DialogDto {
        chat_id: item.chat_id.or(chat.id).filter(|v| *v > 0),
        kind: dialog_kind.to_string(),
        title,
        avatar_url: avatar_path(peer_id, has_avatar),
        color: item
            .avatar
            .color
            .clone()
            .or_else(|| chat.color.clone())
            .or_else(|| (peer_id > 0).then(|| peers.color(peer_id)).flatten()),
        unread_count: item.counter.max(0),
        last_message: last_message_from_raw(item, self_id, peers),
        date_update: item
            .date_update
            .clone()
            .or_else(|| item.date_last_activity.clone()),
        pinned: item.pinned,
        // Заглушён — если СВОЙ id стоит в списке глушения. Непустой
        // `mute_list` сам по себе означает лишь то, что кто-то другой не хочет
        // получать уведомления.
        muted: chat.mute_list.contains(&self_id),
        can_write: chat.text_field_enabled && chat.restrictions.send,
        members: chat.user_counter.filter(|v| *v > 0),
        entity_type,
        entity_id,
        task_id,
        entity_link_url: chat
            .entity_link
            .url
            .clone()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty()),
        role: normalize_role(chat.role.as_deref()),
        online,
        was_online,
        archived: false,
        folder_id: None,
        id,
    })
}

/// Превью последнего сообщения строки списка.
fn last_message_from_raw(
    item: &RecentItemRaw,
    self_id: i64,
    peers: &dyn PeerNames,
) -> Option<LastMessageDto> {
    let raw = &item.message;
    let text = raw.text.as_deref().unwrap_or_default();
    // Файл приезжает `false`, когда его нет, и объектом, когда есть.
    let has_file = !matches!(raw.file, serde_json::Value::Null | serde_json::Value::Bool(false));
    if raw.id == 0 && text.is_empty() && !has_file {
        return None;
    }
    Some(LastMessageDto {
        id: raw.id,
        text: truncate_chars(&bbcode::to_plain(text), PREVIEW_CHARS),
        author_id: raw.author_id.to_string(),
        author_name: (raw.author_id > 0)
            .then(|| peers.display_name(raw.author_id))
            .flatten(),
        date: raw.date.clone(),
        out: raw.author_id != 0 && raw.author_id == self_id,
        // `author_id == 0` — системное сообщение портала (приглашение в чат,
        // смена названия, «задача просрочена»).
        system: raw.author_id == 0,
        has_file,
    })
}

// ---------------------------------------------------------------------------
// Сообщение ленты (Ф5).
// ---------------------------------------------------------------------------

/// Автор сообщения в проводной форме.
///
/// Не [`crate::login::B24UserDto`] (владелец вебхука) намеренно: у автора
/// сообщения есть цвет заглушки аватара и признак бота, но нет ни `admin`, ни
/// прав — это РАЗНЫЕ роли, и общая структура заставила бы одну из них врать.
///
/// Имя приходит из [`PeerNames`], а не из ответа портала: `users[]` в
/// `im.dialog.messages.get` живьём чаще всего ПУСТ.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderDto {
    /// Идентификатор СТРОКОЙ (единый инвариант id-полей плагина).
    pub id: String,
    /// Отображаемое имя, если оно известно кэшу.
    pub name: Option<String>,
    /// Цвет заглушки аватара.
    pub color: Option<String>,
    /// Это бот портала.
    pub bot: bool,
    /// Путь ПРОКСИ аватара (Р6) либо `None`, если фото у сотрудника нет.
    ///
    /// Ровно та же форма, что у [`DialogDto::avatar_url`]:
    /// `/api/bitrix/media/avatar/<user_id>`. CDN-адреса портала здесь не бывает
    /// никогда — инвариант «внешних URL в DOM нет» един для колонки и для
    /// ленты, а исключение сломало бы правило и усложнило бы аудит.
    pub avatar_url: Option<String>,
}

/// Сообщение ленты.
///
/// # Три поля, которых у портала нет, а в форме они есть
///
/// | поле | значение | почему |
/// |---|---|---|
/// | `reply_to` | ВСЕГДА `null` | `REPLY_ID` отправляется, но при ЧТЕНИИ портал его не отражает (Р10) |
/// | `fwd` | ВСЕГДА `null` | пересылки сообщений вебхуку недоступны (§6) |
/// | `media[].waveform` / `player_url` | всегда пусты | у портала их нет вовсе |
///
/// Поля сохранены ради ИДЕНТИЧНОСТИ ФОРМЫ с панелями VK и Telegram: строка
/// ленты у всех трёх рисуется построчно одинаковым кодом, и каждое удалённое
/// поле превратилось бы в `if` во фронте.
///
/// # Ловушка: `URL_PREVIEW` переписывает текст
///
/// `im.message.add` по умолчанию (`URL_PREVIEW: "Y"`) оборачивает голые ссылки
/// в `[URL]…[/URL]`, поэтому ОТПРАВЛЕННЫЙ текст и ПРОЧИТАННЫЙ обратно не
/// совпадают байт в байт. Сравнивать их (например, ради дедупа оптимистичной
/// строки в Ф6) можно только после [`crate::bbcode::to_plain`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDto {
    /// Идентификатор сообщения. У портала растёт ШАГОМ 2.
    pub id: i64,
    /// Диалог СТРОКОЙ (`"chat27"`, `"19"`) — так, как его спросил фронт.
    pub dialog_id: String,
    /// Своё сообщение (`author_id == self_id`). У системного всегда `false`.
    pub out: bool,
    /// Дата в формате ATOM, как её отдал портал.
    pub date: Option<String>,
    /// Автор. `None` у системных сообщений — у них автора нет.
    pub sender: Option<SenderDto>,
    /// Системное сообщение ленты: `author_id == 0` ЛИБО `params.SYSTEM == "Y"`.
    ///
    /// Рисуется отдельной строкой без бабла и аватара: «Игорь создал чат» —
    /// это событие, а не реплика.
    pub system: bool,
    /// Текст ПОСЛЕ разбора BB-кодов — для поиска, копирования и превью.
    pub text: String,
    /// Тот же текст СТРУКТУРНЫМИ СЕГМЕНТАМИ — единственная форма, из которой
    /// фронт собирает разметку узлами (`innerHTML` запрещён).
    pub segments: Vec<bbcode::Segment>,
    /// Приложенные файлы (`params.FILE_ID` × корневой `files[]`).
    pub media: Vec<crate::attach::MediaDto>,
    /// Карточки `params.ATTACH` — блоки MESSAGE/LINK/GRID.
    pub cards: Vec<crate::attach::CardDto>,
    /// Inline-клавиатура `params.KEYBOARD`.
    pub reply_markup: Option<crate::attach::ReplyMarkupDto>,
    /// Сообщение отредактировано (`params.IS_EDITED == "Y"`).
    ///
    /// Появляется с задержкой до `recent_sync_secs`: события правки портал не
    /// присылает (`ONIMV2MESSAGEUPDATE` документирован, но живьём не приходит).
    pub edited: bool,
    /// Кто поставил «лайк» — идентификаторы СТРОКАМИ. Единственная доступная
    /// вебхуку реакция (§6).
    pub likes: Vec<String>,
    /// Сообщение УДАЛЕНО (`params.IS_DELETED == "Y"`).
    ///
    /// Портал удаление не стирает: он заменяет текст заглушкой и оставляет
    /// строку в истории. Без этого признака удалённая строка, убранная кадром
    /// `message_delete`, возвращалась бы при следующей загрузке ленты.
    pub deleted: bool,
    /// Код системного действия (`params.CODE[0]`): `CHAT_JOIN`, `CHAT_LEAVE`, …
    pub action: Option<String>,
    /// Непрочитано владельцем вебхука.
    pub unread: bool,
    /// ВСЕГДА `None` (Р10) — см. шапку структуры.
    pub reply_to: Option<i64>,
    /// ВСЕГДА `None` (§6) — см. шапку структуры.
    pub fwd: Option<serde_json::Value>,
}

/// Проекция сырого сообщения портала в [`MessageDto`].
///
/// Чистая: ни сети, ни кэша, ни времени. Файлы приходят индексом
/// ([`crate::attach::index_files`]), имена — через [`PeerNames`], «своё» — по
/// `self_id`.
///
/// `dialog_id` приходит АРГУМЕНТОМ, а не берётся из сообщения: в самом
/// сообщении лежит числовой `chat_id`, а у личного диалога `DIALOG_ID` — это
/// вообще `user_id` собеседника (разные числа, см. шапку модуля). Отдавать
/// фронту не тот идентификатор, которым он спрашивал, значит промахиваться
/// ключом `Map` на каждой странице истории.
pub fn message_from_raw(
    raw: &crate::api::schema::MessageRaw,
    dialog_id: &str,
    self_id: i64,
    files: &std::collections::HashMap<i64, &crate::api::schema::FileRaw>,
    peers: &dyn PeerNames,
) -> MessageDto {
    use crate::attach;

    let text = raw.text.as_deref().unwrap_or_default();
    // Система — это ЛИБО отсутствие автора, ЛИБО явный флаг: `im.message.add
    // SYSTEM=Y` сохраняет сообщение с `author_id == 0`, но обратное неверно —
    // часть системных строк портал помечает только флагом.
    let system = raw.author_id == 0 || attach::is_yes(attach::param(&raw.params, attach::P_SYSTEM));

    MessageDto {
        id: raw.id,
        dialog_id: dialog_id.to_string(),
        out: raw.author_id != 0 && raw.author_id == self_id,
        date: raw.date.clone(),
        sender: sender_from_id(raw.author_id, peers),
        system,
        text: bbcode::to_plain(text),
        segments: bbcode::to_segments(text),
        media: attach::media_from_params(&raw.params, files),
        cards: attach::cards_from_params(&raw.params),
        reply_markup: attach::keyboard_from_params(&raw.params),
        edited: attach::is_yes(attach::param(&raw.params, attach::P_IS_EDITED)),
        likes: attach::likes_from_params(&raw.params),
        deleted: attach::is_yes(attach::param(&raw.params, attach::P_IS_DELETED)),
        action: attach::action_from_params(&raw.params),
        unread: raw.unread,
        reply_to: None,
        fwd: None,
    }
}

/// Автор по идентификатору. `None` у системных сообщений (`author_id == 0`).
fn sender_from_id(author_id: i64, peers: &dyn PeerNames) -> Option<SenderDto> {
    (author_id > 0).then(|| SenderDto {
        id: author_id.to_string(),
        name: peers.display_name(author_id),
        color: peers.color(author_id),
        bot: peers.is_bot(author_id),
        avatar_url: avatar_path(author_id, peers.has_avatar(author_id)),
    })
}

/// Нормализует страницу истории по ВОЗРАСТАНИЮ идентификатора.
///
/// # Зачем это отдельная функция
///
/// Порядок ответа портала ЗАВИСИТ ОТ НАПРАВЛЕНИЯ запроса и это не случайность
/// одной ревизии, а поведение метода:
///
/// | запрос | порядок ответа |
/// |---|---|
/// | базовый (`LIMIT` без границ) | по УБЫВАНИЮ id (новые первыми) |
/// | `LAST_ID` (листаем вверх) | по УБЫВАНИЮ id |
/// | `FIRST_ID` (догружаем новое) | по ВОЗРАСТАНИЮ id |
///
/// Фронт про это знать не должен: лента рисуется сверху вниз от старого к
/// новому, и разный порядок означал бы две ветки вставки, вторая из которых
/// проверяется только при догрузке снизу — то есть почти никогда.
///
/// Дубликаты по `id` при этом схлопываются: страница с пересекающейся границей
/// (`before_id` ровно на стыке) иначе дала бы во фронте две одинаковые строки.
pub fn sort_messages_ascending(mut messages: Vec<MessageDto>) -> Vec<MessageDto> {
    messages.sort_by_key(|m| m.id);
    messages.dedup_by_key(|m| m.id);
    messages
}

/// Склеивает два источника списка в один, БЕЗ дублей и с сохранением порядка.
///
/// # Зачем это вообще нужно
///
/// `im.recent.get` отдаёт весь список, включая чаты задач, но не умеет
/// страниц. `im.recent.list` умеет страницы, но **чатов задач не отдаёт
/// вовсе** — проверено живьём. Полный список поэтому собирается из ОБОИХ:
/// основа — первый, дозагрузка хвоста — второй (Р3).
///
/// Приоритет у `primary`: если один и тот же диалог пришёл из обоих
/// источников, побеждает строка из `im.recent.get` — она свежее и содержит
/// полный блок `chat` (у `im.recent.list` он бывает урезан).
pub fn merge_dialogs(primary: Vec<DialogDto>, extra: Vec<DialogDto>) -> Vec<DialogDto> {
    let mut out = primary;
    let seen: std::collections::HashSet<String> = out.iter().map(|d| d.id.clone()).collect();
    let mut seen = seen;
    for d in extra {
        if seen.insert(d.id.clone()) {
            out.push(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{StrOrBool, StrOrNum, UserRaw};
    use crate::peers::PeerCache;

    /// Живой ответ `im.recent.get` тестового портала (сокращён до трёх строк:
    /// чат задачи, личный диалог, общий чат).
    const RECENT_GET: &str = include_str!("../tests/fixtures/recent_get.json");

    fn items() -> Vec<RecentItemRaw> {
        let v: serde_json::Value = serde_json::from_str(RECENT_GET).unwrap();
        serde_json::from_value(v["result"].clone()).unwrap()
    }

    fn find<'a>(items: &'a [RecentItemRaw], id: &str) -> &'a RecentItemRaw {
        items
            .iter()
            .find(|i| dialog_id_string(&i.id) == id)
            .unwrap_or_else(|| panic!("нет строки {id}"))
    }

    /// ПОЛИМОРФНЫЙ `id`: строка у групп, ЧИСЛО у личных — и то, и другое
    /// обязано стать одной строкой.
    #[test]
    fn the_polymorphic_id_normalises_to_a_string() {
        let items = items();
        let ids: Vec<String> = items.iter().map(|i| dialog_id_string(&i.id)).collect();
        assert!(ids.contains(&"chat7".to_string()), "{ids:?}");
        assert!(ids.contains(&"19".to_string()), "личный id пришёл числом");

        let personal = find(&items, "19");
        assert!(matches!(personal.id, StrOrNum::Num(19)), "фикстура изменилась");
        assert_eq!(personal.chat_id, Some(21), "chat_id — ДРУГОЕ число");
    }

    /// РОЛЬ НОРМАЛИЗУЕТСЯ: `im.recent.get` пишет «OWNER», `im.dialog.get` —
    /// «owner».
    #[test]
    fn the_role_is_normalised_to_lowercase() {
        let items = items();
        assert_eq!(find(&items, "chat7").chat.role.as_deref(), Some("OWNER"));

        let dto = dialog_from_raw(find(&items, "chat7"), 1, &NoNames).unwrap();
        assert_eq!(dto.role.as_deref(), Some("owner"));

        assert_eq!(normalize_role(Some("owner")).as_deref(), Some("owner"));
        assert_eq!(normalize_role(Some(" MANAGER ")).as_deref(), Some("manager"));
        assert_eq!(normalize_role(Some("")), None);
        assert_eq!(normalize_role(None), None);
    }

    /// Чат задачи опознаётся по `entity_type`, а не по типу чата, и несёт
    /// `task_id` отдельным полем.
    #[test]
    fn a_task_chat_carries_its_task_id_and_lands_in_the_task_kind() {
        let items = items();
        let dto = dialog_from_raw(find(&items, "chat7"), 1, &NoNames).unwrap();
        assert_eq!(dto.kind, kind::TASK);
        assert_eq!(dto.entity_type.as_deref(), Some("TASKS_TASK"));
        assert_eq!(dto.task_id.as_deref(), Some("1"));
        assert!(dto
            .entity_link_url
            .as_deref()
            .unwrap()
            .starts_with("/company/personal/user/1/tasks/task/view/1/"));

        // Обычный чат задачей не становится ни при каких условиях.
        let general = dialog_from_raw(find(&items, "chat3"), 1, &NoNames).unwrap();
        assert_eq!(general.kind, kind::CHAT);
        assert_eq!(general.task_id, None);
        assert_eq!(general.entity_type.as_deref(), Some("GENERAL"));
    }

    /// Полная проекция личного диалога на живой фикстуре.
    #[test]
    fn a_personal_dialog_projects_completely() {
        let items = items();
        // Имена приезжают БЕСПЛАТНО из блоков `user` того же ответа: кэш
        // наполняется без единого запроса (см. `peers.rs`).
        let peers = PeerCache::new();
        let users: Vec<crate::api::schema::UserRaw> = {
            let v: serde_json::Value = serde_json::from_str(RECENT_GET).unwrap();
            v["result"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|row| serde_json::from_value(row["user"].clone()).ok())
                .collect()
        };
        peers.absorb_users(&users);
        assert!(peers.len() >= 3, "поглотили {} записей", peers.len());

        let dto = dialog_from_raw(find(&items, "19"), 1, &peers).unwrap();
        assert_eq!(dto.id, "19");
        assert_eq!(dto.kind, kind::USER);
        assert_eq!(dto.title, "Алексей Пруцков");
        assert_eq!(dto.unread_count, 1);
        assert!(dto.can_write);
        assert!(!dto.muted && !dto.pinned && !dto.archived);
        assert_eq!(dto.folder_id, None);
        // У этого собеседника аватар пустой, а `avatar_hr` — заглушка
        // blank.gif: обе формы означают «аватара нет».
        assert_eq!(dto.avatar_url, None);
        assert_eq!(dto.color.as_deref(), Some("#58cc47"));

        let last = dto.last_message.unwrap();
        assert_eq!(last.id, 55);
        assert_eq!(last.text, "Я с вами в команде!");
        assert_eq!(last.author_id, "19");
        assert!(!last.out, "чужое сообщение");
        assert!(!last.system);
    }

    /// Аватар уезжает ПУТЁМ ПРОКСИ, а CDN-адрес портала — никуда (Р6).
    #[test]
    fn avatars_are_proxy_paths_and_never_cdn_urls() {
        let items = items();
        let dto = dialog_from_raw(find(&items, "17"), 1, &NoNames).unwrap();
        assert_eq!(dto.avatar_url.as_deref(), Some("/api/bitrix/media/avatar/17"));

        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("cdn-ru.bitrix24.ru"), "{json}");
        assert!(!json.contains("http"), "{json}");

        // Групповому чату прокси аватара не адресуется вовсе.
        let group = dialog_from_raw(find(&items, "chat3"), 1, &NoNames).unwrap();
        assert_eq!(group.avatar_url, None);

        assert_eq!(avatar_path(0, true), None);
        assert!(!has_real_avatar(&StrOrBool::Bool(false)));
        assert!(!has_real_avatar(&StrOrBool::Str(String::new())));
        assert!(!has_real_avatar(&StrOrBool::Str(
            "/bitrix/js/im/images/blank.gif".into()
        )));
        assert!(has_real_avatar(&StrOrBool::Str("https://cdn/x.jpg".into())));
    }

    /// Системное сообщение (`author_id == 0`) и «своё» (`author_id ==
    /// self_id`) — разные признаки, и оба нужны фронту.
    #[test]
    fn system_and_own_last_messages_are_flagged() {
        let items = items();
        let system = dialog_from_raw(find(&items, "chat3"), 1, &NoNames)
            .unwrap()
            .last_message
            .unwrap();
        assert!(system.system);
        assert!(!system.out, "система не может быть «своим» сообщением");
        assert_eq!(system.author_id, "0");

        let own = dialog_from_raw(find(&items, "chat7"), 1, &NoNames)
            .unwrap()
            .last_message
            .unwrap();
        assert!(own.out, "author_id 1 == self_id 1");
        // Тот же диалог глазами ДРУГОГО владельца вебхука.
        let alien = dialog_from_raw(find(&items, "chat7"), 15, &NoNames)
            .unwrap()
            .last_message
            .unwrap();
        assert!(!alien.out);
    }

    /// BB-коды в превью разбираются, а длинный текст обрезается по символам.
    #[test]
    fn the_preview_is_plain_text_and_bounded() {
        let mut item = RecentItemRaw {
            id: StrOrNum::Str("chat3".into()),
            ..RecentItemRaw::default()
        };
        item.message.id = 53;
        item.message.author_id = 0;
        item.message.text = Some(
            "[USER=1 REPLACE]Игорь Герасимов[/USER] добавил права администратора".to_string(),
        );
        let dto = dialog_from_raw(&item, 1, &NoNames).unwrap();
        let last = dto.last_message.unwrap();
        assert_eq!(last.text, "Игорь Герасимов добавил права администратора");

        item.message.text = Some("я".repeat(500));
        let last = dialog_from_raw(&item, 1, &NoNames)
            .unwrap()
            .last_message
            .unwrap();
        assert_eq!(last.text.chars().count(), PREVIEW_CHARS);
        assert!(last.text.ends_with('…'));

        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("", 3), "");
    }

    /// Глушение — это СВОЙ id в списке, а не непустой список.
    #[test]
    fn mute_is_about_our_own_id() {
        let mut item = RecentItemRaw {
            id: StrOrNum::Str("chat7".into()),
            ..RecentItemRaw::default()
        };
        item.chat.mute_list = vec![15, 19];
        assert!(!dialog_from_raw(&item, 1, &NoNames).unwrap().muted);
        assert!(dialog_from_raw(&item, 15, &NoNames).unwrap().muted);
    }

    /// Поле ввода гасится и `text_field_enabled`, и `restrictions.send` —
    /// достаточно любого из двух.
    #[test]
    fn can_write_needs_both_flags() {
        let mut item = RecentItemRaw {
            id: StrOrNum::Str("chat7".into()),
            ..RecentItemRaw::default()
        };
        item.chat.text_field_enabled = true;
        item.chat.restrictions.send = true;
        assert!(dialog_from_raw(&item, 1, &NoNames).unwrap().can_write);

        item.chat.restrictions.send = false;
        assert!(!dialog_from_raw(&item, 1, &NoNames).unwrap().can_write);

        item.chat.restrictions.send = true;
        item.chat.text_field_enabled = false;
        assert!(!dialog_from_raw(&item, 1, &NoNames).unwrap().can_write);
    }

    /// Имена приходят ТОЛЬКО через трейт — проекция сети не касается.
    #[test]
    fn names_come_from_the_trait_only() {
        let peers = PeerCache::new();
        let raw: UserRaw = serde_json::from_value(serde_json::json!({
            "id": 19, "name": "Алексей Пруцков", "status": "online",
            "last_activity_date": "2026-08-04T16:28:29+03:00", "color": "#58cc47"
        }))
        .unwrap();
        peers.absorb_users(&[raw]);

        let item = RecentItemRaw {
            id: StrOrNum::Num(19),
            kind: Some("user".into()),
            ..RecentItemRaw::default()
        };
        let dto = dialog_from_raw(&item, 1, &peers).unwrap();
        assert_eq!(dto.title, "Алексей Пруцков", "заголовок взят из кэша");
        assert!(dto.online);
        assert_eq!(dto.was_online.as_deref(), Some("2026-08-04T16:28:29+03:00"));

        // Без кэша та же строка проецируется, просто без имени.
        let dto = dialog_from_raw(&item, 1, &NoNames).unwrap();
        assert_eq!(dto.title, "19");
        assert!(!dto.online);
    }

    #[test]
    fn a_row_without_an_id_is_dropped() {
        let item = RecentItemRaw::default();
        assert!(dialog_from_raw(&item, 1, &NoNames).is_none());
    }

    /// Разбор `DIALOG_ID` из URL: три формы платформы и ничего больше.
    #[test]
    fn dialog_ids_from_the_url_accept_exactly_three_forms() {
        assert_eq!(parse_dialog_id("19").as_deref(), Some("19"));
        assert_eq!(parse_dialog_id(" chat7 ").as_deref(), Some("chat7"));
        assert_eq!(parse_dialog_id("sg12").as_deref(), Some("sg12"));

        for hostile in [
            "",
            " ",
            "+19",
            "-19",
            "19.0",
            "chat",
            "chat-7",
            "chat7a",
            "CHAT7",
            "../../etc/passwd",
            "chat7/../chat8",
            "19%00",
            "chat７",
            &"9".repeat(64),
        ] {
            assert!(parse_dialog_id(hostile).is_none(), "принят мусор {hostile:?}");
        }
    }

    /// Вид диалога: порядок проверок значим.
    #[test]
    fn dialog_kinds_cover_the_whole_dictionary() {
        assert_eq!(
            dialog_kind("chat7", Some("chat"), Some("tasksTask"), Some("TASKS_TASK")),
            kind::TASK
        );
        // Задача опознаётся по сущности даже если тип чата обычный.
        assert_eq!(dialog_kind("chat7", Some("chat"), Some("chat"), Some("TASKS_TASK")), kind::TASK);
        assert_eq!(dialog_kind("sg12", Some("chat"), Some("chat"), None), kind::GROUP);
        assert_eq!(dialog_kind("19", Some("user"), None, None), kind::USER);
        assert_eq!(dialog_kind("chat3", Some("chat"), Some("general"), Some("GENERAL")), kind::CHAT);
        assert_eq!(dialog_kind("chat11", Some("chat"), Some("crm"), Some("CRM")), kind::CHAT);
        assert_eq!(dialog_kind("chat5", Some("chat"), Some("lines"), None), kind::OPENLINE);
        assert_eq!(dialog_kind("chat6", Some("chat"), Some("copilot"), None), kind::COPILOT);
        assert_eq!(dialog_kind("chat8", Some("chat"), Some("collab"), None), kind::COLLAB);
        // Незнакомый тип чата — это обычный чат, а не ошибка.
        assert_eq!(dialog_kind("chat9", Some("chat"), Some("что-то-новое-в-2030"), None), kind::CHAT);
    }

    // =======================================================================
    // Сообщения ленты (Ф5).
    // =======================================================================

    /// Живая страница `im.dialog.messages.get` тестового портала (chat27).
    const MSGS_CHAT27: &str = include_str!("../tests/fixtures/msgs_chat27.json");

    /// Живая страница системных сообщений (chat3).
    const MSGS_CHAT3: &str = include_str!("../tests/fixtures/msgs_chat3.json");

    fn page(raw: &str) -> crate::api::schema::MessagesRaw {
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        serde_json::from_value(v["result"].clone()).expect("живая страница не разобралась")
    }

    /// Проецирует живую страницу целиком, как это делает роут.
    fn project(raw: &str, self_id: i64, peers: &dyn PeerNames) -> Vec<MessageDto> {
        let page = page(raw);
        let files = page.files.clone().into_values();
        let index = crate::attach::index_files(&files);
        sort_messages_ascending(
            page.messages
                .iter()
                .map(|m| message_from_raw(m, "chat27", self_id, &index, peers))
                .collect(),
        )
    }

    fn find_msg(list: &[MessageDto], id: i64) -> &MessageDto {
        list.iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("нет сообщения {id}"))
    }

    /// ПОЛНАЯ ПРОЕКЦИЯ живой страницы: текст, сегменты, вложения, карточки,
    /// клавиатура, правка, лайк, системность.
    #[test]
    fn a_live_history_page_projects_completely() {
        let peers = PeerCache::new();
        peers.absorb_users(&page(MSGS_CHAT27).users.into_values());
        let msgs = project(MSGS_CHAT27, 1, &peers);
        assert_eq!(msgs.len(), 18);

        // Текст с BB-кодами: plain для превью, сегменты для ленты.
        let plain = find_msg(&msgs, 77);
        assert_eq!(plain.text, "PROBE plain EDITED");
        assert!(plain.edited, "params.IS_EDITED == Y");
        assert!(plain.out, "author_id 1 == self_id 1");
        assert_eq!(plain.sender.as_ref().unwrap().id, "1");
        assert_eq!(
            plain.sender.as_ref().unwrap().name.as_deref(),
            Some("Игорь Герасимов"),
            "имя приехало из кэша, а не из ответа"
        );
        assert!(!plain.system);
        assert_eq!(plain.reply_to, None, "Р10: портал не отражает ответы");
        assert_eq!(plain.fwd, None);

        // Файл: params.FILE_ID × корневой files[].
        let file = find_msg(&msgs, 97);
        assert_eq!(file.media.len(), 1);
        assert_eq!(file.media[0].file_id, 95);
        assert_eq!(file.media[0].index, 0);
        assert_eq!(file.media[0].name.as_deref(), Some("forge_probe.txt"));

        // Карточка ATTACH и клавиатура KEYBOARD — разные поля разных
        // сообщений.
        let card = find_msg(&msgs, 79);
        assert_eq!(card.cards.len(), 1);
        assert_eq!(card.cards[0].blocks.len(), 3);
        assert!(card.reply_markup.is_none());

        let kb = find_msg(&msgs, 81);
        let markup = kb.reply_markup.as_ref().expect("клавиатура потерялась");
        assert_eq!(markup.rows[0][0].text, "Кнопка");
        assert!(kb.cards.is_empty());

        // Лайк — единственная доступная реакция, идентификаторы строками.
        assert_eq!(find_msg(&msgs, 85).likes, vec!["1".to_string()]);
        assert!(find_msg(&msgs, 81).likes.is_empty());
    }

    /// СИСТЕМНОЕ СООБЩЕНИЕ: `author_id == 0` ЛИБО `params.SYSTEM == "Y"`.
    #[test]
    fn system_messages_are_flagged_by_the_author_or_by_the_flag() {
        let msgs = project(MSGS_CHAT27, 1, &NoNames);

        // Автора нет — система, автора нет и «своим» быть не может.
        let joined = find_msg(&msgs, 71);
        assert!(joined.system);
        assert!(!joined.out);
        assert_eq!(joined.sender, None, "у системного сообщения нет автора");
        assert_eq!(joined.action.as_deref(), Some("CHAT_JOIN"));
        // BB-коды системной строки разобраны: пользователь видит имена, а не
        // разметку.
        assert_eq!(joined.text, "Игорь Герасимов пригласил в чат Максим Крылов");
        assert!(joined
            .segments
            .iter()
            .any(|s| s.kind == bbcode::kind::USER && s.value.as_deref() == Some("1")));

        // Флаг без нулевого автора — тоже система.
        let raw = crate::api::schema::MessageRaw {
            id: 500,
            author_id: 1,
            text: Some("напоминание".into()),
            params: serde_json::json!({"SYSTEM": "Y"}),
            ..Default::default()
        };
        let dto = message_from_raw(&raw, "chat27", 1, &std::collections::HashMap::new(), &NoNames);
        assert!(dto.system, "params.SYSTEM == Y — это тоже система");
        assert!(dto.out, "но автор у неё есть");

        // Вся страница chat3 — системные строки с BB-кодами.
        let system = project(MSGS_CHAT3, 1, &NoNames);
        assert_eq!(system.len(), 5);
        assert!(system.iter().all(|m| m.system && m.sender.is_none()));
        assert_eq!(
            system[0].text,
            "Игорь Герасимов добавил права администратора для Максим Крылов."
        );
    }

    /// НОРМАЛИЗАЦИЯ ПОРЯДКА: страница всегда уезжает по возрастанию id, каким
    /// бы порядком её ни отдал портал.
    #[test]
    fn the_page_is_always_sorted_ascending_without_duplicates() {
        // Живой базовый вызов приходит по УБЫВАНИЮ.
        let raw_ids: Vec<i64> = page(MSGS_CHAT27).messages.iter().map(|m| m.id).collect();
        assert_eq!(raw_ids[0], 113, "фикстура изменилась: портал отдаёт новые первыми");
        assert!(raw_ids.windows(2).all(|w| w[0] > w[1]));

        let ids: Vec<i64> = project(MSGS_CHAT27, 1, &NoNames).iter().map(|m| m.id).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
        assert_eq!(ids.first(), Some(&67));
        assert_eq!(ids.last(), Some(&113));

        // Пересекающиеся страницы не дают дублей во фронте.
        let m = |id: i64| MessageDto {
            id,
            ..MessageDto::default()
        };
        let merged = sort_messages_ascending(vec![m(5), m(1), m(5), m(3)]);
        assert_eq!(merged.iter().map(|m| m.id).collect::<Vec<_>>(), vec![1, 3, 5]);
        assert!(sort_messages_ascending(Vec::new()).is_empty());
    }

    /// «Своё сообщение» зависит от владельца вебхука, а не от порядка строк.
    #[test]
    fn out_depends_on_the_webhook_owner() {
        let mine = project(MSGS_CHAT27, 1, &NoNames);
        let alien = project(MSGS_CHAT27, 15, &NoNames);
        assert!(find_msg(&mine, 85).out);
        assert!(!find_msg(&alien, 85).out);
        // Системное не становится «своим» ни у кого.
        assert!(!find_msg(&mine, 71).out && !find_msg(&alien, 71).out);
    }

    /// СТРАЖ ФОРМЫ: в сериализованном DTO нет ни одного адреса портала.
    #[test]
    fn no_message_dto_carries_a_portal_url() {
        let json = serde_json::to_string(&project(MSGS_CHAT27, 1, &NoNames)).unwrap();
        for leak in ["urlDownload", "urlShow", "unifiedLink", "downloadUrl", "bitrix24.ru"] {
            assert!(!json.contains(leak), "{leak} утёк в ленту");
        }
        // …а «внешний» адрес из ТЕКСТА сообщения остаётся: это то, что написал
        // человек, и прятать его было бы искажением переписки. В DOM он
        // попадает текстовым узлом (`b24-seg-url`), а не `href`.
        assert!(json.contains("example.com"));
    }

    /// Пустое сообщение проецируется без паники: у портала бывает и такое
    /// (сообщение, состоящее из одного вложения).
    #[test]
    fn an_empty_message_projects_into_an_empty_dto() {
        let raw = crate::api::schema::MessageRaw::default();
        let dto = message_from_raw(&raw, "19", 1, &std::collections::HashMap::new(), &NoNames);
        assert_eq!(dto.dialog_id, "19", "dialog_id берётся из запроса, а не из chat_id");
        assert!(dto.system, "author_id 0 — это система");
        assert_eq!(dto.text, "");
        assert!(dto.segments.is_empty());
        assert!(dto.media.is_empty() && dto.cards.is_empty());
        assert!(dto.reply_markup.is_none());
        assert!(!dto.edited && !dto.unread);
        assert!(dto.likes.is_empty() && dto.action.is_none());
    }

    /// Склейка двух источников: дублей нет, порядок и приоритет сохранены.
    #[test]
    fn merging_two_sources_keeps_the_primary_row() {
        let d = |id: &str, title: &str| DialogDto {
            id: id.into(),
            title: title.into(),
            ..DialogDto::default()
        };
        let merged = merge_dialogs(
            vec![d("chat7", "задача"), d("19", "Алексей")],
            vec![d("19", "устаревшая копия"), d("chat3", "Общий чат")],
        );
        assert_eq!(
            merged.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            vec!["chat7", "19", "chat3"]
        );
        assert_eq!(merged[1].title, "Алексей", "победила строка im.recent.get");

        assert!(merge_dialogs(Vec::new(), Vec::new()).is_empty());
    }
}
