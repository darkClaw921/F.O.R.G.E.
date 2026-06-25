# post_push_test

Хендлер POST /api/push/test (tmux-web/src/pwa.rs), Фаза 2 PWA — ЗАГЛУШКА. Контракт ответа TestResp { sent: usize, pruned: usize } стабилен с Фазы 2. Сейчас всегда 200 { sent: 0, pruned: 0 } — реальная доставка (RFC8188 aes128gcm шифрование payload + ES256 VAPID-JWT + HTTP POST на push-сервис через push::send_to_all, prune мёртвых 404/410) реализуется в Фазе 3 на RustCrypto без openssl. Здесь намеренно НЕ тянутся крейты отправки. state.pwa == None -> 404. TODO(Фаза 3) помечен в коде.
