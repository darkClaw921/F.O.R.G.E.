# plugins/tracker/src/models.rs

Модель данных личного трекера (крейт forge-tracker).

СУЩНОСТИ.
- TaskList { id, name, color: Option<String>, order } — пользовательский список (блок // LISTS в UI).
- TrackerTask { id, list_id: Option, title, notes: Option, status, priority 0..=4, tags, estimate_min: Option<u32>, start: Option, due: Option, order, created_at, updated_at, done_at: Option } — сама задача.
- TaskStatus = Inbox | Today | Doing | Waiting | Done — он же колонки канбана. Сериализуется СТАБИЛЬНЫМИ строками inbox/today/doing/waiting/done: имена уходят в JSON стора и в REST, поэтому переименование варианта это ломающее изменение формата. Есть TaskStatus::ALL, as_str, parse (регистр не важен).
- ChecklistItem { id, task_id, title, done, done_at: Option, order } — пункт чек-листа.
- Milestone { id, title, date, list_id: Option, color: Option } — веха Гантта; к задачам НЕ привязана и переживает удаление любой из них.
- TimeEntry { id, task_id, item_id: Option, started_at, stopped_at: Option, note: Option, source } — отрезок времени. Активный отрезок это stopped_at == None. source: timer (SOURCE_TIMER) или manual (SOURCE_MANUAL).
- TrackerFile { version, lists, tasks, items, milestones, entries } — envelope файла ~/.config/forge/tracker.json.

ТРИ РЕШЕНИЯ В ТИПАХ.
1) root_path нет НИ В ОДНОЙ структуре — задачи трекера глобальные (главное отличие от Todo в tmux-web/src/todos.rs). Закреплено тестом no_model_carries_root_path.
2) Коллекции ПЛОСКИЕ: пункты и отрезки лежат своими списками и ссылаются через task_id, а не вложены в задачу. Причина: отрезок принадлежит одновременно задаче и (опционально) пункту, а отчёты читают отрезки поперёк всех задач — за день, за неделю, по спискам.
3) Даты — строки RFC 3339 UTC; поля start/due/date допускают и голую дату YYYY-MM-DD (Гантт рисует по дням).

СОВМЕСТИМОСТЬ: deny_unknown_fields не ставится нигде (файл от более новой версии обязан читаться старым кодом), все необязательные поля под serde(default). Именование snake_case, как в todos.json.

ПАТЧИ (TaskPatch, ListPatch, ItemPatch, MilestonePatch, EntryPatch) для частичного обновления. Обнуляемые поля объявлены как Option<Option<T>> с deserialize_with = some_option, чтобы различать три состояния: ключа нет (не трогаем), ключ со значением (ставим), ключ = null (СБРАСЫВАЕМ). Без этого «убрать срок у задачи» было бы невыразимо.
