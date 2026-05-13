# static/index.html#git-tab-button

Кнопка таба Git в #tab-bar (static/index.html строка 38). Атрибуты: id='tab-git' class='tab-btn' type='button'. Расположена после #tab-tasks. Соседний span #git-status-meta (строка 39) с class='tab-meta' зарезервирован под индикатор статуса (ahead/behind, dirty count). JS-логика обработки клика и переключения на #git pane — Phase 4 (task tw-41w switchTab). Использует существующие .tab-btn стили (Phase 6.A) — фон transparent, hover --bg-input, active --accent. Phase 3 — tw-2tg.
