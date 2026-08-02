# plugins/telegram/src/state.rs

Разделяемое состояние плагина Telegram (обновлено в Фазе 3).

Поля: config (неизменяемая часть) + credentials под RwLock (единственное, что меняется в рантайме — их вводят в wizard'е; effective_config() отдаёт снапшот для build_runtime), broadcast(1024) БЕЗ фильтрации по диалогу (unread-бейдж и тосты обязаны работать с любой вкладки — отсюда постоянный WS на фронте), login под Mutex (переходы автомата сериализованы; set_login пишет И броадкастит под ОДНИМ lock'ом), runtime под RwLock, uploads, peers, unread, media_inflight, workers + CancellationToken, authorized_notify.

Фаза 3 добавила:
- peers: RwLock<HashMap<PeerId,PeerRef>> — PeerRef теперь НАСТОЯЩИЙ grammers_session::types::PeerRef (в Фазе 1 была заглушка {id:i64}). Ключ PeerId, а не сырой i64: он Copy+Hash, несёт тип peer'а и получается прямым разбором dialog_id; с i64-ключом каждая запись требовала бы bot_api_dialog_id() -> Option, то есть лишнюю точку отказа на горячем пути updates-воркера.
- unread: RwLock<HashMap<String,i32>> + bump_unread/set_unread/unread_of/reset_unread — серверный счётчик непрочитанного. Нужен потому, что события летят в любую вкладку, а только что открытая вкладка получает актуальные счётчики уже в GET /dialogs. Источник истины всё равно Telegram: кэш пересевается на каждом запросе списка.
- shutdown_workers теперь даёт каждому воркеру WORKER_STOP_GRACE=2с на самостоятельный выход по токену и только потом abort'ит: updates-воркер на выходе сохраняет состояние апдейтов в сессию, а abort в произвольной await-точке этого не даст — после рестарта catch_up пере-выкачивал бы уже показанные сообщения.
