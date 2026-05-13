# fetchGitAll

Async-функция в static/app.js (Phase 4, ~строка 912). Параллельный fetch всех git-данных через Promise.all([fetchGitStatus(), fetchGitLog()]).

Цель: один тик polling = один параллельный round-trip. fetchGitStatus и fetchGitLog внутри сами обрабатывают HTTP-ошибки (без throw), поэтому Promise.all не падает целиком если одна из частей сбойнула.

Точки вызова:
- switchTab('git') — однократный initial fetch при показе таба.
- startGitPolling() — setInterval каждые GIT_POLL_INTERVAL_MS (5000ms) пока юзер на табе.

Side-effects: обновляет state.gitStatus и state.gitLog, перерисовывает toolbar, files, graph, commit-button enabled-state.
