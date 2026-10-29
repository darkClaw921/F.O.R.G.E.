//! Типизированные обёртки методов портала.
//!
//! # Единый стиль обёртки
//!
//! Каждая обёртка — это ровно три вещи:
//!
//! 1. **имя метода строкой** — константа `M_*` рядом с обёрткой, чтобы опечатка
//!    в имени была одна на весь крейт и находилась поиском;
//! 2. **типизированные параметры** — аргументы функции, а не свободный JSON;
//! 3. **типизированный ответ** — структура из [`super::schema`].
//!
//! Так же добавляются методы следующих фаз: список диалогов и сообщения (Ф4,
//! Ф5), отправка (Ф6), поток событий (Ф7), файлы (Ф8, Ф9), задачи (Ф10).
//! Отступление от стиля («позову-ка я `call_value` прямо из роута») ломает
//! два инварианта сразу: имя метода перестаёт быть единственным, а ответ
//! перестаёт проверяться типом.
//!
//! # Что здесь есть
//!
//! | обёртка | метод | фаза | зачем |
//! |---|---|---|---|
//! | [`B24Client::profile`] | `profile` | Ф2 | «кто владелец вебхука» — первый шаг автомата логина |
//! | [`B24Client::scope`] | `scope` | Ф2 | «хватает ли прав» — второй шаг, порядок неизменен |
//! | [`B24Client::counters`] | `im.counters.get` | Ф2 | самый дешёвый чекпоинт «есть ли новое» |
//! | [`B24Client::server_time`] | `server.time` | Ф2 | безобидный метод для проб транспорта |
//! | [`B24Client::recent_get`] | `im.recent.get` | Ф4 | ОСНОВА списка: единственный источник чатов задач |
//! | [`B24Client::recent_list`] | `im.recent.list` | Ф4 | дозагрузка хвоста списка; чатов задач НЕ отдаёт |
//! | [`B24Client::user_get`] | `im.user.get` | Ф4 | один пользователь |
//! | [`B24Client::user_list_get`] | `im.user.list.get` | Ф4 | пачка пользователей; ответ — КАРТА, не массив |
//! | [`B24Client::dialog_get`] | `im.dialog.get` | Ф4 | карточка диалога (`role` строчными!) |
//! | [`B24Client::dialog_users_list`] | `im.dialog.users.list` | Ф4 | участники диалога полными объектами |
//! | [`B24Client::cold_start`] | `batch` | Ф4 | список + счётчики ОДНИМ запросом (Р3) |
//! | [`B24Client::dialog_messages`] | `im.dialog.messages.get` | Ф5 | страница истории (два порядка!) |
//! | [`B24Client::message_add`] | `im.message.add` | Ф6 | отправка; НЕ ретраится никогда (Р9) |
//! | [`B24Client::message_update`] | `im.message.update` | Ф6 | правка; пустой текст УДАЛЯЕТ сообщение |
//! | [`B24Client::message_delete`] | `im.message.delete` | Ф6 | удаление |
//! | [`B24Client::message_like`] | `im.message.like` | Ф6 | «лайк» — единственная реакция портала |
//! | [`B24Client::dialog_read`] | `im.dialog.read` | Ф6 | отметка о прочтении |
//! | [`B24Client::dialog_unread`] | `im.dialog.unread` | Ф6 | ручное «непрочитано»; `MESSAGE_ID` ОБЯЗАТЕЛЕН |
//! | [`B24Client::dialog_writing`] | `im.dialog.writing` | Ф6 | «печатает…» (только исходящее) |
//! | [`B24Client::recent_pin`] | `im.recent.pin` | Ф6 | закрепить/открепить диалог (Р14) |
//! | [`B24Client::recent_hide`] | `im.recent.hide` | Ф6 | скрыть диалог из списка (Р14) |
//!
//! # Три ловушки списка диалогов, зашитые в обёртки
//!
//! 1. **`im.recent.list` не отдаёт чаты задач**, а `im.recent.get` отдаёт.
//!    Поэтому основа списка — второй, а первый только дозагружает хвост.
//! 2. **`total: -1`** у `im.recent.list` не значит ничего; единственный
//!    достоверный признак продолжения — `hasMore`.
//! 3. **`LIMIT` не гарантирует ровно N записей**: портал фильтрует ПОСЛЕ
//!    выборки. Пагинацию строить по «пришло меньше запрошенного ИЛИ пусто»,
//!    а не арифметикой.
//!
//! # Порядок `profile` → `scope` неизменен
//!
//! Сначала выясняется, что вебхук вообще принят (`INVALID_CREDENTIALS` иначе),
//! и только потом — что у него есть нужные права (`insufficient_scope`).
//! Обратный порядок дал бы пользователю «не хватает прав» на чужом или
//! опечатанном вебхуке, то есть отправил бы чинить не то.

use std::collections::HashMap;

use serde_json::json;

use super::batch::BatchCall;
use super::error::B24Error;
use super::schema::{CountersRaw, ProfileRaw, RecentItemRaw, RecentListRaw, UserMapRaw, UserRaw};
use super::B24Client;

/// Метод «кто я» — владелец вебхука.
pub const M_PROFILE: &str = "profile";

/// Метод «какие права выданы вебхуку».
pub const M_SCOPE: &str = "scope";

/// Метод «сколько непрочитанного».
pub const M_COUNTERS: &str = "im.counters.get";

/// Метод «время портала».
pub const M_SERVER_TIME: &str = "server.time";

/// Метод пакетного вызова (см. [`super::batch`]).
pub const M_BATCH: &str = "batch";

/// Метод «весь список диалогов» — ОСНОВА списка (Р3).
///
/// Единственный источник, отдающий чаты задач (`type:"tasksTask"`): в
/// `im.recent.list` их нет вовсе.
pub const M_RECENT_GET: &str = "im.recent.get";

/// Метод «страница списка диалогов» — дозагрузка при длинном списке (Р3).
pub const M_RECENT_LIST: &str = "im.recent.list";

/// Метод «один пользователь».
pub const M_USER_GET: &str = "im.user.get";

/// Метод «пачка пользователей» — возвращает КАРТУ, а не массив.
pub const M_USER_LIST_GET: &str = "im.user.list.get";

/// Метод «карточка диалога».
pub const M_DIALOG_GET: &str = "im.dialog.get";

/// Метод «участники диалога».
pub const M_DIALOG_USERS_LIST: &str = "im.dialog.users.list";

/// Метод «страница истории сообщений».
pub const M_DIALOG_MESSAGES_GET: &str = "im.dialog.messages.get";

/// Потолок `LIMIT` у `im.dialog.messages.get` — ПЛАТФОРМЕННЫЙ.
///
/// Больше пятидесяти портал не отдаёт независимо от запрошенного (живьём
/// `LIMIT=100` вернул весь чат из тринадцати сообщений, но документация и
/// поведение на длинных чатах фиксируют потолок 50). Просить больше —
/// значит строить пагинацию на числе, которого не будет.
pub const MESSAGES_MAX_LIMIT: i64 = 50;

/// Метод «отправить сообщение» (Ф6).
///
/// Единственный метод отправки текста. Возвращает ЧИСЛО — идентификатор
/// созданного сообщения. Повторам не подлежит ни при какой ошибке (Р9,
/// [`super::NO_RETRY_METHODS`]): дедуп-идентификатора у него нет, и повтор
/// после обрыва создаёт у собеседника второе сообщение.
pub const M_MESSAGE_ADD: &str = "im.message.add";

/// Метод «изменить сообщение» (Ф6).
///
/// ЛОВУШКА: пустой `MESSAGE` не «оставляет как было», а УДАЛЯЕТ сообщение.
/// Поэтому пустой текст отсекается на нашей стороне (`400 invalid_input`), а не
/// доезжает до портала — удаление обязано быть явным действием пользователя.
pub const M_MESSAGE_UPDATE: &str = "im.message.update";

/// Метод «удалить сообщение» (Ф6).
pub const M_MESSAGE_DELETE: &str = "im.message.delete";

/// Метод «лайк» — ЕДИНСТВЕННАЯ доступная вебхуку реакция (§6 плана).
///
/// Эмодзи-реакций у платформы для вебхука нет вовсе: `im.message.reaction.*`
/// не существует, а `imbot.v2.Chat.Message.Reaction.*` требует scope `imbot` и
/// зарегистрированного бота.
pub const M_MESSAGE_LIKE: &str = "im.message.like";

/// Метод «выполнить команду бота в контексте сообщения» (forge-zw3d).
///
/// Живая клавиатура ботов (см. `attach::button_action::COMMAND`) шлёт кнопки
/// БЕЗ `ACTION`/`ACTION_VALUE` — только `COMMAND`+`COMMAND_PARAMS`+`BOT_ID`.
/// Официальная документация числит метод в scope `im` (НЕ `imbot`), и живая
/// проба портала `test-work-2` это подтвердила: `im.message.command` с
/// `MESSAGE_ID`/`BOT_ID`/`COMMAND`/`COMMAND_PARAMS` от обычного входящего
/// вебхука ответил `result: true` без единой ошибки прав.
///
/// НЕ идемпотентен и БЕЗ дедуп-идентификатора — как `im.message.add`: повтор
/// после обрыва означает ВТОРОЕ выполнение бот-команды (второй голос,
/// повторная заявка и т.п.), поэтому метод стоит в
/// [`super::NO_RETRY_METHODS`].
pub const M_MESSAGE_COMMAND: &str = "im.message.command";

/// Метод «отметить диалог прочитанным» (Ф6).
pub const M_DIALOG_READ: &str = "im.dialog.read";

/// Метод «пометить диалог непрочитанным» (Ф6).
///
/// ЛОВУШКА: `MESSAGE_ID` ОБЯЗАТЕЛЕН. Без него портал отвечает
/// `MESSAGE_ID_ERROR` («First unread message id can't be empty»), поэтому
/// обёртка принимает его значением, а не `Option`, — вызов без идентификатора
/// невозможен по типам.
pub const M_DIALOG_UNREAD: &str = "im.dialog.unread";

/// Метод «печатает…» (Ф6). Только ИСХОДЯЩИЙ: входящих событий набора текста
/// платформа вебхуку не присылает (§6 плана).
pub const M_DIALOG_WRITING: &str = "im.dialog.writing";

/// Метод «подписаться на поток событий мессенджера» (Ф7).
///
/// ЕДИНСТВЕННЫЙ realtime-механизм, доступный входящему вебхуку: `event.bind`,
/// `pull.*` и исходящие вебхуки отвечают `WRONG_AUTH_TYPE` /
/// `insufficient_scope`. Метод ИДЕМПОТЕНТЕН — повторный вызов не создаёт вторую
/// подписку и не сбрасывает очередь, поэтому звать его при каждом подъёме
/// воркера безопасно.
///
/// ЛОВУШКА: метода НЕТ в реестре `methods.json` портала, а `method.get` по нему
/// отвечает пустотой — при этом сам метод работает. Реестр не является
/// источником истины о доступности.
pub const M_EVENT_SUBSCRIBE: &str = "im.v2.Event.subscribe";

/// Метод «забрать накопленные события» (Ф7).
///
/// Параметр `offset` — это ПОДТВЕРЖДЕНИЕ (ack) уже обработанного, а не позиция
/// чтения: неподтверждённые события портал переотдаёт на каждом вызове. Ответ —
/// `{events, nextOffset, hasMore}` (см. [`super::schema::EventsPageRaw`]).
pub const M_EVENT_GET: &str = "im.v2.Event.get";

