//! HTTP-поверхность плагина Bitrix24.
//!
//! Всё живёт под префиксом `/api/bitrix/*` плюс WebSocket `/ws/bitrix` —
//! коллизий с роутами хоста нет по построению.
//!
//! Сборка идёт в [`build_router`], который вызывается из
//! [`crate::register_routes`] ДО `.fallback(...)` и `.layer(auth)` хоста:
//! только так Bearer-auth и csrf_guard автоматически покрывают роуты плагина
//! в remote-mode.
//!
//! # ЗАМОРОЖЕННЫЙ КОНТРАКТ ИМЁН
//!
//! Таблица ниже — источник истины для `tmux-web/static/js/bitrix/api.js`. Обе
//! стороны обязаны совпадать дословно: расхождение даёт молчаливый 404, а не
//! ошибку сборки. Перечислены СРАЗУ все пути всех фаз — чтобы имена не
//! расползлись, когда роуты начнут добавляться по одному.
//!
//! | метод | путь | фаза | что делает |
//! |---|---|---|---|
//! | GET    | `/api/bitrix/status`                    | **Ф1** | состояние плагина и подключения; 200 всегда |
//! | GET    | `/ws/bitrix`                            | **Ф1** | поток событий ВСЕХ порталов в одном сокете (см. [`crate::ws`]) |
//! | POST   | `/api/bitrix/login/webhook`             | Ф3  | принять URL входящего вебхука (три формы ввода) |
//! | POST   | `/api/bitrix/logout`                    | Ф3  | стереть вебхук АДРЕСУЕМОГО портала ЛОКАЛЬНО (в портал не ходим) |
//! | GET    | `/api/bitrix/portals`                   | **МП** | список порталов (вкладок) — без единого следа секрета |
//! | POST   | `/api/bitrix/portals`                   | **МП** | подключить портал по вебхуку |
//! | PATCH  | `/api/bitrix/portals/:pid`              | **МП** | переименовать вкладку и/или переставить вкладки |
//! | DELETE | `/api/bitrix/portals/:pid`              | **МП** | отвязать портал (`?purge_cache=1` — снести и кэш) |
//! | GET    | `/api/bitrix/filters`                   | Ф4  | директории `all` / `chats` / `tasks` / `unread` (Р2) |
//! | GET    | `/api/bitrix/dialogs`                   | Ф4  | список диалогов (`?limit&offset&filter`) |
//! | GET    | `/api/bitrix/dialogs/:id/messages`      | Ф5  | страница истории (`?limit&before_id&after_id`) |
//! | GET    | `/api/bitrix/messages/:mid`             | Ф5  | одно сообщение (`?dialog_id`) — догрузка realtime |
//! | POST   | `/api/bitrix/dialogs/:id/messages`      | **Ф6** | отправить сообщение (+ файл из staging, Ф9) |
//! | PATCH  | `/api/bitrix/messages/:mid`             | **Ф6** | правка сообщения |
//! | DELETE | `/api/bitrix/messages/:mid`             | **Ф6** | удаление сообщения |
//! | POST   | `/api/bitrix/dialogs/:id/read`          | **Ф6** | отметить прочитанным (окно 800 мс) |
//! | POST   | `/api/bitrix/dialogs/:id/unread`        | **Ф6** | пометить непрочитанным (`message_id` обязателен) |
//! | POST   | `/api/bitrix/dialogs/:id/typing`        | **Ф6** | «печатает…» (окно 8 с) |
//! | POST   | `/api/bitrix/messages/:mid/like`        | **Ф6** | переключить «лайк» |
//! | POST   | `/api/bitrix/messages/:mid/command`     | forge-zw3d | выполнить команду бота (кнопка `COMMAND`) |
//! | POST   | `/api/bitrix/dialogs/:id/pin`           | Ф6  | закрепить/открепить диалог (Р14) |
//! | POST   | `/api/bitrix/dialogs/:id/hide`          | Ф6  | скрыть диалог из списка (Р14) |
//! | GET    | `/api/bitrix/tasks`                     | **Ф10** | список задач (`?filter&start&responsible&group`) |
//! | GET    | `/api/bitrix/tasks/counters`            | **Ф10** | счётчики задач + непрочитанное чатов задач |
//! | GET    | `/api/bitrix/tasks/:id`                 | **Ф10** | карточка задачи (БЕЗ `select` — ловушка платформы) |
//! | POST   | `/api/bitrix/tasks/:id/:action`         | **Ф10** | действие по матрице `action` |
//! | GET    | `/api/bitrix/media/file/:dialog/:mid/:index` | **Ф8** | байты вложения через прокси |
//! | GET    | `/api/bitrix/media/:dialog/:mid/:index` | **Ф8** | синоним предыдущего (форма соседних панелей) |
//! | GET    | `/api/bitrix/media/avatar/:user_id`     | **Ф8**  | аватар через прокси (Р6) |
//! | POST   | `/api/bitrix/upload`                    | **Ф9** | приём файла (raw body + `X-Forge-File-Name`) |
//! | GET    | `/api/bitrix/dialogs/:id/prompt`        | **анализ** | заметка пользователя к диалогу |
//! | PUT    | `/api/bitrix/dialogs/:id/prompt`        | **анализ** | сохранить заметку (пустая строка — стереть) |
//! | GET    | `/api/bitrix/dialogs/:id/analysis`      | **анализ** | последний сохранённый разбор + тред Q&A |
//! | POST   | `/api/bitrix/dialogs/:id/analysis`      | **анализ** | запустить разбор / обновление / вопрос |
//! | DELETE | `/api/bitrix/dialogs/:id/analysis`      | **анализ** | стереть разбор (тред и заметка целы) |
//! | DELETE | `/api/bitrix/dialogs/:id/analysis/thread` | **анализ** | стереть тред Q&A (разбор и заметка целы) |
//! | POST   | `/api/bitrix/dialogs/:id/analysis/cancel` | **анализ** | остановить идущий разбор |
//! | POST   | `/api/bitrix/voice/:dialog/:mid/:index/transcribe` | **голос** | запустить расшифровку голосового |
//! | GET    | `/api/bitrix/voice/:dialog/:mid/:index/transcript` | **голос** | готовая расшифровка голосового |
//! | GET    | `/api/bitrix/voice/settings` | **голос** | выбранный движок расшифровки |
//! | PUT    | `/api/bitrix/voice/settings` | **голос** | переключить движок (`local`/`cloud`) |
//!
//! ## Решения, зафиксированные здесь навсегда
//!
//! 1. **`bitrix`, а не `bitrix24`** — во всех именах: каталог, крейт, роуты,
//!    CLI-флаг, конфиг, JS-каталог. Цифра в пути ничего не уточняет, зато
//!    удлиняет каждое имя и провоцирует опечатки (`bitrix24` / `bitrix_24` /
//!    `b24`). DOM- и CSS-префикс при этом `b24-` — прецедент разнесения имени
//!    модуля и префикса уже есть у Telegram (`telegram` / `tg-`).
//! 2. **`dialogs`, а не `chats`.** Диалог у Bitrix24 — это и личная
//!    переписка, и групповой чат, и ЧАТ ЗАДАЧИ (`entity_type="TASKS_TASK"`).
//!    Одно слово на все три вида нужно и по существу, и ради симметрии с
//!    `/api/telegram/dialogs` и `/api/vk/dialogs`: главный инструмент
//!    сопровождения трёх панелей — построчный дифф фронтовых модулей.
//! 3. **Задачи не заводят вторую вкладку** (Р2). `~/tasks` — это директория
//!    ОДНОГО списка диалогов, а `GET /api/bitrix/tasks` отдаёт карточки для
//!    шапки над лентой, а не второй мессенджер. Уточнено в Ф10: счётчики
//!    задач висят на `/api/bitrix/tasks/counters`, то есть на СТАТИЧЕСКОМ
//!    сегменте рядом с `/:id`. Коллизии нет (у `matchit` статический сегмент
//!    приоритетнее параметра), но полагаться на это молча нельзя — есть тест
//!    `the_counters_route_is_not_shadowed_by_the_id_route`.
//! 4. **Медиа разделено на `/media/file/…` и `/media/avatar/:user_id`.**
//!    Аватар адресуется пользователем, вложение — тройкой
//!    диалог/сообщение/индекс; общий шаблон пути потребовал бы фиктивных
//!    сегментов. Уточнено в Ф8: у вложения есть ВТОРОЙ, короткий путь
//!    `/media/:dialog/:mid/:index` — ровно тот, которым адресуются панели VK и
//!    Telegram. Оба ведут в один хендлер, коллизии нет (число сегментов
//!    разное), а построчный дифф трёх фронтов — главный инструмент их
//!    сопровождения, и расхождение в одном адресе стоило бы дороже, чем лишняя
//!    строка в таблице роутов.
//! 5. **Правка, удаление и «лайк» висят на `/messages/:mid`, а не на
//!    `/dialogs/:id/messages/:mid`.** Уточнено в Ф6 по факту реализации:
//!    портал адресует все три операции ОДНИМ `MESSAGE_ID` (`im.message.update`,
//!    `im.message.delete`, `im.message.like` диалога не принимают вовсе).
//!    Обязательный сегмент `:id` в пути был бы фикцией — проверить его сервер
//!    не может, а иллюзию «операция ограничена этим диалогом» создаёт. Диалог
//!    всё же нужен, но только чтобы ПЕРЕЧИТАТЬ строку после действия, и потому
//!    приезжает необязательным полем тела — ровно как `?dialog_id` у
//!    `GET /api/bitrix/messages/:mid`, который живёт на том же пути с Ф5.
//! 6. **Файл уезжает в портал тем же роутом, что и текст** (Ф9). У
//!    `POST /api/bitrix/dialogs/:id/messages` появилось необязательное поле
//!    `upload_id`, а отдельного `/files` нет. Причина в методе платформы:
//!    `im.v2.File.upload` — это «залей файл И отправь сообщение» одним
//!    вызовом, поэтому второй роут означал бы второй путь отправки со своей
//!    копией дедупа, рассылки кадра `message_new` и перечитывания строки.
//!    `POST /api/bitrix/upload` при этом в портал не ходит вовсе — он только
//!    кладёт байты в staging и выдаёт `upload_id`.
//!
//! 7. **Состав вкладок живёт на `/portals`, а подключение — на `/login`**
//!    (пометка **МП** — мультипортальность). Соблазн объявить старые роуты
//!    устаревшими и оставить один CRUD велик, но экран подключения — это
//!    единственный путь, по которому в devforge попадает СЕКРЕТ, и у него своя
//!    форма ответа (`LoginStateDto`, та же, что у `/status` и WS-кадра
//!    `login_state`), свой набор состояний автомата и свой набор тестов. Оба
//!    роута подключения работают с порталом, который выбрал [`Portal`], и оба
//!    больше не переписывают список порталов целиком (см. шапку [`login`]).
//!
//!    Формы путей `/portals` и `/portals/:pid` — обычная пара «коллекция и
//!    элемент»; `:pid` — это `id` из ответа `GET /portals`, он же значение
//!    заголовка `X-Forge-Portal`. Третьего способа назвать портал у API нет.
//!
//! ## Порядок проверок во всех data-роутах (Ф4+)
//!
//! Авторизация (409) → соединение (503) → резолв объекта (404) → разбор
//! параметров (400). Дословно как у Telegram и VK, и на это будет тест
//! `hostile_dialog_ids_hit_auth_before_any_parsing`: иной порядок позволил бы
//! неподключённому клиенту прощупывать валидность идентификаторов портала.
//!
//! Query разбирается ВРУЧНУЮ внутри хендлера, тело — через
//! `Option<Json<T>>`: экстракторы axum отработали бы ДО тела хендлера и
//! отдали 400 раньше проверки авторизации.
//!
//! ## Резолв портала — ступень НУЛЕВАЯ
//!
//! Состоянием роутера служит [`crate::registry::B24Registry`], а не один
//! портал: состав реестра меняется на живом процессе, и привязка `with_state`
//! к конкретному `Arc<B24State>` пережила бы отключение своего портала. Портал
//! выбирает КАЖДЫЙ запрос — экстрактором [`Portal`] (заголовок
//! `X-Forge-Portal` → `?portal=` → первый слот реестра).
//!
//! Он единственное исключение из правила «никаких экстракторов до лестницы», и
//! исключение по построению: лестница работает НАД состоянием портала, значит
//! портал обязан быть выбран раньше неё. Ценой исключения экстрактор не имеет
//! права отвечать 400 ни на каком вводе — иначе мусорный `X-Forge-Portal`
//! отбивался бы раньше 409, и неподключённый клиент получил бы оракул состава
//! реестра. Подробности и полный список решений — в шапке [`portal`].
//!
//! Суб-модули: [`portal`] — экстрактор портала, [`portals`] — CRUD состава
//! порталов (различаются одной буквой: первый реестр ЧИТАЕТ, второй его
//! МЕНЯЕТ), [`error`] — единый конверт ошибок, [`login`] — подключение и
//! отвязка портала (Ф3), [`dialogs`] — директории, список диалогов, история и
//! отправка (Ф4–Ф6), [`media`] — прокси вложений и аватаров (Ф8), [`upload`] —
//! приём файла и заливка его в портал (Ф9), [`analysis`] — анализ чата через
//! Claude. Группа `tasks` добавляется Ф10 тем же `.merge(...)`.
//!
//! ## Две лестницы у группы анализа
//!
//! Семь путей анализа стоят на ДВУХ разных лестницах, и это зафиксированное
//! решение, а не оплошность: `/prompt`, `GET|DELETE /analysis` и
//! `DELETE /analysis/thread` работают с ЛОКАЛЬНЫМ файлом и связи с порталом не
//! требуют (иначе вкладка прятала бы сделанный разбор ровно тогда, когда
//! портал отвалился), а `POST /analysis` и `/analysis/cancel` начинаются с
//! выгрузки истории и потому проходят полный `require_ready`. Подробности и
//! схема — в шапке [`analysis`].

