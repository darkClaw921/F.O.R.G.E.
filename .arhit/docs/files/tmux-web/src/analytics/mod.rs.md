# tmux-web/src/analytics/mod.rs

Точка входа модуля аналитики Claude Code: хранилище AnalyticsStore и публичные DTO роутов /api/analytics/* (Фаза 3), плюс подмодули parse / rollup / scan / cache (Фазы 1-2).

AnalyticsStore — то, что лежит в AppState. Cheap-clonable Arc-обёртка: клон это Arc::clone, ни индекс, ни роллапы не копируются. Внутри tokio RwLock<Data> и Arc<ScanProgress>. Data = {index: AnalyticsIndex, files: путь → SessionRollup, updated_at_ms, scanning, ready, last_error}. Счётчики прогресса живут отдельно в ScanProgress (атомарные), а не под локом: хендлер status читает их не вставая в очередь за пишущим локом скана, иначе прогресс переставал бы обновляться ровно тогда, когда он нужен. Пути projects_dir и cache_path — Option: отсутствие $HOME делает аналитику пустой, но не роняет процесс, а юнит-тесты main.rs создают store с обоими None, чтобы не трогать домашний каталог.

Методы:
- new(projects_dir, cache_path) / with_default_paths() — ~/.claude/projects и ~/.forge/analytics-cache.json.
- init_lazy() — вызывается один раз при сборке AppState. Читает ТОЛЬКО снимок кэша через spawn_blocking; каталог транскриптов не открывает ни разу. Снимка нет, он битый или другой версии — состояние остаётся Idle, это не ошибка.
- ensure_scan(full) -> bool — запускает фоновый проход, если он нужен и не идёт; хендлер не ждёт завершения. full=true сбрасывает индекс и роллапы (полный пересбор).
- rescan() -> StatusDto — удаляет снимок кэша, ensure_scan(true), сразу возвращает статус.
- status() / projects() / summary(&SummaryParams) — ответы строятся ЦЕЛИКОМ по индексу в памяти, без обращений к диску.

Ключевые решения (ломать нельзя):
1. ЛЕНИВАЯ ИНДЕКСАЦИЯ. На старте devforge только init_lazy(). Первый скан запускает ПЕРВЫЙ ЗАПРОС к любому роуту аналитики — все четыре хендлера зовут ensure_scan.
2. REFRESH_MIN_INTERVAL_MS = 60000. Инкрементальный проход не начинается, если индекс готов и последний проход был меньше минуты назад. Это условие работоспособности, а не оптимизация: поллинг status раз в полсекунды без порога перезапускал бы обход бесконечно, состояние никогда не доходило бы до ready и прогресс-бар не исчезал бы с экрана (дефект найден живым прогоном). rescan() порог обходит намеренно — это явное действие человека.
3. state_of() сначала проверяет ready, потом scanning: готовый индекс остаётся Ready и во время освежающего прохода. state=scanning означает ровно одно — показывать нечего, идёт первая сборка. Что проход идёт, клиент видит по progress.running.
4. Запрос до готовности индекса — 200, а не ошибка: summary отвечает state:'scanning' с progress и пустыми сериями. Ни 202, ни 503 — иначе каждый клиент лечил бы это состояние отдельной веткой.
5. Невалидные параметры молча заменяются умолчаниями (bucket=quarter → day, from=nonsense → начало диапазона, project=ghost → пустые серии, не 404).
6. previous_range — предыдущий период ТОЙ ЖЕ ДЛИНЫ вплотную перед from, а не предыдущий календарный месяц.
7. Медиана паузы повторно не фильтруется: паузы «отошёл» не попадают в Samples ещё в Фазе 1, вторая фильтрация отрезала бы честные длинные паузы.
8. top_projects считается отдельными запросами по каждому проекту — суммарный бакет уже сложен по всем проектам и разложить его обратно нельзя.

DTO (serde camelCase): StatusDto, ProjectDto, SummaryDto, KpiDto (26 плиток), SeriesDto (по одному полю на чарт, длина всех дневных массивов равна длине buckets), ProgressDto, RangeDto, ToolDto, ModelDto, ProjectSeriesDto, а также SummaryParams (query-параметры from/to/project/bucket) и AnalyticsState (Idle/Scanning/Ready).

Зависимости: rollup (AnalyticsIndex, Query, Bucket, DayBucket), scan (ScanProgress, scan_incremental, claude_projects_dir), cache (CacheSnapshot, load/save), chrono. Потребитель — tmux-web/src/main.rs (поле AppState analytics и четыре хендлера).

## Фаза 7 — новые метрики и справочные плитки
KPI пополнились полями `ttr`, `repeatShare`, `toolEntropyBits`, `slashShare`, `ackShare`, `nightMessages`, `nightShare`. Серии — `ttr`, `repeatShare`, `toolEntropyBits`, `uniqueProjects`, `slashShare`, `ackShare`, `hourXWeekday` (24×7 за период, всегда заполнена — ровная карта нулей честнее пустоты), `nightMessages`, `nightHours`, `versions` (по версиям CLI на корзину).

`unique_projects_per_bucket` считает уникальные проекты по проектным дням индекса, а не по суммарному бакету: бакет уже сложен по всем проектам и разложить его обратно нельзя.

Блок `globals` — глобальные счётчики CLI из `~/.claude.json` (`skillUsage`, `pluginUsage`, `toolUsage`, топ-20 на список). Файл ≈360 КБ, поэтому читается лениво в `summary()` и только при смене mtime (`refresh_globals`); отсутствие или битый JSON — не ошибка, поля просто нет. Это НЕ метрика периода: счётчики за всё время и фильтрам не подчиняются, в UI подписано явно.

`AnalyticsStore::new` принимает третий путь — `config_path` (`~/.claude.json`); `with_default_paths` подставляет его из `scan::claude_config_path`.