/// `LIMIT` обычного цикла опроса событий.
///
/// Ровно платформенный дефолт: пачка из ста событий покрывает даже всплеск
/// активного портала, а остаток честно приезжает признаком `hasMore` и
/// вычитывается через две секунды.
pub const EVENT_POLL_LIMIT: i64 = 100;

/// `LIMIT` ХОЛОДНОГО СТАРТА (потолок платформы).
///
/// Первый вызов делается БЕЗ `offset` и ровно с одной целью — получить
/// `nextOffset` как базу курсора. Сами события отбрасываются: ретроспективы у
/// платформы нет, очередь копится с момента `subscribe`, и показать её в ленте
/// значило бы вывалить пользователю всё, что случилось за время, пока devforge
/// не работал (историю грузит UI сам).
pub const EVENT_COLD_START_LIMIT: i64 = 1000;

/// Ключ команды `im.recent.get` в пакете фолбэк-синхронизации.
///
/// Тот же, что у холодного старта: обе ветки разбираются одним кодом.
pub const SYNC_RECENT_KEY: &str = COLD_START_RECENT_KEY;

/// Ключ команды `im.counters.get` в пакете фолбэк-синхронизации.
pub const SYNC_COUNTERS_KEY: &str = COLD_START_COUNTERS_KEY;

/// Ключ команды `tasks.task.counters.get` в пакете фолбэк-синхронизации (Ф10).
pub const SYNC_TASK_COUNTERS_KEY: &str = "task_counters";

/// Метод «выдать адрес скачивания файла» (Ф8).
///
/// ЕДИНСТВЕННЫЙ рабочий путь к байтам вложения. Поля `urlDownload`, `urlShow` и
/// `viewerAttrs.unifiedLink`, которые портал кладёт прямо в дескриптор файла,
/// БЕЗ СЕССИОННОЙ КУКИ не работают: живая проверка дала `HTTP 200` с HTML
/// страницы логина у первых двух и `403` у третьего. Именно поэтому их нет ни в
/// [`super::schema::FileRaw`], ни в [`crate::attach::MediaDto`] — не объявить
/// проще, чем не отдать.
///
/// # Ответ СОДЕРЖИТ СЕКРЕТ
///
/// `{"result":{"downloadUrl":"https://<портал>/rest/<uid>/<КОД ВЕБХУКА>/download/
/// ?token=…"}}`. Код вебхука в этом адресе — целиком, поэтому
/// [`super::schema::FileDownloadRaw`] не реализует `Serialize`, адрес не
/// логируется даже на `trace` и не покидает [`crate::routes::media`]: браузеру
/// уходят БАЙТЫ, а не ссылка, и редирект на неё запрещён.
pub const M_FILE_DOWNLOAD: &str = "im.v2.File.download";

/// Метод «залить файл И отправить сообщение» (Ф9, Р7).
///
/// ЕДИНСТВЕННЫЙ путь отправки файла, доступный входящему вебхуку. Классическая
/// связка Диска (`im.disk.folder.get` → `disk.folder.uploadfile` →
/// `im.disk.file.commit`) требует scope `disk`, которого вебхуку НЕ выдают:
/// живая проверка вернула `insufficient_scope`. У этого метода scope — `im`,
/// тот же, что у всей остальной переписки.
///
/// ```json
/// {"dialogId":"chat27",
///  "fields":{"name":"probe.txt","content":"<base64>","message":"подпись"}}
/// → {"result":{"file":{…},"messageId":97,"chatId":27,"dialogId":"chat27"}}
/// ```
///
/// # Один вызов — ДВА действия
///
/// Метод и заливает файл, и отправляет сообщение, возвращая его `messageId`.
/// Соблазн добавить следом [`M_MESSAGE_ADD`] «чтобы отправить файл» стоил бы
/// ВТОРОЙ строки в чате, и убрать её потом было бы нечем.
///
/// # Тело — файл в base64
///
/// Потолок платформы — 100 МБ, но 100 МБ файла дают ~133 МБ строки и столько
/// же на сериализованное тело. Поэтому наш дефолт —
/// [`crate::config::DEFAULT_MAX_UPLOAD_BYTES`] (25 MiB), и проверяется он ДО
/// приёма байтов от браузера ([`crate::routes::upload::post_upload`]).
///
/// # Р9: повтора нет
///
/// Метод стоит в [`super::NO_RETRY_METHODS`], и запрет не обходится вложением
/// в `batch`. Дедуп-идентификатора у портала нет: повтор дал бы второй файл на
/// Диске и вторую строку в чате.
///
/// Ошибки портала: `FILE_EMPTY`, `INVALID_CONTENT`, `FOLDER_ERROR`,
/// `UPLOAD_FAILED`, `SEND_FAILED`, `FILE_TOO_LARGE`, `ACCESS_DENIED`.
pub const M_FILE_UPLOAD: &str = "im.v2.File.upload";

// ---------------------------------------------------------------------------
// Ф10: задачи.
// ---------------------------------------------------------------------------

/// Метод «список задач» (Ф10).
///
/// Ответ — `{"result":{"tasks":[…]}, "total": N}`: **`total` лежит РЯДОМ с
/// `result`**, а не внутри, поэтому берётся из конверта
/// ([`super::error::B24Response::total`]), а не из тела результата.
///
/// Страница ФИКСИРОВАНА платформой: пятьдесят записей, листается смещением
/// `start = (N-1)*50`. Параметра `LIMIT` у метода нет вовсе.
///
/// # ДВЕ ЛОВУШКИ, ОБЕ МОЛЧАЛИВЫЕ
///
/// 1. **`{"STATUS": 2}` в фильтре даёт НОЛЬ записей** при двух подходящих
///    задачах. `STATUS` — псевдополе, пересекающееся с виртуальными
///    состояниями; работают только `REAL_STATUS` и операторные формы
///    (`<STATUS`, `<REAL_STATUS` — проверено живьём). Проверку делает
///    [`task_list_params`], страж — `tests/tasks_traps.rs`.
/// 2. **camelCase в `select` МОЛЧА игнорируется**: `select:["ID","chatId"]`
///    вернул `chatId: null`, а `["ID","CHAT_ID"]` — `chatId: 7`. Имена в
///    `select` — UPPERCASE v1, а ключи ОТВЕТА — camelCase. Отсюда
///    [`crate::tasks::LIST_SELECT`].
pub const M_TASK_LIST: &str = "tasks.task.list";

/// Метод «карточка задачи» (Ф10).
///
/// Зовётся **БЕЗ `select`** — намеренно. С ним пришлось бы перечислять полсотни
/// имён в правильном регистре, а любая опечатка дала бы не ошибку, а `null` в
/// поле (ловушка 2). Без `select` портал отдаёт всё, включая `chatId`, `action`
/// и `newCommentsCount`, и стоит это столько же.
///
/// Ответ: `{"result":{"task":{…}}}` — карточка под ключом `task`.
pub const M_TASK_GET: &str = "tasks.task.get";

/// Метод «счётчики задач» (Ф10).
///
/// `{userId}` обязателен. Живой ответ: `{"expired":{"counter":2,…},
/// "new_comments":{"counter":0,…},"mentioned":{"counter":0,…}}`.
///
/// Это ДРУГИЕ счётчики, чем `im.counters.get`: там непрочитанные СООБЩЕНИЯ
/// чатов задач (`TYPE.TASKS_TASK`), здесь — состояние самих задач. Складывать
/// их нельзя: просроченная задача может не иметь ни одного непрочитанного
/// комментария и наоборот.
pub const M_TASK_COUNTERS: &str = "tasks.task.counters.get";

/// Метод «чат сущности» (Ф10).
///
/// # ГЛАВНАЯ ЛОВУШКА ЗАДАЧ
///
/// `ENTITY_TYPE` обязан быть `"TASKS_TASK"`. Значение `"TASKS"` МОЛЧА
/// возвращает `{"result": null}` при HTTP 200 — не ошибку, не пустой объект, а
/// именно `null`, который легко принять за «чата нет». Единственный источник
/// значения — [`crate::dto::ENTITY_TASK`]; литерал `"TASKS"` запрещён
/// тестом-стражем, который читает исходники плагина построчно.
///
/// Ответ: `{"result":{"ID":7}}` либо `{"result":null}`.
pub const M_CHAT_GET: &str = "im.chat.get";

/// Метод «закрепить/открепить диалог» (Р14).
pub const M_RECENT_PIN: &str = "im.recent.pin";

/// Метод «скрыть диалог из списка» (Р14).
pub const M_RECENT_HIDE: &str = "im.recent.hide";

/// Потолок `LIMIT` у `im.recent.list` (документированный максимум портала).
///
/// Больше просить бессмысленно: портал молча урежет. Меньше — тоже вариант,
/// но фильтрация у него идёт ПОСЛЕ выборки, и мелкие страницы возвращают ещё
/// меньше записей, чем запрошено (живьём `LIMIT=5` дал четыре).
pub const RECENT_LIST_MAX_LIMIT: i64 = 200;

/// Ключ команды `im.recent.get` в пакете холодного старта.
pub const COLD_START_RECENT_KEY: &str = "recent";

/// Ключ команды `im.counters.get` в пакете холодного старта.
pub const COLD_START_COUNTERS_KEY: &str = "counters";

/// Ключ первой страницы `im.recent.list` в пакете списка диалогов.
pub const DIALOGS_LIST_KEY: &str = "list";

/// Ключ задач с участием владельца вебхука в пакете списка диалогов.
pub const DIALOGS_TASKS_KEY: &str = "tasks";

/// Результат [`B24Client::dialogs_bundle`].
#[derive(Debug, Default)]
pub struct DialogsBundle {
    /// Строки `im.recent.get` — верхушка списка, включая чаты задач.
    pub recent: Vec<RecentItemRaw>,
    /// Счётчики непрочитанного.
    pub counters: CountersRaw,
    /// Первая страница `im.recent.list`; `None` — команда отказала.
    pub list: Option<RecentListRaw>,
    /// Первая страница задач с участием (`MEMBER`), свежие по активности.
    pub tasks: Vec<super::schema::TaskRaw>,
}

/// Право на мессенджер: чаты, сообщения, счётчики, поток событий.
///
/// Без него плагин бесполезен целиком.
pub const SCOPE_IM: &str = "im";

/// Право на задачи: карточка, статусы, действия (Ф10).
///
/// Технически его отсутствие не смертельно — чаты задач приходят и через `im`
/// (чат задачи это обычный IM-чат), деградирует только карточка над лентой. Но
/// автомат подключения (Ф3) всё равно требует его НА ВХОДЕ вместе с `im`
/// (см. [`crate::login::REQUIRED_SCOPES`]): пересоздать вебхук с недостающей
/// галочкой — операция на полминуты, а панель, молча работающая наполовину,
/// диагностируется пользователем гораздо дороже.
pub const SCOPE_TASK: &str = "task";

/// Синоним `task` у части редакций портала.
pub const SCOPE_TASKS: &str = "tasks";

/// Что делает вызов `im.message.like` (Ф6).
///
/// Метод ПЕРЕКЛЮЧАЮЩИЙ: `auto` ставит лайк, если его не было, и снимает, если
/// был. Явные `plus`/`minus` нужны там, где решение уже принято (повтор
/// действия после обрыва не должен переворачивать состояние обратно).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LikeAction {
    /// Переключить (значение по умолчанию у портала и у нас).
    #[default]
    Auto,
    /// Поставить.
    Plus,
    /// Снять.
    Minus,
}

