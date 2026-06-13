# bearer_auth

tmux-web/src/auth.rs. Bearer-аутентификация для remote-mode (auth_token=Some). При None — passthrough (localhost). Path-исключения: /healthz, /, статика. Для /ws/* принимает токен через ?token= (браузер не ставит headers на WS). Работает В ПАРЕ с csrf_guard, который подключён всегда и закрывает localhost-режим от drive-by.
