# tmux-web/static/js/echo/stats.js

Echo stats sparkline. Canvas-based, без зависимостей. initStats(pollMs=30000) стартует polling getStats('minute'). updateFromWs({tokens_in_per_min, tokens_out_per_min}) — incremental update от WS StatsUpdate в последний bucket. redraw рендерит две линии (accent для tokens_in, warn для tokens_out) с baseline. Цвета берутся из CSS var(--accent), var(--warn), var(--fg-dim) — реагирует на смену темы. updateSummary в  показывает ↓X ↑Y (с k/M суффиксами).
