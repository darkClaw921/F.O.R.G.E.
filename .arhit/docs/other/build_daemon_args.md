# build_daemon_args

Сборка argv дочернего процесса daemon'а (tmux-web/src/daemon.rs).

build_daemon_args(port: u16, opts: &cli::RunOptions) -> Vec<String> — чистая функция: 'run --port N' плюс все флаги, которые пользователь указал у devforge start: --remote, --bind <addr>, --token <hex>, --pwa, --telegram. Используется daemon::start (cmd.args(build_daemon_args(port, opts))).

ЗАЧЕМ ВЫНЕСЕНА: раньше сборка была инлайновой и пробрасывала только remote/bind/token, из-за чего 'devforge start --pwa' молча поднимал daemon БЕЗ PWA. Отдельная чистая функция покрыта unit-тестами (без флагов argv остаётся ровно 'run --port N'; каждый флаг доезжает; лишних флагов не добавляется), поэтому следующий opt-in флаг нельзя забыть пробросить незаметно.
