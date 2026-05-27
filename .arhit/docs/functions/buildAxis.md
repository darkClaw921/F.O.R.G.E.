# buildAxis

Строит верхнюю ось гант-диаграммы с равномерными засечками .gantt-tick (gantt.js). buildAxis(t0,t1): span=t1-t0; число засечек ticks по числу дней (days<=MAX_TICKS=12 -> max(MIN_TICKS=6, ceil(days)), иначе MAX_TICKS). Формат текста засечки выбирается по масштабу окна: если span<=2*DAY_MS (дневные диапазоны 'today'/'yesterday') — fmtAxisTime(ms) (HH:MM), иначе fmtAxisDate(ms) (день/месяц, для 7д/30д/'all'). Крайняя правая граница (100%) опускается, чтобы подпись не вылезла за край. Возвращает div.gantt-axis (CSS position:sticky, прилипает к верху #gantt-canvas).
