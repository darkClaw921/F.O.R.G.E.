# tmux-web/src/analytics/rollup.rs

Структуры-накопители аналитики Claude Code: дневные бакеты, роллап сессии, сводный индекс, их слияние и выборка под фильтр.

## Единица хранения
Все чарты дашборда — временные ряды по дням/неделям с фильтром по проекту, поэтому единица хранения — DayBucket: всё, что случилось за один календарный день (ЛОКАЛЬНАЯ зона машины) в рамках одной сессии. Слияние роллапов — поэлементное сложение бакетов, что делает дешёвыми и инкрементальное дочитывание, и объединение 1300+ сессий.

## Хранение распределений (Samples + LogHist)
Медиана и перцентили паузы «читает ли код» — ключевая метрика; считать её по пяти отображаемым бакетам нельзя (слишком грубо), а хранить сырые сэмплы за весь архив дорого. Компромисс — Samples { count, sum, kept: Vec<u32>, hist: LogHist }: точный вектор до MAX_KEPT_SAMPLES = 20000 значений на день плюс логарифмическая гистограмма на 128 бинов (32 октавы × 4 суб-бина, шаг ~19 % по значению; значения 0..3 точны). Пока сэмплов немного (обычный случай) quantile() точен по вектору; после переполнения — оценка по гистограмме с погрешностью не хуже ~13 % (закреплено тестом). Гистограмма складывается при слиянии без потерь.

## DayBucket
Поля: date (YYYY-MM-DD), human_messages, assistant_messages, sessions_started, tokens_in/out, cache_read, cache_create, thinking, cost_usd, lines_added/removed, tool_calls, interruptions, compacts, subagent_records, subagent_starts, gen_ms (сумма измеренных длительностей ходов), api_ms и tool_ms (доли totalAPIDuration/totalToolDuration сессии), human_ms (сумма пауз = «человеко-время»), away_count, quick_replies, pause_buckets [5], pauses/msg_lens/turn_ms (Samples), hour_x_weekday [24][7] (0 = понедельник), versions (BTreeSet), models (ModelStat), tools (ToolStat{calls, interrupted}), agents.
- DayBucket::merge() — поэлементное сложение всех счётчиков, распределений, карт и теплокарты.
- DayBucket::pause_bucket_index() — 5 бакетов по PAUSE_BUCKET_EDGES_MS = [5s, 15s, 60s, 300s]: <5 c, 5–15 c, 15–60 c, 1–5 мин, >5 мин.

## SessionRollup
Итог разбора одного файла: session_id, cwd, project_dir (закодированное имя каталога через claude_memory::encode_project_dir — КОПИИ КОДА НЕТ), git_branch, ai_title, cli_versions, start_ts/end_ts, days (BTreeMap), totals (SessionTotals), parsed_lines, skipped_lines, parse_offset, file_len, file_mtime_ms, turn_state.

Автомат ходов (TurnState { last_human_ts, last_assistant_ts, pending_end_ts, turn_open }) живёт в роллапе, а не в локальных переменных разбора, — чтобы пережить границу инкрементального дочитывания.
- on_human_message() — закрывает предыдущий ход (close_turn_before), считает паузу и длительность хода, учитывает сообщение, длину и теплокарту час×день-недели.
- on_turn_end() — запись system/turn_duration: канонический конец хода; длительность хода = конец − предшествующее сообщение человека.
- close_turn_before() — fallback, если turn_duration в ходе не было: конец хода = таймстемп последней assistant-записи.
- record_pause() — пауза относится ко дню, в который ход ЗАВЕРШИЛСЯ (тогда человек начал читать). Пауза > 30 мин → away_count, вне медианы/перцентилей/гистограммы/human_ms. Пауза < 10 c → quick_replies.
- on_assistant() — токены (вкл. thinking), модель, вызовы tool_use; sidechain-записи идут в subagent_records и НЕ двигают fallback-конец хода.
- on_interruption(), on_subagent_start() (agent-name без таймстемпа → относится к последнему известному дню), close_open_turn() (только для заведомо законченной сессии).
- redistribute_totals() — ИДЕМПОТЕНТНО раскладывает итоги cost-state (cost_usd, lines, api_ms, tool_ms) по дневным бакетам пропорционально числу записей ассистента в дне; стоимость по моделям — пропорционально сообщениям модели; sessions_started ставится на первый день сессии. Сначала обнуляет распределённые поля, поэтому безопасно вызывать после каждого дочитывания.

## SessionTotals
Итоги ТОЛЬКО из ПОСЛЕДНЕЙ записи cost-state: present, total_cost_usd, total_api_duration_ms, total_tool_duration_ms, lines_added/removed, start_time_ms, model_usage. Промежуточные cost-state кумулятивны — суммировать их ЗАПРЕЩЕНО, каждая следующая перезаписывает предыдущую.

## AnalyticsIndex / Query
AnalyticsIndex { days, projects: BTreeMap<dir, ProjectRollup>, sessions } — сводный индекс.
- add_session() вливает роллап сессии в общий индекс и в её проект.
- query(&Query { from, to, project, bucket }) — выборка дневных бакетов; ничего не пересчитывает по файлам, работает по уже собранным DayBucket. Bucket::Week группирует по понедельнику недели (week_key).
- total(&Query) — суммарный бакет за период для KPI-плиток.

## Зависимости
chrono (локальная зона, дни недели, недельные ключи), serde (снимок кэша в Фазе 2), crate::claude_memory::encode_project_dir.

## Фаза 7 — разнообразие, ночь и версии
- `Distinct` — HyperLogLog на 512 регистров для оценки числа различных слов за день: точное множество раздуло бы снимок кэша на десятки мегабайт, а HLL занимает 512 байт при любом словаре и складывается поэлементным максимумом без потерь. Ошибка ~4.6 % на больших словарях, доли процента на малых (линейный счёт). Сериализуется base64-строкой; снимок другой формы молча считается пустым.
- Новые поля `DayBucket`: `words_total`, `words_distinct`, `prompt_hashes` (BTreeMap хеш→счёт, потолок `MAX_PROMPT_KEYS = 20 000`), `prompts_counted`, `slash_commands`, `one_word_acks`. Все складываются при `merge`, поэтому повтор промпта в РАЗНЫХ сессиях одного дня виден.
- Методы-производные: `ttr()`, `repeat_share()`, `tool_entropy_bits()` (Шеннон по `tools`, 0 при одном инструменте, log2(N) при равномерном), `slash_share()`, `ack_share()`, `night_messages()` (часы 0..`NIGHT_HOURS`).
- `on_human_message(ts, &HumanText)` вместо `(ts, len)` — признаки приходят готовыми из `analytics::text`.
- `bucket_key(date, bucket)` стал публичным: серии, которые считаются не по суммарному бакету (уникальные проекты в день), обязаны раскладываться по тем же корзинам, что `AnalyticsIndex::query`.
- `NIGHT_HOURS = 6` — единая константа ночи для бэкенда и фронта (уезжает в `series.nightHours`).

