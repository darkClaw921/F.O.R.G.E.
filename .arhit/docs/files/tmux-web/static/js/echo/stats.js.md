# tmux-web/static/js/echo/stats.js

Статистика Echo: initStats запускает polling каждые 30 секунд, stopStats отменяет его. pollOnce запрашивает GET /api/echo/stats?range=hour и читает buckets, а не items. Ответ содержит 60 хронологических минутных точек {ts,tokens_in,tokens_out,cache_creation,cache_read}; renderer берёт tokens_in/out, дополняет короткий набор нулями слева до 60 точек, суммирует последние точки в статус ↓/↑. Cache counters не входят в показанные суммы. updateFromWs сохраняет инкрементальный текущий bucket. Ошибки сети логируются debug без toast; chart не критичен.
