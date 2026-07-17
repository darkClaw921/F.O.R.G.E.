# alloc_worktree_name

Функция tmux-web/src/worktree.rs. Подбирает свободное имя новой рабочей копии и возвращает (имя, путь).

Назначение: генерирует уникальное имя каталога для worktree по схеме именования фичи. Базовое имя — 'wt-<secs>', где secs — текущее Unix-время в секундах (SystemTime::now().duration_since(UNIX_EPOCH); при ошибке 'часы до эпохи' используется 0). Если каталог base.join(name) уже существует, добавляется числовой суффикс: 'wt-<secs>-2', 'wt-<secs>-3', … до первого свободного. Практически коллизия возможна лишь при нескольких worktree, созданных в одну и ту же секунду.

Сигнатура: pub fn alloc_worktree_name(base: &Path) -> (String, PathBuf) (синхронная).

Параметры:
- base: каталог-контейнер рабочих копий, обычно <toplevel>/.forge-worktrees (см. worktrees_base).

Возврат: кортеж (dir_name, full_path), где dir_name — только имя каталога ('wt-…'), а full_path == base.join(dir_name) — абсолютный путь будущей копии, готовый для worktree_add.

Схема имён фичи: dir_name='wt-<ts>' → имя ветки 'forge/<dir_name>' (forge/wt-<ts>) → имя tmux-окна dir_name.replacen('wt-','wt:',1) = 'wt:<ts>'. Эту связку формирует вызывающий create_worktree_window. Покрыта unit-тестами (префикс wt-, соответствие пути, суффикс при коллизии).
