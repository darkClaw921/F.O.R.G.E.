# collect_project_activity

HostApi::collect_project_activity (plugin boundary, объявлен в plugins/echo-host-api/src/lib.rs; реализация EchoHostAdapter в tmux-web/src/echo_host.rs).

Назначение: собирает СТРУКТУРИРОВАННУЮ активность проектов хоста с момента since_unix (unix seconds) для генерации предложений задач в «Сводке дня». В отличие от collect_git_activity (склеивает всё в единый markdown-блок для grounding раздела «Что сделано»), возвращает Vec<ProjectActivity> — по одному элементу на уникальный git-корень рабочих директорий tmux-сессий.

Сигнатура: async fn collect_project_activity(&self, since_unix: i64) -> anyhow::Result<Vec<ProjectActivity>>. В trait есть default-реализация, возвращающая пустой вектор (для тестовых stub'ов и прочих impl'ов).

Реализация (echo_host.rs): обходит tmux-сессии (list_sessions); при ошибке логирует warn и возвращает пусто. Для каждой сессии резолвит git-корень рабочей директории, дедуплицирует корни, для каждого выполняет git log --since=<since_unix> и формирует ProjectActivity { path: git-корень, name: basename, git_log: markdown-список коммитов }. Не-git каталоги и ошибки отдельных репозиториев тихо пропускаются. collect_git_activity реализован поверх collect_project_activity.

Контракт: один элемент на уникальный git-корень; проект включается даже с пустым git_log (активен в сессии — кандидат на задачи); нет сессий → пустой вектор.

Связи: вызывается из daily_report::generate_report; результат уходит в generate_suggestions → промпт второго one_shot. path служит стабильным ключом проекта и используется как path при создании TODO через POST /api/todos.