pub mod analysis;
pub mod dialogs;
pub mod error;
pub mod login;
pub mod media;
pub mod portal;
pub mod portals;
pub mod tasks;
pub mod upload;
pub mod voice;

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

pub use portal::Portal;

use crate::registry::B24Registry;
use crate::state::LoginStateDto;

/// Ответ `GET /api/bitrix/status`.
///
/// `enabled` всегда `true`: роут существует только когда плагин включён
/// флагом `--bitrix`, поэтому сам факт ответа 200 уже означает «плагин
/// активен» (а 404 — «выключен», и именно так фронт гейтит кнопку вкладки).
/// Поле оставлено явным, чтобы фронт читал одну и ту же форму и не полагался
/// на HTTP-статус.
///
/// # Чего здесь нет
///
/// **Ни кода вебхука, ни его отпечатка, ни хоста портала.** Соблазн показать
/// «вы подключены к такому-то порталу» понятен, но `/api/bitrix/status`
/// уходит в браузер, в devtools и в HAR-файл поддержки, а хост сужает
/// перебор кода до одного портала. Отпечаток остаётся только в логах сервера
/// ([`crate::config::WebhookUrl::fingerprint`]).
///
/// Технически положить сюда вебхук невозможно и по типам: `WebhookUrl` не
/// реализует `Serialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDto {
    /// Плагин зарегистрирован в этом процессе.
    pub enabled: bool,
    /// Состояние подключения (см. [`crate::state::LoginFlow::kind`]).
    ///
    /// ЕДИНСТВЕННЫЙ источник экрана подключения по REST; ровно та же структура
    /// приезжает WS-кадром `login_state` и ответом обоих роутов
    /// [`crate::routes::login`], поэтому фронт применяет её одной функцией.
    pub login: LoginStateDto,
    /// Жива ли связь с порталом прямо сейчас
    /// ([`crate::state::ConnHealth::connected`]).
    ///
    /// Отдельно от `login`, потому что это РАЗНЫЕ вопросы: вебхук может быть
    /// принят порталом (`authorized`), а сеть — отвалиться минуту назад. Фронт
    /// по этому полю ставит метку связи в статусбаре, не трогая экран
    /// подключения.
    pub connected: bool,
    /// Потолок размера отправляемого файла, байты (Р7). Фронт показывает по
    /// нему подсказку и отсекает слишком большие файлы ДО заливки.
    pub max_upload_bytes: u64,
    /// Действующий лимит запросов в секунду — фронтовый планировщик
    /// настраивается по нему, а не по своей копии константы.
    pub rate_limit_rps: u32,
    /// Возможности, которых у платформы НЕТ (§6 плана).
    ///
    /// Отдаются с бэкенда, а не зашиты во фронт: разметку браузер держит в
    /// кэше PWA неделями, а список ограничений может измениться вместе с
    /// редакцией портала.
    pub capabilities: CapabilitiesDto,
    /// Подсказка «где взять вебхук» для экрана подключения.
    pub hint: String,
    /// Предупреждение о том, что именно делает выход.
    ///
    /// Отдельным полем, а не строкой в разметке: отозвать входящий вебхук
    /// через REST невозможно, и пользователь обязан узнать, что на портале
    /// вебхук остаётся действующим, — иначе «вышел» будет прочитано как
    /// «отозвал».
    pub logout_hint: String,
}

