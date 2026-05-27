# attachCommitHover

Навешивает hover-обработчики на одну черту .gantt-commit (gantt.js). mouseenter: отмена hideTimer + setTimeout(POPOVER_SHOW_DELAY=120мс)→openPopoverFor. mouseleave: отмена showTimer + scheduleHide (POPOVER_HIDE_DELAY=150мс грейс). Вызывается из renderCommits для каждой черты.
