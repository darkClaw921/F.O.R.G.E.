# main.rs::resolve_notify_session

Phase 3 — Резолвит целевую tmux-сессию для уведомления. Приоритет: 1) override из body (trim non-empty); 2) project.notify_session (trim non-empty); 3) fallback <prefix>-main если tmux_prefix непустой; 4) None (caller возвращает 400).
