# src/git.rs

Backend git-модуль для интеграции с git CLI через subprocess. Все вызовы — tokio::process::Command для async-runtime. 

Структуры (все pub, derive Serialize):
- GitStatus { repo: bool, branch: Option<String>, head: Option<String>, upstream: Option<String>, ahead: u32, behind: u32, clean: bool, files: Vec<GitFile> } — сводка статуса git-репо. repo=false для не-git папок.
- GitFile { path: String, orig_path: Option<String>, x: char, y: char, staged: bool, kind: &'static str } — один entry из porcelain v2. orig_path заполнен для renamed/copied. kind: modified/added/deleted/renamed/copied/untracked/conflict/ignored/unknown.
- GitCommit { hash: String, abbrev: String, parents: Vec<String>, author: String, email: String, date: String, subject: String, refs: Vec<String> } — один коммит из git log.

Публичные функции:
- pub async fn status(cwd: &Path) -> Result<GitStatus> — git status --porcelain=v2 --branch -z, парсинг через split на NUL-байты. Сначала проверяет git rev-parse --is-inside-work-tree; не репо → GitStatus{repo:false,..}.
- pub async fn log(cwd: &Path, limit: u32) -> Result<Vec<GitCommit>> — git log с --pretty=format и разделителями \x1f (US, поля) / \x1e (RS, записи). Пустой репо (does not have any commits yet / bad default revision / ambiguous argument HEAD) → Ok(vec![]).
- pub async fn stage(cwd: &Path, paths: &[String]) -> Result<()> — git add -- <paths>.
- pub async fn unstage(cwd: &Path, paths: &[String]) -> Result<()> — git restore --staged -- <paths>; на пустом репо без HEAD fallback на git rm --cached.
- pub async fn commit(cwd: &Path, message: &str) -> Result<String> — git -c commit.gpgsign=false commit -m <msg>. Возвращает abbrev hash из stdout '[branch abc1234] subject'. nothing-to-commit → bail!('nothing to commit').

Приватные helpers:
- async fn is_inside_work_tree(cwd) — проверяет git rev-parse.
- fn parse_status_v2(stdout: &[u8]) -> Result<GitStatus> — парсер porcelain v2 -z.
- fn parse_entry_1/2/unmerged — парсеры конкретных типов entry.
- fn classify(x, y) -> &'static str — XY-коды → kind с приоритетами untracked > conflict > rename/copy > add > delete > modify.

Зависимости: anyhow (Context, Result, bail), serde::Serialize, tokio::process::Command, std::path::Path, tracing (warn). Подключён в src/main.rs как 'mod git;' с #[allow(dead_code)] до Phase 2.
