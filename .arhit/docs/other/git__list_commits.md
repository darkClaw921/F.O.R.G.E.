# git::list_commits

Read-only обёртка над git log, отдающая список коммитов git-корня текущей сессии для гант-диаграммы вкладки Tasks. Файл: tmux-web/src/git.rs.

Сигнатура: pub async fn list_commits(cwd: &Path, since_unix: Option<i64>, until_unix: Option<i64>) -> anyhow::Result<Vec<Commit>>.

Что делает:
- Определяет корень репозитория через crate::paths::resolve_root(cwd) (поднимается по ancestors() до первой папки с маркером .beads/ или .git/, иначе сам cwd) — согласовано с tasks::list_tasks.
- Спавнит subprocess git через tokio::process::Command (async, не блокирует runtime) с current_dir(root) и аргументами: log --max-count=2000 --pretty=format:%H%x1f%ct%x1f%an%x1f%s. При since_unix=Some(v) добавляется --since=<v>; при until_unix=Some(v) добавляется --until=<v>; при None флаг не добавляется. Флаги --since/--until у git log независимы: --since=X --until=Y отдаёт коммиты в диапазоне [X, Y] (используется кнопками Сегодня/Вчера ганта).
- Парсит stdout приватной функцией parse_log: построчно, splitn(4) по разделителю 0x1F (ASCII Unit Separator, константа FIELD_SEP) на hash/ct/author/subject. %ct -> ts:i64 через parse. Битые строки (мало полей, непарсимый ct) и пустые строки пропускаются.

Возврат: Ok(Vec<Commit>) в порядке git log (новые сверху). Graceful degradation — при не-git каталоге, ненулевом exit git или ошибке spawn возвращает Ok(vec![]), НЕ Err (гант опционален и должен показывать задачи без коммитов, а не ронять эндпоинт). Ошибки логируются через tracing::debug.

struct Commit { hash:String, ts:i64, subject:String, author:String } с #[derive(Serialize)] -> JSON {hash,ts,subject,author}. ts — committer date (%ct), а не author date, чтобы порядок на оси совпадал с историей после rebase.

Связи: вызывается из main.rs::get_git_commits (хендлер GET /api/git/commits), который читает ?since=/?until= и оборачивает результат в Json({"commits":[...]}). Зависит от crate::paths::resolve_root. Юнит-тесты в модуле покрывают parse_log (well-formed, пропуск битых строк, табы/разделители в subject, пустой stdout) и сериализацию Commit. Соседние функции того же модуля: git::commit_detail (детали одного коммита) и git::is_valid_hash (валидация hash).
