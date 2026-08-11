# bitrix-task-counters

СЧЁТЧИКИ ЗАДАЧ Bitrix24 (Ф10) — два независимых семейства, которые НЕЛЬЗЯ складывать.

| что считает | метод портала | обнуляется | где живёт |
|---|---|---|---|
| непрочитанные СООБЩЕНИЯ чатов задач | im.counters.get -> TYPE.TASKS_TASK | чтением ленты | dto::CountersDto::tasks, B24State::counters |
| СОСТОЯНИЕ задач: просрочена, новые комментарии, упоминания | tasks.task.counters.get {userId} | действием над задачей | tasks::TaskCountersDto, B24State::task_counters |

Живые ответы портала:
tasks.task.counters.get -> {"expired":{"counter":2,"code":6291456},"new_comments":{"counter":0},"mentioned":{"counter":0}}
im.counters.get -> {"TYPE":{"ALL":9,"NOTIFY":5,"CHAT":0,"DIALOG":4,"MESSENGER":4,"TASKS_TASK":0}}

ПОЧЕМУ ДВЕ ЯЧЕЙКИ В СОСТОЯНИИ, А НЕ ОДНА. Просроченная задача может не иметь ни одного непрочитанного комментария, а чат задачи — светиться непрочитанным без всякой просрочки. Общее хранилище означало бы, что обновление одного семейства обнуляет другое, а обновляются они независимо и из разных мест. Сложенный бейдж вдобавок нельзя обнулить ни чтением ленты, ни закрытием задачи — он «горит» навсегда, и пользователь перестаёт на него смотреть.

ОТКУДА БЕРУТСЯ БЕЙДЖИ:
* бейдж вкладки #b24-unread-badge — TYPE.ALL (включает NOTIFY, Р13: отдельной директории под уведомления в v1 нет);
* директория ~/tasks — TYPE.TASKS_TASK, то есть непрочитанные СООБЩЕНИЯ;
* метка просрочки и бейдж newCommentsCount в строке колонки — из карточки задачи;
* карточка над лентой — поле «новых комментариев» из task.newCommentsCount.

ДОСТАВКА. Оба семейства тянет фолбэк-воркер recent_sync ТРЕТЬЕЙ командой того же batch (B24Client::sync_bundle, ключ SYNC_TASK_COUNTERS_KEY). Внешний пакет стоит ОДИН хит лимита интенсивности независимо от числа команд внутри, поэтому счётчики задач достаются бесплатно; отдельный запрос стоил бы половины бюджета в 2 rps каждую минуту ради двух чисел. userId у tasks.task.counters.get ОБЯЗАТЕЛЕН — без известного владельца вебхука команда в пакет не кладётся вовсе.

WS-кадр counters расширен полями expired/new_comments/mentioned (все #[serde(default)]) — набор ТЕГОВ кадров при этом не менялся, поэтому страж bitrix_contract.test.mjs остаётся зелёным. Фронт кладёт их в b24State.counters рядом с total/chat/dialog/tasks/notify, но НЕ складывает.

Роут GET /api/bitrix/tasks/counters отдаёт оба семейства одним ответом (TaskCountersResp: expired, new_comments, mentioned, unread) и, пока снимки воркера свежи, не стоит ни одного запроса к порталу.

Тесты: the_two_counter_families_never_mix, the_slow_worker_brings_both_counter_families_in_one_batch, a_fresh_snapshot_costs_the_counters_route_nothing (plugins/bitrix/tests/tasks.rs). Страница: bitrix-tasks-model.
