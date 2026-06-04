# AttentionState::idle_snapshot

Возвращает HashMap<String, u64> «затихших» (idle) tmux-сессий: имя сессии -> сколько секунд прошло с момента, когда индикатор генерации Claude погас (Instant::elapsed().as_secs()).

Назначение: основа фичи «Следующий шаг». Echo-воркер next_step опрашивает этот снимок и для сессий, простоявших idle >= IDLE_THRESHOLD_SECS (10с), генерирует предложение следующего шага.

Источник данных — карта idle_started_at внутри AttentionState. Она поддерживается строго по фронтам флага is_generating в set_generating: фронт true->false (сессия реально генерировала и затихла) ставит Instant::now(); фронт false->true (началась новая серия генерации) удаляет отметку. При первом наблюдении сессии (флаг сразу false, prev=false) отметка НЕ ставится — затихать нечему.

Ключевое ограничение/бизнес-логика: сессии с needs_attention=true (открыт Claude permission/plan/question prompt, см. self.map) ИСКЛЮЧАЮТСЯ из снимка. Там нужен ответ пользователя, а не автоген следующего шага — это и есть подавление генерации при needs_attention.

Зависимости: читает idle_started_at и map (needs_attention). Используется EchoHostAdapter::idle_sessions (tmux-web/src/echo_host.rs), который оборачивает результат в Vec<IdleSession{name, idle_secs}> для HostApi. Расположение: tmux-web/src/attention.rs:191.
