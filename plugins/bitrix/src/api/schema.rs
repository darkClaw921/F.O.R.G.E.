//! Сырые структуры ответов портала.
//!
//! # Три правила, действующие на весь модуль
//!
//! 1. **НИ ОДНА структура не выводит `Serialize`.** Сырьё портала не должно
//!    уметь уехать в HTTP-ответ браузеру даже случайно: в нём попадаются поля,
//!    которых пользователю видеть незачем (`downloadUrl` с кодом вебхука
//!    внутри — Ф8), а проводные DTO собираются отдельно в `dto.rs` (Ф4).
//!    Правило проверяется тестом [`tests::no_schema_struct_derives_serialize`],
//!    который читает исходник этого файла.
//! 2. **Все поля `#[serde(default)]`.** Портал добавляет и убирает поля между
//!    релизами и редакциями; отсутствие поля обязано давать пустое значение, а
//!    не падение разбора всего ответа.
//! 3. **`deny_unknown_fields` не используется нигде.** Незнакомые поля
//!    игнорируются — иначе каждый апдейт портала ломал бы плагин.
//!
//! # Ловушки формата, отражённые в типах (§5 плана)
//!
//! | ловушка | как выглядит | ответ модуля |
//! |---|---|---|
//! | PHP-массивы | `[]` когда пусто и `{…}` когда заполнено | [`PhpMap`] |
//! | полиморфный `id` | строка у групп, число у личных диалогов | [`StrOrNum`] |
//! | `image` | `false` ИЛИ объект | `#[serde(untagged)]` на месте (Ф4) |
//! | `birthday` | `""` ИЛИ `false` | [`StrOrBool`] |
//! | `total: -1` | у `im.recent.list` | знаковые типы |
//!
//! В Фазе 2 здесь живут только структуры, нужные транспорту и подключению
//! (`profile`, `scope`, `im.counters.get`). Диалоги, сообщения, файлы и задачи
//! добавляют Фазы 4–10 — по этим же трём правилам.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

/// Поле, которое PHP отдаёт МАССИВОМ, когда оно пусто, и ОБЪЕКТОМ, когда в нём
/// что-то есть.
///
/// Это не причуда конкретного метода, а свойство сериализации PHP: пустой
/// ассоциативный массив неотличим от пустого списка, и `json_encode` печатает
/// его как `[]`. Живьём так ведут себя `CHAT`, `CHAT_UNREAD`, `result_error` у
/// `batch`, `params` у сообщения и ещё десяток полей.
///
/// Строгий `HashMap` падал бы ровно в самом частом случае — когда данных нет.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PhpMap<T> {
    /// Заполненная форма.
    Map(HashMap<String, T>),
    /// Пустая форма (`[]`) либо список — данных нет.
    ///
    /// Список не отбрасывается молча: непустой список означает, что портал
    /// сменил форму поля, и это видно в отладке.
    List(Vec<T>),
}

impl<T> Default for PhpMap<T> {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl<T> PhpMap<T> {
    /// Приводит к обычной карте; пустая PHP-форма даёт пустую карту.
    pub fn into_map(self) -> HashMap<String, T> {
        match self {
            Self::Map(m) => m,
            Self::List(_) => HashMap::new(),
        }
    }

    /// Значения независимо от формы.
    ///
    /// Нужен там, где ключ карты — тот же идентификатор, что лежит ВНУТРИ
    /// значения, и потому не несёт информации: `users` у
    /// `im.dialog.messages.get` приезжает списком, а `im.user.list.get` тот же
    /// набор отдаёт картой `{"15":{…}}`. Разбирать их двумя ветками пришлось бы
    /// в каждом вызывающем.
    pub fn into_values(self) -> Vec<T> {
        match self {
            Self::Map(m) => m.into_values().collect(),
            Self::List(v) => v,
        }
    }

    /// Сколько записей в заполненной форме.
    pub fn len(&self) -> usize {
        match self {
            Self::Map(m) => m.len(),
            Self::List(v) => v.len(),
        }
    }

    /// Пусто ли поле.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Значение, которое портал отдаёт то строкой, то числом.
///
/// Живьём: `id` в `im.recent.get` (строка `"chat7"` у групп, число `19` у
/// личных), `entity_id`, `user_id` в разных методах. Нормализация — всегда в
/// строку: идентификаторы в DTO плагина строковые (единый инвариант с VK и
/// Telegram).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StrOrNum {
    /// Строковая форма.
    Str(String),
    /// Числовая форма.
    Num(i64),
    /// ЯВНЫЙ `null`.
    ///
    /// Отличается от отсутствия поля тем, что `#[serde(default)]` на него не
    /// действует: дефолт подставляется только когда ключа НЕТ. Портал же
    /// присылает `null` сплошь и рядом (`chatId` у задачи без чата, `groupId`,
    /// `subStatus`), и без этого варианта одно такое поле роняло бы разбор
    /// всего ответа — то есть карточку, а не поле.
    Null,
}

impl Default for StrOrNum {
    fn default() -> Self {
        Self::Str(String::new())
    }
}

impl StrOrNum {
    /// Каноническая строка; `null` — пустая.
    pub fn as_string(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Num(n) => n.to_string(),
            Self::Null => String::new(),
        }
    }

    /// Число, если значение им является (или строка разбирается в число).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Num(n) => Some(*n),
            Self::Str(s) => s.parse().ok(),
            Self::Null => None,
        }
    }
}

/// Поле, которое портал отдаёт то строкой, то `false`.
///
/// Живьём: `birthday` (`""` или `false`), `last_activity_date` (datetime или
/// `false`), `image` у файла. `false` семантически означает «значения нет».
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StrOrBool {
    /// Значение есть.
    Str(String),
    /// Значения нет (`false`).
    Bool(bool),
}

impl Default for StrOrBool {
    fn default() -> Self {
        Self::Bool(false)
    }
}

