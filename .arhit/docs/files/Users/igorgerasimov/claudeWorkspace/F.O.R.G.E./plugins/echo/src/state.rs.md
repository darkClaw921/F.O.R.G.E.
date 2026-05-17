# /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./plugins/echo/src/state.rs

EchoState — глобальное состояние плагина Echo. Cheap-clonable (внутри Arc и broadcast::Sender). Поля: host: Arc<OnceCell<Arc<dyn HostApi>>> (устанавливается register_routes), broadcast: broadcast::Sender<ServerEvent> (capacity 256, для WS-подписчиков). EchoConfigStub — placeholder под полную EchoConfig из Phase 6. ServerEvent enum (Phase 1: только _Placeholder вариант; в Phase 3 появятся AssistantChunk, AssistantDone, ActionButtons, Notification, StatsUpdate, AutonomousTaskEvent, Error).
