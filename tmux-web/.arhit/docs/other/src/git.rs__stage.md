# src/git.rs::stage

pub async fn stage(cwd: &Path, paths: &[String]) -> Result<()>. Добавляет файлы в git index.

Команда: git add -- <paths>
- '--' separator защищает от путей вида '-foo' (которые git иначе интерпретирует как флаги).
- paths.iter() передаются прямо в .args() — без shell-склейки.

Edge cases:
- paths.is_empty() → ранний return Ok(()) (git без аргументов после -- ругается).
- non-zero exit → bail!('git add failed (exit {:?}): {}', code, stderr.trim()).
