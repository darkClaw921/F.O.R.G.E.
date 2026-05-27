# renderPopover

Рендерит содержимое shared попапа .gantt-commit-popover чистым DOM/textContent (gantt.js). Вход detail (объект|null), hash, fallbackSubject. Очищает el, строит .gantt-popover-head (hash7 + дата fmtDate(ts*1000)), .gantt-popover-author, .gantt-popover-subject (detail.subject или fallback), .gantt-popover-body <pre> при непустом trim (обрезка 800), список .gantt-commit-files с .gantt-file (.gantt-file-status status-<буква> + путь). detail===null → минимальный fallback hash7+subject.