impl StrOrBool {
    /// Непустая строка либо `None`.
    pub fn as_opt(&self) -> Option<&str> {
        match self {
            Self::Str(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Разбор вложенного поля, у которого «значения нет» приходит как `null` или
/// `false`, — с падением на значение по умолчанию вместо ошибки.
///
/// # Зачем это существует
///
/// `#[serde(default)]` покрывает только ОТСУТСТВУЮЩИЙ ключ. Явный `null` на
/// месте структуры — это `invalid type: null`, то есть ошибка разбора ВСЕГО
/// документа, а не одного поля. В ответах портала это различие не смысловое:
/// JSON приходит из PHP, где «пусто» выражается то `[]`, то `null`, то
/// `false`, причём соседние ключи одного объекта могут расходиться. Пустой
/// массив `[]` serde принимает молча (seq-форма структуры), а `null` — нет, и
/// именно поэтому ловушка не видна глазами: в живом ответе обе формы стоят
/// рядом и выглядят одинаково безобидно.
///
/// # Чем это кончалось (найдено живой проверкой Ф9 на двух стендах)
///
/// ОДИН чат с `entity_link: null` в `im.recent.get` ронял разбор ВСЕГО списка.
/// Ошибка глохла в [`super::methods`] (`from_value(…).ok().unwrap_or_default()`),
/// а воркер синхронизации не пишет строку на пустом результате — панель
/// показывала ноль диалогов, и в логе не оставалось НИ ОДНОЙ строки. Один чат
/// ронял не себя, а список.
///
/// # Почему ошибки НЕ глотаются целиком
///
/// На значение по умолчанию падают только `null` и булево — две формы «здесь
/// ничего нет». Объект неожиданной формы по-прежнему разбирается обычным путём
/// и обычным путём краснеет: молчаливое проглатывание ЛЮБОЙ ошибки вернуло бы
/// ровно тот дефект, ради которого функция написана, — расхождение со схемой,
/// невидимое ни в панели, ни в логе.
pub fn nullable_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let v = Value::deserialize(de)?;
    if v.is_null() || v.is_boolean() {
        return Ok(T::default());
    }
    T::deserialize(v).map_err(serde::de::Error::custom)
}

/// Список идентификаторов, который портал отдаёт ЛИБО массивом, ЛИБО объектом.
///
/// # Ловушка, найденная на живом портале
///
/// PHP не различает список и словарь: `array()` уезжает в JSON как `[]`, а
/// `array(2816 => true)` — как `{"2816":true}`. Поэтому `chat.mute_list`
/// приходит пустым МАССИВОМ, пока чат никем не заглушён, и становится
/// ОБЪЕКТОМ `{"<user_id>":true}`, как только его заглушил хоть один участник.
/// Тип поля в ответе зависит от данных, а не от схемы метода.
///
/// Стоило это ровно того же, что и `entity_link: null` (см.
/// [`nullable_default`]): один заглушённый чат ронял разбор ВСЕГО списка
/// диалогов, `decode_part` глотал ошибку в `unwrap_or_default`, и панель
/// показывала ноль чатов при живом счётчике непрочитанного. На тестовом
/// портале дефект не воспроизводился никогда — там никто не пользовался
/// mute'ом, и поле всегда было `[]`.
///
/// # Что считается элементом
///
/// - `null`, `false`, `[]`, `{}` — пустой список;
/// - массив — его элементы (число или строка с числом, как везде у портала);
/// - объект — его КЛЮЧИ, но только те, у которых значение не `false`:
///   портал кладёт в такую карту пару `user_id => true`, и `false` в ней
///   означает «этот участник как раз НЕ в списке».
///
/// Всё остальное (строка, число на месте списка) по-прежнему ошибка разбора:
/// глотать любую неожиданную форму значило бы вернуть тот самый молчаливый
/// дефект, ради которого функция написана.
pub fn id_list<'de, D>(de: D) -> Result<Vec<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(de)?;
    if v.is_null() || v.is_boolean() {
        return Ok(Vec::new());
    }
    if let Some(map) = v.as_object() {
        return Ok(map
            .iter()
            .filter(|(_, val)| !matches!(val, Value::Bool(false)))
            .filter_map(|(key, _)| key.parse::<i64>().ok())
            .collect());
    }
    let items: Vec<StrOrNum> = Vec::deserialize(v).map_err(serde::de::Error::custom)?;
    Ok(items.iter().filter_map(StrOrNum::as_i64).collect())
}

/// Ответ метода `profile` — владелец вебхука.
///
/// Живой ответ портала:
///
/// ```json
/// {"ID":"1","ADMIN":true,"NAME":"Игорь","LAST_NAME":"Герасимов",
///  "TIME_ZONE":"Europe/Moscow","PERSONAL_PHOTO":"https://cdn-ru.bitrix24.ru/…"}
/// ```
///
/// `ID` приходит СТРОКОЙ, но `im.user.get` тот же идентификатор отдаёт числом
/// — отсюда [`StrOrNum`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileRaw {
    /// Идентификатор пользователя портала.
    #[serde(rename = "ID", default)]
    pub id: StrOrNum,
    /// Имя.
    #[serde(rename = "NAME", default)]
    pub name: String,
    /// Фамилия.
    #[serde(rename = "LAST_NAME", default)]
    pub last_name: String,
    /// Администратор ли портала.
    #[serde(rename = "ADMIN", default)]
    pub admin: bool,
    /// Часовой пояс пользователя.
    #[serde(rename = "TIME_ZONE", default)]
    pub time_zone: String,
    /// Аватар. Публичный CDN-URL, но в DOM он всё равно не попадёт (Р6).
    #[serde(rename = "PERSONAL_PHOTO", default)]
    pub personal_photo: StrOrBool,
}

/// Разбивка счётчиков по типам (`im.counters.get`, ключ `TYPE`).
///
/// Три независимых счётчика, а не один: личные диалоги (`DIALOG`), групповые
/// чаты (`CHAT`) и чаты задач (`TASKS_TASK`) считаются порталом отдельно, и
/// панель показывает их как разные директории (Р2).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CounterTypesRaw {
    /// Всего непрочитанного.
    #[serde(rename = "ALL", default)]
    pub all: i64,
    /// Уведомления портала (в бейдж включаются, отдельной директории нет — Р13).
    #[serde(rename = "NOTIFY", default)]
    pub notify: i64,
    /// Групповые чаты.
    #[serde(rename = "CHAT", default)]
    pub chat: i64,
    /// Личные диалоги.
    #[serde(rename = "DIALOG", default)]
    pub dialog: i64,
    /// Мессенджер целиком.
    #[serde(rename = "MESSENGER", default)]
    pub messenger: i64,
    /// Чаты задач.
    #[serde(rename = "TASKS_TASK", default)]
    pub tasks_task: i64,
    /// Открытые линии.
    #[serde(rename = "LINES", default)]
    pub lines: i64,
    /// Копилот.
    #[serde(rename = "COPILOT", default)]
    pub copilot: i64,
    /// Коллабы.
    #[serde(rename = "COLLAB", default)]
    pub collab: i64,
}

/// Ответ метода `im.counters.get`.
///
/// Самый дешёвый метод портала (`operating: 0`, доли секунды) и потому главный
/// чекпоинт «есть ли новое». Считает ТОЛЬКО непрочитанное: свои отправленные
/// сообщения и правки его не двигают — поэтому он не заменяет ни поток
/// событий, ни фолбэк-синхронизацию.
///
/// `DIALOG` и `CHAT` — те самые PHP-поля, которые пустыми приезжают как `[]`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CountersRaw {
    /// Разбивка по типам.
    #[serde(rename = "TYPE", default)]
    pub types: CounterTypesRaw,
    /// Непрочитанное по конкретным личным диалогам: `{"19": 1, "17": 2}`.
    #[serde(rename = "DIALOG", default)]
    pub dialog: PhpMap<i64>,
    /// Непрочитанное по групповым чатам.
    #[serde(rename = "CHAT", default)]
    pub chat: PhpMap<i64>,
    /// Чаты с непрочитанным флагом.
    #[serde(rename = "CHAT_UNREAD", default)]
    pub chat_unread: PhpMap<Value>,
    /// Чаты задач.
    #[serde(rename = "TASKS_TASK", default)]
    pub tasks_task: PhpMap<i64>,
}

