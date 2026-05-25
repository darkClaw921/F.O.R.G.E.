# deduplicate_generating

Дедуплицирует флаги is_generating (индикатор ✶) среди tmux-сессий одной итерации watcher_loop. Структурно-парная к deduplicate_attention, но работает с сырым сигналом changed (prev != current хэш последних 50 строк pane от update_generation) вместо detected.

ОСЬ ГРУППИРОВКИ — ТОЛЬКО session_group (union-find). Ось gen_hash УДАЛЕНА (фикс бага 2026-05-25, симметрично deduplicate_attention): без linked-сессий совпавший gen_hash — случайное совпадение содержимого двух независимых сессий, гасить индикатор у одной из них неверно.

Внутри группы: если ни одна не changed — все false; если хотя бы одна changed — primary через pick_primary_gen (attached>0 побеждает, иначе max id), ему true, остальным false. Независимые сессии (group=None) не группируются → ✶ горит на каждой реально работающей сессии.