impl LikeAction {
    /// Значение параметра `ACTION` — дословно, как его ждёт портал.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Plus => "plus",
            Self::Minus => "minus",
        }
    }

    /// Разбирает значение из тела запроса.
    ///
    /// Отсутствие значения — это `auto` (переключение), а НЕ ошибка: кнопка
    /// «♥» шлёт именно его. Незнакомое значение — ошибка (`None`): молча
    /// подставить `auto` значило бы поставить лайк там, где просили снять.
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).unwrap_or("") {
            "" | "auto" => Some(Self::Auto),
            "plus" => Some(Self::Plus),
            "minus" => Some(Self::Minus),
            _ => None,
        }
    }
}

impl B24Client {
    /// `profile` — владелец вебхука.
    ///
    /// Первый шаг проверки подключения. Возвращает ровно того пользователя,
    /// чей `user_id` стоит в URL вебхука: подменить его нельзя, `user_id`
    /// криптографически связан с кодом.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `INVALID_CREDENTIALS` — вебхук неверен или отозван;
    /// транспортные отказы — портал недоступен.
    pub async fn profile(&self) -> Result<ProfileRaw, B24Error> {
        self.call(M_PROFILE, json!({})).await
    }

    /// `scope` — список прав вебхука.
    ///
    /// Второй шаг проверки подключения. Живьём на тестовом портале:
    /// `["im","task","tasks","tasks_extended"]`.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::profile`].
    pub async fn scope(&self) -> Result<Vec<String>, B24Error> {
        self.call(M_SCOPE, json!({})).await
    }

    /// `im.counters.get` — счётчики непрочитанного.
    ///
    /// Считает ТОЛЬКО непрочитанное: свои отправленные сообщения и правки его
    /// не двигают, поэтому он дополняет поток событий, а не заменяет его.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `insufficient_scope`, если у вебхука нет права
    /// [`SCOPE_IM`]; транспортные отказы.
    pub async fn counters(&self) -> Result<CountersRaw, B24Error> {
        self.call(M_COUNTERS, json!({})).await
    }

    /// `server.time` — время портала строкой ISO-8601.
    ///
    /// Самый безобидный метод портала: не требует прав, ничего не меняет и
    /// почти не расходует `operating`. Годится как проба транспорта, когда
    /// проверять права ещё рано.
    ///
    /// # Errors
    ///
    /// Транспортные отказы и отказ портала по существу.
    pub async fn server_time(&self) -> Result<String, B24Error> {
        self.call(M_SERVER_TIME, json!({})).await
    }

    /// `im.recent.get` — весь список диалогов одним ответом.
    ///
    /// Пагинации у метода нет вовсе: он отдаёт плоский массив. Взамен это
    /// ЕДИНСТВЕННЫЙ источник, который отдаёт чаты задач, — поэтому он и
    /// основа списка (Р3), а `im.recent.list` только дозагружает хвост.
    ///
    /// `last_sync_date` (формат ATOM) превращает вызов в инкрементальный:
    /// портал отдаст только изменившиеся строки. Ограничение платформы —
    /// **не старше 7 дней** (Р21): с более старой датой ответ бесполезен, и
    /// вызывающий обязан сделать полную перезагрузку.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `insufficient_scope` без права [`SCOPE_IM`];
    /// транспортные отказы и оба лимита портала.
    pub async fn recent_get(
        &self,
        last_sync_date: Option<&str>,
    ) -> Result<Vec<RecentItemRaw>, B24Error> {
        self.call(M_RECENT_GET, recent_get_params(last_sync_date))
            .await
    }

    /// `im.recent.list` — страница списка диалогов.
    ///
    /// Чаты задач НЕ отдаёт (проверено живьём) — поэтому в одиночку списком
    /// служить не может. `total` в ответе равен `-1` и не используется:
    /// признак «есть ещё» — только `hasMore`.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::recent_get`].
    pub async fn recent_list(&self, limit: i64, offset: i64) -> Result<RecentListRaw, B24Error> {
        self.call(M_RECENT_LIST, recent_list_params(limit, offset))
            .await
    }

    /// `im.user.get` — один пользователь.
    ///
    /// Нужен там, где идентификатор ровно один (например, владелец вебхука).
    /// Для списка используйте [`B24Client::user_list_get`]: резолв по одному
    /// при бюджете 2 запроса в секунду означал бы, что чат из тридцати
    /// участников открывается пятнадцать секунд.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::recent_get`].
    pub async fn user_get(&self, id: i64) -> Result<UserRaw, B24Error> {
        self.call(M_USER_GET, json!({ "ID": id })).await
    }

