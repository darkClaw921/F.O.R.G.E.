//! Общее состояние плагина встреч.
//!
//! | поле | зачем |
//! |---|---|
//! | [`MeetState::config`] | таймауты, пороги VAD и лимиты — данные, а не константы |
//! | [`MeetState::broadcast`] | поток событий всем WS-подписчикам, **без фильтрации** |
//! | `db` | SQLite; открывается в `spawn_workers`, а не в `init` |
//! | `badges` | **тёплый индекс** состояний для ленты |
//! | [`MeetState::deps`] | результат проб внешних бинарей (Ф5 наполняет) |
//! | [`MeetState::hub`] | слоты записи, whisper и саммари |
//! | `workers` + [`MeetState::shutdown`] | graceful shutdown с грейсом на дофлаш WAV |
//! | `ws_heartbeat`, `ws_idle_timeout` | тайминги WS — данные, иначе тест idle-таймаута спал бы минуту |
//!
//! # Почему база открывается не в `init`
//!
//! `init` синхронен: он вызывается на пути старта devforge и обязан
//! возвращаться быстрее 500 мс, ничего не запуская и не трогая файловую
//! систему записей. Открытие SQLite — это `create_dir_all`, файл на диске и
//! рабочий поток соединения, то есть ровно то, чего `init` делать не должен.
//! Поэтому база живёт в [`tokio::sync::OnceCell`] и появляется первым делом в
//! `spawn_workers`, вместе с миграциями, прогревом индекса и
//! `reconcile_on_boot`.
//!
//! Пока её нет, роуты отвечают `503 db_unavailable` — кроме
//! `GET /api/meet/status` и `GET /api/meet/badges`, которые базы не касаются
//! вовсе.
//!
//! # Тёплый индекс бейджей
//!
//! `RwLock<HashMap<SourceKey, MeetingBadge>>`. Батч ленты дёргается на КАЖДОМ
//! рендере, и ходить за этим в SQLite значит платить запросом за каждый
//! скролл. 10 000 встреч — около 800 КиБ, дешевле одного кадра видео.
//!
//! База остаётся источником истины: индекс прогревается из неё на старте
//! ([`MeetState::warm_badge_index`]) и обновляется ТОЛЬКО через
//! [`MeetState::publish_badge`], которую зовёт единственная точка мутации
//! статуса `repo::meetings::set_status`. Второй путь записи в индекс — это и
//! есть тот самый разъезд с базой, который потом ищут сутки.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex, OnceCell, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::MeetConfig;
use crate::db::repo::{MeetingBadge, MeetingRow, SourceKey};
use crate::db::Db;
use crate::deps::DepsReport;
use crate::error::MeetError;
use crate::hub::MeetHub;
use crate::ws::protocol::{Limits, MeetServerMsg};

/// Ёмкость broadcast-канала. Столько же, сколько у остальных плагинов.
///
/// При переполнении подписчик получает `Lagged`, а мы шлём ему `Resync` —
/// разрывать соединение при отставании нельзя, реконнект дороже
/// перечитывания.
pub const BROADCAST_CAPACITY: usize = 1024;

