# cli::run_remote

Реализация CLI подкоманды devforge remote (Phase 2).

## Назначение
CLI-управление локальным реестром remote-серверов (~/.config/forge/remote_servers.json). Работает ВСЕГДА — независимо от того, запущен ли devforge с --remote или нет. Через эту команду пользователь регистрирует удалённые devforge-инстансы, к которым потом можно подключаться (в Phase 3+ — через ?server=ID в API/UI).

## Подкоманды
### devforge remote list (alias: ls)
Табличный вывод реестра: ID | LABEL | URL. Токен НЕ печатается. Если реестр пуст — печатает подсказку, как добавить.

### devforge remote add URL --token HEX [--label NAME]
Добавляет запись в реестр. Логика:
- Если --label не указан — выводится из host-а URL (derive_label_from_url).
- Вызывает store.add(label, url, token), который генерит id через slugify(label) с дедупликацией.
- Atomic save через store.save().
- Печатает ID/Label/URL добавленной записи.

### devforge remote remove ID (aliases: rm, delete)
Удаляет запись. Возвращает ошибку, если id неизвестен. Atomic save после remove.

## CLI парсинг
parse_remote() диспатчит на parse_remote_add() при -add-. Принимает форму как --token=value, так и --token value. Positional URL — первый non-flag. Все unknown flags отлавливаются.

## Архитектура
- Команды работают НАПРЯМУЮ с remote_servers.json через RemoteServerStore — НЕ через REST API локального devforge. Это сознательное решение: не требуется запущенный сервер.
- Парсинг отдельным parse_remote() — в parse_from() при first arg == remote. Это нужно чтобы --token/--remote не путались с глобальными флагами runner-а.

## Зависит от
- crate::remotes::RemoteServerStore — реализация store.
- crate::remotes::default_remotes_path() — путь к файлу.

## Связи (см. также)
- RemoteServerStore::add / remove / save — низкоуровневые операции (документация в remotes.rs).
- /api/remote-servers — параллельный REST интерфейс (только в remote_mode).
