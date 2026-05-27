# fetchCommitDetail

Async-загрузка деталей коммита (gantt.js). GET /api/git/commit?path=<enc sessionCwdOrNull()>&hash=<enc hash> (path только если cwd не null). Возвращает json.commit (объект|null); non-ok статус или исключение → null + console.warn. Результат кэшируется в detailCache вызывающим openPopoverFor.
