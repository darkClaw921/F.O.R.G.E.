# tmux::capture_pane

Захватывает stdout команды tmux capture-pane -p -t <session> -S -30 (последние 30 строк панели). Используется attention::watcher_loop для детекции Claude permission prompt.

Сигнатура: pub async fn capture_pane(session: &str) -> anyhow::Result<String>

Особое поведение: трактует stderr-маркеры 'no server running' и 'can't find session' как Ok(String::new()), а не Err — это нормальная гонка между tmux::list_sessions и capture-pane (сессия может исчезнуть между двумя вызовами). Прочие сбои (отсутствие tmux в PATH, нечитаемая stderr) — Err.

Реализация через tokio::process::Command (async, не блокирует runtime). Файл: src/tmux.rs.
