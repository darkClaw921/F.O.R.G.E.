# git-tab-bootstrap

Bootstrap-listeners для Git-таба в static/app.js (Phase 4, P4-T7).

Listeners (~строка 4196):
- $tabGit click → switchTab('git') — переключает на Git-таб; switchTab сам поднимает fetchGitAll() и startGitPolling().
- $gitReload click → fetchGitAll() — ручной reload status+log (полезно если visibilitychange paused polling).
- $gitCommitBtn click → commitNow() — делает POST /api/git/commit.
- $gitCommitMsg input → updateCommitBtnState() — пересчитывает disabled-состояние кнопки на каждый ввод символа.

Lifecycle-обработчики:
- beforeunload: stopGitPolling() — гасит setInterval перед закрытием/reload.
- visibilitychange (document.hidden=true): stopGitPolling() — не шуршим git CLI subprocess пока вкладка скрыта.
- visibilitychange (document.hidden=false): если state.activeTab === 'git' → fetchGitAll() + startGitPolling() возобновляют опрос с свежего снапшота без ожидания interval-tick.

Идемпотентность: все if ($el) — guard от отсутствующих DOM-узлов (на случай если HTML не содержит #git-* элементы).
