# is_dangerous

Проверка текста запроса на опасные подстроки для авто-апрува (tmux-web/src/auto_approve.rs). Область проверки берёт danger_scope; None означает «проверять нечего» и трактуется как «не опасно». Текст нормализуется normalize_ws и приводится к нижнему регистру. Источники подстрок: встроенный DEFAULT_DANGEROUS плюс UserSettings::auto_approve_blocklist (дополнение, не замена).