/// Состояние плагина.
pub struct MeetState {
    pub config: MeetConfig,
    pub broadcast: broadcast::Sender<MeetServerMsg>,
    db: OnceCell<Arc<Db>>,
    badges: RwLock<HashMap<SourceKey, MeetingBadge>>,
    pub deps: RwLock<DepsReport>,
    pub hub: Arc<MeetHub>,
    /// Идущие сеансы записи. Через них `stop`, `cancel`, `show-window`,
    /// `screenshot` и подтверждение человека добираются до живого бота.
    pub sessions: crate::recording::SessionRegistry,
    /// Источник ботов. До Ф4 здесь [`crate::recording::MissingBotFactory`],
    /// который честно отвечает `409 chrome_unavailable`; тесты ставят
    /// сценарную фабрику и проверяют весь пайплайн без браузера.
    ///
    /// `RwLock`, а не `OnceCell`: подмена фабрики в тесте должна быть
    /// возможна, а в бою она происходит ровно один раз на старте.
    bot_factory: std::sync::RwLock<Arc<dyn crate::recording::BotFactory>>,
    /// Откуда берутся байты моделей whisper.
    ///
    /// `RwLock` по той же причине, что и у фабрики ботов: в бою источник один
    /// (HTTP), а тест обязан уметь подставить свой — качать полтора гигабайта
    /// на каждый прогон нельзя, и без сети тест падать не должен.
    model_source: std::sync::RwLock<Arc<dyn crate::models::ModelSource>>,
    /// Модели, которые качаются прямо сейчас.
    ///
    /// Нужно, чтобы второе нажатие «скачать» не запускало вторую загрузку в
    /// тот же файл: два писателя в один `.part` дали бы перемешанные байты,
    /// прошедшие проверку размера и упавшие на контрольной сумме — то есть
    /// час загрузки впустую.
    downloading: RwLock<std::collections::HashSet<String>>,
    /// Идущие пост-обработки: `meeting_id` → чем их гасить.
    ///
    /// Отмена встречи обязана убирать задание И ИЗ ОЧЕРЕДИ whisper тоже, а
    /// сеанса записи к этому моменту уже нет — он закончился вместе с
    /// браузером.
    jobs: RwLock<HashMap<String, CancellationToken>>,
    pub workers: Mutex<Vec<JoinHandle<()>>>,
    pub shutdown: CancellationToken,
    /// Период heartbeat-пинга WS.
    pub ws_heartbeat: Duration,
    /// Сколько тишины считается мёртвым соединением.
    pub ws_idle_timeout: Duration,
}

impl std::fmt::Debug for MeetState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeetState")
            .field("cache_dir", &self.config.cache_dir)
            .field("db_path", &self.config.db_path)
            .field("db_open", &self.db.initialized())
            .field("subscribers", &self.broadcast.receiver_count())
            .finish()
    }
}

impl MeetState {
    /// Боевой конструктор — с продовыми таймингами WS.
    pub fn new(config: MeetConfig) -> Arc<Self> {
        Self::with_ws_timings(config, crate::ws::HEARTBEAT_INTERVAL, crate::ws::IDLE_TIMEOUT)
    }

