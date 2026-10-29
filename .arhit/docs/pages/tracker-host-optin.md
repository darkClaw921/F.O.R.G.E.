Включение плагина forge-tracker в хост devforge (фаза 3, epic forge-trk-f3-yreko).

ЧТО СДЕЛАНО

Плагин forge-tracker подключён к бинарю devforge строго как OPT-IN, по образцу vk/meet. Без флага поведение сервера не меняется вообще: роуты не регистрируются, весь префикс /api/tracker/* и /ws/tracker отдают 404. Именно этот 404 — контракт фронтового гейта (кнопка вкладки «Мои задачи» не рисуется). Гейт обеспечивается ОТСУТСТВИЕМ роутов, а не проверкой флага внутри плагина: крейт про opt-in ничего не знает.

ТРИ ИСТОЧНИКА ВКЛЮЧЕНИЯ (приоритет CLI > file > env > default(false))

1. CLI: `devforge run --tracker` / `devforge start --tracker` (tmux-web/src/cli.rs, поле RunOptions::tracker, строка в help_text).
2. Файл: ~/.config/forge/server_config.json {"tracker": true} (ServerConfig::tracker: Option<bool>).
3. Окружение: DEVFORGE_TRACKER=1 (константа server_config::ENV_TRACKER, truthy 1/true/yes/on).

Итоговое значение — EffectiveConfig::tracker_enabled, вычисляется в server_config::resolve тем же выражением, что и meet_enabled.

ПРОБРОС В ДЕМОН

daemon::build_daemon_args добавляет "--tracker" в argv дочернего процесса. Без этой строки `devforge start --tracker` молча поднял бы сервер без роутов плагина — самый частый класс ошибки при добавлении фича-флага (тест daemon_args_carry_tracker плюс исчерпывающий литерал в daemon_args_carry_all_flags_together, который намеренно ломает компиляцию при добавлении нового флага).

БЛОК РЕГИСТРАЦИИ В main.rs

Стоит сразу после блока forge-meet и ДО .fallback(static_embed::serve_static) и ДО .layer(...). Порядок — инвариант, а не стиль: регистрация после .fallback дала бы 404 на роуты плагина, после .layer — дыру в авторизации (bearer_auth и csrf_guard покрывают только зарегистрированное под ними).

    let mut tracker_state: Option<Arc<forge_tracker::TrackerState>> = None;
    let app = if tracker_enabled {
        match forge_tracker::TrackerConfig::from_home().and_then(forge_tracker::init) {
            Ok(state) => { register_routes → spawn_workers → info; tracker_state = Some(state); }
            Err(e) => { warn; сервер стартует БЕЗ плагина }
        }
    } else { app };

`?` здесь ЗАПРЕЩЁН: он превратил бы беду с одним локальным JSON в отказ всего devforge.

SHUTDOWN

forge_tracker::shutdown(&state) вызывается в общем shutdown-блоке сразу после forge_meet::shutdown и до финального снимка истории сессий. Гасит сторожа забытых таймеров и WS-циклы; сохранять стор не нужно — каждая мутация уже записана атомарно к моменту ответа роута.

ЗАВИСИМОСТЬ

tmux-web/Cargo.toml: forge-tracker = { path = "../plugins/tracker" }. Отдельного tracker-host-api нет — плагину от хоста нужен только Router.

ТЕСТЫ

- tmux-web/src/cli.rs: parse_tracker_flag_default_false / _sets_true / _is_independent_of_the_other_flags / _carried_by_start / _rejected_for_stop_status, help_text_mentions_tracker.
- tmux-web/src/server_config.rs: tracker_priority_matches_the_other_plugins, tracker_cli_and_env_agree, resolve_tracker_is_independent_of_the_other_plugins, resolve_reads_the_tracker_variable_from_the_process_environment (#[ignore], мутирует глобальное окружение).
- tmux-web/src/daemon.rs: daemon_args_carry_tracker.
- tmux-web/tests/tracker_smoke.rs (новый файл, близнец meet_smoke.rs): 404 на восьми путях без register_routes, 200 с плагином, задача по проводу видна в /api/tracker/state, ветка Err(init) оставляет хост живым и роуты несмонтированными.

ЖИВАЯ ПРОВЕРКА ГЕЙТА (изолированный HOME и TMUX_TMPDIR, свободный порт)

- без флага: /api/tracker/{status,state,tasks,report} → 404, /healthz → 200, / → 200;
- --tracker: /api/tracker/status → 200 {"enabled":true,...}, POST /api/tracker/tasks создаёт задачу, GET /api/tracker/state её показывает;
- DEVFORGE_TRACKER=1 → 200; server_config.json {"tracker": true} → 200;
- перезапуск сервера: задача, отрезок и НЕЗАКРЫТЫЙ таймер восстановлены из ~/.config/forge/tracker.json (0600).

ОГРАНИЧЕНИЕ, ВЫЯСНЕННОЕ ЖИВОЙ ПРОБОЙ

Воспроизвести падение init на живом сервере нечем: TrackerStore::load переживает отсутствующий, пустой и битый tracker.json, а единственная реальная ошибка (TrackerConfig::from_home без HOME) до плагина не доходит — на ней раньше падает сам хост (TodoStore: "HOME env var is not set"). Поэтому ветка Err проверяется формой обработки в tracker_smoke.rs, а не искусственной поломкой конфига.