# fetchGitLog

Async-функция в static/app.js. Делает GET /api/git/log?limit=100, парсит JSON-массив GitCommit { hash, abbrev, parents[], author, email, date, subject, refs[] } и сохраняет в state.gitLog. После успешного fetch вызывает renderGitGraph() для перерисовки канваса. На HTTP-ошибки или сетевые сбои пишет console.warn и НЕ модифицирует state (старый граф остаётся). Не пробрасывает исключения, чтобы Promise.all в fetchGitAll не валился из-за одного сбоя. Вызывается: 1) fetchGitAll() при switch на git-tab; 2) при polling каждые GIT_POLL_INTERVAL_MS; 3) явно из кнопки 'Reload'.
