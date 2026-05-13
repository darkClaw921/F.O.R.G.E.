# tmux-web/src/todos.rs

Phase 1 backend foundation. Хранилище TODO-карточек проекта (storage column kanban-доски, до promote'а в bd-task).

## Модель
- Todo: id (UUID v4 String), project_id, title, description (Option<String>), priority (u8, 0..=4, default 2), issue_type (String, default 'task'), labels (Vec<String>), created_at, updated_at (RFC3339-строки UTC). Все опциональные поля #[serde(default)] для совместимости.
- TodoStore: Arc<RwLock<Inner>>, lazy-load из <project_root>/.forge/todos.json. Cheap-clonable.

## API
- new(project_root: PathBuf) -> Result<Self> — создаёт .forge/, грузит todos.json (если есть).
- list(project_id: &str) -> Vec<Todo>.
- get(id: &str) -> Option<Todo> (поиск по всем проектам, id глобально уникален).
- create(project_id, title, description) -> Result<Todo> — генерит UUID v4, ставит created_at=updated_at=now.
- update(id, Option<title>, Option<Option<description>>) -> Result<Option<Todo>> — двойной Option у description: None=не трогать, Some(None)=очистить, Some(Some(s))=записать.
- delete(id) -> Result<bool>.
- save() — экстренный flush.

## Persistence
Atomic save через tempfile + rename (как в projects::ProjectStore::save). На POSIX rename атомарен в пределах одного mount-point.

## Время
RFC3339 формируется вручную (без chrono) через алгоритм Howard Hinnant. Формат: YYYY-MM-DDTHH:MM:SS.sssZ.

## Ограничения
- Save держит lock на время IO — приемлемо для масштабов сотен TODO.
- Поиск get() — O(N) по всем проектам. Для крупных каталогов имеет смысл добавить вторичный индекс id→Vec<Todo>.
