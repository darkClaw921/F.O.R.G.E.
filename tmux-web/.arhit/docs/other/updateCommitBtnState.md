# updateCommitBtnState

JS-функция в static/app.js (Phase 4, P4-T6). Обновляет disabled-состояние кнопки Commit.

Логика: кнопка enabled тогда и только тогда, когда (a) сообщение в $gitCommitMsg после trim непусто И (b) в state.gitStatus.files есть хотя бы один файл с f.staged === true.

Вызывается:
- после fetchGitStatus (изменился список staged-файлов через polling или toggleStage);
- на input в $gitCommitMsg (пользователь печатает);
- в finally commitNow (после очистки textarea кнопка снова disabled).

Безопасно вызывать когда $gitCommitBtn / $gitCommitMsg отсутствуют (early-return).
