# ProjectStore::find_any

Метод ProjectStore в tmux-web/src/projects.rs. Ищет проект по id с учётом transient_active.

## Сигнатура
pub fn find_any(&self, id: &str) -> Option<&Project>

## Зачем
ProjectStore::get смотрит только в self.projects (registered). Но активный проект может быть transient — синтетический Project с id вида __path__:<abs-path>, который не сохраняется в реестре. Сущности типа Todo (создаваемые в активном проекте) хранят project_id из active(); если активным был transient — project_id будет __path__:..., и при последующем поиске через get() мы получим None даже если transient ещё активен.

## Алгоритм
1. Если установлен transient_active и его id совпадает с искомым → вернуть его.
2. Иначе вызвать get(id) (поиск по registered).

## Использование
- promote_todo (main.rs ~line 1249): загружаем snapshot Project для todo по todo.project_id. До исправления использовал get() — падал с 500 'project ... is gone' для todo созданных под transient проектом.

## Бажный сценарий до фикса
Юзер открывает приложение с активным transient проектом (forge-cwd без регистрации) → создаёт TODO → переносит в Open → промоут падает с 'project __path__:/Users/.../F.O.R.G.E. for todo ... is gone'.