/// Пользователь портала — общая форма для `im.user.get`, `im.user.list.get`,
/// `im.dialog.users.list` и блока `user` в `im.recent.get`.
///
/// # Три «шатающихся» поля
///
/// Портал отдаёт одни и те же поля разными типами в разных методах, и это не
/// баг конкретной ревизии, а следствие того, что значения приходят из PHP без
/// схемы:
///
/// | поле | формы | тип здесь |
/// |---|---|---|
/// | `birthday` | `""` в `im.user.get`, `false` в `im.search.user.list` | [`StrOrBool`] |
/// | `last_activity_date` | datetime либо `false` (у ботов) | [`StrOrBool`] |
/// | `work_position` | `null` либо строка | `Option<String>` |
///
/// `last_name` тоже приходит `null` — живьём так выглядит бот «Команда заботы
/// Битрикс24», у которого всё имя лежит в `first_name`.
///
/// # Ловушка: НОТАЦИЯ ЗАВИСИТ ОТ ИСТОЧНИКА
///
/// Методы `im.*` пишут snake_case (`first_name`, `last_activity_date`), а
/// полезная нагрузка событий `im.v2.Event.get` — camelCase (`firstName`,
/// `lastActivityDate`). Это ОДИН И ТОТ ЖЕ пользователь одного и того же
/// портала. Отсюда `alias` на каждом расходящемся поле: без них поглощение
/// имён из событий (Ф7) молча давало бы пользователей без имени, а строка
/// списка подписывалась бы числом.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserRaw {
    /// Идентификатор. Число почти везде, но `profile` отдаёт строкой.
    #[serde(default)]
    pub id: StrOrNum,
    /// Учётка активна.
    #[serde(default)]
    pub active: bool,
    /// Полное имя, как его склеил портал.
    #[serde(default)]
    pub name: Option<String>,
    /// Имя.
    #[serde(default, alias = "firstName")]
    pub first_name: Option<String>,
    /// Фамилия. `null` у ботов и у части внешних учёток.
    #[serde(default, alias = "lastName")]
    pub last_name: Option<String>,
    /// Должность: `null` либо строка.
    #[serde(default, alias = "workPosition")]
    pub work_position: Option<String>,
    /// Цвет аватара-заглушки (`#df532d`) — им же красится кружок с инициалами.
    #[serde(default)]
    pub color: Option<String>,
    /// Аватар: CDN-URL, пустая строка либо заглушка `blank.gif`. Наружу НЕ
    /// отдаётся (Р6) — в DTO уезжает адрес прокси.
    #[serde(default)]
    pub avatar: StrOrBool,
    /// Аватар в высоком разрешении. Та же судьба.
    #[serde(default, alias = "avatarHr")]
    pub avatar_hr: StrOrBool,
    /// Пол (`M`/`F`).
    #[serde(default)]
    pub gender: Option<String>,
    /// День рождения: `""` ИЛИ `false`.
    #[serde(default)]
    pub birthday: StrOrBool,
    /// Внешний пользователь (экстранет).
    #[serde(default)]
    pub extranet: bool,
    /// Учётка «Битрикс24.Сеть».
    #[serde(default)]
    pub network: bool,
    /// Это бот.
    #[serde(default)]
    pub bot: bool,
    /// Это коннектор открытых линий.
    #[serde(default)]
    pub connector: bool,
    /// Присутствие: `online` / `offline` / `dnd`.
    #[serde(default)]
    pub status: Option<String>,
    /// Пользователь простаивает: `false` либо datetime начала простоя.
    #[serde(default)]
    pub idle: StrOrBool,
    /// Когда был активен: datetime ИЛИ `false`.
    #[serde(default, alias = "lastActivityDate")]
    pub last_activity_date: StrOrBool,
    /// Тип записи: `user` / `bot` / `extranet`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Отделы (числа). Пустой список — законное значение.
    #[serde(default)]
    pub departments: Vec<i64>,
}

/// Ответ `im.user.list.get` — **карта**, а не массив.
///
/// Ключ — идентификатор пользователя СТРОКОЙ (`{"15":{…},"19":{…}}`), потому
/// что в PHP это ассоциативный массив. Пустой результат приезжает как `[]` —
/// отсюда [`PhpMap`], а не голый `HashMap`.
pub type UserMapRaw = PhpMap<UserRaw>;

/// Аватар строки списка диалогов (`recent[].avatar`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AvatarRaw {
    /// Адрес картинки либо пустая строка. В DTO не уезжает (Р6).
    #[serde(default)]
    pub url: StrOrBool,
    /// Цвет заглушки.
    #[serde(default)]
    pub color: Option<String>,
}

/// Ссылка на сущность, к которой привязан чат (`chat.entity_link`).
///
/// У чата задачи это `{"type":"TASKS","url":"/company/personal/user/1/tasks/
/// task/view/1/…","id":"1"}` — путь ОТНОСИТЕЛЬНЫЙ, хост портала в нём
/// отсутствует.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EntityLinkRaw {
    /// Вид сущности: `TASKS`, `DEAL`, `GENERAL`, …
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Относительный путь на портале.
    #[serde(default)]
    pub url: Option<String>,
    /// Идентификатор сущности (строкой; у сделки — `DEAL|1`).
    #[serde(default)]
    pub id: StrOrNum,
}

/// Ограничения чата (`chat.restrictions`).
///
/// Из всего набора плагину нужно ровно одно поле — `send`: по нему гасится
/// поле ввода. Остальные (`rename`, `call`, `leave`, …) осознанно не
/// перечислены: они управляют функциями, которых у панели нет.
#[derive(Debug, Clone, Deserialize)]
pub struct RestrictionsRaw {
    /// Можно ли писать в чат.
    #[serde(default = "yes")]
    pub send: bool,
}

impl Default for RestrictionsRaw {
    fn default() -> Self {
        Self { send: true }
    }
}

/// Значение по умолчанию для «можно» — отсутствие ограничения означает
/// разрешение, а не запрет.
fn yes() -> bool {
    true
}

/// Блок `chat` строки списка.
///
/// **У личных диалогов он урезан до трёх полей** (`text_field_enabled`,
/// `background_id`, `mute_list`), у групповых приходит целиком. Поэтому здесь
/// всё, кроме служебного, — `Option`/`default`: строгие поля роняли бы разбор
/// на каждой личной переписке.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatRaw {
    /// `chat_id`.
    #[serde(default)]
    pub id: Option<i64>,
    /// Название чата.
    #[serde(default)]
    pub name: Option<String>,
    /// Владелец (user_id).
    #[serde(default)]
    pub owner: Option<i64>,
    /// Цвет.
    #[serde(default)]
    pub color: Option<String>,
    /// Тип чата портала: `chat`, `general`, `tasksTask`, `crm`, `open`,
    /// `sonetGroup`, `copilot`, `collab`, …
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Сущность, к которой привязан чат: `TASKS_TASK`, `CRM`, `GENERAL`, …
    #[serde(default)]
    pub entity_type: Option<String>,
    /// Идентификатор сущности (для задачи — её `task_id`).
    #[serde(default)]
    pub entity_id: StrOrNum,
    /// Кто заглушил чат (список user_id). Свой id в списке ⇒ `muted`.
    ///
    /// Разбирается [`id_list`], а не `nullable_default`: пока чат никем не
    /// заглушён, портал шлёт `[]`, а после первого же mute — ОБЪЕКТ
    /// `{"<user_id>":true}`. Тип зависит от данных, и на `Vec<i64>` это
    /// роняло разбор всего списка диалогов.
    #[serde(default, deserialize_with = "id_list")]
    pub mute_list: Vec<i64>,
    /// Модераторы чата. Та же двойственность формы, что у [`Self::mute_list`].
    #[serde(default, deserialize_with = "id_list")]
    pub manager_list: Vec<i64>,
    /// Число участников.
    #[serde(default)]
    pub user_counter: Option<i64>,
    /// Ограничения (нужно `send`).
    #[serde(default, deserialize_with = "nullable_default")]
    pub restrictions: RestrictionsRaw,
    /// Роль пользователя в чате. **`im.recent.get` пишет `"OWNER"`, а
    /// `im.dialog.get` — `"owner"`**: нормализуется в проекции (`dto.rs`).
    #[serde(default)]
    pub role: Option<String>,
    /// Разрешено ли поле ввода.
    #[serde(default = "yes")]
    pub text_field_enabled: bool,
    /// Ссылка на сущность.
    #[serde(default, deserialize_with = "nullable_default")]
    pub entity_link: EntityLinkRaw,
    /// `C` — обычный чат, `X` — сущностный (задача), `P` — личный, `O` —
    /// общий.
    #[serde(default)]
    pub message_type: Option<String>,
}