/// Что панель умеет, а что платформа не отдаёт.
///
/// Все поля — «умеем ли», а не «сломано ли»: UI по ним не показывает ошибок,
/// а честно прячет или блокирует элементы (входящее «печатает», realtime
/// присутствия, эмодзи-реакции).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesDto {
    /// Приходят ли ВХОДЯЩИЕ события «печатает». У портала — нет: отправка
    /// работает, входящих событий не существует.
    pub incoming_typing: bool,
    /// Есть ли realtime онлайн/оффлайн. У портала — нет: только
    /// фолбэк-синхронизация раз в `recent_sync_secs`.
    pub realtime_presence: bool,
    /// Приходят ли правки сообщений потоком событий. У портала — нет:
    /// `ONIMV2MESSAGEUPDATE` документирован, но живьём не приходит, поэтому
    /// правка видна с задержкой до `recent_sync_secs`.
    pub realtime_edits: bool,
    /// Есть ли эмодзи-реакции. У портала — только «лайк».
    pub reactions: bool,
    /// Отражает ли портал ответы (`reply_to`) при ЧТЕНИИ. Нет: `REPLY_ID`
    /// отправляется, но обратно не приходит — поэтому цитата дублируется
    /// текстом (Р10).
    pub reply_to_on_read: bool,
    /// Есть ли пересылка сообщений. У портала под вебхуком — нет.
    pub forwarding: bool,
    /// Есть ли задачи как отдельная сущность. Да — это то, чего нет у
    /// Telegram и VK.
    pub tasks: bool,
}

