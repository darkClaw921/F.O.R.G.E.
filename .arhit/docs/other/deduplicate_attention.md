# deduplicate_attention

Дедуплицирует флаги needs_attention среди tmux-сессий одной итерации watcher_loop. Возвращает Vec<(session_name, final_flag)>.

ОСЬ ГРУППИРОВКИ — ТОЛЬКО session_group (union-find). Ось pane_hash УДАЛЕНА (фикс бага 2026-05-25): spawn_tmux_attach (pty.rs) делает прямой 'tmux attach -t', а не 'new-session -t', поэтому linked-сессий нет и session_group всегда None. Совпавший pane_hash означал лишь случайное совпадение содержимого двух НЕЗАВИСИМЫХ сессий — гашение одной из них приводило к симптому 'вкладка не светится оранжевым, пока в неё не перейдёшь (attach)'.

Внутри группы (по session_group): если ни одна не detected — все false; если хотя бы одна detected — primary через pick_primary (attached>0 побеждает, иначе max id), ему true, остальным false. Сессии с group=None никогда не группируются → каждая светится по своему detected. Парная функция — deduplicate_generating (ось session_group, сигнал changed).
