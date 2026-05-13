# Фича: Оранжевая подсветка вкладки при Claude permission prompt

## Цель
Когда в одной из tmux-сессий Claude Code открыл permission prompt (ждёт ответа Yes/No), вкладка этой сессии в сайдбаре tmux-web должна подсвечиваться оранжевым, чтобы пользователь сразу видел где требуется его внимание. Подсветка должна сама исчезать после ответа.

## Реализация

### Phase 1 — Backend infra (epic tw-val)
- **src/tmux.rs::capture_pane** — async-обёртка над 'tmux capture-pane -p -t <session> -S -30'. Захватывает последние 30 строк панели. Stderr-маркеры 'no server running' и 'can.t find session' трактуются как Ok("") (нормальная гонка между list_sessions и capture).
- **src/attention.rs** — новый модуль:
  - AttentionState — Arc<RwLock<HashMap<String, bool>>> с методами new/snapshot/set. Cheaply cloneable.
  - detect_claude_prompt(pane: &str) -> bool — строгий AND-детектор по трём маркерам: '❯ 1. Yes', '2. Yes, and don', 'No, tell Claude'.
  - watcher_loop(projects, attention) — async-loop с шагом 1500мс: list_sessions фильтруется по active project prefix, затем capture_pane + detect → attention.set.

### Phase 2 — Wiring (epic tw-25z)
- **AppState.attention: Arc<AttentionState>** добавлено в src/main.rs.
- **mod attention;** зарегистрирован.
- **tokio::spawn(attention::watcher_loop(...))** запускается в main() — передаются Arc-клоны полей AppState, не AppState целиком (избегаем цикла).
- **SessionDto** = SessionInfo + needs_attention: bool. Хендлер list_sessions делает attention.snapshot() один раз и заполняет dto.

### Phase 3 — Frontend (epic tw-cbt)
- **static/app.js::renderSidebar** — вешает класс .needs-attention на <li> когда s.needs_attention === true. Порядок классов: active затем needs-attention для предсказуемого CSS-каскада.
- **static/style.css** — три селектора: .session-item.needs-attention (фон #3a2010, левая граница #ff8a3d), .session-item.needs-attention .session-name (имя оранжевое #ff8a3d, bold), .session-item.needs-attention.active (более тёмный фон #4a2814 для комбинации). Расположено после .session-item.active в каскаде.

### Phase 4 — Verification & docs (epic tw-s5m)
- cargo check + cargo build --release: clean (только pre-existing warning в src/pty.rs).
- cargo test --test-threads=1: 29 passed, 0 failed (5 attention-тестов: detects_full_prompt, ignores_plain_shell_output, requires_all_three_markers, attention_state_snapshot_and_set, attention_state_is_cheaply_cloneable).

## Тайминг
- Watcher период: 1500мс.
- Frontend polling /api/sessions: 3000мс.
- Итог: оранжевая подсветка появляется/исчезает за ~1.5с (детект) + до ~3с (polling) = до ~4.5с. План указывает ~5с.

## Manual e2e (для пользователя)
1. Запустить tmux-web (cargo run --release).
2. В активном проекте (с настроенным tmux-prefix) создать 2 tmux-сессии.
3. В одной запустить claude и довести до permission prompt (например, попросить выполнить команду).
4. Открыть веб-UI — вкладка с Claude должна стать оранжевой за ~5с.
5. Ответить на prompt в Claude — оранжевый исчезнет за ~5с.
6. Negative: открыть обычный shell — оранжевый НЕ должен появиться.

## Файлы
- src/tmux.rs (capture_pane)
- src/attention.rs (новый модуль)
- src/main.rs (AppState, SessionDto, mod attention, spawn watcher)
- static/app.js (renderSidebar)
- static/style.css (.needs-attention селекторы)

## План
/Users/igorgerasimov/.claude/plans/wild-munching-thimble.md