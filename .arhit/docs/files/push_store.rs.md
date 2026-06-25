# push_store.rs

Хранилище push-подписок Web Push (opt-in PWA, флаг --pwa). Файл: tmux-web/src/push_store.rs.

НАЗНАЧЕНИЕ: персистентное хранилище подписок устройств на push-уведомления. Каждый браузер/устройство, согласившийся получать Web Push, шлёт свой PushSubscription на POST /api/push/subscribe; подписка сохраняется здесь и переживает рестарт сервера (иначе после перезапуска некому слать пуши). Файл ~/.forge/push_subscriptions.json (рядом с user_settings.json и vapid.json), резолвится от HOME через default_subscriptions_path().

МОДЕЛЬ: StoredSubscription = сериализованный браузерный PushSubscription + серверные метаданные. Поля: endpoint (URL push-сервиса FCM/Mozilla/Apple — уникальный идентификатор, дедуп и удаление по нему); keys.p256dh (публичный ECDH-ключ браузера, 65-байт uncompressed point, base64url) + keys.auth (auth-secret 16б, base64url) — нужны push.rs для RFC8291-шифрования; device_label (опциональная метка устройства для UI); created_at (RFC3339, ставит сервер при subscribe).

КЛЮЧЕВЫЕ МЕТОДЫ PushSubscriptionStore (Arc<RwLock<Inner>>, cheap-clone, один экземпляр на процесс в PwaCtx.subs): new(path) — конструктор с путём; upsert(sub) — вставка/обновление с дедупом по endpoint + atomic save; remove(endpoint) -> Ok(bool) — идемпотентное удаление (false если не было, без записи на диск); prune(&[endpoint]) — батч-удаление мёртвых подписок (вызывается push::send_to_all для 404/410); list()/snapshot — чтение для воркера доставки.

PERSISTENCE: паттерн UserSettingsStore — atomic save (tmp + fs::rename поверх; на POSIX rename атомарен в рамках mount-point: при kill -9 на диске либо старый, либо новый файл, не битый). Политика 'битый файл не блокирует работу': повреждённый JSON -> пустой список (no panic).

ЗАВИСИМОСТИ: используется pwa.rs (subscribe/unsubscribe/test хендлеры), push.rs (send_to_all читает подписки и прунит мёртвые), main.rs (инициализация PwaCtx.subs при --pwa). Тесты: upsert dedup, prune batch, idempotent remove, atomic write, corrupt-file -> empty.
