# closeTask

JS DELETE /api/tasks/:id?reason=... . Optimistic: status → closed (карточка переезжает в колонку closed). На ошибку — rollback. Без тела ответа (204). Reason пробрасывается через querystring.
