# git::commit_detail

Async-функция, возвращающая полные детали одного коммита (мета + тело + список изменённых файлов) как Option<CommitDetail>, или None при любой проблеме (graceful, как весь модуль git). Сигнатура: pub async fn commit_detail(cwd:&Path, hash:&str) -> anyhow::Result<Option<CommitDetail>>. Используется хендлером GET /api/git/commit (main.rs::get_git_commit) для hover-попапа коммита на гант-диаграмме вкладки Tasks.

Безопасность: hash приходит из недоверенного ?hash=, поэтому ПЕРЕД любым spawn проходит через git::is_valid_hash; при невалидном hash → Ok(None) без запуска git (защита от инъекции аргументов).

Реализация — двухвызовный вариант (для надёжного парсинга, без эвристик 'где кончается body'): git-корень ищется через crate::paths::resolve_root(cwd), оба вызова git show идут с current_dir(root) через tokio::process::Command:
1) git show --no-patch --format=%H%x1f%ct%x1f%an%x1f%s%x1f%b <hash> — одна запись с метой (hash/ts/author/subject) и многострочным телом (%b). Парсится приватной parse_meta.
2) git show --name-status --format= <hash> — пустой --format= гасит шапку, остаются только строки status<TAB>path (для R/C — status<TAB>old<TAB>new). Парсится приватной parse_name_status.
Разделение на два вызова исключает смешивание многострочного body и блока файлов.

Graceful: невалидный hash → Ok(None) без spawn; ошибка spawn / ненулевой exit первого вызова / коммит не найден / пустая мета → Ok(None). Ненулевой exit ВТОРОГО вызова трактуется как 'нет файлов' (files=[]), мета уже получена — попап без списка файлов всё ещё полезен.

Структуры: CommitDetail {hash,ts,subject,body,author,files:Vec<FileChange>} и FileChange {status,path} — обе #[derive(Debug,Clone,Serialize)]. В FileChange.path для переименований кладётся НОВЫЙ путь (последняя колонка).

Приватные парсеры parse_meta (5 полей по FIELD_SEP 0x1F, остаток после 4-го разделителя = body, хвостовой \n обрезается, None при <5 полей или непарсимом ct) и parse_name_status (split по табу, status=первая колонка, path=последняя, пустые/без-таба строки пропускаются) покрыты юнит-тестами без spawn git.
