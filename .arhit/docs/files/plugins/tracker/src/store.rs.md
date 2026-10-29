# plugins/tracker/src/store.rs

TrackerStore — плоское JSON-хранилище личного трекера (Arc<RwLock<Inner>> поверх ~/.config/forge/tracker.json).

ОТЛИЧИЕ ОТ tmux-web/src/todos.rs. Приём тот же (RwLock + atomic save), но ГРУППИРОВКИ ПО КОРНЮ ЗДЕСЬ НЕТ: у TODO-карточек ключ хранилища root_path, потому что карточка живёт внутри проекта; задача трекера принадлежит человеку. Один плоский набор данных на машину, Inner без HashMap по путям.

ЖИВУЧЕСТЬ. Файла нет → пустой стор. Файл битый или нечитаемый → tracing::warn и пустой стор, сервер обязан подняться (трекер не критичная подсистема). Запись только через forge_atomic::write_bytes_atomic (tmp → fsync → rename → 0600) и ВНУТРИ write-lock, поэтому на диске всегда согласованный снимок.

КОНСТРУКТОРЫ: open(&TrackerConfig), load(path), file_path(), snapshot() (полный TrackerFile для GET /api/tracker/state фазы 2).

СПИСКИ: lists (сортировка по order, затем по имени), create_list, update_list, delete_list, reorder_lists. Удаление списка НЕ удаляет его задачи — они переезжают в list_id = None; то же с вехами списка.

ЗАДАЧИ: tasks, task, create_task(NewTask), update_task(TaskPatch), set_status, delete_task, reorder_tasks. Перевод в Done ставит done_at, возврат в работу его снимает. delete_task КАСКАДНЫЙ: уносит пункты чек-листа и все отрезки времени задачи (осиротевший отрезок был бы временем ни на что и портил бы суммы). Вехи не трогаются. Приоритет обрезается до 4.

ЧЕК-ЛИСТ: items(task_id) по order, checklist_progress → ChecklistProgress { done, total } с percent(), add_item, update_item, toggle_item, delete_item, reorder_items. Смена done двигает done_at в обе стороны. delete_item СОХРАНЯЕТ время: отрезки остаются у задачи и лишь теряют item_id — человек это время потратил.

ВЕХИ: milestones, create_milestone, update_milestone, delete_milestone.

ТАЙМЕРЫ (главные инварианты фазы):
- start_timer(task_id, item_id, note): если по задаче отрезок УЖЕ идёт, новый НЕ создаётся — возвращается текущий. Двойной клик или повторный POST не раздваивают учёт. Ограничение действует ВНУТРИ задачи: параллельные таймеры по разным задачам штатны.
- stop_timer(task_id) идемпотентен: нет активного → Ok(None). Чужие активные отрезки не трогает.
- stop_entry(entry_id) — остановка конкретного отрезка, уже остановленный возвращается как есть.
- create_entry / update_entry / delete_entry — ручные отрезки задним числом с валидацией stopped_at > started_at по ИТОГОВОМУ состоянию (сдвиг одной границы не должен вывернуть отрезок наизнанку). Второй активный отрезок по той же задаче отбивается как Conflict.
- entries(task_id), all_entries, active_entries (блок // RUNNING), active_entry_for_task.

АГРЕГАЦИИ (делегируют в модуль report, беря read-lock): task_seconds/task_seconds_at, item_seconds_at, report/report_at, gantt/gantt_at, totals/totals_at. Варианты с суффиксом _at принимают явный момент «сейчас» и потому проверяемы тестом.

NewTask — структура входа создания задачи (а не десяток позиционных аргументов: набор полей будет расти при разборе prompt-строки в фазе 4).
