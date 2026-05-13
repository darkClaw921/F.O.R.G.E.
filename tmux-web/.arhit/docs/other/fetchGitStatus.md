# fetchGitStatus

Async-функция в static/app.js (Phase 4, ~строка 1233). Получает снапшот GitStatus из GET /api/git/status, обновляет state.gitStatus и инициирует перерисовку UI.

Шаги:
1. fetch('/api/git/status').
2. Если r.ok — парсит JSON в state.gitStatus и вызывает renderGitToolbar(), renderGitFiles(), updateCommitBtnState().
3. Если HTTP не-2xx — console.warn, state.gitStatus НЕ трогается (старый снапшот остаётся виден). Это намеренно: при polling временные сбои не должны мигать UI.
4. catch (network failure) — то же самое: console.warn без исключения наружу.

Вызывается из:
- fetchGitAll() (параллельно с fetchGitLog) при switchTab('git') и каждые 5s polling.
- toggleStage() / commitNow() после успешного действия — для немедленного refresh.

НЕ пробрасывает исключение: Promise.all в fetchGitAll не должен падать целиком из-за одного сбоя.

См. также: fetchGitLog, fetchGitAll, renderGitToolbar, renderGitFiles, updateCommitBtnState.