/// Последнее сообщение строки списка (`recent[].message`).
///
/// Это НЕ полное сообщение ленты: у него нет ни `params`, ни вложений — только
/// то, что нужно для превью.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecentMessageRaw {
    /// Идентификатор сообщения.
    #[serde(default)]
    pub id: i64,
    /// Текст с BB-кодами (разбирает [`crate::bbcode`]).
    #[serde(default)]
    pub text: Option<String>,
    /// Есть ли файл. Живьём `false` либо объект — отсюда `Value`.
    #[serde(default)]
    pub file: Value,
    /// Автор; `0` — системное сообщение.
    #[serde(default)]
    pub author_id: i64,
    /// Дата в формате ATOM.
    #[serde(default)]
    pub date: Option<String>,
    /// `received` / `delivered` / `error`.
    #[serde(default)]
    pub status: Option<String>,
}

/// Строка списка диалогов — общий элемент `im.recent.get` и
/// `im.recent.list.items`.
///
/// # Ловушка: `id` полиморфен
///
/// У группового чата это строка (`"chat7"`), у личного — **число** (`19`).
/// Отсюда [`StrOrNum`] и нормализация в строку на выходе: единая строковая
/// форма — единственный способ не разветвлять фронт (см. `dto.rs`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecentItemRaw {
    /// `DIALOG_ID`: `"chat7"` / `"sg12"` / `19`.
    #[serde(default)]
    pub id: StrOrNum,
    /// Числовой `chat_id` — ДРУГОЕ поле, путать нельзя.
    #[serde(default)]
    pub chat_id: Option<i64>,
    /// `chat` (групповой) либо `user` (личный).
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Заголовок строки.
    #[serde(default)]
    pub title: Option<String>,
    /// Аватар и цвет строки.
    #[serde(default, deserialize_with = "nullable_default")]
    pub avatar: AvatarRaw,
    /// Последнее сообщение.
    #[serde(default, deserialize_with = "nullable_default")]
    pub message: RecentMessageRaw,
    /// Идентификатор последнего прочитанного сообщения. У личных бывает `0`.
    #[serde(default)]
    pub last_id: i64,
    /// Диалог закреплён.
    #[serde(default)]
    pub pinned: bool,
    /// Помечен непрочитанным вручную.
    #[serde(default)]
    pub unread: bool,
    /// Дата последнего изменения строки (ATOM).
    #[serde(default)]
    pub date_update: Option<String>,
    /// Дата последней активности (ATOM).
    #[serde(default)]
    pub date_last_activity: Option<String>,
    /// Счётчик непрочитанного по этому диалогу.
    #[serde(default)]
    pub counter: i64,
    /// Блок чата (у личных урезан).
    #[serde(default, deserialize_with = "nullable_default")]
    pub chat: ChatRaw,
    /// Блок собеседника. У группового чата приезжает `{"id":0}`.
    #[serde(default, deserialize_with = "nullable_default")]
    pub user: UserRaw,
}

/// Ответ `im.recent.list` (постраничный).
///
/// # Три ловушки разом
///
/// 1. Поля **camelCase** (`hasMore`), хотя все остальные методы `im.*` пишут
///    snake_case, — потому что метод новый и сделан командой `im.v2`.
/// 2. Корневой `total` живьём равен **-1** и не значит ничего: ориентир
///    пагинации — только `hasMore` (поэтому `total` здесь и не объявлен, он
///    остаётся в [`super::B24Response`]).
/// 3. `LIMIT` не гарантирует ровно N записей: фильтрация идёт ПОСЛЕ выборки,
///    и `LIMIT=5` живьём отдал 4 элемента с `hasMore:true`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecentListRaw {
    /// Страница строк.
    #[serde(default)]
    pub items: Vec<RecentItemRaw>,
    /// Есть ли ещё страницы — ЕДИНСТВЕННЫЙ достоверный признак.
    #[serde(rename = "hasMore", default)]
    pub has_more: bool,
    /// Дублирующий признак портала; читается только для диагностики.
    #[serde(rename = "hasMorePages", default)]
    pub has_more_pages: bool,
}

/// Размеры картинки у файла — ИЛИ `false`, если файл не картинка.
///
/// Это не «поле может отсутствовать», а именно два разных типа в одном поле:
/// `"image": false` у документа и `"image": {"height":61,"width":72}` у
/// картинки. Строгий `Option<ImageSize>` падал бы на КАЖДОМ документе.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageRaw {
    /// Размеры есть — файл является картинкой.
    Size {
        /// Высота, пиксели.
        #[serde(default)]
        height: i64,
        /// Ширина, пиксели.
        #[serde(default)]
        width: i64,
    },
    /// `false` — файл картинкой не является.
    No(bool),
}

impl Default for ImageRaw {
    fn default() -> Self {
        Self::No(false)
    }
}

impl ImageRaw {
    /// Размеры, если файл — картинка.
    pub fn size(&self) -> Option<(i64, i64)> {
        match self {
            Self::Size { height, width } if *height > 0 && *width > 0 => Some((*height, *width)),
            _ => None,
        }
    }
}

/// Превью-адреса файла (`file.mediaUrl`).
///
/// Живьём это `{"preview":{"250":""}}` у документа и заполненный адрес у
/// картинки. Ключ карты — ШИРИНА превью строкой; сам адрес наружу не уезжает
/// (он с того же CDN, что и `urlShow`, и без сессионной куки не работает),
/// читается только его НЕПУСТОТА — она и есть признак
/// [`crate::attach::MediaDto::has_thumb`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaUrlRaw {
    /// Ширина превью строкой → адрес (пустой, если превью нет).
    #[serde(default)]
    pub preview: PhpMap<String>,
}

impl MediaUrlRaw {
    /// Есть ли НЕПУСТОЕ превью хоть одной ширины.
    ///
    /// Именно непустое: портал присылает ключ `"250"` со значением `""` у
    /// каждого документа, и проверка «ключ есть» обещала бы фронту картинку,
    /// которой не существует.
    pub fn has_preview(&self) -> bool {
        match &self.preview {
            PhpMap::Map(m) => m.values().any(|v| !v.trim().is_empty()),
            PhpMap::List(v) => v.iter().any(|s| !s.trim().is_empty()),
        }
    }
}

