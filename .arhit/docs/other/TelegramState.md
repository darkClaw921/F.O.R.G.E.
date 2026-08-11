# TelegramState

Разделяемое состояние плагина Telegram (plugins/telegram/src/state.rs). Создаётся один раз в crate::init, оборачивается в Arc и раздаётся axum-хендлерам через with_state, WS-соединениям и фоновым воркерам.

ПОЛЯ MTProto И UI: config, credentials, broadcast (ёмкость 1024, БЕЗ фильтрации — unread-бейдж и тосты должны работать с любой вкладки), login (Mutex — переходы автомата строго сериализованы), runtime (RwLock), uploads, peers (PeerId -> PeerRef), unread, notify_defaults / notify_overrides / archived_ids (серверный источник истины о том, «звенит» ли диалог), media_inflight, workers, shutdown, authorized_notify, ws_heartbeat / ws_idle_timeout.

ПОЛЯ «АНАЛИЗА ЧАТА» (Этап 2):
- prompts: Arc<Mutex<PromptStore>> — пер-чатовые заметки, последние разборы и треды Q&A. Под Arc потому, что ТУ ЖЕ мапу держит AnalysisHub: сохранение разбора по концу run'а идёт под ТЕМ ЖЕ Mutex, что и правка заметки из хендлера, и потому не может её обогнать.
- analysis: AnalysisHub — реестр активных анализов. Создаётся ОДИН раз здесь, а не на запрос: в нём живут дедуп по диалогу, потолок параллелизма и таблица run_id -> процесс, по которой работает отмена.

МЕТОДЫ-ОБЁРТКИ (все отдают СНИМОК, а не guard: держать Mutex на время рендера ответа означало бы сериализовать все запросы вкладки на одном локальном файле): chat_prompt, set_chat_prompt, chat_analysis, clear_chat_analysis, chat_thread, clear_chat_thread. Все мутирующие меняют мапу и файл под ОДНИМ lock'ом — иначе два одновременных сохранения из двух вкладок могли бы записать файлы в порядке, обратном порядку правок, и более старая заметка победила бы.

analysis_sink() -> Arc<dyn AnalysisSink> — приёмник кадров для AnalysisHub::start. Хелпер здесь, а не Arc::new(...) в каждом роуте, ради одного: приёмник обязан вещать ИМЕННО в broadcast ЭТОГО состояния, иначе кадры run'а не увидит ни одна открытая вкладка.

ВАЖНО ДЛЯ ТЕСТОВ: TelegramConfig::default() указывает на РЕАЛЬНЫЙ ~/.forge/telegram-chat-prompts.json. Любой тестовый конфиг обязан задавать prompts_path в tempdir, иначе прогон cargo test затирает живые заметки пользователя.
