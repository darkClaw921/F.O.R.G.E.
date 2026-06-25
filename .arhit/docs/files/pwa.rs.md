# pwa.rs

PWA HTTP-хендлеры + DTO (opt-in PWA, флаг --pwa). Файл: tmux-web/src/pwa.rs.

НАЗНАЧЕНИЕ: REST-эндпоинты PWA. Все роуты регистрируются в main.rs ТОЛЬКО при app_state.pwa.is_some() (если --pwa) и ДО auth-layer — поэтому в remote-mode попадают под Bearer-auth + csrf_guard (same-origin + Content-Type:application/json). Без --pwa роутов нет -> 404 (fallback на статику), что для фронтового bootstrap.js = 'PWA выключено'.

ЭНДПОИНТЫ:
- GET /api/pwa/config (get_pwa_config) -> 200 {enabled:true, vapidPublicKey:<b64url>} (PwaConfig, serde camelCase). vapidPublicKey = публичный VAPID-ключ (65-байт uncompressed point, base64url-no-pad) для pushManager.subscribe({applicationServerKey}). Если state.pwa==None (теоретический рассинхрон) -> 404.
- POST /api/push/subscribe (post_push_subscribe): тело SubscribeReq {endpoint, keys:{p256dh,auth}, device_label?} = браузерный PushSubscription. Сервер ставит created_at (RFC3339) и делает store.upsert (дедуп по endpoint) -> 200 {ok:true}. 404 без PWA, 500 при ошибке записи.
- POST /api/push/unsubscribe (post_push_unsubscribe): тело {endpoint}. store.remove, идемпотентно (повтор/несуществующий endpoint тоже 200) -> 200 {ok:true}.
- POST /api/push/test (post_push_test): реальная RFC8291-доставка тестового пуша всем подпискам через push::send_to_all (Фаза 3; раньше Фаза 2 — заглушка sent:0). Шифрует payload {title,body,data:{url}}, прикладывает VAPID-JWT, POST на endpoint; мёртвые (404/410) прунятся -> 200 {sent, pruned}. HTTP-клиент создаётся на вызов (дёргается редко).

DTO: PwaConfig (camelCase enabled/vapidPublicKey), SubscribeReq/SubscribeKeys/UnsubscribeReq (deserialize браузерной формы), OkResp{ok}, TestResp{sent,pruned}.

ЗАВИСИМОСТИ: AppState.pwa (PwaCtx с vapid+subs), push_store (upsert/remove), push (send_to_all для /test), vapid (public_key для config). Регистрация роутов: main.rs блок 'if app_state.pwa.is_some()'. Тесты: config camelCase, subscribe browser-shape deserialize, device_label optional, ok_resp, created_at rfc3339 utc.