    /// `im.user.list.get` — пачка пользователей.
    ///
    /// **Возвращает КАРТУ `{"15":{…},"19":{…}}`, а не массив** — ключ это
    /// идентификатор строкой. Пустой результат приезжает как `[]` (PHP), что и
    /// разбирает [`UserMapRaw`].
    ///
    /// Пустой вход не порождает запроса.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::recent_get`].
    pub async fn user_list_get(&self, ids: &[i64]) -> Result<HashMap<String, UserRaw>, B24Error> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let raw: UserMapRaw = self.call(M_USER_LIST_GET, user_list_params(ids)).await?;
        Ok(raw.into_map())
    }

    /// `im.dialog.get` — карточка диалога.
    ///
    /// Отличие от строки списка: здесь `role` приходит СТРОЧНЫМИ буквами
    /// (`"owner"`), тогда как `im.recent.get` пишет `"OWNER"`. Нормализацию
    /// делает проекция ([`crate::dto::normalize_role`]), а не вызывающий.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::recent_get`], плюс `ACCESS_ERROR`, если
    /// диалог недоступен владельцу вебхука.
    pub async fn dialog_get(&self, dialog_id: &str) -> Result<super::schema::ChatRaw, B24Error> {
        self.call(M_DIALOG_GET, json!({ "DIALOG_ID": dialog_id }))
            .await
    }

    /// `im.dialog.users.list` — участники диалога.
    ///
    /// Отдаёт ПОЛНЫЕ объекты пользователей, поэтому один вызов заодно
    /// наполняет кэш имён ([`crate::peers::PeerCache::absorb_users`]).
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::dialog_get`].
    pub async fn dialog_users_list(&self, dialog_id: &str) -> Result<Vec<UserRaw>, B24Error> {
        self.call(M_DIALOG_USERS_LIST, json!({ "DIALOG_ID": dialog_id }))
            .await
    }

    /// `im.dialog.messages.get` — страница истории диалога.
    ///
    /// # Три параметра и два направления
    ///
    /// | аргумент | параметр портала | смысл |
    /// |---|---|---|
    /// | `limit` | `LIMIT` | клампится [`MESSAGES_MAX_LIMIT`] |
    /// | `before_id` | `LAST_ID` | СТРОГО старее: `id < LAST_ID` (листаем вверх) |
    /// | `after_id` | `FIRST_ID` | СТРОГО новее: `id > FIRST_ID` (догружаем низ) |
    ///
    /// **Порядок ответа зависит от направления**: базовый вызов и `LAST_ID`
    /// отдают по УБЫВАНИЮ id, `FIRST_ID` — по ВОЗРАСТАНИЮ. Нормализует его
    /// вызывающий ([`crate::dto::sort_messages_ascending`]) — здесь ответ
    /// остаётся таким, каким его прислал портал, чтобы разница была видна в
    /// тестах транспорта.
    ///
    /// Обе границы одновременно портал трактует непредсказуемо, поэтому при
    /// заданном `before_id` второй параметр НЕ отправляется вовсе (см.
    /// [`dialog_messages_params`]).
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `DIALOG_ID_EMPTY` / `ACCESS_ERROR` (диалога нет или
    /// он недоступен владельцу вебхука), `FIRST_ID_STRING` / `LAST_ID_STRING`
    /// (граница не число), плюс транспортные отказы и оба лимита портала.
    pub async fn dialog_messages(
        &self,
        dialog_id: &str,
        limit: i64,
        before_id: Option<i64>,
        after_id: Option<i64>,
    ) -> Result<super::schema::MessagesRaw, B24Error> {
        self.call(
            M_DIALOG_MESSAGES_GET,
            dialog_messages_params(dialog_id, limit, before_id, after_id),
        )
        .await
    }

    /// Холодный старт панели: список диалогов И счётчики ОДНИМ запросом (Р3).
    ///
    /// # Почему именно `batch`
    ///
    /// Внешний `batch` стоит ОДИН хит лимита интенсивности (2 req/s) и не
    /// расходует `operating` вложенных методов. По отдельности те же два
    /// вызова — это два хита и лишние полсекунды ожидания на боевом лимите,
    /// причём ровно в тот момент, когда пользователь смотрит на пустую панель.
    ///
    /// # Что происходит с отказом одной команды
    ///
    /// `halt: 0`: команды независимы. Отказ `im.counters.get` не отменяет
    /// список — счётчики просто приедут нулевыми, и панель покажет диалоги без
    /// бейджа. Обратное тоже верно. Ошибкой считается только отказ САМОГО
    /// `batch` (транспорт, лимит, отказ портала).
    ///
    /// # Errors
    ///
    /// Всё, чем может ответить [`B24Client::batch`]. Отдельные `result_error`
    /// ошибкой НЕ являются — они приводят к пустому значению своей половины.
    pub async fn cold_start(&self) -> Result<(Vec<RecentItemRaw>, CountersRaw), B24Error> {
        self.recent_and_counters(None).await
    }

    /// Пакет страницы списка диалогов: `im.recent.get` + ПЕРВАЯ страница
    /// `im.recent.list` + `im.counters.get` + задачи с участием владельца
    /// вебхука — всё ещё ОДИН хит лимита.
    ///
    /// # Почему нужны оба источника диалогов ВСЕГДА
    ///
    /// Живьём `im.recent.get` отдаёт не весь список, а только верхушку: 16
    /// строк при 81 диалоге на одном портале и 33 при 213 на другом. Раньше
    /// `im.recent.list` звался, только если `im.recent.get` вернул 200+ строк,
    /// — этого не происходило никогда, и большая часть чатов молча пропадала.
    /// Чаты задач при этом есть только в `im.recent.get`, поэтому один
    /// `im.recent.list` тоже не годится.
    ///
    /// # Зачем здесь задачи
    ///
    /// Чат задачи попадает в `im.recent.get`, только пока в нём недавно
    /// писали: живьём 6 чатов задач при 28 задачах с участием. Поэтому
    /// директория «задачи» добирается из `tasks.task.list{MEMBER}` — фильтр
    /// портала, покрывающий ВСЕ роли: постановщик, ответственный,
    /// соисполнитель, наблюдатель. `member_id = None` ⇒ без этой команды.
    ///
    /// Отказ отдельной команды (`halt: 0`) даёт пустую/`None` часть, а не
    /// ошибку: список без хвоста лучше пустой панели.
    ///
    /// # Errors
    ///
    /// Всё, чем может ответить [`B24Client::batch`].
    pub async fn dialogs_bundle(&self, member_id: Option<i64>) -> Result<DialogsBundle, B24Error> {
        let mut calls = vec![
            BatchCall::new(COLD_START_RECENT_KEY, M_RECENT_GET, recent_get_params(None)),
            BatchCall::new(
                DIALOGS_LIST_KEY,
                M_RECENT_LIST,
                recent_list_params(RECENT_LIST_MAX_LIMIT, 0),
            ),
            BatchCall::new(COLD_START_COUNTERS_KEY, M_COUNTERS, json!({})),
        ];
        if let Some(uid) = member_id.filter(|v| *v > 0) {
            calls.push(BatchCall::new(
                DIALOGS_TASKS_KEY,
                M_TASK_LIST,
                member_tasks_params(uid),
            ));
        }
        let outcome = self.batch(calls).await?;
        Ok(DialogsBundle {
            recent: decode_part::<Vec<RecentItemRaw>>(&outcome, COLD_START_RECENT_KEY)
                .unwrap_or_default(),
            counters: decode_part::<CountersRaw>(&outcome, COLD_START_COUNTERS_KEY)
                .unwrap_or_default(),
            list: decode_part::<RecentListRaw>(&outcome, DIALOGS_LIST_KEY),
            tasks: decode_part::<super::schema::TaskListRaw>(&outcome, DIALOGS_TASKS_KEY)
                .map(|t| t.tasks)
                .unwrap_or_default(),
        })
    }

    /// Список диалогов и счётчики ОДНИМ пакетом, с необязательной инкрементой.
    ///
    /// Общее тело холодного старта (Ф4) и фолбэк-синхронизации (Ф7): с
    /// `last_sync_date` портал отдаёт только строки, изменившиеся с указанного
    /// момента, без него — весь список. Разница между двумя ветками ровно в
    /// этом параметре, поэтому вторая копия пакета была бы вторым местом, где
    /// однажды забудут `SKIP_OPENLINES` или ключ разбора.
    ///
    /// **Дата уезжает через urlencode пакета** (`+03:00` → `%2B03:00`): без
    /// кодирования портал прочитал бы `+` как пробел и ответил бы данными за
    /// другой момент времени — молча и правдоподобно.
    ///
    /// Ограничение платформы (Р21): дата не старше 7 дней. С более старой
    /// вызывающий обязан уйти в ветку полной перезагрузки — проверка живёт в
    /// [`crate::recent_sync`], а не здесь, потому что «сколько прошло» знает
    /// только он.
    ///
    /// # Errors
    ///
    /// Всё, чем может ответить [`B24Client::batch`].
    pub async fn recent_and_counters(
        &self,
        last_sync_date: Option<&str>,
    ) -> Result<(Vec<RecentItemRaw>, CountersRaw), B24Error> {
        let (recent, counters, _) = self.sync_bundle(last_sync_date, None).await?;
        Ok((recent, counters))
    }

    /// Список, счётчики сообщений И счётчики задач — ОДНИМ пакетом (Ф10).
    ///
    /// Третья команда добавляется в ТОТ ЖЕ `batch`, а не отдельным вызовом:
    /// внешний пакет стоит один хит лимита интенсивности независимо от числа
    /// команд внутри, поэтому счётчики задач достаются фолбэк-воркеру
    /// бесплатно. Отдельный запрос стоил бы половины бюджета в 2 rps каждую
    /// минуту — ради двух чисел в бейдже.
    ///
    /// `user_id = None` (владелец вебхука ещё не известен) означает «без
    /// счётчиков задач»: у `tasks.task.counters.get` параметр `userId`
    /// обязателен, и звать его с нулём бессмысленно.
    ///
    /// # Errors
    ///
    /// Всё, чем может ответить [`B24Client::batch`]. Отказ ОДНОЙ команды
    /// внутри пакета ошибкой не является (`halt: 0`): список без счётчиков
    /// лучше, чем пустая панель.
    pub async fn sync_bundle(
        &self,
        last_sync_date: Option<&str>,
        user_id: Option<i64>,
    ) -> Result<
        (
            Vec<RecentItemRaw>,
            CountersRaw,
            Option<super::schema::TaskCountersRaw>,
        ),
        B24Error,
    > {
        let mut calls = vec![
            BatchCall::new(
                COLD_START_RECENT_KEY,
                M_RECENT_GET,
                recent_get_params(last_sync_date),
            ),
            BatchCall::new(COLD_START_COUNTERS_KEY, M_COUNTERS, json!({})),
        ];
        if let Some(uid) = user_id.filter(|v| *v > 0) {
            calls.push(BatchCall::new(
                SYNC_TASK_COUNTERS_KEY,
                M_TASK_COUNTERS,
                json!({ "userId": uid }),
            ));
        }
        let outcome = self.batch(calls).await?;

        let recent = decode_part::<Vec<RecentItemRaw>>(&outcome, COLD_START_RECENT_KEY)
            .unwrap_or_default();
        let counters =
            decode_part::<CountersRaw>(&outcome, COLD_START_COUNTERS_KEY).unwrap_or_default();
        let tasks =
            decode_part::<super::schema::TaskCountersRaw>(&outcome, SYNC_TASK_COUNTERS_KEY);
        Ok((recent, counters, tasks))
    }

    // -----------------------------------------------------------------

    // Ф6: цикл переписки.
    // -----------------------------------------------------------------

    /// `im.message.add` — отправить текст. Возвращает идентификатор созданного
    /// сообщения.
    ///
    /// # Р9: этот вызов НЕ ПОВТОРЯЕТСЯ НИКОГДА
    ///
    /// Запрет живёт не здесь, а в транспорте ([`super::NO_RETRY_METHODS`]),
    /// поэтому его нельзя обойти ни новым роутом, ни вложением метода в
    /// `batch`. Причина: у портала нет дедуп-идентификатора (аналога
    /// `random_id` у VK), и повтор после обрыва — это второе сообщение у
    /// собеседника. Транспортный отказ доезжает до фронта как `503`, а решение
    /// «повторить» принимает пользователь кнопкой на строке.
    ///
    /// # Что делает с текстом сам портал
    ///
    /// При `URL_PREVIEW: "Y"` (значение по умолчанию, мы его не переопределяем)
    /// голые ссылки ПЕРЕПИСЫВАЮТСЯ в `[URL]…[/URL]`. Поэтому отправленная
    /// строка и строка в истории не совпадают байт в байт, и сверять их можно
    /// только после [`crate::bbcode::to_plain`].
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `MESSAGE_EMPTY` (пустой текст), `ERROR_NO_ACCESS`
    /// (нет прав писать в этот чат), `*_ERROR` (портал не принял параметры);
    /// транспортные отказы и оба лимита портала.
    pub async fn message_add(
        &self,
        dialog_id: &str,
        text: &str,
        reply_id: Option<i64>,
    ) -> Result<i64, B24Error> {
        let resp = self
            .call_value(M_MESSAGE_ADD, message_add_params(dialog_id, text, reply_id))
            .await?;
        message_id_from_result(&resp.result).ok_or_else(|| B24Error::Decode {
            reason: format!("{M_MESSAGE_ADD}: result is not a message id"),
        })
    }

    /// `im.message.update` — изменить текст сообщения.
    ///
    /// **Пустой текст УДАЛЯЕТ сообщение** — это поведение портала, а не наше.
    /// Обёртка на пустую строку до сети не доходит: удаление обязано идти через
    /// [`B24Client::message_delete`], иначе «стёр всё и нажал Enter»
    /// оборачивался бы безвозвратной потерей строки без единого вопроса.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `CANT_EDIT_MESSAGE` (истекло окно правки либо
    /// сообщение чужое) — контракт превращает его в `409 invalid_state`; плюс
    /// транспортные отказы.
    pub async fn message_update(&self, message_id: i64, text: &str) -> Result<bool, B24Error> {
        if text.trim().is_empty() {
            return Err(B24Error::Api {
                code: crate::api::error::ERR_MESSAGE_EMPTY.to_string(),
                description: "an empty text would delete the message; use delete instead"
                    .to_string(),
            });
        }
        self.call_flag(M_MESSAGE_UPDATE, message_update_params(message_id, text))
            .await
    }

    /// `im.message.delete` — удалить сообщение.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::message_update`].
    pub async fn message_delete(&self, message_id: i64) -> Result<bool, B24Error> {
        self.call_flag(M_MESSAGE_DELETE, json!({ "MESSAGE_ID": message_id }))
            .await
    }

    /// `im.message.like` — поставить, снять или переключить «лайк».
    ///
    /// Результат виден не в ответе (он всегда `true`), а в `params.LIKE`
    /// сообщения — поэтому вызывающий перечитывает строку, если хочет показать
    /// новое состояние.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::message_update`].
    pub async fn message_like(
        &self,
        message_id: i64,
        action: LikeAction,
    ) -> Result<bool, B24Error> {
        self.call_flag(M_MESSAGE_LIKE, like_params(message_id, action))
            .await
    }

    /// `im.message.command` — выполнить команду бота (кнопка клавиатуры вида
    /// `COMMAND`, forge-zw3d).
    ///
    /// В отличие от `CALL`/`DIALOG` НЕ требует scope `imbot` — живая проба
    /// портала подтвердила успех с обычным входящим вебхуком (см.
    /// [`M_MESSAGE_COMMAND`]).
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::message_update`], плюс `BOT_ID_ERROR` портала
    /// (кнопка без `BOT_ID` до сети не доезжает — см.
    /// `attach::COMMAND_NEEDS_BOT_ID`).
    pub async fn message_command(
        &self,
        message_id: i64,
        bot_id: i64,
        command: &str,
        command_params: Option<&str>,
    ) -> Result<bool, B24Error> {
        self.call_flag(
            M_MESSAGE_COMMAND,
            message_command_params(message_id, bot_id, command, command_params),
        )
        .await
    }

    /// `im.dialog.read` — отметить прочитанным.
    ///
    /// Возвращает оставшийся счётчик непрочитанного диалога (`counter` живого
    /// ответа). Портал отдаёт объект `{dialogId, chatId, lastId, counter}`,
    /// причём `dialogId` приезжает ЧИСЛОМ даже для строкового идентификатора —
    /// поэтому разбирается он как `Value`, а наружу уезжает только счётчик.
    ///
    /// Без `message_id` метод читает диалог ЦЕЛИКОМ — ровно то, что нужно
    /// «пользователь долистал до низа».
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::dialog_messages`].
    pub async fn dialog_read(
        &self,
        dialog_id: &str,
        message_id: Option<i64>,
    ) -> Result<i64, B24Error> {
        let resp = self
            .call_value(M_DIALOG_READ, dialog_read_params(dialog_id, message_id))
            .await?;
        Ok(resp
            .result
            .get("counter")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default())
    }

    /// `im.dialog.unread` — пометить диалог непрочитанным С УКАЗАННОГО
    /// сообщения.
    ///
    /// `message_id` принимается ЗНАЧЕНИЕМ, а не `Option`: без него портал
    /// отвечает `MESSAGE_ID_ERROR`, и единственный способ гарантировать, что
    /// такой вызов не уедет в сеть, — не дать его выразить.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::dialog_messages`].
    pub async fn dialog_unread(&self, dialog_id: &str, message_id: i64) -> Result<bool, B24Error> {
        self.call_flag(
            M_DIALOG_UNREAD,
            json!({ "DIALOG_ID": dialog_id, "MESSAGE_ID": message_id }),
        )
        .await
    }

    /// `im.dialog.writing` — «печатает…».
    ///
    /// Односторонний метод: отправка работает, а ВХОДЯЩИХ событий набора текста
    /// вебхуку не приходит вовсе (§6 плана) — поэтому в панели чужое «печатает»
    /// не показывается никогда, и это не баг.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::dialog_messages`].
    pub async fn dialog_writing(&self, dialog_id: &str) -> Result<bool, B24Error> {
        self.call_flag(M_DIALOG_WRITING, json!({ "DIALOG_ID": dialog_id }))
            .await
    }

    /// `im.recent.pin` — закрепить или открепить диалог (Р14).
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::dialog_messages`].
    pub async fn recent_pin(&self, dialog_id: &str, pin: bool) -> Result<bool, B24Error> {
        self.call_flag(M_RECENT_PIN, recent_pin_params(dialog_id, pin))
            .await
    }

    /// `im.recent.hide` — скрыть диалог из списка (Р14).
    ///
    /// Скрытие НЕ удаляет ни чат, ни историю: диалог возвращается в список от
    /// первого же нового сообщения.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::dialog_messages`].
    pub async fn recent_hide(&self, dialog_id: &str) -> Result<bool, B24Error> {
        self.call_flag(M_RECENT_HIDE, json!({ "DIALOG_ID": dialog_id }))
            .await
    }

    // -----------------------------------------------------------------
    // Ф7: поток событий.
    // -----------------------------------------------------------------

    /// `im.v2.Event.subscribe` — подписаться на события мессенджера.
    ///
    /// Зовётся ОДИН раз при подъёме воркера. Повтор безвреден (метод
    /// идемпотентен), но и бессмысленен: подписка уже есть, а вызов стоит хита
    /// лимита интенсивности.
    ///
    /// С момента подписки портал КОПИТ события в очереди — в том числе пока
    /// devforge выключен. Ретроспективы при этом нет: события до первой
    /// подписки не существуют вовсе.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `insufficient_scope` без права [`SCOPE_IM`];
    /// транспортные отказы и оба лимита портала.
    pub async fn event_subscribe(&self) -> Result<bool, B24Error> {
        self.call_flag(M_EVENT_SUBSCRIBE, json!({})).await
    }

    /// `im.v2.Event.get` — забрать пачку событий и ПОДТВЕРДИТЬ предыдущую.
    ///
    /// `offset` — это ack: всё, что старше него, портал считает обработанным и
    /// больше не отдаёт. `None` означает «ничего не подтверждаю» и возвращает
    /// очередь с самого начала — ровно то, что нужно холодному старту, и ровно
    /// то, чего нельзя делать в цикле (иначе каждая пачка приезжала бы заново).
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::event_subscribe`].
    pub async fn events_get(
        &self,
        offset: Option<&str>,
        limit: i64,
    ) -> Result<super::schema::EventsPageRaw, B24Error> {
        self.call(M_EVENT_GET, events_get_params(offset, limit))
            .await
    }

    /// `im.v2.File.download` — адрес скачивания файла Диска (Ф8).
    ///
    /// # ВОЗВРАЩАЕТ СЕКРЕТ
    ///
    /// В строке — код вебхука целиком. Её нельзя ни логировать, ни класть в
    /// DTO, ни отдавать заголовком `Location`: единственный законный получатель
    /// — [`crate::routes::media`], который тут же скачивает по ней байты и
    /// забывает адрес. Тест-страж `download_urls_never_leak_to_the_client`
    /// ловит регрессию со стороны ответа, а отсутствие `Serialize` у
    /// [`super::schema::FileDownloadRaw`] — со стороны типов.
    ///
    /// # Errors
    ///
    /// `404`-подобные отказы портала (`FILE_NOT_FOUND`, `ACCESS_DENIED`),
    /// транспортные ошибки и оба лимита. Пустой `downloadUrl` — это
    /// [`B24Error::Decode`]: адрес есть либо его нет, «пустая строка» означала
    /// бы скачивание из ниоткуда.
    pub async fn file_download(&self, file_id: i64) -> Result<String, B24Error> {
        let raw: super::schema::FileDownloadRaw = self
            .call(M_FILE_DOWNLOAD, json!({ "fileId": file_id }))
            .await?;
        let url = raw.download_url.trim().to_string();
        if url.is_empty() {
            return Err(B24Error::Decode {
                // Ни `file_id`, ни тем более адрес в текст не подставляются:
                // текст ошибки уезжает в лог и в `/status`.
                reason: format!("{M_FILE_DOWNLOAD}: empty downloadUrl"),
            });
        }
        Ok(url)
    }

    /// `im.v2.File.upload` — залить файл и отправить сообщение ОДНИМ вызовом
    /// (Ф9, Р7).
    ///
    /// `content` — уже готовая base64-строка: собирает её
    /// [`crate::routes::upload`] потоком из staging-файла, а не эта обёртка.
    /// Разделение не косметическое — здесь строка только перекладывается в
    /// JSON, и держать в этой функции ещё и чтение с диска значило бы, что
    /// транспорт знает про файловую систему.
    ///
    /// Пустая подпись НЕ отправляется полем `message` вовсе: портал у части
    /// методов трактует пустой текст как ошибку параметров, а «файл без
    /// подписи» — совершенно нормальный случай.
    ///
    /// Возвращает `messageId` отправленного сообщения — им лента перечитывает
    /// строку и показывает вложение.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `FILE_EMPTY`, `INVALID_CONTENT`, `FOLDER_ERROR`,
    /// `UPLOAD_FAILED`, `SEND_FAILED`, `FILE_TOO_LARGE`, `ACCESS_DENIED`;
    /// транспортные отказы и оба лимита портала. [`B24Error::Decode`] — ответ
    /// без `messageId`: файл, возможно, и залился, но сослаться на строку
    /// нечем, и делать вид, что всё получилось, нельзя.
    ///
    /// **Повтора нет** (Р9) — см. [`M_FILE_UPLOAD`].
    pub async fn file_upload(
        &self,
        dialog_id: &str,
        name: &str,
        content_b64: &str,
        message: &str,
    ) -> Result<i64, B24Error> {
        let mut fields = serde_json::Map::new();
        fields.insert("name".into(), json!(name));
        fields.insert("content".into(), json!(content_b64));
        if !message.trim().is_empty() {
            fields.insert("message".into(), json!(message));
        }
        let raw: super::schema::FileUploadRaw = self
            .call(
                M_FILE_UPLOAD,
                json!({"dialogId": dialog_id, "fields": serde_json::Value::Object(fields)}),
            )
            .await?;
        message_id_from_result(&raw.message_id).ok_or_else(|| B24Error::Decode {
            // Ни имени файла, ни диалога в тексте: он уезжает в лог.
            reason: format!("{M_FILE_UPLOAD}: result has no messageId"),
        })
    }

    // -----------------------------------------------------------------
    // Ф10: задачи.
    // -----------------------------------------------------------------

    /// `tasks.task.list` — страница задач и их общее число.
    ///
    /// Возвращает `(задачи, total)`. `total` берётся ИЗ КОНВЕРТА: у этого
    /// метода он лежит рядом с `result`, а не внутри него. `None` означает
    /// «портал числа не прислал» — так бывает при `start: -1`, и выдавать за
    /// него ноль нельзя, иначе список схлопнулся бы в пустой.
    ///
    /// `filter` проходит через [`task_list_params`], который ОТКАЗЫВАЕТСЯ
    /// отправлять запрещённый ключ `STATUS` (ловушка 3): молчаливый ноль
    /// записей хуже отказа.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `insufficient_scope` без права [`SCOPE_TASK`];
    /// транспортные отказы и оба лимита портала. [`B24Error::Decode`] — фильтр
    /// содержит запрещённый ключ (до сети такой вызов не доходит).
    pub async fn task_list(
        &self,
        filter: &serde_json::Value,
        start: i64,
    ) -> Result<(Vec<super::schema::TaskRaw>, Option<i64>), B24Error> {
        let params = task_list_params(filter, start)?;
        let resp = self.call_value(M_TASK_LIST, params).await?;
        let total = resp.total;
        let page: super::schema::TaskListRaw =
            serde_json::from_value(resp.result).map_err(|e| B24Error::Decode {
                reason: format!("{M_TASK_LIST}: {e}"),
            })?;
        Ok((page.tasks, total))
    }

    /// `tasks.task.get` — карточка задачи **БЕЗ `select`**.
    ///
    /// Отсутствие `select` — не оптимизация, а защита от ловушки 2: имена в
    /// нём пишутся UPPERCASE, ответ приходит camelCase, и camelCase в `select`
    /// портал молча игнорирует, отдавая `null` вместо значения. Без `select`
    /// угадывать нечего.
    ///
    /// # «Задачи нет» — это НЕ ошибка портала
    ///
    /// На несуществующий (или недоступный) идентификатор портал отвечает
    /// `{"result": []}` — успехом с пустым PHP-массивом, а не отказом
    /// (проверено живьём на `taskId: 999999`). Строгий разбор в
    /// `{"task": {…}}` дал бы здесь `Decode`, то есть `502 bitrix_error`, — а
    /// правильный ответ пользователю `404 unknown_task`. Поэтому обёртка
    /// возвращает `Option`, и пустота отличается от поломки по типу.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `ACCESS_DENIED`; транспортные отказы и оба лимита
    /// портала.
    pub async fn task_get(
        &self,
        task_id: i64,
    ) -> Result<Option<super::schema::TaskRaw>, B24Error> {
        let resp = self
            .call_value(M_TASK_GET, json!({ "taskId": task_id }))
            .await?;
        // `[]`, `null` и пустой объект — все три означают «такой задачи нет».
        if !resp.result.is_object() {
            return Ok(None);
        }
        let raw: super::schema::TaskGetRaw =
            serde_json::from_value(resp.result).map_err(|e| B24Error::Decode {
                reason: format!("{M_TASK_GET}: {e}"),
            })?;
        if raw.task.id.as_i64().unwrap_or_default() <= 0 {
            return Ok(None);
        }
        Ok(Some(raw.task))
    }

    /// Семантическое действие над задачей: `tasks.task.start` и соседи.
    ///
    /// Метод приходит ИЗ ТАБЛИЦЫ [`crate::tasks::ACTION_METHODS`], а не из
    /// URL: собирать имя метода из чужой строки означало бы позволить дёрнуть
    /// любой метод портала одним запросом.
    ///
    /// Семантические методы предпочтительнее `tasks.task.update {STATUS}`: они
    /// проводят бизнес-логику портала (контроль, оценка) и пишут в чат задачи
    /// ПРАВИЛЬНОЕ системное сообщение — ровно то, которое пользователь потом
    /// видит в ленте.
    ///
    /// # Errors
    ///
    /// [`B24Error::Api`] с `ACCESS_DENIED` (действие недоступно этой роли);
    /// транспортные отказы и оба лимита портала.
    pub async fn task_action(&self, method: &str, task_id: i64) -> Result<bool, B24Error> {
        self.call_flag(method, json!({ "taskId": task_id })).await
    }

    /// `tasks.task.counters.get` — счётчики задач сотрудника.
    ///
    /// # Errors
    ///
    /// Те же, что у [`B24Client::task_get`].
    pub async fn task_counters(
        &self,
        user_id: i64,
    ) -> Result<super::schema::TaskCountersRaw, B24Error> {
        self.call(M_TASK_COUNTERS, json!({ "userId": user_id }))
            .await
    }

    /// `im.chat.get {ENTITY_TYPE, ENTITY_ID}` — чат сущности.
    ///
    /// # ЗДЕСЬ ЖИВЁТ ГЛАВНАЯ ЛОВУШКА ЗАДАЧ
    ///
    /// `entity_type` НЕ является параметром намеренно: единственное значение —
    /// [`crate::dto::ENTITY_TASK`] (`"TASKS_TASK"`). Похожее `"TASKS"` портал
    /// принимает с HTTP 200 и отвечает `{"result": null}`, то есть «чата нет»
    /// — и ошибка живёт ровно до того дня, когда кто-нибудь заметит, что у
    /// задач никогда не открывается лента. Раз значение одно, пусть его
    /// невозможно будет передать неправильно.
    ///
    /// `Ok(None)` означает «у задачи нет чата» (Р20) — и `null`, и нулевой
    /// идентификатор дают именно его, а не «чат с номером 0».
    ///
    /// # Errors
    ///
    /// Транспортные отказы и оба лимита портала. Отказ портала по существу
    /// (нет прав, нет задачи) — [`B24Error::Api`].
    pub async fn task_chat_id(&self, task_id: i64) -> Result<Option<i64>, B24Error> {
        let raw: Option<super::schema::ChatIdRaw> = self
            .call(
                M_CHAT_GET,
                json!({ "ENTITY_TYPE": crate::dto::ENTITY_TASK, "ENTITY_ID": task_id.to_string() }),
            )
            .await?;
        Ok(raw.and_then(|c| c.id.as_i64()).filter(|v| *v > 0))
    }

    /// Вызов метода, отвечающего «получилось/не получилось».
    ///
    /// Успехом считается САМ ФАКТ успешного конверта: половина командных
    /// методов портала отвечает `true`, но встречаются и объекты (`im.dialog.
    /// read`), и числа. Требовать здесь строго `bool` значило бы ронять
    /// исправную операцию из-за формы ответа, которую мы всё равно не
    /// используем.
    async fn call_flag(&self, method: &str, params: serde_json::Value) -> Result<bool, B24Error> {
        let resp = self.call_value(method, params).await?;
        Ok(resp.result.as_bool().unwrap_or(true))
    }
}