/// Файл, приложенный к сообщению (корневой массив `files[]` ответа
/// `im.dialog.messages.get`).
///
/// Объявлен в Фазе 4 ради одной ловушки — [`ImageRaw`]; Фаза 5 дополнила его
/// полями карточки вложения, Фаза 8 добавит скачивание через прокси.
///
/// # Чего здесь НЕТ и не будет
///
/// Полей `urlDownload`, `urlShow` и `viewerAttrs.unifiedLink`. Они приходят от
/// портала, но не работают без сессионной куки (живая проверка: `urlDownload`
/// отдаёт HTML страницы логина, `unifiedLink` — 403), а `downloadUrl` из
/// `im.v2.File.download` содержит код вебхука и наружу не отдаётся никогда.
/// Не объявить их — самый надёжный способ не отдать (Ф8).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileRaw {
    /// Идентификатор файла на Диске.
    #[serde(default)]
    pub id: i64,
    /// Чат, к которому файл относится.
    #[serde(rename = "chatId", default)]
    pub chat_id: i64,
    /// Имя файла.
    #[serde(default)]
    pub name: Option<String>,
    /// Расширение без точки.
    #[serde(default)]
    pub extension: Option<String>,
    /// Размер в байтах.
    #[serde(default)]
    pub size: i64,
    /// `file` / `image` / `video` / `audio`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Размеры картинки ЛИБО `false`.
    #[serde(default)]
    pub image: ImageRaw,
    /// Состояние загрузки на стороне портала (`done`).
    #[serde(default)]
    pub status: Option<String>,
    /// Автор.
    #[serde(rename = "authorId", default)]
    pub author_id: i64,
    /// Превью (см. [`MediaUrlRaw`]).
    #[serde(rename = "mediaUrl", default)]
    pub media_url: MediaUrlRaw,
    /// Голосовое сообщение — у портала это ОТДЕЛЬНЫЙ флаг, а не `type`
    /// (`type` у него остаётся `audio`).
    #[serde(rename = "isVoiceNote", default)]
    pub is_voice_note: bool,
    /// Кружок-видеосообщение. Тот же случай: `type` остаётся `video`.
    #[serde(rename = "isVideoNote", default)]
    pub is_video_note: bool,
}

/// Ответ `im.v2.File.download` — **СОДЕРЖИТ КОД ВЕБХУКА** (Ф8).
///
/// ```json
/// {"downloadUrl":"https://<портал>/rest/1/<КОД>/download/?token=im%7C…"}
/// ```
///
/// # Почему у этой структуры нет и не будет `Serialize`
///
/// Секрет плагина — это ВЕСЬ URL вебхука целиком, и `downloadUrl` цитирует его
/// дословно. Отсутствие `Serialize` делает невозможным случайное попадание
/// адреса в JSON-ответ: `serde_json::to_value` по нему просто не скомпилируется.
/// Дисциплина «не забыть убрать поле» здесь заменена запретом на уровне типов —
/// вместе с тест-стражем `download_urls_never_leak_to_the_client` это два
/// независимых рубежа.
///
/// Значение живёт ровно столько, сколько идёт скачивание в
/// [`crate::routes::media`]: в кэш, в лог и в DTO оно не попадает никогда
/// (в `meta.json` вместо адреса ложится отпечаток,
/// [`crate::media_cache::source_tag`]).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileDownloadRaw {
    /// Адрес скачивания. Наружу не отдаётся НИ В КАКОМ виде.
    #[serde(rename = "downloadUrl", default)]
    pub download_url: String,
}

/// Ответ `im.v2.File.upload` — файл залит И сообщение отправлено (Ф9, Р7).
///
/// ```json
/// {"file":{ /* дескриптор файла, см. FileRaw */ },
///  "messageId":97, "chatId":27, "dialogId":"chat27"}
/// ```
///
/// # Почему `message_id` — это [`Value`], а не `i64`
///
/// Та же ловушка платформы, что у результата `im.message.add`: живьём приезжает
/// числом, но строковая форма встречается у части методов портала повсеместно,
/// а отличить одну от другой заранее нечем. Строгий `i64` уронил бы разбор
/// ответа об УСПЕШНОЙ отправке — пользователь увидел бы ошибку у файла, который
/// уже лежит в чате, и нажал бы «повторить», получив дубль. Разбор обеих форм —
/// [`super::methods::message_id_from_result`].
///
/// # Чего здесь нет
///
/// Дескриптор файла СОЗНАТЕЛЬНО не разбирается: строка ленты всё равно
/// перечитывается методом истории ([`crate::routes::dialogs`]), и второй путь
/// сборки [`crate::attach::MediaDto`] — из ответа заливки — разъехался бы с
/// первым на первом же изменении формата портала.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileUploadRaw {
    /// Идентификатор отправленного сообщения (число либо строка).
    #[serde(rename = "messageId", default)]
    pub message_id: Value,
}

/// Одно сообщение ленты (`im.dialog.messages.get.messages[]`).
///
/// # Ловушка: `params` — то `[]`, то объект
///
/// Пустых параметров у сообщения нет вовсе, и PHP печатает их как `[]`;
/// заполненные приезжают объектом. Отсюда [`Value`], а не структура: набор
/// ключей открытый (`KEYBOARD`, `ATTACH`, `FILE_ID`, `IS_EDITED`, `LIKE`,
/// `CODE`, `SYSTEM`, `NOTIFY`, `COMPONENT_ID`, `IS_DELETED`, `MENU`…), портал
/// добавляет свои между релизами, и объявить их структурой значило бы ронять
/// разбор всей страницы истории на каждом новом ключе.
///
/// Читает `params` ровно один модуль — [`crate::attach`], и только по
/// известным ключам.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessageRaw {
    /// Идентификатор сообщения. У портала растёт ШАГОМ 2 — арифметике
    /// `id + 1` доверять нельзя.
    #[serde(default)]
    pub id: i64,
    /// Чат, которому принадлежит сообщение (ЧИСЛОВОЙ `chat_id`, не
    /// `DIALOG_ID`).
    #[serde(default)]
    pub chat_id: i64,
    /// Автор; `0` — СИСТЕМНОЕ сообщение (создание чата, приглашение, смена
    /// названия, уведомление задачи).
    #[serde(default)]
    pub author_id: i64,
    /// Дата в формате ATOM.
    #[serde(default)]
    pub date: Option<String>,
    /// Текст с BB-кодами (разбирает [`crate::bbcode`]).
    #[serde(default)]
    pub text: Option<String>,
    /// Непрочитано ли сообщение владельцем вебхука.
    #[serde(default)]
    pub unread: bool,
    /// Параметры сообщения — см. шапку структуры.
    #[serde(default)]
    pub params: Value,
}

/// Ответ `im.dialog.messages.get`.
///
/// # Две ловушки разом
///
/// 1. **`users` часто ПУСТ.** Портал кладёт туда участников не всегда, и
///    полагаться на него нельзя: имена берутся из [`crate::peers::PeerCache`],
///    а этот массив лишь бесплатно его наполняет.
/// 2. **Файлы лежат В КОРНЕ, а не в сообщении.** Сообщение несёт только
///    `params.FILE_ID: [95]`, дескриптор файла — здесь. Связка делается по
///    идентификатору ([`crate::attach::index_files`]).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessagesRaw {
    /// Числовой `chat_id` диалога (у личных он ОТЛИЧАЕТСЯ от `DIALOG_ID`).
    #[serde(default)]
    pub chat_id: i64,
    /// Сообщения страницы. Порядок ЗАВИСИТ ОТ ЗАПРОСА: базовый вызов и
    /// `LAST_ID` отдают по убыванию id, `FIRST_ID` — по возрастанию.
    /// Нормализует его роут ([`crate::routes::dialogs`]).
    #[serde(default)]
    pub messages: Vec<MessageRaw>,
    /// Участники — списком у `im.dialog.messages.get`, картой у соседних
    /// методов, пустым PHP-массивом чаще всего.
    #[serde(default)]
    pub users: PhpMap<UserRaw>,
    /// Дескрипторы файлов, на которые ссылаются `params.FILE_ID`.
    #[serde(default)]
    pub files: PhpMap<FileRaw>,
}

// ===========================================================================
// Поток событий `im.v2.Event.get` (Ф7).
// ===========================================================================

