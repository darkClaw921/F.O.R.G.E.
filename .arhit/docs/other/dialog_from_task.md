# dialog_from_task

dto::dialog_from_task(&TaskDto) -> Option<DialogDto> (plugins/bitrix/src/dto.rs) — строка списка диалогов для чата задачи, которого нет в im.recent.* (в чате давно не писали). id=chat{chatId}, kind=task, title=название задачи, date_update=activity_date (или created_date), entity_type=TASKS_TASK, task_id/entity_id=id задачи, can_write=true, unread_count=0 (будь непрочитанное — чат был бы в im.recent.get и победил бы при склейке). None — у задачи нет чата (Р20). Используется get_dialogs для задач из tasks.task.list{MEMBER}.
