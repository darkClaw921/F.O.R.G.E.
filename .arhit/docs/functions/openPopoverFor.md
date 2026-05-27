# openPopoverFor

Показывает hover-попап для черты коммита .gantt-commit (gantt.js). hash/subject из dataset. Если detailCache.has(hash) — мгновенный renderPopover из кэша + positionPopover; иначе ++popoverToken, renderPopover(null) (fallback hash7+subject) + positionPopover, затем fetchCommitDetail(hash): кэширует ответ, и если token актуален и попап не скрыт — перерисовывает. Ошибка fetch → detailCache.set(hash,null).