/// Разбирает одну часть пакетного ответа, ГРОМКО жалуясь на неудачу.
///
/// # Почему это не `from_value(…).ok()`
///
/// Раньше здесь стояло именно оно, и неудачный разбор превращался в пустое
/// значение молча. Живая проверка Ф9 показала, чем это кончается: один чат с
/// `entity_link: null` ронял разбор ВСЕГО списка диалогов, панель показывала
/// ноль чатов, а в логе не оставалось НИ ОДНОЙ строки — воркер синхронизации
/// тоже молчит, потому что пишет только при непустом результате. Дефект
/// невозможно было ни увидеть, ни продиагностировать по логу.
///
/// Поведение при отказе НЕ изменилось (часть пакета пропадает, соседние
/// доезжают): падать целиком из-за одного поля — ровно та крайность, от
/// которой это место и защищает. Изменилось одно: отказ теперь ВИДЕН.
///
/// # Секрет в тексте ошибки
///
/// Сообщение serde цитирует значение, а в сырье портала попадаются поля с
/// кодом вебхука внутри (`downloadUrl`). Поэтому текст пропускается через
/// [`super::error::scrub_urls`] — тот же страж, что чистит ошибки транспорта.
/// Само значение в лог не уходит НИКОГДА.
fn decode_part<T: serde::de::DeserializeOwned>(
    outcome: &super::batch::BatchOutcome,
    key: &str,
) -> Option<T> {
    let raw = outcome.get(key)?;
    match serde_json::from_value::<T>(raw.clone()) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                target: "forge_bitrix",
                part = key,
                error = %super::error::scrub_urls(&e.to_string()),
                "forge-bitrix: a part of the batch response did not match the schema; it is dropped"
            );
            None
        }
    }
}