impl Default for CapabilitiesDto {
    fn default() -> Self {
        Self {
            incoming_typing: false,
            realtime_presence: false,
            realtime_edits: false,
            reactions: false,
            reply_to_on_read: false,
            forwarding: false,
            tasks: true,
        }
    }
}

/// Подсказка экрана подключения.
///
/// Живёт на бэкенде, а не в `index.html`: разметку браузер держит в кэше PWA
/// неделями, а формулировка про права вебхука меняется вместе с интерфейсом
/// портала. Последнее предложение — ограничение Р15, и оно обязано быть на
/// экране: вебхук выпускается от имени ОДНОГО сотрудника, и в remote-mode
/// все, кто открыл devforge, увидят переписку владельцев ВСЕХ подключённых
/// вебхуков — список порталов общий на инстанс, а не на зрителя.
///
/// Про саму мультипортальность текст молчит намеренно: подсказка отвечает на
/// вопрос «где взять вебхук», а строку вкладок пользователь видит глазами.
pub const WEBHOOK_HINT: &str = "В Битрикс24: Разработчикам → Другое → Входящий вебхук. \
Отметьте права «Чат и уведомления» (im) и «Задачи» (task), сохраните и вставьте сюда \
адрес вида https://ваш-портал.bitrix24.ru/rest/1/код/ — подойдёт и адрес вместе с именем \
метода, и без хвостового слэша. Вебхук выпускается от имени одного сотрудника: все, кто \
откроет этот devforge, увидят переписку владельцев подключённых вебхуков.";

