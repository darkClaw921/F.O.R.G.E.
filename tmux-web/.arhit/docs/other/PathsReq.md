# PathsReq

Body-структура для POST /api/git/stage и POST /api/git/unstage. PathsReq { paths: Vec<String> }. derive(Debug, Deserialize). paths — список относительных путей внутри активного проекта. Переиспользуется обоими хендлерами (stage и unstage). Пустой paths отклоняется на уровне handler'а (400).
