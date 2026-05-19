# attention::AttentionState

Разделяемое состояние watcher-флагов для tmux-сессий, src/attention.rs. Cheaply cloneable (Arc).

Содержит ТРИ независимых HashMap под Arc<RwLock>:
- map: HashMap<session_name, bool> — флаг 'нужно внимание' (Claude permission/plan/question prompt). Пишется detect_claude_prompt из watcher_loop через .set(). Читается get_sessions через .snapshot() и пробрасывается во фронт как SessionDto.needs_attention.
- generating: HashMap<session_name, bool> — флаг 'идёт генерация'. true означает, что за прошедший тик (1.5с) hash содержимого последних 30 строк pane отличается от сохранённого. Пишется update_generation(). Читается через generating_snapshot() и пробрасывается во фронт как SessionDto.is_generating. Frontend подсвечивает пульсирующим значком .claude-spark.
- last_hash: HashMap<session_name, u64> — сохранённый хэш capture_pane_full(name, 30) с предыдущего тика. Внутреннее поле, наружу не отдаётся. Используется только update_generation для сравнения.

Методы:
- new() / Default — создаёт пустое состояние.
- snapshot() -> HashMap<String, bool> — owned копия map.
- set(&str, bool) — пишет в map (не удаляет ключи при false).
- generating_snapshot() -> HashMap<String, bool> — owned копия generating.
- update_generation(&str, u64) -> bool — атомарно: читает last_hash[name], сохраняет current_hash, считает is_gen = (prev != current) либо false при первом наблюдении, пишет в generating[name]. Возвращает финальный флаг.

Семантика 'первого тика': при первом наблюдении сессии generating=false (нет точки сравнения), но хэш сохраняется. Это предотвращает ложное срабатывание при появлении новой сессии.
