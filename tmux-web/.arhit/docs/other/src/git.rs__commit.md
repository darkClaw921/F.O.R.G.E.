# src/git.rs::commit

pub async fn commit(cwd: &Path, message: &str) -> Result<String>. Создаёт git-коммит с сообщением.

Команда: git -c commit.gpgsign=false commit -m <message>
- '-c commit.gpgsign=false' критично для headless-окружений: иначе git может запустить gpg --sign и зависнуть на интерактивном passphrase prompt.
- '-m' гарантирует non-interactive (никакого editor).

Возврат: abbrev hash из stdout.
- Формат stdout: '[branch abc1234] subject' или '[branch (root-commit) abc1234] subject'.
- Парсинг: ищем '[' и ']', берём substring inside, split_whitespace().last() — это abbrev hash.
- Если parsing fail → bail!('git commit succeeded but stdout has unexpected format: {}', stdout.trim()).

Edge cases:
- 'nothing to commit' (в stdout или stderr) → bail!('nothing to commit') (читаемая ошибка для frontend).
- Pre-commit hook fail или другая ошибка → bail!('git commit failed (exit {:?}): {}', code, stderr).
- Если stderr пуст, в сообщение об ошибке идёт stdout (некоторые ошибки git пишут только в stdout).

ВАЖНО: НЕ тестировать вызовом локально — правило проекта запрещает git add/commit от агента (CLAUDE.md). Ручная проверка пользователем.
