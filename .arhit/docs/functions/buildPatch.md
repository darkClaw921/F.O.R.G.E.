# buildPatch

Тело PATCH карточки задачи трекера: только изменённые поля (tmux-web/static/js/tracker/modals.js).

ЧИСТАЯ функция: форму описывает объект form, задачу до правки — task. Здесь живёт разница «не передали» / «передали null» (Option<Option<T>> + some_option в plugins/tracker/src/models.rs): пустое поле означает СБРОС и уезжает как null, неизменённое не уезжает вовсе. Именно это удерживает инвариант «частичное редактирование не затирает соседние поля».

Поля: title (непустой и изменившийся), list_id, status, priority, notes, estimate_min (через estimateValue), start (fromDateInput), due (fromDueInput), tags.

ТЕГИ. С появлением редактора чипов form.tags — это МАССИВ строк из tagEditor.getTags(); строка тоже принимается (старая форма поля '#тег #тег', prompt-строка, e2e) и разбирается splitTags из js/tracker/tags.js. Сравнение по tags.join(' ') со старым списком: PATCH уходит только при фактическом изменении, порядок значим.
