# tmux-web/src/attention.rs::deduplicate_attention

Дедуплицирует needs_attention среди сессий одной итерации watcher_loop. Алгоритм: union-find по двум осям — pane_hash (точное совпадение видимой панели) и session_group (Some(g): linked tmux-сессии). Внутри объединённой группы: если хотя бы у одного detected=true — выбирается primary через pick_primary, остальные false. Если ни у кого detected=false — все false (без изменений). Подавляет 'оранжевое отображение всей группы' при общем pane_hash или session_group.
