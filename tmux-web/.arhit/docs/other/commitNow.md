# commitNow

JS-функция в static/app.js (Phase 4, P4-T6). Делает коммит staged-файлов через POST /api/git/commit.

Поведение:
1. trim() сообщения из textarea $gitCommitMsg. Пустое → showGitError('Сообщение коммита пустое') и return.
2. hideGitError() и временно disable кнопку (анти-double-click).
3. fetch POST /api/git/commit с Content-Type: application/json и body {message}.
4. HTTP не-2xx → читает text() ответа, выводит в $gitCommitError через showGitError.
5. Успех → $gitCommitMsg.value = '' и await fetchGitAll() — обновляет status + log (Phase 5).
6. Network error (fetch reject) → catch, showGitError с err.message.
7. finally → updateCommitBtnState() пересчитывает disabled.

Зависит от: fetchGitAll, showGitError, hideGitError, updateCommitBtnState. Бекенд: handlers::git::commit (POST /api/git/commit).
Вызывается из: bootstrap-listener $gitCommitBtn 'click' (P4-T7).
