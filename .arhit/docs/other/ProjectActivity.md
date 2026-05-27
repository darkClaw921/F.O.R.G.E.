# ProjectActivity

ProjectActivity — DTO plugin boundary (plugins/echo-host-api/src/lib.rs). Derive: Debug, Clone, Serialize, Deserialize. Описывает активность одного проекта (git-репозитория) за день, возвращается из HostApi::collect_project_activity.

Поля:
- path: String — git-корень репозитория (абсолютный путь). Стабильный ключ проекта; используется как path при создании TODO через POST /api/todos, поэтому значение должно сохраняться неизменным по всему потоку данных.
- name: String — basename git-корня для отображения в UI (заголовок группы предложений).
- git_log: String — коммиты репозитория с начала дня в виде markdown-списка (- %h %s). Может быть пустым: проект считается активным (кандидатом на задачи) уже потому, что присутствует в tmux-сессии, даже если за день в нём не было коммитов.

Связи: создаётся в EchoHostAdapter::collect_project_activity (tmux-web/src/echo_host.rs); потребляется daily_report::generate_suggestions, где из списка ProjectActivity строится промпт второго one_shot для генерации предложений задач. project_path в JSON-ответе модели должен совпадать с ProjectActivity.path.
