# TelegramState

Разделяемое состояние плагина Telegram (plugins/telegram/src/state.rs).

ЧТО ЭТО: создаётся один раз в forge_telegram::init, оборачивается в Arc и раздаётся axum-хендлерам
через with_state, WS-соединениям и фоновым воркерам. Внутри только примитивы tokio, поэтому
Send+Sync и переживает любое число клонов Arc.

ПОЛЯ И ПОЧЕМУ ИМЕННО ТАК:
- config: TelegramConfig — конфиг, с которым плагин стартовал.
- broadcast: broadcast::Sender<TgServerEvent> ёмкостью 1024 (BROADCAST_CAPACITY) — единственный
  путь событий к WS-клиентам. Фильтрации по активному диалогу НЕТ намеренно: unread-бейдж и тосты
  должны работать с любой вкладки, поэтому сервер вещает всё, а решает фронт.
- login: Mutex<LoginFlow> — переходы автомата строго сериализованы.
- runtime: RwLock<Option<TgRuntime>> — читается на каждом API-вызове, пишется при подключении.
- uploads: Mutex<HashMap<String, UploadEntry>> — реестр незавершённых загрузок, TTL 30 мин.
- peers: RwLock<HashMap<i64, PeerRef>> — кэш резолва peer'ов.
- media_inflight: Mutex<HashMap<String, Arc<Notify>>> — дедуп одновременных скачиваний медиа.
- workers: Mutex<Vec<JoinHandle<()>>> + shutdown: CancellationToken — graceful shutdown.
- authorized_notify: Notify — будит воркеров при переходе в Authorized.

НОВОЕ (D1 — «уведомления от ненужных чатов»): ТРИ КЭША, делающие СЕРВЕР источником истины по
вопросу «звенит ли диалог». Раньше это решал фронт по неполной странице /dialogs — чат вне
загруженной сотни был ему неотличим от незаглушённого.
- notify_defaults: RwLock<NotifyDefaults> — глобальные дефолты трёх категорий Telegram;
  загружаются updates::refresh_notify_defaults ДО старта потока апдейтов и на каждом реконнекте.
- notify_overrides: RwLock<HashMap<i64, bool>> — уже РАЗРЕШЁННЫЙ (с наследованием) muted по
  диалогам, ключ — Bot API-id.
- archived_ids: RwLock<HashSet<i64>> — какие диалоги лежат в архиве.

ИНВАРИАНТ КЭШЕЙ: они ДОПОЛНЯЮТСЯ, а не пересеваются целиком. Списки диалогов приходят страницами
(основной список, архив, папки), и полная замена стирала бы знание о чатах из соседних страниц —
то есть ВОЗВРАЩАЛА бы исходный дефект.

МЕТОДЫ КЭШЕЙ:
- notify_defaults() -> NotifyDefaults;
- set_notify_defaults(d) -> bool — true, если значение ДЕЙСТВИТЕЛЬНО изменилось; по этому флагу
  решается, слать ли кадр notify_defaults_changed (иначе каждый реконнект рассылал бы его зря);
- set_dialog_muted(id, muted) / is_dialog_muted(id) -> Option<bool> — None это «НЕ ЗНАЕМ», а не
  «не заглушён»; ровно на этой разнице держится подстановка дефолта категории;
- set_archived(id, archived) / is_archived(id) -> bool — промах кэша даёт false («показать»
  безопаснее, чем «спрятать»);
- remember_dialog_flags(id, muted, archived);
- remember_dialogs(&[DialogDto]) — засев кэшей свежим листингом, записи дополняются, битые id
  пропускаются. Это то, что ОБЯЗАН звать GET /api/telegram/dialogs после сборки DTO.

ОСТАЛЬНЫЕ МЕТОДЫ: new(config) -> Arc<Self>; broadcast_event(ev) (SendError глотается — отсутствие
подписчиков это норма); subscribe(); login_state_dto(); is_authorized(); set_login(next) — замена
состояния и broadcast делаются под ОДНИМ lock'ом, поэтому клиенты не увидят события в порядке,
обратном реальным переходам; register_worker(handle); shutdown_workers() (идемпотентен).
