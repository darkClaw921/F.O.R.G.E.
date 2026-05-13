# updateTask

JS PATCH /api/tasks/:id. Принимает (id, payload-patch). applyOptimisticPatch обновляет issue in-place в state.tasksData (status/title/priority/description/labels), renderTasks. После ответа сервера применяет полученные поля поверх существующего объекта (Object.assign). При ошибке — rollbackIssue к prev. payload.labels — csv-строка, конвертится в массив для optimistic-вью.
