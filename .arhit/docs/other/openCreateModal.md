# openCreateModal

JS modal-builder для создания задачи. Принимает preset (опц. {status}). Поля: title (input), description (textarea), priority (select 0-4), type (select task/bug/feature/epic/chore/docs/question), labels (input csv). Кнопки Cancel и Create. Если preset.status задан и не 'open' — после создания отправляет follow-up updateTask(created.id, {status: preset.status}) чтобы карточка попала в нужную колонку (br create без -s даёт open). Также вызывается из col-add (+) кнопок в kanban-headers.