/// Страница потока событий — ответ `im.v2.Event.get`.
///
/// # Ловушка: нотация здесь camelCase
///
/// Методы `im.*` пишут snake_case, а `im.v2.*` — camelCase (`nextOffset`,
/// `hasMore`, `eventId`). Это тот же самый портал и тот же самый вебхук;
/// перепутать нотацию значит получить `next_offset = 0` на каждом цикле, то
/// есть переотдачу всей очереди событий бесконечно.
///
/// # `nextOffset` — не «сколько прочитано», а «что подтверждать»
///
/// Живьём приезжает числом (`46`), но хранится и отправляется СТРОКОЙ:
/// документация портала не обещает числовой формы, а нам с этим значением
/// ничего не нужно делать, кроме как вернуть его обратно параметром `offset`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventsPageRaw {
    /// События пачки. Пустой массив — норма (за интервал ничего не случилось).
    #[serde(default)]
    pub events: Vec<EventRaw>,
    /// Значение, которое обязано уехать параметром `offset` следующего вызова.
    #[serde(rename = "nextOffset", default)]
    pub next_offset: StrOrNum,
    /// В очереди осталось ещё — следующий запрос делается через 2 с (Р4).
    #[serde(rename = "hasMore", default)]
    pub has_more: bool,
}

/// Одно событие потока.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventRaw {
    /// Идентификатор события в очереди портала.
    ///
    /// НЕ ключ дедупа: живьём одно и то же сообщение приходило под РАЗНЫМИ
    /// `eventId` (наблюдалось `message.id = 61` под `eventId` 1 и 3), поэтому
    /// дедуп строится по `message.id` (Р19).
    #[serde(rename = "eventId", default)]
    pub event_id: StrOrNum,
    /// Тип: `ONIMV2MESSAGEADD`, `ONIMV2MESSAGEDELETE`, `ONIMV2REACTIONCHANGE`,
    /// `ONIMV2JOINCHAT`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Время события (ATOM).
    #[serde(default)]
    pub date: Option<String>,
    /// Полезная нагрузка.
    #[serde(default)]
    pub data: EventDataRaw,
}

/// Полезная нагрузка события.
///
/// **Самодостаточна**: у `ONIMV2MESSAGEADD` внутри лежат и сообщение, и чат, и
/// автор целиком — обогащающих запросов поток событий не требует вовсе. Именно
/// это делает realtime дешёвым: один вызов на цикл вместо N.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventDataRaw {
    /// Сообщение целиком (`ONIMV2MESSAGEADD`, `ONIMV2REACTIONCHANGE`).
    ///
    /// В нём ПРОДУБЛИРОВАНЫ обе нотации (`chatId` и `chat_id`, `authorId` и
    /// `author_id`), поэтому здесь работает тот же [`MessageRaw`], что и на
    /// странице истории, — и строка, приехавшая realtime'ом, проецируется в
    /// DTO тем же кодом, что строка из `im.dialog.messages.get`.
    #[serde(default)]
    pub message: Option<MessageRaw>,
    /// Идентификатор удалённого сообщения (`ONIMV2MESSAGEDELETE`): самого
    /// сообщения в этом событии нет.
    #[serde(rename = "messageId", default)]
    pub message_id: StrOrNum,
    /// Чат события.
    #[serde(default)]
    pub chat: EventChatRaw,
    /// Диалог строкой — приходит у `ONIMV2JOINCHAT`, дублируя `chat.dialogId`.
    #[serde(rename = "dialogId", default)]
    pub dialog_id: Option<String>,
    /// Автор события. **`Value`, а не `UserRaw`**: у системного сообщения
    /// портал кладёт сюда пустой PHP-массив `[]`, и строгая структура роняла бы
    /// разбор всей пачки на первой же строке «Игорь создал чат».
    #[serde(default)]
    pub user: Value,
    /// Название реакции (`like`) — `ONIMV2REACTIONCHANGE`.
    #[serde(default)]
    pub reaction: Option<String>,
    /// `add` / `delete` — живьём ПУСТО, ориентироваться на него нельзя
    /// (состав лайков берётся из `message.params.LIKE`).
    #[serde(default)]
    pub action: Option<String>,
}

impl EventDataRaw {
    /// Автор события, если он там есть.
    ///
    /// `None` и когда поля нет, и когда оно `[]`, и когда идентификатор нулевой
    /// (системное сообщение). Пустой пользователь в кэше имён хуже отсутствия:
    /// он занял бы место под именем «0» и пережил бы настоящего.
    pub fn user(&self) -> Option<UserRaw> {
        let raw: UserRaw = serde_json::from_value(self.user.clone()).ok()?;
        (raw.id.as_i64().unwrap_or_default() > 0).then_some(raw)
    }
}

/// Блок `chat` события.
///
/// Отдельная структура, а не [`ChatRaw`]: у события ВСЕ поля в camelCase
/// (`dialogId`, `entityType`, `textFieldEnabled`), а у методов `im.*` — в
/// snake_case. Навесить сюда `alias` на каждое поле [`ChatRaw`] значило бы
/// смешать две нотации в одном типе и потерять то место, по которому видно,
/// откуда пришли данные.
///
/// Полей ровно столько, сколько нужно проекции события: адрес диалога, его вид
/// и связь с задачей. Всё остальное (права, ограничения, счётчик участников)
/// приезжает строкой списка, и дублировать это здесь незачем.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventChatRaw {
    /// Числовой `chat_id`.
    #[serde(default)]
    pub id: Option<i64>,
    /// `DIALOG_ID` строкой (`"chat7"`) — ровно то, чем адресуется фронт.
    #[serde(rename = "dialogId", default)]
    pub dialog_id: Option<String>,
    /// Название чата.
    #[serde(default)]
    pub name: Option<String>,
    /// Тип чата портала: `chat`, `tasksTask`, `sonetGroup`, …
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Сущность чата: `TASKS_TASK`, `CRM`, …
    #[serde(rename = "entityType", default)]
    pub entity_type: Option<String>,
    /// Идентификатор сущности (у задачи — её номер).
    #[serde(rename = "entityId", default)]
    pub entity_id: StrOrNum,
}

impl EventChatRaw {
    /// `DIALOG_ID` строкой.
    ///
    /// Приоритет у `dialogId`: у личной переписки он равен идентификатору
    /// СОБЕСЕДНИКА, а не `"chat" + id`, и собрать его из числового `chat_id`
    /// невозможно в принципе. Числовая сборка — запасной путь для событий, где
    /// портал прислал только `id`.
    pub fn dialog_id(&self) -> Option<String> {
        if let Some(d) = self.dialog_id.as_deref().map(str::trim) {
            if !d.is_empty() {
                return Some(d.to_string());
            }
        }
        self.id.filter(|v| *v > 0).map(|id| format!("chat{id}"))
    }
}

// ===========================================================================
// Ф10: задачи.
// ===========================================================================

/// Сотрудник в карточке задачи (`creator`, `responsible`).
///
/// Отдельная форма от [`UserRaw`]: задачи отдают короткий объект с готовой
/// подписью `name` и относительной ссылкой на профиль, тогда как `im.*` —
/// полный профиль с присутствием. Складывать их в один тип значило бы
/// объявить половину полей `Option` и потерять то, что у задач подпись УЖЕ
/// собрана порталом.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskUserRaw {
    /// Идентификатор (строкой у задач).
    #[serde(default)]
    pub id: StrOrNum,
    /// Готовая подпись «Имя Фамилия».
    #[serde(default)]
    pub name: Option<String>,
    /// Относительная ссылка на профиль (`/company/personal/user/1/`).
    #[serde(default)]
    pub link: Option<String>,
    /// Абсолютный CDN-адрес фото. В DTO не уезжает (Р6).
    #[serde(default)]
    pub icon: Option<String>,
    /// Должность; часто `null`.
    #[serde(rename = "workPosition", default)]
    pub work_position: Option<String>,
}

