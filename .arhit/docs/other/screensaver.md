# screensaver

Модуль static/js/screensaver/screensaver.js — ASCII-заставка «Таверна дворфов». Полноэкранная вью #screensaver в #main, открывается кнопкой 🍺 (#screensaver-toggle) в #settings-bar ВМЕСТО области сессий, по образцу #daily-summary.

РЕНДЕР — единый ASCII-композитор: вся сцена (интерьер таверны ИЗНУТРИ + 5 столов + 10 дворфов) рисуется в ОДНУ моноширинную сетку COLS×ROWS (100×34) в одном <pre id=ss-screen>. Слои: статичный интерьер (база, buildInterior) → анимированный ambient (пламя камина/свечи/дым, renderAmbient) → бюсты дворфов (renderFrame по копии базы). Единая сетка даёт идеальное выравнивание «дворфы сидят за столами». Примитивы рисования: newGrid/cloneGrid/setCh(пробел=прозрачно)/box/hline/vline/paintText/blit.

ИНТЕРЬЕР (buildInterior): внешние стены (двойная рамка), потолочные балки, вывеска THE BROKEN BUILD TAVERN, окна с луной/звёздами, полки с бутылками, баннер BUILD GUILD, доска-меню (Эль/Мёд/Рагу/Rollback FREE), камин с живым пламенем (FIRE) + кот, висячие фонари, бочки в углах, пол. Анимируемые зоны фиксируются в FIRE/CANDLES/SMOKE.

ДВОРФЫ: 8 вариантов детализированных бюстов (makeBust: шлемы рогатые/остроконечные/крылатые/корона, разные глаза/носы/бороды, аксессуары — трубка, шрам), 2 кадра idle/drink (кружка U= едет от стола к лицу). placeDwarves раскладывает 10 дворфов за 5 столами (drawTable). Каждый дворф: bbox {r,c,w,h}, headRow/headCol, состояние реплики.

РЕПЛИКИ: ~55 LINES (дев-юмор F.O.R.G.E.: билды/CI/коммиты/баги/tmux/Claude/Rust) + ~28 RETORTS (ответы соседа). autoSpeak по таймеру, клик по дворфу (onDwarfClick) — хит-тест: пиксель→ячейка(row,col)→поиск дворфа по bbox. Сосед иногда отвечает (forcedLine).

ОБЛАЧКА: отдельные .ss-bubble в слое .ss-bubbles; позиция в пикселях по координате головы (positionBubbles через getBoundingClientRect относительно сцены — учитывает transform центрирования). Размер сцены подгоняется fitFont (вписывает COLS×ROWS в #ss-stage), cellW измеряется probe-span (measureCell). Пересчёт на resize.

ЦИКЛ/ОСТАНОВКА: requestAnimationFrame с троттлингом RENDER_MS≈120мс (~8fps), перерисовка <pre> только при смене кадра (_lastFrameStr). hideScreensaver отменяет rAF и listener'ы; self-guard (offsetParent===null); Esc; visibilitychange; prefers-reduced-motion замораживает кадры. Внешние хуки: ws/attach.js (ws.onopen) и tabs/tabs.js (switchTab) вызывают hideScreensaver(). Экспорт: showScreensaver/hideScreensaver/closeScreensaver(→fetchSessions). XSS: текст через textContent.
