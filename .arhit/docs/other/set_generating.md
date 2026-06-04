# set_generating

Метод AttentionState (tmux-web/src/attention.rs): единственный writer карты generating (финальный флаг is_generating). Поддерживает две производные карты строго по фронтам флага: (1) gen_started_at — момент начала серии генерации (false->true ставит, ->false удаляет; питает generating_age_snapshot/tooltip); (2) idle_started_at — момент затухания (true->false ставит idle-метку, false->true снимает; первое наблюдение prev=false&flag=false метку не ставит). idle_started_at питает idle_snapshot и фичу «Следующий шаг».
