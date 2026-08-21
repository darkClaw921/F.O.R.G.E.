# Гейт стадий: деградация вместо блокировки (forge-0s0l5)

## Инцидент

20.08.2026 у пользователя две записанные встречи (12 мин и 63 мин) ушли в `failed / cloud_not_configured`, роуты отвечали 409, саммари не было. Ф15 сделала умолчанием `stt_backend = cloud` и `summary_backend = cloud`; ключа polza.ai у пользователя не было. При этом локальный путь был ПОЛНОСТЬЮ готов: whisper-cli, модель ggml-large-v3-turbo-q5_0 (574 МБ), Metal, ffmpeg. Смена умолчания превратила рабочую установку в неработающую.

## Правило

Гейт ДЕГРАДИРУЕТ, а не блокирует. Отказ имеет смысл ровно тогда, когда не работает НИ ОДИН путь стадии. Умолчание осталось `cloud` — менялся только гейт.

## Таблица (одна на обе стадии) — `plugins/meet/src/deps.rs::choose_route`

| конфиг | облако рабочее | локальный путь рабочий | путь | событие журнала |
|---|---|---|---|---|
| cloud | да | да | облако | — |
| cloud | да | нет | облако | — |
| cloud | нет | да | локально | `*_fallback_local` |
| cloud | нет | нет | — | 409, код облачной причины |
| local | да | да | локально | — |
| local | нет | да | локально | — |
| local | да | нет | облако | `*_fallback_cloud` |
| local | нет | нет | — | 409, код локальной причины |

Код отказа берётся у ЗАКАЗАННОГО пути: выбравший облако чинит ключ, а не ставит полтора гигабайта весов.

Что считается рабочим:
* облако для расшифровки — есть ключ И (не пробовали ИЛИ есть ffmpeg: окна режутся и жмутся в mp3 нашей стороной);
* локальная расшифровка — whisper + модель + Metal (`transcribe_blocker`);
* облачное саммари — есть ключ;
* локальное саммари — файл Claude CLI найден (`summary_local_blocker`, синхронная проверка без запуска процесса).

`summary_disabled` проверяется ДО таблицы: выключенное саммари — отказ человека считать, а не нехватка пути.

## Откат обязан быть ВИДЕН

Молчаливая подмена движка хуже честного отказа: человек будет думать, что платит за облако, пока считает локальный whisper. Поэтому откат виден в трёх местах:

1. событие журнала встречи: `stt_fallback_local` / `stt_fallback_cloud` / `summary_fallback_local` / `summary_fallback_cloud` с текстом `StagePlan::fallback_note` (чего не хватило + что поставить);
2. поля `GET /api/meet/status`: `config.stt_effective`, `config.stt_fallback_reason`, `config.stt_blocker` и та же тройка для саммари. Роут по-прежнему НИКОГДА не падает — решения гейта это поля, а не отказ;
3. подсказка у переключателя стадии во вкладке настроек (`backendNote` в `tmux-web/static/js/settings/meet-tab.js`): «ЗАКАЗАНА в облаке, а считается локально: ключ API не задан».

`TranscriptMeta::engine` называет ФАКТИЧЕСКИ отработавший путь: при откате на локальный там `whisper.cpp`, а не `polza.ai` — иначе непонятно, почему расшифровка бесплатна.

## Точки кода

* `plugins/meet/src/deps.rs` — `Route`, `StagePlan`, `choose_route`, `blocker_hint`, `DepsReport::plan_transcribe`, `cloud_transcribe_blocker`, `plan_summary`, `summary_local_blocker`; `transcribe_blocker_for` стал тонкой обёрткой `plan_transcribe(..).err()`;
* `plugins/meet/src/pipeline/run.rs` — развилка стадии расшифровки берёт `plan`, пишет событие отката, `cloud_stt = plan.route.is_cloud()`;
* `plugins/meet/src/summary/mod.rs` — развилка саммари берёт `plan_summary`, пишет событие отката до прогона; отказ гейта это провал САММАРИ, а не встречи;
* `plugins/meet/src/routes/meetings.rs` — `summary_blocker` = `plan_summary(..).err()`;
* `plugins/meet/src/routes/mod.rs` — новые поля `StatusConfigDto`;
* `plugins/meet/src/error.rs` — `DepUnavailable` несёт в тексте совет из `blocker_hint`.

## Тесты

* `deps::tests::the_transcription_gate_degrades_instead_of_blocking` и `the_summary_gate_degrades_by_the_same_table` — по восемь строк каждая;
* `tests/pipeline.rs::a_cloud_default_without_a_key_falls_back_to_the_ready_local_whisper` — регрессия ровно на инцидент (ready, engine = whisper.cpp, событие в журнале);
* `tests/pipeline_cloud.rs::all_four_combinations_take_the_path_they_are_told_to` и `with_no_working_path_at_all_the_stage_refuses_by_the_configured_code`;
* `tests/routes_result.rs::the_dependency_ladder_follows_the_backend` — случай 4 плюс поля статуса;
* `tmux-web/tests/frontend/settings_meet_tab.test.mjs` — подсказка отката у переключателя.

## Ловушка лестницы 409

`check_ladder` смотрит нехватку компонентов РАНЬШЕ перехода состояний. Появление кода `claude_cli_unavailable` у локального саммари поэтому перекрыло `invalid_state` в двух тестах `routes_result.rs`: тесту про запрет перехода нужен мок Claude CLI, иначе он проверяет не то, что заявляет.