# tmux-web/src/tmux.rs

Интеграция с tmux CLI: листинг сессий, создание новой сессии, kill-session, capture-pane, send-keys. Все вызовы — через tokio::process::Command (не блокирует async-runtime). Парсинг строго по формату, заданному в '-F'.

ОСОБЕННОСТЬ tmux-сервера: при отсутствии запущенного сервера tmux выдаёт ошибку 'no server running on /tmp/tmux-1000/default'. Это НЕ ошибка для web-viewer — list_sessions трактует её как 'сессий нет' и возвращает пустой список.

ОСНОВНЫЕ ЭЛЕМЕНТЫ:

- pub struct SessionInfo (Debug, Clone, Serialize, Deserialize, PartialEq, Eq) — метаданные одной tmux-сессии для отдачи во фронтенд:
  * name: String — имя сессии (#{session_name}), уникально в рамках tmux-сервера.
  * id: String — внутренний id (/bin/zsh, , ...).
  * attached: u32 — сколько клиентов сейчас прикреплено к сессии.
  * windows: u32 — количество окон.
  * created: i64 — Unix-таймстамп создания (#{session_created}).
  * path: String — стартовый cwd сессии (#{session_path}).
  * session_group: Option<String> (с #[serde(default)]) — ДОБАВЛЕНО в Phase 1.1 forge-bjm. Имя tmux session-group (#{session_group}). tmux позволяет создавать 'linked' сессии, которые делят одни и те же окна (tmux new-session -t <existing>); все сессии одной группы получают одинаковое значение #{session_group}. Если сессия не входит ни в какую группу — tmux возвращает пустую строку, что мапится в None. Используется attention::watcher_loop для дедупликации — сессии одной группы рендерят одну и ту же логическую работу, поэтому needs_attention=true должен подсвечиваться только у одной из них.

- LS_FORMAT (const &str) — формат для tmux list-sessions -F. Поля разделены '|' в порядке: name | id | attached | windows | created | path | session_group. session_group идёт ПОСЛЕДНИМ намеренно — старый формат без этого поля (6 колонок) остаётся парсибельным для backward compatibility с уже запущенными tmux-серверами.

- pub async fn list_sessions() -> anyhow::Result<Vec<SessionInfo>> — выполняет 'tmux list-sessions -F LS_FORMAT'. Если сервер не запущен → Ok(vec![]). Если tmux отсутствует в /usr/local/opt/node@20/bin:/Users/igorgerasimov/.codeium/windsurf/bin:/Users/igorgerasimov/ydb/bin:/Users/igorgerasimov/yandex-cloud/bin:/Library/Frameworks/Python.framework/Versions/3.12/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin/python3:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/igorgerasimov/ngrokTest:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/local/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/appleinternal/bin:/opt/pkg/env/active/bin:/opt/pmk/env/global/bin:/opt/X11/bin:/Library/Apple/usr/bin:/Applications/Wireshark.app/Contents/MacOS:/Applications/VMware Fusion.app/Contents/Public:/usr/local/share/dotnet:~/.dotnet/tools:/usr/local/go/bin:/Library/Frameworks/Mono.framework/Versions/Current/Commands:/usr/local/opt/node@20/bin:/Users/igorgerasimov/.codeium/windsurf/bin:/Users/igorgerasimov/ydb/bin:/Users/igorgerasimov/yandex-cloud/bin:/Library/Frameworks/Python.framework/Versions/3.12/bin:/Users/igorgerasimov/Library/pnpm:/Users/igorgerasimov/.local/bin:/Users/igorgerasimov/.cargo/bin:/Users/igorgerasimov/.orbstack/bin:/Users/igorgerasimov/.cache/lm-studio/bin:/Users/igorgerasimov/.orbstack/bin:/Users/igorgerasimov/.cache/lm-studio/bin:/Users/igorgerasimov/.claude/plugins/cache/claude-plugins-official/context7/unknown/bin:/Users/igorgerasimov/.claude/plugins/cache/claude-plugins-official/swift-lsp/1.0.0/bin:/Users/igorgerasimov/.claude/plugins/cache/caveman/caveman/c2ed24b3e5d4/bin:/Users/igorgerasimov/.claude/plugins/marketplaces/claude-plugins-official/plugins/frontend-design/bin:/Users/igorgerasimov/.claude/plugins/marketplaces/claude-code-plugins/plugins/frontend-design/bin → Err. Битые строки (несовпадение колонок) пропускаются с warning'ом.

- fn parse_session_line(line: &str) -> Option<SessionInfo> — парсит одну строку tmux ls. Использует splitn(7, '|'). Поля path и session_group опциональны для обратной совместимости со старым форматом (5 или 6 колонок). Пустой session_group мапится в None.

- pub async fn capture_pane(session: &str) -> anyhow::Result<String> — захватывает содержимое pane активного окна сессии. ВАЖНО (Phase 1.4): захват только видимой части pane (без -S -30 history). Это устраняет persistent-true ложные срабатывания, когда старый Claude prompt остаётся в scrollback.

- pub async fn new_session(name, cwd) — создаёт новую сессию tmux с указанной cwd (-c).
- pub async fn kill_session(name) — убивает сессию.
- pub async fn send_keys(session, text) — отправляет ввод в активный pane сессии (используется ботом-уведомителем notifier).

ТЕСТЫ: модуль содержит парсер-тесты для LS_FORMAT включая ok-кейс с session_group (Some), legacy-формат (6 колонок без session_group → None), пустой session_group → None, zero attached, missing path, too few columns, bad numbers, empty name + valid/invalid_session_names для ограничений на имена.

ЗАВИСИМОСТИ:
- anyhow для error-handling.
- serde::{Serialize, Deserialize} для сериализации SessionInfo в API.
- tokio::process::Command — async-spawn tmux.
