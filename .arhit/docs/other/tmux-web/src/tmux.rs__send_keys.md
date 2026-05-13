# tmux-web/src/tmux.rs::send_keys

Phase 1. Async-функция отправки текста в tmux-сессию через 'tmux send-keys'.

Сигнатура: pub async fn send_keys(session: &str, text: &str) -> anyhow::Result<()>

## Поведение
1. Валидирует имя сессии через is_valid_session_name (только [A-Za-z0-9_-]+, непустое).
2. Если text пуст — Ok(()) без действий.
3. Многострочный text разбивается по '\n'. Для каждой строки:
   - tmux send-keys -t <session> -l <line> (флаг -l = literal, без интерпретации escape).
   - Затем отдельный tmux send-keys -t <session> Enter.
4. Не запускает shell — все аргументы передаются через Command::args. Безопасно для произвольных пользовательских строк (нет SQL/shell-injection).
5. На любой не-нулевой exit от tmux возвращает Err с stderr.

## Использование
Вызывается из notifier'а (Phase 2) при доставке текста промоутнутого TODO в указанную tmux-сессию.
