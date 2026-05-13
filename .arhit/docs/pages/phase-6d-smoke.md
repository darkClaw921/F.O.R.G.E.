Phase 6.D smoke results (2026-05-09):

1. GET /api/tasks → total=71 ✓
2. WS /ws/tasks connect → first message kind=snapshot ✓
3. br create rt-test (-p2 -t task) → клиент получил {kind:upsert, issue.id=forge-fzw, status=open} в течение <1s ✓
4. br update forge-fzw -s in_progress → клиент получил upsert status=in_progress ✓
5. br close forge-fzw -r 'smoke 6d' → клиент получил upsert status=closed ✓
6. Reconnect WebSocket после kill→start cargo run → новый клиент получил snapshot с total=72 (новая закрытая задача учтена) ✓

Запуск: cargo run в фоне (pid 55499 потом 56961), python3 /tmp/smoke_6d.py использовал websockets 16.0. Watcher properly walked up из tmux-web/ до F.O.R.G.E./.beads (find_beads_dir). После теста: cargo killed, tmux server already off.