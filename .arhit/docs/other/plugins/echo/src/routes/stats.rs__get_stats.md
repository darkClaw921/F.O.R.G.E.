# plugins/echo/src/routes/stats.rs::get_stats

GET /api/echo/stats?range=hour|day. range=hour: 60 минутных bucket'ов от now-59 до now (заполнение нулями для пустых минут). range=day: 24 часовых bucket'а, каждый = сумма 60 минутных. Response: {range, buckets: [{ts, tokens_in, tokens_out, cache_creation, cache_read}]}. Bad range → 400.
