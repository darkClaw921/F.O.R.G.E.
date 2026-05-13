# CommitReq

Body-структура для POST /api/git/commit. CommitReq { message: String }. derive(Debug, Deserialize). message передаётся в git commit -m как есть; multi-line OK. Пустые / whitespace-only сообщения отклоняются на уровне handler'а (400 'empty message').
