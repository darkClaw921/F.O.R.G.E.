# post_push_unsubscribe

Хендлер POST /api/push/unsubscribe (tmux-web/src/pwa.rs), Фаза 2 PWA. Тело { endpoint } (DTO UnsubscribeReq). Вызывает ctx.subs.remove(endpoint). ИДЕМПОТЕНТЕН: повторная отписка или отписка несуществующего endpoint тоже 200 { ok: true } — store.remove в этом случае не пишет на диск (Ok(false)). Ошибка записи на диск при фактическом удалении -> 500. state.pwa == None -> 404.
