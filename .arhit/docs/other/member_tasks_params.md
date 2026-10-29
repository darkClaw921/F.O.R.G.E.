# member_tasks_params

api::methods::member_tasks_params(user_id) — параметры tasks.task.list для задач с участием пользователя в любой роли: filter{MEMBER}, order ACTIVITY_DATE desc (первая страница из 50 — самые живые задачи), select LIST_SELECT (UPPERCASE, ловушка 2), start 0. Используется в B24Client::dialogs_bundle.