/// Карточка задачи (`tasks.task.get` → `result.task`, элемент
/// `tasks.task.list` → `result.tasks[]`).
///
/// # Три особенности, снятые живьём
///
/// 1. **`chatId` полиморфен**: `tasks.task.get` отдаёт ЧИСЛО (`7`), а
///    `tasks.task.list` — СТРОКУ (`"9"`). Отсюда [`StrOrNum`]: строгий `i64`
///    ронял бы разбор ровно на списке, то есть на директории `~/tasks`.
/// 2. **`action` приходит ТОЛЬКО у `tasks.task.get`.** В списке матрицы нет
///    вовсе — поэтому кнопки рисуются по карточке, а не по строке списка.
/// 3. **`subStatus` есть только в списке** (`"-1"` = «нет виртуального
///    состояния»), в карточке его нет.
///
/// Все значения — `#[serde(default)]`: набор полей у задач меняется от
/// редакции к редакции портала чаще, чем где-либо ещё.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskRaw {
    /// Идентификатор задачи (строкой).
    #[serde(default)]
    pub id: StrOrNum,
    /// Заголовок.
    #[serde(default)]
    pub title: Option<String>,
    /// Описание. В BB-кодах, если `descriptionInBbcode == "Y"`.
    #[serde(default)]
    pub description: Option<String>,
    /// `"Y"` ⇒ описание размечено BB-кодами, а не HTML.
    #[serde(rename = "descriptionInBbcode", default)]
    pub description_in_bbcode: Option<String>,
    /// Статус числовым кодом v1 (`"2"` … `"7"`).
    #[serde(default)]
    pub status: StrOrNum,
    /// Виртуальное состояние (просрочено/новое); `"-1"` ⇒ нет. Только в
    /// списке.
    #[serde(rename = "subStatus", default)]
    pub sub_status: StrOrNum,
    /// Приоритет: `0` низкий, `1` средний, `2` высокий.
    #[serde(default)]
    pub priority: StrOrNum,
    /// Постановщик.
    #[serde(rename = "createdBy", default)]
    pub created_by: StrOrNum,
    /// Ответственный.
    #[serde(rename = "responsibleId", default)]
    pub responsible_id: StrOrNum,
    /// Соисполнители.
    #[serde(default)]
    pub accomplices: Vec<StrOrNum>,
    /// Наблюдатели.
    #[serde(default)]
    pub auditors: Vec<StrOrNum>,
    /// Крайний срок (ATOM с таймзоной портала).
    #[serde(default)]
    pub deadline: Option<String>,
    /// Когда поставлена.
    #[serde(rename = "createdDate", default)]
    pub created_date: Option<String>,
    /// Когда закрыта.
    #[serde(rename = "closedDate", default)]
    pub closed_date: Option<String>,
    /// Последняя активность (в том числе комментарий в чате задачи).
    #[serde(rename = "activityDate", default)]
    pub activity_date: Option<String>,
    /// Рабочая группа (`"0"` ⇒ задача вне группы).
    #[serde(rename = "groupId", default)]
    pub group_id: StrOrNum,
    /// Объект группы: `[]` когда её нет и `{"id":…,"name":…}` когда есть —
    /// PHP-ловушка §5, поэтому `Value`, а не структура.
    #[serde(default)]
    pub group: Value,
    /// Постановщик объектом.
    #[serde(default)]
    pub creator: Option<TaskUserRaw>,
    /// Ответственный объектом.
    #[serde(default)]
    pub responsible: Option<TaskUserRaw>,
    /// Непрочитанных комментариев ПО ЭТОЙ задаче.
    #[serde(rename = "newCommentsCount", default)]
    pub new_comments_count: i64,
    /// Идентификатор чата задачи. **Ноль либо отсутствие = «чата нет»** (Р20).
    #[serde(rename = "chatId", default)]
    pub chat_id: StrOrNum,
    /// `"Y"` ⇒ задачу ещё не открывали.
    #[serde(rename = "notViewed", default)]
    pub not_viewed: Option<String>,
    /// `"Y"` ⇒ уведомления по задаче заглушены.
    #[serde(rename = "isMuted", default)]
    pub is_muted: Option<String>,
    /// `"Y"` ⇒ задача в избранном.
    #[serde(default)]
    pub favorite: Option<String>,
    /// **Матрица прав** — готовый набор булевых флагов доступных действий.
    ///
    /// Значения разбираются как [`Value`], а не `bool`, намеренно: одно
    /// неожиданное значение (строка `"Y"`, число) уронило бы разбор ВСЕЙ
    /// карточки, а стоит оно ровно одной невидимой кнопки.
    #[serde(default)]
    pub action: PhpMap<Value>,
}

/// Ответ `tasks.task.get`: карточка лежит под ключом `task`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskGetRaw {
    /// Сама задача.
    #[serde(default)]
    pub task: TaskRaw,
}

/// Ответ `tasks.task.list`: `result.tasks[]`.
///
/// **`total` лежит РЯДОМ с `result`**, а не внутри него, поэтому здесь его
/// нет: он приезжает полем конверта ([`super::error::B24Response::total`]).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskListRaw {
    /// Страница задач.
    #[serde(default)]
    pub tasks: Vec<TaskRaw>,
}

/// Одна ячейка счётчиков задач: `{"counter":2,"code":6291456}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskCounterCellRaw {
    /// Само число.
    #[serde(default)]
    pub counter: i64,
}

/// Ответ `tasks.task.counters.get {userId}`.
///
/// Живьём: `{"expired":{"counter":2,…},"new_comments":{"counter":0,…},
/// "mentioned":{"counter":0,…}}`. Это ДРУГИЕ счётчики, чем у
/// `im.counters.get`: там непрочитанные СООБЩЕНИЯ чатов задач, здесь —
/// состояние самих задач. Складывать их нельзя.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskCountersRaw {
    /// Просроченных задач.
    #[serde(default)]
    pub expired: TaskCounterCellRaw,
    /// Задач с новыми комментариями.
    #[serde(default)]
    pub new_comments: TaskCounterCellRaw,
    /// Упоминаний.
    #[serde(default)]
    pub mentioned: TaskCounterCellRaw,
}

