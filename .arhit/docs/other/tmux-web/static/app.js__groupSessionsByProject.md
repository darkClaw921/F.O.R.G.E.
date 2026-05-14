# tmux-web/static/app.js::groupSessionsByProject

Phase 6 helper — группирует массив сессий по project_id внутри одного origin'а. Сессии с project_id=null/undefined попадают в ключ orphanKey (по умолчанию '__orphan__'). Внутри каждой группы сортирует сессии по name.localeCompare() для стабильного порядка. Возвращает Map<key, SessionDto[]>. Выделена в отдельную функцию, чтобы (а) переиспользовать логику группировки в renderOriginSection и (б) дать регресс-тестам (cca8.2) стабильный контракт для проверки структуры. Экспортирована в window.__forge.groupSessionsByProject.
