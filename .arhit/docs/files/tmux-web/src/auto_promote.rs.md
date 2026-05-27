# tmux-web/src/auto_promote.rs

Подготовительный модуль фичи «авто-промоут TODO по очереди» (эпик forge-83eb). Содержит ТОЛЬКО in-memory типы состояния цепочки; фоновый воркер run() добавит Фаза 4b (forge-83eb.6). Вынесен ДО рефакторинга promote_todo_core (Фаза 3, forge-83eb.5), чтобы разорвать цикл зависимостей: promote_todo_core будет писать в AutoChainMap, поэтому тип и поле AppState.auto_chain обязаны существовать раньше.

## Назначение
Пользователь помечает TODO-карточку флагом auto_promote (см. todos::Todo, добавлено в Фазе 1.1). После закрытия текущей задачи цепочки фоновый воркер автоматически промоутит следующую верхнюю TODO с этим флагом — без ручного нажатия «promote».

## Типы
- pub struct AutoChainEntry { active_task_id: String, session: Option<String> } (derive Debug, Clone). Запись о текущей «голове» цепочки авто-промоута для одного root_path.
  * active_task_id — bd-id последней промоутнутой задачи цепочки (ручной или авто). При её закрытии воркер (Фаза 4b) промоутит следующую верхнюю TODO с флагом auto_promote.
  * session — tmux-сессия для уведомления о новой задаче, протягивается по цепочке (наследуется от ручного промоута, запустившего цепочку). None = фолбэк на cfg.session из notifier_config::NotifierConfig.
  Помечена #[allow(dead_code)] — поля читаются в Фазе 3 (promote_todo_core пишет голову) и 4b (run читает её).
- pub type AutoChainMap = Arc<RwLock<HashMap<String, AutoChainEntry>>>. Отображение root_path -> AutoChainEntry — состояние всех активных цепочек. Ключ — строковый путь корня (paths::resolve_root от cwd сессии), т.к. TODO привязаны к корню, а не к проекту (концепция Project удалена, remove-projects-concept.md). Cheap-clonable.

## Состояние и persist
Состояние держится ТОЛЬКО в памяти — persist на диск не делается (осознанное MVP-решение). При рестарте процесса цепочка обнуляется; self-heal происходит при следующем ручном promote, который заново запишет голову. Терять при рестарте нечего критичного: незавершённые TODO остаются в todos.json.

## Concurrency
AutoChainMap — Arc<RwLock<HashMap<..>>>: дёшево клонируется (Arc внутри), кладётся в AppState.auto_chain и шарится между HTTP-handler'ами (promote_todo_core берёт write-lock при фиксации головы) и фоновым воркером (Фаза 4b: run, который читает голову, ждёт закрытия задачи и промоутит следующую). RwLock допускает параллельные чтения и сериализованные записи; мутации редки (раз на промоут).

## Связи
- main.rs: mod auto_promote; поле AppState.auto_chain: auto_promote::AutoChainMap инициализируется как Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())) (std::sync::RwLock явно, т.к. в main.rs RwLock импортирован из tokio::sync). Поле помечено #[allow(dead_code)] до Фазы 3.
- Будущие зависимости: todos::Todo.auto_promote (флаг карточки), notifier_config::NotifierConfig.session (фолбэк сессии), promote_todo_core (Фаза 3, пишет голову), auto_promote::run (Фаза 4b, читает/мутирует).

## Зависимости
std::collections::HashMap, std::sync::{Arc, RwLock}.