/// Собирает Router плагина со ВСЕМ РЕЕСТРОМ внутри.
///
/// Возвращает `Router` без параметра state — его можно сразу мерджить в
/// приложение хоста (`app.merge(build_router(registry))`).
///
/// # Почему состоянием роутера стал реестр, а не портал
///
/// До мультипортальности сюда приезжал `Arc<B24State>`, то есть портал
/// выбирался В МОМЕНТ СБОРКИ. Роутер собирается один раз при старте, а состав
/// реестра меняется на живом процессе ([`crate::registry::B24Registry::add`] и
/// `remove`, Ф4) — привязка к `Arc` пережила бы отключение своего портала и
/// продолжила бы обслуживать запросы состоянием, которого уже нет в списке.
/// Поэтому портал выбирает КАЖДЫЙ запрос, экстрактором [`Portal`].
///
/// Единственное, что по-прежнему фиксируется при сборке, — лимит тела
/// загрузки: он берётся из ОБЩИХ настроек ([`B24Registry::shared`]), одинаков
/// у всех порталов по построению ([`crate::config::BitrixRootConfig::portal_configs`]
/// раздаёт их копией) и обязан быть известен слою `DefaultBodyLimit` до того,
/// как появится запрос.
pub fn build_router(registry: Arc<B24Registry>) -> Router {
    // Лимит тела загрузки берётся ИЗ КОНФИГА и навешивается точечно на
    // `/api/bitrix/upload` — глобальный `DefaultBodyLimit` приложения при этом
    // не трогается (см. [`upload::router`]).
    let max_upload_bytes = registry.shared().max_upload_bytes;
    Router::new()
        .route("/api/bitrix/status", get(get_status))
        .route("/ws/bitrix", get(crate::ws::bitrix_ws))
        .merge(login::router())
        .merge(portals::router())
        .merge(dialogs::router())
        .merge(analysis::router())
        .merge(tasks::router())
        .merge(media::router())
        .merge(voice::router())
        .merge(upload::router(max_upload_bytes))
        .with_state(registry)
}

