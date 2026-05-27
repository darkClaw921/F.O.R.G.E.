Hover-попап деталей git-коммита на гант-диаграмме вкладки Tasks (tmux-web/static/js/tasks/gantt.js + tmux-web/static/css/tasks.css). Заменяет нативный title богатым попапом с метаданными коммита и списком изменённых файлов.

АРХИТЕКТУРА (чистый DOM, без библиотек):
- Один shared элемент .gantt-commit-popover на всю страницу (ленивая ensurePopover(), аппендится в document.body, переиспользуется всеми чертами). На нём mouseenter (отменяет скрытие) / mouseleave (планирует скрытие).
- Module-level кэш detailCache: Map<hash, detail|null> — кэширует и успешный detail, и null (отсутствие/ошибку), чтобы не повторять fetch.
- Таймеры showTimer (показ) и hideTimer (скрытие); popoverToken — монотонный счётчик против гонки fetch (устаревший ответ не перерисует попап).
- Константы: POPOVER_SHOW_DELAY=120мс, POPOVER_HIDE_DELAY=150мс, POPOVER_GAP=10px, POPOVER_MARGIN=8px.

ПОТОК:
- renderCommits проставляет каждой .gantt-commit dataset.hash (полный sha) и dataset.subject, оставляет нативный title как мгновенный fallback, и вызывает attachCommitHover(line).
- attachCommitHover: mouseenter → отмена hideTimer + setTimeout(120мс) → openPopoverFor(line); mouseleave → отмена showTimer + scheduleHide().
- openPopoverFor: hash из dataset; если detailCache.has(hash) — мгновенный renderPopover из кэша; иначе renderPopover(null,...) (fallback hash7+subject) + fetchCommitDetail(hash) и по ответу (с проверкой token) перерисовка.
- fetchCommitDetail: GET /api/git/commit?path=<enc cwd>&hash=<enc hash> (path только если sessionCwdOrNull() не null); возвращает json.commit (объект|null); non-ok/исключение → null + warn.
- renderPopover: всё через textContent (никакого innerHTML с git-данными). Содержимое: head (hash7 моноширинный + дата fmtDate(ts*1000)), author, subject (жирн), body в <pre> при непустом trim (обрезка до 800 симв), список .gantt-commit-files → .gantt-file (.gantt-file-status status-<перваяБуква статуса в upper> + .gantt-file-path). detail===null → минимальный fallback hash7+subject.
- positionPopover: position:fixed, координаты от getBoundingClientRect черты; попап справа от черты, при нехватке места слева; кламп по window.innerWidth/innerHeight с POPOVER_MARGIN.

CSS (tasks.css, только переменные темы): .gantt-commit-popover (bg --bg-elev, border --border-input, box-shadow --shadow-pill, max-width 360px, max-height 50vh overflow auto, z-index 1000, [hidden]→display:none). Статусы файлов: status-A=--success, status-M=--info, status-D=--danger, status-R/status-C=--fg-dim (в теме нет --warning). .gantt-commit получил hover-стейт (cursor:pointer, утолщение 2px→4px, --accent-hover).