//! Глобальное состояние Echo плагина.
//!
//! Phase 3 — добавлен `ClaudeRunner` (фасад над Claude CLI) и broadcast
//! рассылает реальные `ServerMsg` (см. [`ws::protocol::ServerMsg`]).
//! `Db` уже подключён с Phase 2. `HostApi` — slot, заполняется в `register_routes`.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use echo_host_api::HostApi;

use crate::actions::Action;
use crate::claude::ClaudeRunner;
use crate::config::EchoConfig;
use crate::db::Db;
use crate::ws::protocol::ServerMsg;

/// TTL для записей `action_registry` (Phase 5b). 30 минут — пользователь
/// успеет нажать кнопку, но мы не держим устаревшие mapping'и вечно.
pub const ACTION_REGISTRY_TTL_SECS: i64 = 30 * 60;

/// Запись registry: actions + timestamp создания (для TTL eviction).
#[derive(Debug, Clone)]
pub struct ActionRegistryEntry {
    pub actions: Vec<Action>,
    pub created_at: i64,
}

/// События, рассылаемые Echo через broadcast WS-подписчикам.
///
/// Phase 3 — тонкая обёртка над `ServerMsg`, обогащённая `conversation_id`
/// для per-conversation фильтрации в WS-loop'е. WS-handler разворачивает
/// `ServerEvent` в `ServerMsg` если `conversation_id` совпадает с query
/// клиента (либо `None` — broadcast всем).
#[derive(Debug, Clone)]
pub struct ServerEvent {
    /// Conversation id, для которого предназначено событие. `None` →
    /// broadcast всем подключённым клиентам.
    pub conversation_id: Option<String>,
    /// Полезная нагрузка.
    pub msg: ServerMsg,
}

impl ServerEvent {
    /// Утилита для broadcast-всем.
    #[allow(dead_code)]
    pub fn broadcast(msg: ServerMsg) -> Self {
        Self {
            conversation_id: None,
            msg,
        }
    }
    /// Утилита для адресного события одной conversation.
    pub fn to_conversation(conversation_id: impl Into<String>, msg: ServerMsg) -> Self {
        Self {
            conversation_id: Some(conversation_id.into()),
            msg,
        }
    }
}

/// Конфигурация Echo плагина (stub для Phase 1-2-3).
///
/// Phase 6 заменит эту структуру на полную `EchoConfig` с полями
/// `cli_path`, `max_parallel_runs`, `default_model`, `capture_lines`,
/// `autonomous_max_tokens_per_day`. Сейчас поддерживается:
/// - `db_path` (override для тестов),
/// - `cli_path` (override бинаря Claude CLI — Phase 3),
/// - `max_parallel_runs` (Phase 3, дефолт 4).
#[derive(Debug, Clone, Default)]
pub struct EchoConfigStub {
    /// Если `Some` — открыть БД по этому пути вместо дефолтного
    /// `~/.config/forge/echo.db`.
    pub db_path: Option<std::path::PathBuf>,
    /// Если `Some` — использовать этот путь к Claude CLI. По умолчанию
    /// `~/.local/bin/claude` (см. [`crate::default_cli_path`]).
    pub cli_path: Option<std::path::PathBuf>,
    /// Сколько одновременных Claude-run'ов разрешено. `None` → 4.
    pub max_parallel_runs: Option<usize>,
}

