# PushSubscriptionStore

Хранилище push-подписок PWA (tmux-web/src/push_store.rs), opt-in под флагом --pwa. Персистит браузерные Web Push подписки в ~/.forge/push_subscriptions.json, чтобы они переживали рестарт сервера (иначе после перезапуска некому слать пуши).

Модель StoredSubscription { endpoint: String (URL push-сервиса браузера, первичный ключ для дедупа/удаления), keys: SubscriptionKeys { p256dh, auth } (base64url ECDH-ключ + auth-secret браузера, нужны Фазе 3 для RFC8188-шифрования payload), device_label: Option<String> (метка устройства для UI, не участвует в дедупе), created_at: String (RFC3339, ставит сервер при subscribe) }.

PushSubscriptionStore — Arc<RwLock<Inner>> (cheap-clone, один экземпляр на процесс в PwaCtx.subs), паттерн копирует UserSettingsStore. Методы:
- new(path) -> Self: грузит существующие подписки; отсутствующий файл -> [] (без warn); битый/нечитаемый -> [] + warn (битый файл не блокирует работу, не паника); файл НЕ создаётся (lazy creation, сохраняет opt-in инвариант).
- list() -> Vec<StoredSubscription>: снимок под read-lock (для итерации без удержания лока при сетевых запросах в Фазе 3).
- upsert(sub) -> Result<()>: вставка или замена по endpoint (идемпотентно — повторный subscribe не плодит дубли; заменяет запись целиком, включая ротированные keys), atomic save.
- remove(endpoint) -> Result<bool>: удаление по endpoint, идемпотентно (не найден -> Ok(false) без записи; удалён -> Ok(true) + atomic save). Эндпоинт unsubscribe отвечает 200 в любом случае.
- prune(&[String]) -> Result<usize>: батч-удаление мёртвых endpoint'ов (404/410 от push-сервиса в Фазе 3) ОДНОЙ записью на диск; возвращает число удалённых; 0 совпадений -> диск не трогается.

save_locked: atomic write (serde_json::to_vec_pretty в <file>.tmp, затем fs::rename поверх; POSIX-атомарен). default_subscriptions_path() резолвит ~/.forge/push_subscriptions.json от HOME.

Зависимости: используется в pwa.rs (хендлеры subscribe/unsubscribe/test) и push.rs (Фаза 3, доставка). Подключается в AppState.pwa.subs (PwaCtx) только при pwa_enabled.
