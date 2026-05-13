# src/git.rs::log

pub async fn log(cwd: &Path, limit: u32) -> Result<Vec<GitCommit>>. Запускает git log с кастомным форматом и парсит коммиты.

Команда: git log --pretty=format:%H\x1f%h\x1f%P\x1f%an\x1f%ae\x1f%aI\x1f%s\x1f%D\x1e -n <limit>
- \x1f = US (Unit Separator), разделитель полей внутри коммита
- \x1e = RS (Record Separator), разделитель коммитов
- 8 полей: hash %H, abbrev %h, parents %P, author %an, email %ae, date %aI (ISO 8601), subject %s, refs %D

Парсинг:
- stdout split('\x1e') на записи
- trim_start_matches('\n') (git добавляет \n между записями)
- record split('\x1f') → 8 полей
- parents = fields[2].split_whitespace() (пустая строка → vec![] для root commit)
- refs = fields[7].split(', ') (пустая строка → vec![])

Edge cases:
- Пустой репо: stderr содержит 'does not have any commits yet' / 'bad default revision HEAD' / 'ambiguous argument HEAD' → return Ok(vec![]).
- Любая другая ошибка → bail! со stderr.
- Малформированная запись (<8 полей) → tracing::warn и пропускаем.

limit передаётся через -n (не --max-count). Клемп лимита — задача handler'а Phase 2.