/// `GET /api/bitrix/status` → 200 всегда.
///
/// Единственный роут, который отвечает 200 в любом состоянии: по нему
/// фронтенд решает, показывать ли кнопку вкладки вообще (404 ⇒ плагин
/// выключен, кнопка остаётся скрытой — и это НЕ ошибка).
pub async fn get_status(Portal(state): Portal) -> Json<StatusDto> {
    let now = chrono::Utc::now().timestamp();
    Json(StatusDto {
        enabled: true,
        login: state.login_state_dto().await,
        connected: state.connected(now).await,
        max_upload_bytes: state.config.max_upload_bytes,
        rate_limit_rps: state.config.rate_limit_rps,
        capabilities: CapabilitiesDto::default(),
        hint: WEBHOOK_HINT.to_string(),
        logout_hint: login::LOGOUT_HINT.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{BitrixConfig, WebhookUrl};
    use crate::state::{B24State, B24UserDto, LoginFlow};

    /// Роутер на ИЗОЛИРОВАННОМ конфиге.
    ///
    /// `BitrixConfig::default()` указывает на реальный `~/.forge/bitrix-cache`,
    /// а logout (Ф3) удаляет вебхук из `~/.config/forge/bitrix.toml` — тесты
    /// обязаны работать в tempdir, иначе прогон `cargo test` отключал бы
    /// пользователя от портала.
    fn app_with(f: impl FnOnce(&mut BitrixConfig)) -> (tempfile::TempDir, Arc<B24State>, Router) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        f(&mut c);
        let state = B24State::new(c);
        (
            dir,
            state.clone(),
            build_router(B24Registry::single(state)),
        )
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(app: Router, uri: &str) -> axum::response::Response {
        app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn status_is_200_without_a_webhook() {
        let (_d, _st, app) = app_with(|_| {});
        let resp = get(app, "/api/bitrix/status").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["enabled"], true);
        assert_eq!(v["login"]["state"], "not_configured");
        assert_eq!(v["rate_limit_rps"], 2);
        assert_eq!(v["capabilities"]["tasks"], true);
        assert_eq!(v["capabilities"]["incoming_typing"], false);
        assert!(v["hint"].as_str().unwrap().contains("вебхук"));
    }

    /// ИНВАРИАНТ ФАЗЫ: `/status` не содержит ни кода вебхука, ни хоста
    /// портала — НИ В ОДНОМ состоянии автомата.
    #[tokio::test]
    async fn status_never_leaks_the_webhook_in_any_state() {
        const PORTAL: &str = "test-work-2.bitrix24.ru";
        const CODE: &str = "wqdzbfhmn83d23j4";

        let states = [
            LoginFlow::NotConfigured,
            LoginFlow::Checking,
            LoginFlow::InvalidUrl {
                error: Some("bad url".into()),
            },
            LoginFlow::InvalidCredentials {
                error: Some("rejected".into()),
            },
            LoginFlow::ScopeMissing {
                error: Some("no im scope".into()),
            },
            LoginFlow::Throttled {
                retry_after_secs: Some(7),
            },
            LoginFlow::Disconnected {
                error: Some("connect timeout".into()),
            },
            LoginFlow::Authorized {
                user: B24UserDto {
                    id: "1".into(),
                    name: Some("Тест".into()),
                    ..Default::default()
                },
                portal: PORTAL.into(),
            },
        ];

        for flow in states {
            let kind = flow.kind();
            let (_d, state, app) = app_with(|c| {
                c.webhook = WebhookUrl::new(format!("https://{PORTAL}/rest/1/{CODE}/"));
            });
            state.set_login(flow).await;

            let resp = get(app, "/api/bitrix/status").await;
            assert_eq!(resp.status(), StatusCode::OK, "состояние {kind}");
            let text = serde_json::to_string(&body_json(resp).await).unwrap();
            assert!(!text.contains(CODE), "код вебхука в /status ({kind}): {text}");
            assert!(
                !text.contains(PORTAL),
                "хост портала в /status ({kind}): {text}"
            );
            assert!(
                !text.contains("fingerprint"),
                "отпечаток в /status ({kind}): {text}"
            );
        }
    }

    /// Гейт кнопки вкладки держится на том, что НЕЗАРЕГИСТРИРОВАННЫЙ плагин
    /// даёт 404, а не 200 с «выключено». Здесь проверяется зеркальная
    /// половина: смонтировано ровно то, что должно быть смонтировано на
    /// текущей фазе, — ни больше, ни меньше.
    #[tokio::test]
    async fn only_the_implemented_routes_are_mounted() {
        let (_d, _st, app) = app_with(|_| {});
        // Загрузка (Ф9) смонтирована, но ТОЛЬКО на POST: `GET` по ней обязан
        // давать 405, а не 404 — 404 означал бы «плагина нет» и погасил бы
        // кнопку вкладки.
        let resp = get(app.clone(), "/api/bitrix/upload").await;
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "/api/bitrix/upload смонтирован и принимает только POST"
        );
        // Данные (Ф4, Ф5) смонтированы и БЕЗ вебхука отвечают 409, а не 404:
        // 404 означает «плагина нет» и гасит кнопку вкладки, а здесь плагин
        // есть — просто портал не подключён (первая ступень лестницы). Роуты с
        // сегментом пути отвечают тем же 409: авторизация проверяется РАНЬШЕ
        // разбора `:id`.
        for uri in [
            "/api/bitrix/dialogs",
            "/api/bitrix/filters",
            "/api/bitrix/dialogs/chat7/messages",
            "/api/bitrix/dialogs/мусор/messages",
            "/api/bitrix/messages/113",
            // Задачи (Ф10) — на той же лестнице: авторизация РАНЬШЕ разбора
            // `:id`, поэтому и мусорный идентификатор даёт 409, а не 404.
            "/api/bitrix/tasks",
            "/api/bitrix/tasks/counters",
            "/api/bitrix/tasks/1",
            "/api/bitrix/tasks/мусор",
        ] {
            let resp = get(app.clone(), uri).await;
            assert_eq!(
                resp.status(),
                StatusCode::CONFLICT,
                "{uri} обязан отвечать 409 not_authorized"
            );
        }
        // А роуты подключения (Ф3) и действий (Ф6) — уже есть: GET на них даёт
        // 405 «метод не тот», а не 404 «пути нет». Разница принципиальна: фронт
        // гейтит кнопку вкладки именно по 404.
        for uri in [
            "/api/bitrix/login/webhook",
            "/api/bitrix/logout",
            "/api/bitrix/dialogs/chat7/read",
            "/api/bitrix/dialogs/chat7/unread",
            "/api/bitrix/dialogs/chat7/typing",
            "/api/bitrix/dialogs/chat7/pin",
            "/api/bitrix/dialogs/chat7/hide",
            "/api/bitrix/messages/113/like",
            "/api/bitrix/tasks/1/start",
        ] {
            let resp = get(app.clone(), uri).await;
            assert_eq!(
                resp.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{uri} обязан быть смонтирован как POST"
            );
        }
    }

    /// `/api/bitrix/tasks/counters` — СТАТИЧЕСКИЙ сегмент рядом с `/:id`.
    ///
    /// У `matchit` статический сегмент приоритетнее параметра, поэтому
    /// коллизии нет. Полагаться на это молча нельзя: перепутанный приоритет
    /// увёл бы счётчики в разбор идентификатора задачи, а тот ответил бы
    /// `404 unknown_task` — то есть бейдж просто перестал бы обновляться, без
    /// единой ошибки.
    #[tokio::test]
    async fn the_counters_route_is_not_shadowed_by_the_id_route() {
        let (_d, state, app) = app_with(|c| {
            c.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
            // Порт 1 закрыт всегда: до сети запрос дойти не должен, а если
            // дойдёт — упрётся в таймаут и ответит 503, а не 404.
            c.base_url_override = "http://127.0.0.1:1/rest/1/abc123def456".into();
            c.connect_timeout_secs = 1;
            c.request_timeout_secs = 1;
        });
        state
            .set_login(LoginFlow::Authorized {
                user: B24UserDto {
                    id: "1".into(),
                    ..Default::default()
                },
                portal: "p.bitrix24.ru".into(),
            })
            .await;
        state.note_ok(chrono::Utc::now().timestamp()).await;

        // Буквенный сегмент, который НЕ является счётчиками, — это
        // `404 unknown_task`. Проверяется ПЕРВЫМ: следующий вызов упрётся в
        // закрытый порт и погасит здоровье связи, после чего лестница будет
        // отвечать `503` раньше разбора `:id`.
        let resp = get(app.clone(), "/api/bitrix/tasks/abc").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], "unknown_task");

        let resp = get(app, "/api/bitrix/tasks/counters").await;
        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "счётчики уехали в разбор `:id` и получили unknown_task"
        );
    }

    /// `/status` отдаёт ЖИВОЕ состояние автомата и здоровье транспорта — это
    /// разные вопросы, и путать их нельзя: вебхук может быть принят порталом,
    /// а сеть отвалиться минуту назад.
    #[tokio::test]
    async fn status_reports_the_current_login_state_and_the_connection_health() {
        let (_d, state, app) = app_with(|c| {
            c.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
        });

        let v = body_json(get(app.clone(), "/api/bitrix/status").await).await;
        assert_eq!(v["login"]["state"], "disconnected");
        assert_eq!(v["connected"], true, "по умолчанию связь считается живой");

        // Переход автомата виден в СЛЕДУЮЩЕМ же ответе: снапшот не кэшируется.
        state
            .set_login(LoginFlow::Authorized {
                user: B24UserDto {
                    id: "1".into(),
                    ..Default::default()
                },
                portal: "p.bitrix24.ru".into(),
            })
            .await;
        let v = body_json(get(app.clone(), "/api/bitrix/status").await).await;
        assert_eq!(v["login"]["state"], "authorized");
        assert_eq!(v["login"]["user"]["id"], "1");

        // Транспортный отказ гасит `connected`, НЕ трогая экран подключения.
        state
            .note_down(chrono::Utc::now().timestamp(), "connect timeout")
            .await;
        let v = body_json(get(app, "/api/bitrix/status").await).await;
        assert_eq!(v["connected"], false);
        assert_eq!(
            v["login"]["state"], "authorized",
            "обрыв сети не должен разлогинивать"
        );
    }

    #[tokio::test]
    async fn max_upload_bytes_is_reported_from_the_config() {
        let (_d, _st, app) = app_with(|c| c.max_upload_bytes = 1234);
        let v = body_json(get(app, "/api/bitrix/status").await).await;
        assert_eq!(v["max_upload_bytes"], 1234);
    }
}
