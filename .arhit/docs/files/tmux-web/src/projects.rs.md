# tmux-web/src/projects.rs

Multi-project registry для tmux-web. Хранит список проектов и id активного, персистится в ~/.config/forge/projects.json (atomic write через tempfile + rename). При первом старте если файла нет — создаёт дефолт с одним проектом forge (path = std::env::current_dir, tmux_prefix = 'forge').

## Типы
- Project { id, name, path: PathBuf, tmux_prefix: String } — один проект. id — slug от name ([a-z0-9_-]+). tmux_prefix используется для фильтрации tmux-сессий (пустой = все, ровно prefix или prefix-* = принадлежит проекту).
- ProjectsFile { projects, active_project_id } — JSON envelope для файла.
- ProjectStore { file_path, projects, active_id } — in-memory реестр с инвариантом: active_id всегда указывает на существующий проект.

## Методы ProjectStore
- load(file_path) -> Self — читает файл; при отсутствии создаёт дефолт + сохраняет. Чинит stale active_id.
- save() -> anyhow::Result<()> — атомарная запись через <file>.tmp + rename (POSIX-атомарно).
- list() -> Vec<Project> — копия списка.
- get(id) -> Option<&Project>.
- active() -> &Project — гарантированно существует.
- active_id() -> &str.
- add(name, path, tmux_prefix?) -> Result<Project> — id=slug(name), prefix default = id; дубликаты → Err.
- remove(id) -> Result<bool> — нельзя удалить активный.
- set_active(id) -> Result<()> — несуществующий id → Err.

## Свободные функции
- default_registry_path() -> Result<PathBuf> — $HOME/.config/forge/projects.json (без крейта dirs).
- slugify(name) -> String — lower-case, [a-z0-9_-], всё прочее → '-' с схлопыванием.
- session_belongs(prefix, session_name) -> bool — пустой prefix матчит всё; иначе name == prefix или name начинается с prefix-.
- ensure_prefixed(prefix, name) -> String — добавляет prefix- если ещё не префиксован.

## Зависимости
- anyhow, serde/serde_json, tracing, std::path. Без новых внешних крейтов.

## Конкурентность
Не Clone; в AppState — Arc<RwLock<ProjectStore>>.

## Тесты (cargo test projects::)
- slugify_basic, session_belongs_rules, ensure_prefixed_rules — pure functions.
- load_save_roundtrip — bootstrap + add + save + reload.
- set_active_and_remove — нельзя удалить активный, set_active валидирует id.
- add_duplicate_rejected.