    /// Конструктор для тестов цикла WS: тайминги в миллисекундах.
    ///
    /// Существует затем, чтобы тест idle-таймаута проверял НАСТОЯЩИЙ цикл, а
    /// не его копию, и при этом не спал минуту.
    pub fn with_ws_timings(
        config: MeetConfig,
        ws_heartbeat: Duration,
        ws_idle_timeout: Duration,
    ) -> Arc<Self> {
        let (broadcast, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let hub = MeetHub::new(&config);
        Arc::new(Self {
            config,
            broadcast,
            db: OnceCell::new(),
            badges: RwLock::new(HashMap::new()),
            deps: RwLock::new(DepsReport::default()),
            hub,
            sessions: crate::recording::SessionRegistry::default(),
            bot_factory: std::sync::RwLock::new(Arc::new(crate::recording::MissingBotFactory)),
            model_source: std::sync::RwLock::new(Arc::new(crate::models::HttpModelSource)),
            downloading: RwLock::new(std::collections::HashSet::new()),
            jobs: RwLock::new(HashMap::new()),
            workers: Mutex::new(Vec::new()),
            shutdown: CancellationToken::new(),
            ws_heartbeat,
            ws_idle_timeout,
        })
    }

    // --- база ---------------------------------------------------------------

    /// Привязывает открытую базу. Повторный вызов — ошибка: два соединения на
    /// один файл означали бы два независимых WAL-писателя.
    pub fn attach_db(&self, db: Arc<Db>) -> Result<(), MeetError> {
        self.db
            .set(db)
            .map_err(|_| MeetError::Internal("база уже привязана".to_string()))
    }

    /// Открытая база либо `None`.
    pub fn db(&self) -> Option<Arc<Db>> {
        self.db.get().cloned()
    }

    /// Открытая база либо [`MeetError::DbUnavailable`].
    pub fn require_db(&self) -> Result<Arc<Db>, MeetError> {
        self.db.get().cloned().ok_or(MeetError::DbUnavailable)
    }

    // --- broadcast ----------------------------------------------------------

    /// Вещает кадр всем подписчикам. Отсутствие подписчиков — норма (ни одной
    /// открытой вкладки), поэтому ошибка отправки игнорируется.
    pub fn broadcast_event(&self, msg: MeetServerMsg) {
        let _ = self.broadcast.send(msg);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MeetServerMsg> {
        self.broadcast.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.broadcast.receiver_count()
    }

    // --- тёплый индекс бейджей ---------------------------------------------

    /// Кладёт бейдж встречи под все её ключи-источники.
    ///
    /// **Единственный писатель индекса.** Зовётся из
    /// `repo::meetings::set_status` и из привязки нового источника; любой
    /// другой вызывающий немедленно создал бы второй путь мутации, а с ним и
    /// возможность разъезда с базой.
    pub async fn publish_badge(&self, row: &MeetingRow, keys: &[SourceKey]) {
        if keys.is_empty() {
            return;
        }
        let badge = row.to_badge();
        let mut index = self.badges.write().await;
        for key in keys {
            index.insert(key.clone(), badge.clone());
        }
    }

    /// Бейдж одного сообщения. `None` означает «встречи нет» — то есть
    /// «можно записать».
    pub async fn badge_of(&self, key: &SourceKey) -> Option<MeetingBadge> {
        self.badges.read().await.get(key).cloned()
    }

    /// Батч по диапазону идентификаторов сообщений — **из памяти, без единого
    /// запроса к SQLite**.
    ///
    /// В ответе только те сообщения, у которых встреча есть: отсутствие бейджа
    /// и означает «можно записать». Статуса `detected` в базе нет, иначе
    /// таблица росла бы от простого скролла ленты.
    pub async fn badges_in_range(
        &self,
        plugin: &str,
        chat_id: &str,
        min_id: i64,
        max_id: i64,
    ) -> Vec<(SourceKey, MeetingBadge)> {
        let index = self.badges.read().await;
        let mut out: Vec<(SourceKey, MeetingBadge)> = index
            .iter()
            .filter(|(k, _)| {
                k.plugin == plugin
                    && k.chat_id == chat_id
                    && k.message_id >= min_id
                    && k.message_id <= max_id
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by_key(|(k, _)| k.message_id);
        out
    }

    /// Сколько записей в индексе.
    pub async fn badge_count(&self) -> usize {
        self.badges.read().await.len()
    }

    /// Прогревает индекс из базы. Вызывается в `spawn_workers` после миграций.
    ///
    /// База — источник истины, индекс — кэш: полная перезапись, а не слияние.
    pub async fn warm_badge_index(&self) -> Result<usize, MeetError> {
        let rows = crate::db::repo::meetings::all_with_sources(self).await?;
        let mut index = HashMap::new();
        for (row, keys) in &rows {
            let badge = row.to_badge();
            for key in keys {
                index.insert(key.clone(), badge.clone());
            }
        }
        let n = index.len();
        *self.badges.write().await = index;
        tracing::info!(
            target: "forge_meet",
            meetings = rows.len(),
            badges = n,
            "forge-meet: тёплый индекс бейджей прогрет"
        );
        Ok(n)
    }

    // --- бот ----------------------------------------------------------------

    /// Текущий источник ботов.
    pub fn bot_factory(&self) -> Arc<dyn crate::recording::BotFactory> {
        self.bot_factory
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Ставит источник ботов. Ф4 зовёт это один раз при старте плагина;
    /// тесты — чтобы подставить сценарного бота вместо Chrome.
    pub fn set_bot_factory(&self, factory: Arc<dyn crate::recording::BotFactory>) {
        *self.bot_factory.write().unwrap_or_else(|e| e.into_inner()) = factory;
    }

    // --- модели ---------------------------------------------------------------

    /// Текущий источник байтов моделей.
    pub fn model_source(&self) -> Arc<dyn crate::models::ModelSource> {
        self.model_source
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Ставит источник моделей. Тесты подставляют свой вместо HTTP.
    pub fn set_model_source(&self, source: Arc<dyn crate::models::ModelSource>) {
        *self.model_source.write().unwrap_or_else(|e| e.into_inner()) = source;
    }

    /// Отмечает модель качающейся. `false` — она уже качается.
    pub async fn begin_download(&self, name: &str) -> bool {
        self.downloading.write().await.insert(name.to_string())
    }

    pub async fn end_download(&self, name: &str) {
        self.downloading.write().await.remove(name);
    }

    pub async fn is_downloading(&self, name: &str) -> bool {
        self.downloading.read().await.contains(name)
    }

    // --- пост-обработка ---------------------------------------------------------

    /// Регистрирует идущую пост-обработку и отдаёт её токен отмены.
    ///
    /// Токен — ребёнок общего `shutdown`: остановка devforge гасит пайплайн
    /// вместе со всем остальным, и отдельного пути для этого заводить не надо.
    pub async fn register_job(&self, meeting_id: &str) -> CancellationToken {
        let token = self.shutdown.child_token();
        self.jobs
            .write()
            .await
            .insert(meeting_id.to_string(), token.clone());
        token
    }

    /// Снимает задание с учёта.
    pub async fn finish_job(&self, meeting_id: &str) {
        self.jobs.write().await.remove(meeting_id);
    }

    /// Гасит пост-обработку встречи. `true` — было что гасить.
    pub async fn cancel_job(&self, meeting_id: &str) -> bool {
        match self.jobs.write().await.remove(meeting_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Идёт ли пост-обработка встречи прямо сейчас.
    pub async fn has_job(&self, meeting_id: &str) -> bool {
        self.jobs.read().await.contains_key(meeting_id)
    }

    // --- прочее -------------------------------------------------------------

    pub fn limits(&self) -> Limits {
        self.hub.limits()
    }

    /// Текущий отчёт о зависимостях.
    pub async fn deps_snapshot(&self) -> DepsReport {
        self.deps.read().await.clone()
    }

    /// Регистрирует фонового воркера для последующего graceful shutdown.
    pub async fn register_worker(&self, handle: JoinHandle<()>) {
        self.workers.lock().await.push(handle);
    }

    /// Даёт каждому воркеру две секунды выйти самому и только потом абортит.
    ///
    /// Пауза не косметическая: писатель WAV на выходе дофлашивает хвост
    /// буфера и закрывает заголовок файла, а `abort()` в произвольной
    /// await-точке этого сделать не даст — на диске остался бы WAV с
    /// неправильной длиной в заголовке.
    pub async fn shutdown_workers(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *self.workers.lock().await);
        for h in handles {
            match tokio::time::timeout(Duration::from_secs(2), h).await {
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        target: "forge_meet",
                        "forge-meet: воркер не вышел за 2 с, снимаю"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Platform;
    use crate::status::{MeetStatus, SummaryStatus};

    pub(crate) fn test_config(dir: &std::path::Path) -> MeetConfig {
        MeetConfig {
            config_path: dir.join("meet.toml"),
            cache_dir: dir.join("cache"),
            db_path: dir.join("cache/meet.db"),
            models_dir: dir.join("cache/models"),
            claude_cli_path: dir.join("no-such-claude"),
            ..MeetConfig::default()
        }
    }

    fn row(id: &str, status: MeetStatus) -> MeetingRow {
        MeetingRow {
            id: id.to_string(),
            url_key: format!("telemost:host/{id}"),
            canonical_url: format!("https://telemost.yandex.ru/j/{id}"),
            platform: Platform::Telemost,
            room_id: id.to_string(),
            title: None,
            status,
            status_reason: None,
            summary_status: SummaryStatus::None,
            mode: "normal".into(),
            bot_name: None,
            created_at: 1,
            updated_at: 1,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            meeting_dir: "/tmp".into(),
            audio_path: None,
            transcript_path: None,
            summary_path: None,
            track_mode: None,
            capture_path: None,
        }
    }

    #[tokio::test]
    async fn the_range_batch_returns_only_messages_that_have_a_meeting() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));

        state
            .publish_badge(
                &row("m1", MeetStatus::Recording),
                &[SourceKey::new("telegram", "7", 100)],
            )
            .await;
        state
            .publish_badge(
                &row("m2", MeetStatus::Ready),
                &[SourceKey::new("telegram", "7", 150)],
            )
            .await;
        // Чужой чат и чужой плагин в диапазон попасть не должны.
        state
            .publish_badge(
                &row("m3", MeetStatus::Ready),
                &[SourceKey::new("telegram", "8", 120)],
            )
            .await;
        state
            .publish_badge(
                &row("m4", MeetStatus::Ready),
                &[SourceKey::new("vk", "7", 120)],
            )
            .await;

        let got = state.badges_in_range("telegram", "7", 90, 160).await;
        assert_eq!(got.len(), 2, "пустышек в ответе быть не должно: {got:?}");
        assert_eq!(got[0].0.message_id, 100, "ответ обязан идти по возрастанию id");
        assert_eq!(got[1].0.message_id, 150);

        // Сообщение без встречи просто отсутствует — это и есть «можно
        // записать».
        assert!(state.badge_of(&SourceKey::new("telegram", "7", 101)).await.is_none());

        // Границы диапазона включительные.
        assert_eq!(state.badges_in_range("telegram", "7", 100, 100).await.len(), 1);
        assert_eq!(state.badges_in_range("telegram", "7", 101, 149).await.len(), 0);
    }

    /// Одна встреча на нескольких сообщениях: ссылку кидают в двух чатах, и
    /// обе кнопки обязаны показывать ОДНУ запись.
    #[tokio::test]
    async fn one_meeting_lights_up_every_message_that_carries_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));
        let keys = [
            SourceKey::new("telegram", "7", 100),
            SourceKey::new("vk", "42", 900),
        ];
        state.publish_badge(&row("m1", MeetStatus::Recording), &keys).await;

        for key in &keys {
            let badge = state.badge_of(key).await.expect("бейдж обязан быть");
            assert_eq!(badge.meeting_id, "m1");
            assert_eq!(badge.status, MeetStatus::Recording);
        }

        // И обе обновляются одним переходом.
        state.publish_badge(&row("m1", MeetStatus::Ready), &keys).await;
        for key in &keys {
            assert_eq!(state.badge_of(key).await.unwrap().status, MeetStatus::Ready);
        }
    }

    /// Оценка памяти из плана — 10 000 встреч примерно 800 КиБ — подтверждается
    /// измерением, а не рассуждением.
    #[tokio::test]
    async fn ten_thousand_badges_fit_the_memory_budget() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));
        for i in 0..10_000i64 {
            state
                .publish_badge(
                    &row(&format!("m{i}"), MeetStatus::Ready),
                    &[SourceKey::new("telegram", "7", i)],
                )
                .await;
        }
        assert_eq!(state.badge_count().await, 10_000);

        // Считаем фактический вес полезной нагрузки: сами структуры плюс
        // строки, на которые они ссылаются.
        let index = state.badges.read().await;
        let mut bytes = 0usize;
        for (k, v) in index.iter() {
            bytes += std::mem::size_of::<SourceKey>() + k.plugin.len() + k.chat_id.len();
            bytes += std::mem::size_of::<MeetingBadge>() + v.meeting_id.len();
        }
        assert!(
            bytes < 4 * 1024 * 1024,
            "тёплый индекс на 10 000 встреч весит {} КиБ — это уже не кэш",
            bytes / 1024
        );
    }

    #[tokio::test]
    async fn without_spawn_workers_there_is_no_database() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));
        assert!(state.db().is_none());
        assert_eq!(state.require_db().unwrap_err(), MeetError::DbUnavailable);
    }

    #[tokio::test]
    async fn attaching_the_database_twice_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));
        let db = Arc::new(Db::open_memory().await.unwrap());
        state.attach_db(db.clone()).unwrap();
        assert!(state.attach_db(db).is_err(), "два соединения на один файл");
    }

    #[tokio::test]
    async fn broadcast_survives_having_no_subscribers() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));
        assert_eq!(state.subscriber_count(), 0);
        // Ни одной открытой вкладки — это норма, а не ошибка.
        state.broadcast_event(MeetServerMsg::Resync);

        let mut rx = state.subscribe();
        state.broadcast_event(MeetServerMsg::Ping);
        assert_eq!(rx.recv().await.unwrap(), MeetServerMsg::Ping);
    }

    #[tokio::test]
    async fn workers_get_two_seconds_before_being_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(test_config(dir.path()));
        let token = state.shutdown.clone();
        state
            .register_worker(tokio::spawn(async move { token.cancelled().await }))
            .await;
        assert_eq!(state.workers.lock().await.len(), 1);

        state.shutdown.cancel();
        state.shutdown_workers().await;
        assert!(state.workers.lock().await.is_empty());
    }
}