/// Ответ `im.chat.get {ENTITY_TYPE, ENTITY_ID}` — ровно `{"ID": 7}`.
///
/// # ГЛАВНАЯ ЛОВУШКА ЗАДАЧ
///
/// С `ENTITY_TYPE = "TASKS"` метод отвечает `{"result": null}` при HTTP 200 —
/// не ошибкой, а пустотой. Единственное верное значение —
/// [`crate::dto::ENTITY_TASK`] (`"TASKS_TASK"`). Поэтому обёртка разбирает
/// ответ как `Option<ChatIdRaw>`: `null` обязан стать «чата нет», а не «чат с
/// идентификатором 0».
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatIdRaw {
    /// Идентификатор чата.
    #[serde(rename = "ID", default)]
    pub id: StrOrNum,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mute_list` живьём меняет ТИП в зависимости от данных: пустой массив,
    /// пока чат никем не заглушён, и объект `{"<user_id>":true}` после первого
    /// же mute'а.
    ///
    /// Найдено на живом портале: один заглушённый чат ронял разбор ВСЕГО
    /// списка диалогов, ошибка глохла в `decode_part(…).unwrap_or_default()`,
    /// и панель показывала ноль чатов при счётчике непрочитанного в сотни.
    /// На тестовом портале не воспроизводилось никогда — там mute'ом никто не
    /// пользовался, и поле всегда было `[]`.
    #[test]
    fn mute_list_survives_both_shapes() {
        let as_array: ChatRaw =
            serde_json::from_value(serde_json::json!({"mute_list": [15, 19]})).unwrap();
        assert_eq!(as_array.mute_list, vec![15, 19]);

        let as_map: ChatRaw =
            serde_json::from_value(serde_json::json!({"mute_list": {"2816": true}})).unwrap();
        assert_eq!(as_map.mute_list, vec![2816]);

        let empty: ChatRaw = serde_json::from_value(serde_json::json!({"mute_list": []})).unwrap();
        assert!(empty.mute_list.is_empty());

        // `false` в карте означает «этот участник как раз НЕ в списке».
        let mixed: ChatRaw =
            serde_json::from_value(serde_json::json!({"mute_list": {"1": true, "2": false}}))
                .unwrap();
        assert_eq!(mixed.mute_list, vec![1]);

        // Обе формы «здесь ничего нет» — как и у `nullable_default`.
        for none in [serde_json::json!(null), serde_json::json!(false)] {
            let row: ChatRaw = serde_json::from_value(serde_json::json!({"mute_list": none}))
                .expect("null и false — это пустой список, а не ошибка");
            assert!(row.mute_list.is_empty());
        }
    }

    /// Строка списка с заглушённым чатом обязана разбираться ЦЕЛИКОМ — и не
    /// в одиночку, а внутри `Vec`, потому что ронял разбор именно массив:
    /// одна плохая строка забирала с собой все остальные.
    #[test]
    fn a_muted_chat_does_not_take_the_whole_page_down() {
        let page = serde_json::json!([
            {"id": "chat1", "chat": {"mute_list": []}},
            {"id": "chat90610", "chat": {"mute_list": {"2816": true}}},
            {"id": 19, "chat": {"mute_list": []}},
        ]);
        let rows: Vec<RecentItemRaw> =
            serde_json::from_value(page).expect("заглушённый чат не роняет страницу");
        assert_eq!(rows.len(), 3, "выжили все три строки, а не одна");
        assert_eq!(rows[1].chat.mute_list, vec![2816]);
    }

    /// ПРАВИЛО МОДУЛЯ: сырьё портала не сериализуемо.
    ///
    /// Проверка построчная по собственному исходнику: отрицательного трейт-бонда
    /// («не реализует `Serialize`») в языке нет, а `derive` — единственный
    /// способ его получить в этом файле.
    #[test]
    fn no_schema_struct_derives_serialize() {
        let src = include_str!("schema.rs");
        let offenders: Vec<&str> = src
            .lines()
            .filter(|l| l.trim_start().starts_with("#[derive"))
            .filter(|l| l.contains("Serialize"))
            .collect();
        assert!(
            offenders.is_empty(),
            "сырьё портала стало сериализуемым:\n{}",
            offenders.join("\n")
        );
    }

    /// Пустой PHP-массив и заполненный объект — одно и то же поле.
    #[test]
    fn php_map_survives_both_forms() {
        let empty: PhpMap<i64> = serde_json::from_str("[]").unwrap();
        assert!(empty.is_empty());
        assert!(empty.into_map().is_empty());

        let filled: PhpMap<i64> = serde_json::from_str(r#"{"19":1,"17":2}"#).unwrap();
        assert_eq!(filled.len(), 2);
        assert_eq!(filled.into_map().get("19"), Some(&1));

        assert!(PhpMap::<i64>::default().is_empty());
    }

    #[test]
    fn polymorphic_scalars_normalise_to_strings() {
        let s: StrOrNum = serde_json::from_str(r#""chat7""#).unwrap();
        assert_eq!(s.as_string(), "chat7");
        assert_eq!(s.as_i64(), None);

        let n: StrOrNum = serde_json::from_str("19").unwrap();
        assert_eq!(n.as_string(), "19");
        assert_eq!(n.as_i64(), Some(19));

        // Строковое число разбирается — `profile.ID` приходит именно так.
        let sn: StrOrNum = serde_json::from_str(r#""1""#).unwrap();
        assert_eq!(sn.as_i64(), Some(1));

        // ЯВНЫЙ `null` — не ошибка разбора. `#[serde(default)]` его не
        // покрывает (дефолт работает только на ОТСУТСТВУЮЩЕМ ключе), а портал
        // шлёт `null` сплошь и рядом: `chatId` у задачи без чата, `groupId`,
        // `subStatus`. Без этого варианта одно поле роняло бы всю карточку.
        let z: StrOrNum = serde_json::from_str("null").unwrap();
        assert_eq!(z.as_string(), "");
        assert_eq!(z.as_i64(), None);

        #[derive(Deserialize)]
        struct Holder {
            #[serde(default)]
            v: StrOrNum,
        }
        let h: Holder = serde_json::from_str(r#"{"v": null}"#).unwrap();
        assert_eq!(h.v.as_i64(), None);
        let h: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(h.v.as_string(), "");

        let b: StrOrBool = serde_json::from_str("false").unwrap();
        assert_eq!(b.as_opt(), None);
        let b: StrOrBool = serde_json::from_str(r#""1990-01-01""#).unwrap();
        assert_eq!(b.as_opt(), Some("1990-01-01"));
        let b: StrOrBool = serde_json::from_str(r#""""#).unwrap();
        assert_eq!(b.as_opt(), None, "пустая строка — это тоже «значения нет»");
    }

    /// Живой ответ `profile` тестового портала.
    #[test]
    fn profile_parses_the_live_response() {
        let raw: ProfileRaw = serde_json::from_str(
            r#"{"ID":"1","ADMIN":true,"NAME":"Игорь","LAST_NAME":"Герасимов",
                "TIME_ZONE":"Europe/Moscow","PERSONAL_PHOTO":"https://cdn-ru.bitrix24.ru/b1/x.jpg",
                "SOMETHING_NEW_IN_2030":{"nested":true}}"#,
        )
        .unwrap();
        assert_eq!(raw.id.as_string(), "1");
        assert_eq!(raw.name, "Игорь");
        assert!(raw.admin);
        assert!(raw.personal_photo.as_opt().is_some());

        // Пустой ответ не должен ронять разбор: все поля с дефолтами.
        let empty: ProfileRaw = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.id.as_string(), "");
        assert!(!empty.admin);
    }

    /// Живой ответ `im.counters.get`: `CHAT` пустой приезжает массивом.
    #[test]
    fn counters_parse_the_live_response_with_empty_php_arrays() {
        let raw: CountersRaw = serde_json::from_str(
            r#"{"TYPE":{"ALL":9,"NOTIFY":5,"CHAT":0,"DIALOG":4,"MESSENGER":4,
                        "TASKS_TASK":0,"LINES":0,"COPILOT":0,"COLLAB":0},
                "DIALOG":{"19":1,"17":2,"3":1},"CHAT":[],"CHAT_UNREAD":[],"TASKS_TASK":[]}"#,
        )
        .unwrap();
        assert_eq!(raw.types.all, 9);
        assert_eq!(raw.types.notify, 5);
        assert_eq!(raw.types.tasks_task, 0);
        assert_eq!(raw.dialog.len(), 3);
        assert_eq!(raw.dialog.into_map().get("17"), Some(&2));
        assert!(raw.chat.is_empty());
        assert!(raw.chat_unread.is_empty());
    }
}
