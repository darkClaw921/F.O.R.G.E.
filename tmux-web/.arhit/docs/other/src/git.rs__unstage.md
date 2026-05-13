# src/git.rs::unstage

pub async fn unstage(cwd: &Path, paths: &[String]) -> Result<()>. Убирает файлы из git index.

Основная команда: git restore --staged -- <paths>
Если она fail с stderr содержащим 'could not resolve HEAD' или 'no such ref' → fallback на git rm --cached -- <paths>. Это нужно для empty repo без HEAD, где restore --staged не может работать.

Edge cases:
- paths.is_empty() → ранний return Ok(()).
- restore fail НЕ из-за отсутствия HEAD → bail! со stderr.
- fallback fail → bail! со stderr fallback'а.
