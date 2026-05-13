# createTask

JS-функция в app.js (IIFE state). POST /api/tasks с payload {title, description?, type?, priority?, labels?, parent?}. Optimistic UI: prepend временного issue с id 'tmp-<rand6>' и __optimistic=true в state.tasksData.issues, renderTasks. После ответа — replace placeholder реальным issue (по найденному idx или unshift если не нашли). На non-ok response или fetch-исключении — alert + удаление tmp-issue. Возвращает созданный issue или null. Используется openCreateModal.
