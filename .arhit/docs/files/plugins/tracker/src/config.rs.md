# plugins/tracker/src/config.rs

TrackerConfig — конфигурация плагина forge-tracker.

Фаза 1 знает ровно один параметр — путь к файлу хранилища, и это сделано намеренно: конфигурация должна оставаться КОНСТРУКТОРОМ, а не глобальным состоянием, иначе тесты начнут писать в живой ~/.config/forge/tracker.json пользователя.

- TrackerConfig::with_store_path(path) — явный путь; им пользуются тесты, подставляя tempfile::tempdir().
- TrackerConfig::from_home() — боевой ~/.config/forge/tracker.json (ошибка, если HOME не задан).
- TrackerConfig::store_path() → &Path.
- default_store_path() — свободная функция того же пути; STORE_FILE_NAME = "tracker.json".

TOML-конфига у трекера нет: секретов и токенов он не хранит, включение делает флаг хоста --tracker (фаза 3), всё остальное — данные внутри самого стора. Файл один и глобальный: задачи трекера не привязаны к проекту.
