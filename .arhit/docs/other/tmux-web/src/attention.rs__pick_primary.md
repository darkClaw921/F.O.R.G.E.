# tmux-web/src/attention.rs::pick_primary

Выбирает primary-индекс среди группы дедупа. Принимает только сессии с detected=true. Приоритет: (a) среди attached>0 — наибольший session_id лексикографически; (b) среди всех detected=true (если все attached=0) — наибольший session_id; (c) fallback — лексикографически наибольшее имя. Возвращает None если в группе нет detected=true.