/// Состояние плагина, передаваемое в axum-handler'ы через `with_state`.
///
/// Cheap-clonable: внутри только `Arc` и `broadcast::Sender`.
#[derive(Clone)]
pub struct EchoState {
    /// Host adapter — устанавливается в [`crate::register_routes`].
    /// Используется во всех routes для доступа к tmux/projects/auth.
    pub host: Arc<tokio::sync::OnceCell<Arc<dyn HostApi>>>,
    /// Broadcast-канал для WS-подписчиков. Buffer = 256 — достаточно для
    /// streaming chunks одного ассистент-ответа без drop'ов медленного клиента.
    pub broadcast: broadcast::Sender<ServerEvent>,
    /// SQLite-хранилище плагина. Открывается и мигрируется в
    /// [`crate::init`] до возврата `Arc<EchoState>`.
    pub db: Arc<Db>,
    /// Фасад над Claude CLI. Phase 3.
    pub runner: Arc<ClaudeRunner>,
    /// JoinHandle'ы фоновых worker'ов (scheduler etc), которые нужно
    /// abort'нуть при graceful shutdown. Phase 4 кладёт сюда scheduler;
    /// Phase 6 добавил memory-rollover loop и т.д.
    pub workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// Phase 5b — реестр actions, привязанных к `message_id`. ws-handler
    /// заполняет после `assistant_done`; `ClientMsg::ActionInvoke` находит
    /// здесь Action по id. TTL [`ACTION_REGISTRY_TTL_SECS`] чистится
    /// лениво при каждом lookup.
    pub action_registry: Arc<Mutex<HashMap<String, ActionRegistryEntry>>>,
    /// Phase 6 — полная конфигурация плагина (cli/db paths, лимиты,
    /// default-model, autonomous cap, rate-limit). Доступна handler'ам
    /// и worker'ам через `state.config`.
    pub config: Arc<EchoConfig>,
    /// Phase 6 — единый cancellation-токен для graceful shutdown. Все
    /// долгоживущие задачи (scheduler, memory loop, WS reader-loop)
    /// должны слушать `state.shutdown.cancelled()` и завершаться.
    pub shutdown: CancellationToken,
}

impl EchoState {
    /// Создаёт state с дефолтной конфигурацией. Сохраняет совместимость
    /// со всеми существующими unit-тестами (вызовы `EchoState::new`).
    pub fn new(db: Arc<Db>, runner: Arc<ClaudeRunner>) -> Self {
        Self::new_with_config(db, runner, EchoConfig::default())
    }

    /// Создаёт state с явной конфигурацией. Используется в production-init
    /// через [`crate::init_with_config`].
    pub fn new_with_config(
        db: Arc<Db>,
        runner: Arc<ClaudeRunner>,
        config: EchoConfig,
    ) -> Self {
        let (broadcast, _) = broadcast::channel(256);
        Self {
            host: Arc::new(tokio::sync::OnceCell::new()),
            broadcast,
            db,
            runner,
            workers: Arc::new(Mutex::new(Vec::new())),
            action_registry: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            shutdown: CancellationToken::new(),
        }
    }

    /// Записывает actions для `message_id` и попутно очищает протухшие
    /// записи. Возвращает `Vec<crate::ws::protocol::ActionDescriptor>` —
    /// уже сериализованный wire-формат для рассылки.
    pub async fn register_actions(
        &self,
        message_id: &str,
        actions: Vec<Action>,
    ) -> Vec<crate::ws::protocol::ActionDescriptor> {
        let now = chrono::Utc::now().timestamp();
        let mut map = self.action_registry.lock().await;
        // Эвикция старых.
        map.retain(|_k, v| now - v.created_at < ACTION_REGISTRY_TTL_SECS);
        let descriptors: Vec<_> = actions.iter().map(|a| a.to_descriptor()).collect();
        map.insert(
            message_id.to_string(),
            ActionRegistryEntry {
                actions,
                created_at: now,
            },
        );
        descriptors
    }

    /// Находит Action по `action_id` среди всех зарегистрированных записей.
    /// Возвращает `None` если не найден / запись протухла.
    pub async fn find_action(&self, action_id: &str) -> Option<Action> {
        let now = chrono::Utc::now().timestamp();
        let mut map = self.action_registry.lock().await;
        map.retain(|_k, v| now - v.created_at < ACTION_REGISTRY_TTL_SECS);
        for entry in map.values() {
            if let Some(a) = entry.actions.iter().find(|a| a.id() == action_id) {
                return Some(a.clone());
            }
        }
        None
    }

    /// Регистрирует JoinHandle background-worker'а — он будет abort'нут
    /// при `shutdown_workers`. Вызывается из [`crate::spawn_workers`].
    pub fn register_worker(&self, handle: JoinHandle<()>) {
        let workers = self.workers.clone();
        // Не блокируем caller'а на async-локе — спавним детачнутый task
        // чтобы добавить handle в вектор.
        tokio::spawn(async move {
            workers.lock().await.push(handle);
        });
    }

    /// Корректно останавливает все зарегистрированные фоновые worker'ы.
    /// Phase 6 hardening вызывает это при graceful shutdown процесса
    /// devforge. Безопасно вызывать несколько раз.
    pub async fn shutdown_workers(&self) {
        let mut workers = self.workers.lock().await;
        for h in workers.drain(..) {
            h.abort();
        }
        tracing::info!(target: "forge_echo", "forge-echo: all background workers aborted");
    }
}