/// Идентификатор сообщения из результата `im.message.add`.
///
/// Живьём приезжает числом (`{"result":77}`), но строковая форма встречается у
/// части методов портала повсеместно, и отличать здесь одну от другой нечем:
/// разбор обеих стоит одной строки, а промах означал бы, что успешно
/// отправленное сообщение считается неотправленным (и пользователь жмёт
/// «повторить», получая дубль).
pub fn message_id_from_result(result: &serde_json::Value) -> Option<i64> {
    result
        .as_i64()
        .or_else(|| result.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        .filter(|v| *v > 0)
}

/// Параметры `im.message.add`.
///
/// `URL_PREVIEW` НЕ передаётся: значение по умолчанию (`"Y"`) — то самое
/// поведение портала, к которому привыкли пользователи веб-мессенджера, и
/// расхождение панели с ним читалось бы как поломка ссылок.
///
/// `REPLY_ID` уходит ТОЛЬКО положительным: ноль портал понял бы как «ответ на
/// сообщение с идентификатором 0», то есть отверг бы всё сообщение целиком.
/// Текстовую цитату к нему добавляет вызывающий (Р10) — портал не отражает
/// `REPLY_ID` при чтении, и без цитаты ответ визуально теряется.
pub fn message_add_params(dialog_id: &str, text: &str, reply_id: Option<i64>) -> serde_json::Value {
    let mut params = json!({ "DIALOG_ID": dialog_id, "MESSAGE": text });
    if let Some(reply) = reply_id.filter(|v| *v > 0) {
        params["REPLY_ID"] = json!(reply);
    }
    params
}

/// Параметры `im.message.update`.
///
/// `IS_EDITED` не передаётся: портал сам ставит `params.IS_EDITED = "Y"` после
/// успешной правки, и это единственный источник метки «(изм.)» в ленте.
pub fn message_update_params(message_id: i64, text: &str) -> serde_json::Value {
    json!({ "MESSAGE_ID": message_id, "MESSAGE": text })
}

/// Параметры `im.dialog.read`.
///
/// `MESSAGE_ID` отправляется только положительным: у метода он НЕобязателен, и
/// «читать до нуля» — это не то же самое, что «читать всё».
pub fn dialog_read_params(dialog_id: &str, message_id: Option<i64>) -> serde_json::Value {
    let mut params = json!({ "DIALOG_ID": dialog_id });
    if let Some(id) = message_id.filter(|v| *v > 0) {
        params["MESSAGE_ID"] = json!(id);
    }
    params
}

/// Параметры `im.message.like`.
pub fn like_params(message_id: i64, action: LikeAction) -> serde_json::Value {
    json!({ "MESSAGE_ID": message_id, "ACTION": action.as_str() })
}

/// Параметры `im.message.command`.
///
/// `COMMAND_PARAMS` уходит ТОЛЬКО если непустой: пустая строка у портала —
/// не «параметра нет», а «пустой параметр», и незачем гадать разницу, если
/// кнопка её вовсе не прислала.
pub fn message_command_params(
    message_id: i64,
    bot_id: i64,
    command: &str,
    command_params: Option<&str>,
) -> serde_json::Value {
    let mut params = json!({
        "MESSAGE_ID": message_id,
        "BOT_ID": bot_id,
        "COMMAND": command,
    });
    if let Some(p) = command_params.filter(|s| !s.is_empty()) {
        params["COMMAND_PARAMS"] = json!(p);
    }
    params
}

/// Параметры `im.recent.pin`: булев флаг у портала — это `"Y"`/`"N"`.
pub fn recent_pin_params(dialog_id: &str, pin: bool) -> serde_json::Value {
    json!({ "DIALOG_ID": dialog_id, "PIN": if pin { "Y" } else { "N" } })
}

/// Параметры `im.recent.get`.
///
/// Чистая функция, а не тело метода: ровно эта же строка уезжает внутрь
/// `batch` на холодном старте, и второй экземпляр параметров разъехался бы с
/// первым при первой же правке.
///
/// `SKIP_OPENLINES: "Y"` — открытые линии (обращения клиентов) это отдельный
/// продукт портала со своей логикой распределения; показывать их в панели
/// разработчика незачем, а тянуть — значит платить за них лимитом.
pub fn recent_get_params(last_sync_date: Option<&str>) -> serde_json::Value {
    let mut params = json!({"SKIP_OPENLINES": "Y"});
    if let Some(date) = last_sync_date.filter(|d| !d.is_empty()) {
        params["LAST_SYNC_DATE"] = json!(date);
    }
    params
}

/// Параметры `im.v2.Event.get`.
///
/// Чистая функция ровно потому же, почему и соседние: именно её результат
/// проверяет тест «повторный `offset` не отдаёт ничего», и «какой offset мы на
/// самом деле подтвердили» видно без запуска воркера.
///
/// Правила:
///
/// 1. **`offset` уходит только непустым.** Пустая строка и `None` — это
///    «ничего не подтверждаю» (холодный старт), а `offset=""` портал понял бы
///    как ноль и вернул бы очередь с начала уже В ЦИКЛЕ, то есть задвоил бы
///    ленту.
/// 2. `LIMIT` клампится в `1..=`[`EVENT_COLD_START_LIMIT`]: потолок платформы,
///    выше портал молча урежет.
pub fn events_get_params(offset: Option<&str>, limit: i64) -> serde_json::Value {
    let mut params = json!({ "limit": limit.clamp(1, EVENT_COLD_START_LIMIT) });
    if let Some(o) = offset.map(str::trim).filter(|o| !o.is_empty()) {
        params["offset"] = json!(o);
    }
    params
}

/// Параметры `im.recent.list`.
///
/// `LIMIT` клампится в `1..=`[`RECENT_LIST_MAX_LIMIT`], `OFFSET` — не
/// отрицателен: портал на мусорные значения отвечает не ошибкой, а пустой
/// страницей, что читалось бы как «диалогов больше нет».
pub fn recent_list_params(limit: i64, offset: i64) -> serde_json::Value {
    json!({
        "SKIP_OPENLINES": "Y",
        "LIMIT": limit.clamp(1, RECENT_LIST_MAX_LIMIT),
        "OFFSET": offset.max(0),
    })
}

/// Параметры `im.dialog.messages.get`.
///
/// Чистая функция — ровно она проверяется тестом клампинга, и ровно её
/// результат уезжает в журнал стенда, поэтому «какой `LIMIT` мы на самом деле
/// попросили» видно без запуска сервера.
///
/// Правила:
///
/// 1. `LIMIT` клампится в `1..=`[`MESSAGES_MAX_LIMIT`] — запрос «дай 500»
///    молча ограничивается, а не отвергается: страницу просит фронт, и
///    ошибка на ровном месте сломала бы ленту.
/// 2. Границы отправляются ТОЛЬКО положительные: `LAST_ID: 0` портал понял бы
///    как «сообщений старее нуля», то есть отдал бы пустоту вместо истории.
/// 3. При обеих заданных границах побеждает `before_id`: одновременная подача
///    `LAST_ID` и `FIRST_ID` — это запрос «старее X и новее Y», у которого нет
///    ни одного осмысленного ответа у метода без диапазонной выборки.
pub fn dialog_messages_params(
    dialog_id: &str,
    limit: i64,
    before_id: Option<i64>,
    after_id: Option<i64>,
) -> serde_json::Value {
    let mut params = json!({
        "DIALOG_ID": dialog_id,
        "LIMIT": limit.clamp(1, MESSAGES_MAX_LIMIT),
    });
    match (before_id.filter(|v| *v > 0), after_id.filter(|v| *v > 0)) {
        (Some(before), _) => params["LAST_ID"] = json!(before),
        (None, Some(after)) => params["FIRST_ID"] = json!(after),
        (None, None) => {}
    }
    params
}

/// Параметры `tasks.task.list`.
///
/// Чистая функция — ровно её результат проверяют тесты-стражи ловушек, поэтому
/// «что мы на самом деле отправили» видно без запуска сервера.
///
/// Правила:
///
/// 1. **`select` только из [`crate::tasks::LIST_SELECT`]** — все имена
///    UPPERCASE. camelCase портал молча игнорирует, и поле приезжает `null`,
///    неотличимо от настоящего `null` (ловушка 2).
/// 2. **Ключ `STATUS` в фильтре ЗАПРЕЩЁН** (ловушка 3): он даёт ноль записей
///    молча. Разрешены `REAL_STATUS` и операторные формы (`<STATUS`,
///    `<REAL_STATUS`) — оператор адресует настоящее поле. Нарушение — это
///    [`B24Error::Decode`] ДО сети: молчаливо подменять ключ нельзя (запрос
///    вернул бы не то, что просили), а отправлять заведомо пустой ответ — тем
///    более.
/// 3. `start` не отрицателен и кратен странице: у метода нет `LIMIT`, страница
///    фиксирована пятьюдесятью записями, и смещение «полтора экрана» дало бы
///    строки, уже показанные на предыдущей странице.
/// 4. `order: {"ID":"desc"}` — новые задачи сверху; портал без явного порядка
///    отдаёт их в порядке своей выборки.
///
/// # Errors
///
/// [`B24Error::Decode`], если фильтр содержит запрещённый ключ.
pub fn task_list_params(
    filter: &serde_json::Value,
    start: i64,
) -> Result<serde_json::Value, B24Error> {
    if let Some(map) = filter.as_object() {
        if let Some(bad) = map
            .keys()
            .find(|k| k.as_str() == crate::tasks::FILTER_STATUS_KEY)
        {
            return Err(B24Error::Decode {
                reason: format!(
                    "{M_TASK_LIST}: filter key '{bad}' silently returns zero rows; \
                     use '{}' or an operator form",
                    crate::tasks::FILTER_REAL_STATUS_KEY
                ),
            });
        }
    }
    let mut params = json!({
        "select": crate::tasks::LIST_SELECT,
        "order": {"ID": "desc"},
        "start": start.max(0) / crate::tasks::PAGE_SIZE * crate::tasks::PAGE_SIZE,
    });
    if filter.as_object().is_some_and(|m| !m.is_empty()) {
        params["filter"] = filter.clone();
    }
    Ok(params)
}

/// Параметры `tasks.task.list` для задач, где пользователь фигурирует в
/// ЛЮБОЙ роли.
///
/// `MEMBER` — фильтр портала по участию: постановщик, ответственный,
/// соисполнитель, наблюдатель. `RESPONSIBLE_ID` покрывал бы одну роль из
/// четырёх. Порядок — по последней активности: первая страница (50 записей)
/// тогда содержит самые живые задачи.
pub fn member_tasks_params(user_id: i64) -> serde_json::Value {
    json!({
        "select": crate::tasks::LIST_SELECT,
        "order": {"ACTIVITY_DATE": "desc"},
        "filter": {crate::tasks::FILTER_MEMBER_KEY: user_id.max(0)},
        "start": 0,
    })
}

/// Параметры `im.user.list.get`: `ID[0]=…&ID[1]=…`.
///
/// Индексы обязательны — форму `ID[]=` портал принимает не везде;
/// разворачивает массив в такие имена [`super::batch::encode_params`].
pub fn user_list_params(ids: &[i64]) -> serde_json::Value {
    json!({ "ID": ids })
}

/// Есть ли у вебхука право на мессенджер.
///
/// Регистр приводится: портал отдаёт права строчными, но повторять эту веру в
/// пяти местах дороже, чем нормализовать один раз здесь.
pub fn has_im_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|s| s.eq_ignore_ascii_case(SCOPE_IM))
}

