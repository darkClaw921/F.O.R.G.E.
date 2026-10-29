# getStats

REST клиент Echo статистики в tmux-web/static/js/echo/api.js. getStats(range) поддерживает hour (60 минутных buckets) и day (24 часовых buckets); опущенный range использует серверный hour. URL encode защищает параметр. Возвращает {range,buckets:[{ts,tokens_in,tokens_out,cache_creation,cache_read}]}; HTTP ошибки проходят через общий call. Пример: await getStats("hour"). minute не допустимый range и возвращает 400.