/// Есть ли у вебхука право на задачи (в любом из двух написаний).
pub fn has_task_scope(scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(SCOPE_TASK) || s.eq_ignore_ascii_case(SCOPE_TASKS))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Неудачный разбор части пакета обязан быть ВИДЕН в логе — и обязан
    /// молчать про секрет.
    ///
    /// Обе половины найдены живой проверкой Ф9. Раньше здесь стояло
    /// `from_value(…).ok()`, и один чат с `entity_link: null` обнулял весь
    /// список диалогов, не оставив в логе ни строки. Но громкий лог сам по
    /// себе — новый риск: сообщение serde цитирует значение, а в сырье портала
    /// попадаются поля с кодом вебхука внутри (`downloadUrl`).
    #[test]
    fn a_part_that_does_not_match_the_schema_is_logged_without_the_secret() {
        crate::testlog::install();
        let mark = crate::testlog::mark();

        let mut outcome = crate::api::batch::BatchOutcome::default();
        outcome.ok.insert(
            "recent".to_string(),
            // Форма заведомо не та — и в тексте ошибки окажется URL с кодом.
            json!("https://p.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/im.recent.get.json"),
        );

        let parsed = decode_part::<Vec<RecentItemRaw>>(&outcome, "recent");
        assert!(parsed.is_none(), "мусор не имеет права разобраться");

        let text = crate::testlog::since(mark);
        assert!(
            text.contains("did not match the schema"),
            "отказ обязан быть виден в логе: {text}"
        );
        assert!(text.contains("part=\"recent\""), "не назван ключ части: {text}");
        assert!(
            !text.contains("wqdzbfhmn83d23j4"),
            "код вебхука утёк в лог: {text}"
        );
        assert!(
            !text.contains("p.bitrix24.ru"),
            "хост портала утёк в лог: {text}"
        );

        // Отсутствующий ключ — не отказ и в лог не пишется: часть пакета могли
        // просто не запрашивать (счётчики задач без известного user_id).
        let mark = crate::testlog::mark();
        assert!(decode_part::<CountersRaw>(&outcome, "counters").is_none());
        assert!(
            crate::testlog::since(mark).is_empty(),
            "непрошенная часть не повод для строки в логе"
        );
    }

    /// Имена методов — контракт с порталом: переименование обязано валить
    /// тест, а не отправлять запрос в никуда.
    #[test]
    fn method_names_are_frozen() {
        assert_eq!(M_PROFILE, "profile");
        assert_eq!(M_SCOPE, "scope");
        assert_eq!(M_COUNTERS, "im.counters.get");
        assert_eq!(M_SERVER_TIME, "server.time");
        assert_eq!(M_BATCH, "batch");
        assert_eq!(M_RECENT_GET, "im.recent.get");
        assert_eq!(M_RECENT_LIST, "im.recent.list");
        assert_eq!(M_USER_GET, "im.user.get");
        assert_eq!(M_USER_LIST_GET, "im.user.list.get");
        assert_eq!(M_DIALOG_GET, "im.dialog.get");
        assert_eq!(M_DIALOG_USERS_LIST, "im.dialog.users.list");
        assert_eq!(M_DIALOG_MESSAGES_GET, "im.dialog.messages.get");
        assert_eq!(M_MESSAGE_ADD, "im.message.add");
        assert_eq!(M_MESSAGE_UPDATE, "im.message.update");
        assert_eq!(M_MESSAGE_DELETE, "im.message.delete");
        assert_eq!(M_MESSAGE_LIKE, "im.message.like");
        assert_eq!(M_MESSAGE_COMMAND, "im.message.command");
        assert_eq!(M_DIALOG_READ, "im.dialog.read");
        assert_eq!(M_DIALOG_UNREAD, "im.dialog.unread");
        assert_eq!(M_DIALOG_WRITING, "im.dialog.writing");
        assert_eq!(M_RECENT_PIN, "im.recent.pin");
        assert_eq!(M_RECENT_HIDE, "im.recent.hide");
        // Ф7: регистр в именах `im.v2.*` ЗНАЧИМ — портал отвечает
        // `ERROR_METHOD_NOT_FOUND` на `im.v2.event.get`.
        assert_eq!(M_EVENT_SUBSCRIBE, "im.v2.Event.subscribe");
        assert_eq!(M_EVENT_GET, "im.v2.Event.get");
        // Ф10: задачи.
        assert_eq!(M_TASK_LIST, "tasks.task.list");
        assert_eq!(M_TASK_GET, "tasks.task.get");
        assert_eq!(M_TASK_COUNTERS, "tasks.task.counters.get");
        assert_eq!(M_CHAT_GET, "im.chat.get");
    }

    /// ЛОВУШКА 3: ключ `STATUS` в фильтре списка задач отвергается ДО сети.
    ///
    /// Живьём `{"RESPONSIBLE_ID":1,"STATUS":2,"GROUP_ID":0}` вернул `total: 0`
    /// при двух полностью подходящих задачах — успехом, молча. Единственный
    /// способ не потерять это знание — отказ на нашей стороне.
    #[test]
    fn the_forbidden_status_filter_never_reaches_the_portal() {
        let err = task_list_params(&json!({"STATUS": 2}), 0).unwrap_err();
        assert!(matches!(err, B24Error::Decode { .. }), "{err:?}");

        // Тот же фильтр в компании законных ключей — тоже отказ.
        assert!(task_list_params(&json!({"RESPONSIBLE_ID": 1, "STATUS": 2}), 0).is_err());

        // А `REAL_STATUS` и операторные формы законны: оператор адресует
        // настоящее поле, а не псевдополе.
        for good in [
            json!({"REAL_STATUS": 2}),
            json!({"<REAL_STATUS": 5}),
            json!({"<STATUS": 5}),
            json!({">STATUS": 2}),
        ] {
            let params = task_list_params(&good, 0).expect("законный фильтр");
            assert_eq!(params["filter"], good);
        }
    }

    /// ЛОВУШКА 2: в `select` не должно быть ни одного camelCase-имени.
    #[test]
    fn the_list_select_never_carries_a_camel_case_name() {
        let params = task_list_params(&json!({}), 0).unwrap();
        let select = params["select"].as_array().expect("select — массив");
        assert!(!select.is_empty());
        for name in select {
            let name = name.as_str().unwrap();
            assert!(
                !name.chars().any(|c| c.is_ascii_lowercase()),
                "camelCase в select: {name} — портал вернёт null молча"
            );
        }
        // Пустой фильтр не отправляется вовсе: `filter: {}` портал трактует
        // непредсказуемо.
        assert!(params.get("filter").is_none());
    }

    /// Смещение страницы кратно платформенной странице и не отрицательно.
    #[test]
    fn the_page_offset_is_snapped_to_the_platform_page() {
        assert_eq!(task_list_params(&json!({}), 0).unwrap()["start"], 0);
        assert_eq!(task_list_params(&json!({}), 50).unwrap()["start"], 50);
        assert_eq!(task_list_params(&json!({}), 137).unwrap()["start"], 100);
        assert_eq!(task_list_params(&json!({}), -5).unwrap()["start"], 0);
    }

    /// Параметры потока событий: `offset` — подтверждение, и пустым он не
    /// уезжает никогда.
    #[test]
    fn event_params_send_the_offset_only_when_there_is_something_to_ack() {
        let cold = events_get_params(None, EVENT_COLD_START_LIMIT);
        assert_eq!(cold["limit"], EVENT_COLD_START_LIMIT);
        assert!(
            cold.get("offset").is_none(),
            "холодный старт ничего не подтверждает"
        );

        let cycle = events_get_params(Some("46"), EVENT_POLL_LIMIT);
        assert_eq!(cycle["offset"], "46");
        assert_eq!(cycle["limit"], EVENT_POLL_LIMIT);

        // Пустая строка — это «нечего подтверждать», а не «offset = 0»:
        // портал по нулю отдал бы очередь с начала прямо в цикле.
        for empty in ["", "   "] {
            assert!(events_get_params(Some(empty), 100).get("offset").is_none());
        }

        // Потолок платформы клампится молча.
        assert_eq!(events_get_params(None, 100_000)["limit"], 1000);
        assert_eq!(events_get_params(None, 0)["limit"], 1);
    }

    /// СТРАЖ Р21 и ловушки `+` в дате: инкрементальная синхронизация уезжает в
    /// пакет с датой, закодированной как `%2B`.
    ///
    /// Без кодирования портал прочитал бы `+` как ПРОБЕЛ и ответил бы данными
    /// за другой момент времени — успехом, молча и правдоподобно.
    #[test]
    fn the_sync_batch_encodes_the_plus_in_the_date() {
        let call = crate::api::batch::BatchCall::new(
            SYNC_RECENT_KEY,
            M_RECENT_GET,
            recent_get_params(Some("2026-08-05T20:14:43+03:00")),
        );
        let command = call.to_command();
        assert!(command.starts_with("im.recent.get?"), "{command}");
        assert!(
            command.contains("2026-08-05T20%3A14%3A43%2B03%3A00"),
            "дата обязана уехать urlencode'нутой: {command}"
        );
        assert!(!command.contains("+03"), "{command}");
        assert_eq!(SYNC_RECENT_KEY, "recent");
        assert_eq!(SYNC_COUNTERS_KEY, "counters");
    }

    /// СТРАЖ Р9: метод отправки обязан оставаться в списке «никогда не
    /// повторять». Список живёт в транспорте, а имя метода — здесь; разъезд
    /// этих двух строк вернул бы дубли сообщений при первом же обрыве.
    #[test]
    fn the_send_method_is_the_one_the_transport_refuses_to_retry() {
        assert!(
            crate::api::NO_RETRY_METHODS.contains(&M_MESSAGE_ADD),
            "im.message.add обязан быть в NO_RETRY_METHODS (Р9)"
        );
        assert!(
            crate::api::NO_RETRY_METHODS.contains(&M_MESSAGE_COMMAND),
            "im.message.command обязан быть в NO_RETRY_METHODS (forge-zw3d): повтор — второе исполнение бот-команды"
        );
        // Соседи по модулю ИДЕМПОТЕНТНЫ и повторяться обязаны: правка одного и
        // того же текста дважды даёт тот же результат, а вот второе
        // `im.message.add` — второе сообщение.
        for m in [M_MESSAGE_UPDATE, M_MESSAGE_DELETE, M_MESSAGE_LIKE] {
            assert!(
                !crate::api::NO_RETRY_METHODS.contains(&m),
                "{m} идемпотентен и должен повторяться"
            );
        }
    }

    /// Параметры отправки: `REPLY_ID` появляется только положительным, а
    /// `URL_PREVIEW` мы не трогаем (дефолт портала).
    #[test]
    fn send_params_carry_the_reply_id_only_when_it_is_real() {
        let plain = message_add_params("chat27", "привет", None);
        assert_eq!(plain["DIALOG_ID"], "chat27");
        assert_eq!(plain["MESSAGE"], "привет");
        assert!(plain.get("REPLY_ID").is_none());
        assert!(
            plain.get("URL_PREVIEW").is_none(),
            "дефолт портала не переопределяем"
        );

        let reply = message_add_params("chat27", "ответ", Some(85));
        assert_eq!(reply["REPLY_ID"], 85);

        for bogus in [Some(0), Some(-3)] {
            assert!(
                message_add_params("chat27", "x", bogus)
                    .get("REPLY_ID")
                    .is_none(),
                "неположительный REPLY_ID отверг бы всё сообщение"
            );
        }
    }

    /// Идентификатор из ответа `im.message.add` — и число, и строка.
    #[test]
    fn a_created_message_id_is_read_from_both_shapes() {
        assert_eq!(message_id_from_result(&json!(114)), Some(114));
        assert_eq!(message_id_from_result(&json!("114")), Some(114));
        assert_eq!(message_id_from_result(&json!(" 114 ")), Some(114));
        // Не идентификатор — это НЕ «отправилось»: иначе фронт заменил бы
        // оптимистичную строку пустотой.
        for bogus in [json!(0), json!(-1), json!(true), json!(null), json!("нет")] {
            assert_eq!(message_id_from_result(&bogus), None, "{bogus}");
        }
    }

    /// Параметры прочтения: `MESSAGE_ID` необязателен и уходит только
    /// положительным.
    #[test]
    fn read_params_omit_a_meaningless_message_id() {
        let all = dialog_read_params("chat27", None);
        assert_eq!(all["DIALOG_ID"], "chat27");
        assert!(all.get("MESSAGE_ID").is_none(), "без границы читаем всё");

        assert_eq!(dialog_read_params("chat27", Some(113))["MESSAGE_ID"], 113);
        assert!(dialog_read_params("chat27", Some(0))
            .get("MESSAGE_ID")
            .is_none());
    }

    /// `ACTION` лайка — закрытый словарь портала; незнакомое значение обязано
    /// стать ошибкой, а не молча превратиться в «переключить».
    #[test]
    fn the_like_action_is_a_closed_dictionary() {
        assert_eq!(LikeAction::parse(None), Some(LikeAction::Auto));
        assert_eq!(LikeAction::parse(Some("")), Some(LikeAction::Auto));
        assert_eq!(LikeAction::parse(Some(" auto ")), Some(LikeAction::Auto));
        assert_eq!(LikeAction::parse(Some("plus")), Some(LikeAction::Plus));
        assert_eq!(LikeAction::parse(Some("minus")), Some(LikeAction::Minus));
        assert_eq!(LikeAction::parse(Some("PLUS")), None, "регистр значим");
        assert_eq!(LikeAction::parse(Some("dislike")), None);

        assert_eq!(like_params(99, LikeAction::Auto)["ACTION"], "auto");
        assert_eq!(like_params(99, LikeAction::Plus)["ACTION"], "plus");
        assert_eq!(like_params(99, LikeAction::Minus)["MESSAGE_ID"], 99);
    }

    /// Булев флаг у портала — это `"Y"`/`"N"`, а не `true`/`false`.
    #[test]
    fn pin_uses_the_portal_spelling_of_a_boolean() {
        assert_eq!(recent_pin_params("chat27", true)["PIN"], "Y");
        assert_eq!(recent_pin_params("chat27", false)["PIN"], "N");
        assert_eq!(recent_pin_params("19", true)["DIALOG_ID"], "19");
    }

    /// Правка не передаёт `IS_EDITED`: метку ставит сам портал.
    #[test]
    fn update_params_do_not_fake_the_edited_mark() {
        let p = message_update_params(85, "новый текст");
        assert_eq!(p["MESSAGE_ID"], 85);
        assert_eq!(p["MESSAGE"], "новый текст");
        assert!(p.get("IS_EDITED").is_none());
    }

    /// `COMMAND_PARAMS` уходит, только если непустой (forge-zw3d).
    #[test]
    fn command_params_omit_empty_params_but_keep_bot_id_and_command() {
        let p = message_command_params(51, 17, "careButton", Some("5"));
        assert_eq!(p["MESSAGE_ID"], 51);
        assert_eq!(p["BOT_ID"], 17);
        assert_eq!(p["COMMAND"], "careButton");
        assert_eq!(p["COMMAND_PARAMS"], "5");

        let without = message_command_params(51, 17, "ping", None);
        assert!(without.get("COMMAND_PARAMS").is_none());

        let empty = message_command_params(51, 17, "ping", Some(""));
        assert!(empty.get("COMMAND_PARAMS").is_none(), "пустая строка — тоже отсутствие");
    }

    /// ПОТОЛОК `LIMIT` — 50, и он клампится молча: страницу просит фронт, а
    /// ошибка на ровном месте сломала бы ленту.
    #[test]
    fn message_page_params_clamp_the_limit_and_pick_one_direction() {
        let p = dialog_messages_params("chat27", 500, None, None);
        assert_eq!(p["DIALOG_ID"], "chat27");
        assert_eq!(p["LIMIT"], MESSAGES_MAX_LIMIT);
        assert!(p.get("LAST_ID").is_none() && p.get("FIRST_ID").is_none());

        assert_eq!(dialog_messages_params("19", 0, None, None)["LIMIT"], 1);
        assert_eq!(dialog_messages_params("19", -5, None, None)["LIMIT"], 1);
        assert_eq!(dialog_messages_params("19", 20, None, None)["LIMIT"], 20);

        // Вверх — LAST_ID (строго старее), вниз — FIRST_ID (строго новее).
        let up = dialog_messages_params("chat27", 50, Some(95), None);
        assert_eq!(up["LAST_ID"], 95);
        assert!(up.get("FIRST_ID").is_none());

        let down = dialog_messages_params("chat27", 50, None, Some(95));
        assert_eq!(down["FIRST_ID"], 95);
        assert!(down.get("LAST_ID").is_none());

        // Обе границы разом — побеждает верхняя: «старее X и новее Y» метод не
        // умеет, и портал ответил бы чем угодно.
        let both = dialog_messages_params("chat27", 50, Some(95), Some(10));
        assert_eq!(both["LAST_ID"], 95);
        assert!(both.get("FIRST_ID").is_none());

        // Ноль и отрицательное — это «границы нет», а не «граница ноль».
        let zero = dialog_messages_params("chat27", 50, Some(0), Some(-3));
        assert!(zero.get("LAST_ID").is_none() && zero.get("FIRST_ID").is_none());
    }

    /// Параметры списка — тоже контракт: `SKIP_OPENLINES` экономит лимит, а
    /// `LAST_SYNC_DATE` появляется только когда он есть (пустая строка
    /// означала бы «изменений с начала времён»).
    #[test]
    fn recent_params_are_minimal_and_incremental_only_on_demand() {
        let full = recent_get_params(None);
        assert_eq!(full["SKIP_OPENLINES"], "Y");
        assert!(full.get("LAST_SYNC_DATE").is_none());

        let incremental = recent_get_params(Some("2026-08-05T20:14:43+03:00"));
        assert_eq!(incremental["LAST_SYNC_DATE"], "2026-08-05T20:14:43+03:00");
        // Пустая строка — это «даты нет», а не «дата пустая».
        assert!(recent_get_params(Some("")).get("LAST_SYNC_DATE").is_none());
    }

    /// `LIMIT` за пределами платформенного диапазона портал молча урезает —
    /// клампим сами, чтобы поведение не зависело от его настроения.
    #[test]
    fn recent_list_params_are_clamped() {
        let p = recent_list_params(1000, -5);
        assert_eq!(p["LIMIT"], RECENT_LIST_MAX_LIMIT);
        assert_eq!(p["OFFSET"], 0);

        let p = recent_list_params(0, 40);
        assert_eq!(p["LIMIT"], 1, "нулевая страница — это не страница");
        assert_eq!(p["OFFSET"], 40);

        let p = recent_list_params(50, 0);
        assert_eq!(p["LIMIT"], 50);
    }

    /// Индексы в `ID[]` обязательны: форму без них портал принимает не везде.
    #[test]
    fn user_list_params_are_indexed() {
        let query = crate::api::batch::encode_params(&user_list_params(&[15, 19, 17]));
        assert_eq!(query, "ID[0]=15&ID[1]=19&ID[2]=17");
    }

    /// Ключи холодного старта — контракт с [`B24Client::cold_start`]: по ним
    /// же результат и разбирается.
    #[test]
    fn cold_start_keys_are_frozen() {
        assert_eq!(COLD_START_RECENT_KEY, "recent");
        assert_eq!(COLD_START_COUNTERS_KEY, "counters");
    }

    #[test]
    fn scope_checks_are_case_insensitive_and_accept_both_task_spellings() {
        let live: Vec<String> = ["im", "task", "tasks", "tasks_extended"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(has_im_scope(&live));
        assert!(has_task_scope(&live));

        assert!(has_im_scope(&["IM".to_string()]));
        assert!(has_task_scope(&["TASKS".to_string()]));

        // Пустой и чужой scope — это «прав нет», а не «наверное, есть».
        assert!(!has_im_scope(&[]));
        assert!(!has_im_scope(&["crm".to_string(), "imopenlines".to_string()]));
        assert!(!has_task_scope(&["im".to_string()]));
    }
}
