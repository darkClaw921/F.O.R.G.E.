//! СВОДНЫЕ ТЕСТЫ БЭКЕНДА «Анализа чата» для Bitrix24.
//!
//! Частные проверки живут рядом с кодом: разбор даты ATOM, извлечение цитат из
//! BB-кодов, пагинация и рендер — в `src/analysis/context.rs`; лестница роутов
//! и перевод ошибок — в `src/routes/analysis.rs` и `tests/dialogs_ladder.rs`.
//!
//! Здесь собрано то, что **невозможно проверить внутри одного модуля**, потому
//! что требует сборки нескольких: выгрузка через настоящий `load_page` против
//! фейкового портала, расход бюджета 2 rps, сквозной рендер на реалистичной
//! пачке и поведение хаба на неуспехе.
//!
//! # Сеть здесь настоящая, а портал — фейковый
//!
//! Стенд поднимает HTTP-сервер на свободном порту и подставляет его через
//! `base_url_override` (правило Р17 разрешает `http://127.0.0.1` только при
//! непустом `FORGE_BITRIX_CONFIG_PATH`, и стенд его выставляет). Мокать
//! `B24Client` не годится: половина проверяемого поведения — это ровно то, что
//! делает `load_page` между клиентом и DTO.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use forge_bitrix::analysis::context;
use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::login::{B24UserDto, LoginFlow};
use forge_bitrix::state::B24State;

/// Потолок ожидания события, которое ОБЯЗАНО произойти.
///
/// Щедрый намеренно. В `cargo test --workspace` этот бинарь гоняется
/// параллельно с десятками других, и спавн мок-процесса `/bin/sh` (а тем
/// более запись им pid-файла) под нагрузкой занимает секунды вместо
/// миллисекунд. Прежние «200 итераций по 20 мс» = 4 с иногда этого не
/// укрывали — тест падал не на дефекте продукта, а на таймере (forge-s4b2k).
/// На зелёном прогоне высокий потолок не стоит ничего: ждём по факту.
const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// Ждёт условие до [`WAIT_CAP`], опрашивая его.
///
/// Макрос, а не функция: условие содержит `.await` (поход в хаб за
/// `active_count`), и заворачивать его в замыкание, возвращающее future,
/// значило бы бороться с временами жизни там, где боремся с флаком.
///
/// НЕ ЗАМЕНЯЙ НА `sleep` и не возвращай счётчик итераций: события здесь —
/// спавн процесса, его смерть, снятие записи из реестра — происходят «когда
/// планировщик и ядро дойдут», а не «через N мс». Таймер на глазок делает
/// тест красным без единого дефекта в продукте.
macro_rules! wait_until {
    ($label:expr, $cond:expr) => {{
        let started = std::time::Instant::now();
        let mut reached = false;
        while started.elapsed() < WAIT_CAP {
            if $cond {
                reached = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            reached,
            "{}: ТАЙМАУТ ожидания ({:?}) — это не «неверное состояние», а «не дождались»",
            $label, WAIT_CAP
        );
    }};
}

/// Читает pid, записанный мок-процессом, дождавшись самой записи.
///
/// Опрос до факта, а не сон: между `spawn` и первой строкой скрипта лежит
/// планировщик ОС, и под параллельным прогоном workspace эта пауза
/// растягивается на секунды.
async fn wait_for_pid(pidfile: &std::path::Path) -> i32 {
    let mut pid: Option<i32> = None;
    wait_until!("mock CLI записал pid", {
        pid = std::fs::read_to_string(pidfile)
            .ok()
            .and_then(|raw| raw.trim().parse::<i32>().ok());
        pid.is_some()
    });
    pid.expect("mock CLI не записал pid — процесс не запустился")
}

// ---------------------------------------------------------------------------
// Фейковый портал.
// ---------------------------------------------------------------------------

/// Состояние стенда: сколько сообщений в чате и сколько запросов пришло.
struct Portal {
    /// Полная история, от старых к новым: `(id, author_id, text, date)`.
    messages: Vec<(i64, i64, String, String)>,
    /// Сколько раз портал вызывали (любым методом).
    calls: AtomicUsize,
    /// Сколько раз вызывали именно историю.
    history_calls: AtomicUsize,
}

impl Portal {
    fn with_messages(count: i64) -> Arc<Self> {
        // Шаг 2 — как у настоящего портала.
        let messages = (0..count)
            .map(|i| {
                (
                    2 + i * 2,
                    if i % 2 == 0 { 7 } else { 1 },
                    format!("реплика {i}"),
                    format!("2026-08-07T{:02}:{:02}:00+03:00", 9 + i / 60, i % 60),
                )
            })
            .collect();
        Arc::new(Self {
            messages,
            calls: AtomicUsize::new(0),
            history_calls: AtomicUsize::new(0),
        })
    }
}

/// Поднимает фейковый портал и возвращает его базовый URL.
///
/// Реализованы ровно два метода, которых требует путь анализа:
/// `im.dialog.messages.get` (страница истории) и `batch` (добор имён, который
/// `load_page` делает одной пачкой `im.user.list.get`). Всё остальное отвечает
/// пустым `result` — молча, потому что вызов чего-то ещё это уже отклонение, и
/// ловит его счётчик [`Portal::calls`], а не отказ стенда.
async fn spawn_portal(portal: Arc<Portal>) -> String {
    async fn handle(
        State(portal): State<Arc<Portal>>,
        axum::extract::Path(method): axum::extract::Path<String>,
        body: Option<Json<Value>>,
    ) -> Json<Value> {
        portal.calls.fetch_add(1, Ordering::SeqCst);
        let body = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));

        // Метод приезжает последним сегментом пути: `.../<method>.json`.
        let method = method.trim_end_matches(".json").to_string();
        if method == "im.dialog.messages.get" {
            portal.history_calls.fetch_add(1, Ordering::SeqCst);
            let limit = body
                .get("LIMIT")
                .and_then(Value::as_i64)
                .unwrap_or(50)
                .clamp(1, 50) as usize;
            let last = body.get("LAST_ID").and_then(Value::as_i64);

            // Портал под `LAST_ID` отдаёт СТРОГО старее и по УБЫВАНИЮ id.
            let mut page: Vec<&(i64, i64, String, String)> = portal
                .messages
                .iter()
                .rev()
                .filter(|(id, ..)| last.is_none_or(|l| *id < l))
                .take(limit)
                .collect();
            page.sort_by_key(|(id, ..)| std::cmp::Reverse(*id));

            let messages: Vec<Value> = page
                .iter()
                .map(|(id, author, text, date)| {
                    json!({
                        "id": id,
                        "chat_id": 27,
                        "author_id": author,
                        "date": date,
                        "text": text,
                        "params": [],
                    })
                })
                .collect();
            return Json(json!({"result": {"messages": messages, "users": [], "files": []}}));
        }

        // Имена авторов `load_page` добирает ОДНОЙ пачкой (`batch` с
        // командами `users0`, `users1`, …), а не прямым `user.list`: внешний
        // batch стоит один хит лимита интенсивности вместо N. Ключи команд
        // портал возвращает как есть, поэтому стенду достаточно отдать тот же
        // набор ключей, что пришёл.
        if method == "batch" {
            // `im.user.list.get` отдаёт КАРТУ «id → карточка», а не массив, и
            // поля в ней строчные — ровно так их читает `UserMapRaw`.
            let users = json!({
                "7": {"id": 7, "name": "Анна Петрова", "first_name": "Анна", "last_name": "Петрова", "active": true},
                "1": {"id": 1, "name": "Игорь Герасимов", "first_name": "Игорь", "last_name": "Герасимов", "active": true},
            });
            let mut ok = serde_json::Map::new();
            if let Some(cmd) = body.get("cmd").and_then(Value::as_object) {
                for key in cmd.keys() {
                    ok.insert(key.clone(), users.clone());
                }
            }
            return Json(json!({"result": {
                "result": Value::Object(ok),
                "result_error": [],
                "result_total": [],
                "result_next": [],
            }}));
        }

        Json(json!({"result": []}))
    }

    let app = Router::new()
        .route("/rest/1/stubcode/:method", post(handle).get(handle))
        .with_state(portal);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/rest/1/stubcode")
}

/// Подключённое состояние, смотрящее на фейковый портал.
///
/// `FORGE_BITRIX_CONFIG_PATH` выставляется намеренно: без него правило Р17
/// отвергает `http://127.0.0.1` в `base_url_override`, и стенд просто не
/// поднялся бы. Заодно это тот самый признак изолированного окружения, ради
/// которого переменная и существует.
async fn connected_state(base: &str) -> (tempfile::TempDir, Arc<B24State>) {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("FORGE_BITRIX_CONFIG_PATH", dir.path().join("bitrix.toml"));

    let cfg = BitrixConfig {
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("bitrix.toml"),
        // Герметичность: заметки и «бинарь» — только внутри tempdir.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        base_url_override: base.to_string(),
        webhook: WebhookUrl::new("https://p.bitrix24.ru/rest/1/stubcode/"),
        connect_timeout_secs: 2,
        request_timeout_secs: 5,
        ..BitrixConfig::default()
    };
    let state = B24State::new(cfg);
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "p.bitrix24.ru".into(),
        })
        .await;
    state.note_ok(chrono::Utc::now().timestamp()).await;
    (dir, state)
}

// ---------------------------------------------------------------------------
// Выгрузка через настоящий load_page.
// ---------------------------------------------------------------------------

/// СКВОЗНАЯ ВЫГРУЗКА: `context::fetch` → `load_page` → фейковый портал.
///
/// Проверяет то, чего не видит юнит-тест с подменённым источником: страницы
/// действительно склеиваются через настоящий `load_page` (с его сортировкой,
/// дедупом и проекцией `message_from_raw`), а не через мою модель этого.
#[tokio::test]
async fn a_full_fetch_goes_through_load_page_and_seams_correctly() {
    let portal = Portal::with_messages(120);
    let base = spawn_portal(portal.clone()).await;
    let (_dir, state) = connected_state(&base).await;
    let client = state.api_client().await.unwrap();

    let got = context::fetch(&state, &client, "chat27", 100).await.unwrap();

    assert_eq!(got.len(), 100, "выгружено ровно столько, сколько просили");
    let ids: Vec<i64> = got.iter().map(|m| m.id).collect();
    let mut want = ids.clone();
    want.sort_unstable();
    want.dedup();
    assert_eq!(ids, want, "порядок по возрастанию id, без дублей");
    // Хвост переписки, а не её начало: просили ПОСЛЕДНИЕ 100 из 120.
    assert_eq!(*ids.last().unwrap(), 2 + 119 * 2);
    assert_eq!(*ids.first().unwrap(), 2 + 20 * 2);
    assert_eq!(
        portal.history_calls.load(Ordering::SeqCst),
        2,
        "100 сообщений — это две страницы по 50, не больше"
    );
}

/// БЮДЖЕТ 2 rps: один разбор не имеет права занять портал больше чем на
/// [`context::MAX_PAGES`] запросов истории.
///
/// Это исполняемая половина решения задачи 3.4. За тем же ведром стоит воркер
/// событий, и «просто попросить побольше» не должно превращаться в паузу, во
/// время которой realtime копит отставание.
#[tokio::test]
async fn one_analysis_never_spends_more_than_the_page_ceiling() {
    let portal = Portal::with_messages(5_000);
    let base = spawn_portal(portal.clone()).await;
    let (_dir, state) = connected_state(&base).await;
    let client = state.api_client().await.unwrap();

    // Просим заведомо невозможное: клампинг обязан удержать и глубину, и
    // число запросов.
    let got = context::fetch(&state, &client, "chat27", i64::MAX)
        .await
        .unwrap();

    assert_eq!(got.len(), 200, "глубина клампится лестницей DEPTH_CHOICES");
    let history = portal.history_calls.load(Ordering::SeqCst);
    assert!(
        history <= context::MAX_PAGES,
        "выгрузка съела {history} страниц при потолке {}",
        context::MAX_PAGES
    );
    // Добор имён идёт ОДНОЙ пачкой и только для НЕЗНАКОМЫХ кэшу авторов:
    // после первой страницы кэш уже полон, и следующие страницы имён не
    // спрашивают вовсе. Это не украшение, а половина экономии: иначе каждая
    // страница удваивала бы расход бюджета.
    let total = portal.calls.load(Ordering::SeqCst);
    assert_eq!(
        total,
        history + 1,
        "имена обязаны добираться ровно один раз: всего {total}, истории {history}"
    );
}

/// REFRESH подтягивает ТОЛЬКО новое и стоит одного запроса к порталу.
///
/// Ради этого числа и затевалась вся инкрементальность: при бюджете 2 rps
/// разница между одним запросом и четырьмя — это разница между «мгновенно» и
/// «панель думает».
#[tokio::test]
async fn refresh_costs_a_single_portal_call() {
    let portal = Portal::with_messages(120);
    let base = spawn_portal(portal.clone()).await;
    let (_dir, state) = connected_state(&base).await;
    let client = state.api_client().await.unwrap();

    // Разобрано до сотого сообщения; новых — девятнадцать.
    let since = 2 + 100 * 2;
    let fresh = context::fetch_since(&state, &client, "chat27", since, 200)
        .await
        .unwrap();

    assert_eq!(fresh.messages.len(), 19);
    assert!(fresh.messages.iter().all(|m| m.id > since));
    assert!(!fresh.truncated);
    assert_eq!(
        portal.history_calls.load(Ordering::SeqCst),
        1,
        "REFRESH обязан стоить одного запроса"
    );
}

// ---------------------------------------------------------------------------
// Сквозной рендер на том, что реально приехало из портала.
// ---------------------------------------------------------------------------

/// Транскрипт собирается из НАСТОЯЩИХ DTO портала: даты ATOM разобраны, цитаты
/// в BB-кодах отделены, имена авторов подставлены `load_page`, URL'ов нет.
///
/// Отличие от юнит-теста рендера: там DTO собраны руками, здесь они прошли
/// весь путь `MessageRaw` → `message_from_raw` → `sort_messages_ascending`.
#[tokio::test]
async fn the_transcript_of_a_real_page_reads_as_a_conversation() {
    let portal = Arc::new(Portal {
        messages: vec![
            (
                2,
                1,
                "привет, нужна смета".to_string(),
                "2026-08-07T12:31:00+03:00".to_string(),
            ),
            (
                4,
                7,
                "[QUOTE]Игорь Герасимов: привет, нужна смета[/QUOTE]сейчас пришлю".to_string(),
                "2026-08-07T12:33:00+03:00".to_string(),
            ),
            (
                6,
                7,
                "смотри [URL=https://portal.example/disk/1]файл[/URL]".to_string(),
                "2026-08-07T12:40:00+03:00".to_string(),
            ),
        ],
        calls: AtomicUsize::new(0),
        history_calls: AtomicUsize::new(0),
    });
    let base = spawn_portal(portal).await;
    let (_dir, state) = connected_state(&base).await;
    let client = state.api_client().await.unwrap();

    let messages = context::fetch(&state, &client, "chat27", 50).await.unwrap();
    let transcript = context::render(&messages, context::MAX_TRANSCRIPT_CHARS);

    assert_eq!(transcript.messages_used, 3);
    let lines: Vec<&str> = transcript.text.lines().collect();

    // Даты ATOM разобраны в человеческую метку.
    assert!(lines[0].starts_with("[07.08 12:31] "), "{}", lines[0]);
    // Имя автора подставлено добором имён (`batch`) внутри `load_page`.
    assert!(lines[1].contains("Анна Петрова"), "{}", lines[1]);
    // Цитата отделена от ответа и не задвоена.
    assert!(
        lines[1].contains("↳ Игорь Герасимов: «привет, нужна смета»"),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with("сейчас пришлю"), "{}", lines[1]);
    assert_eq!(
        transcript.text.matches("привет, нужна смета").count(),
        2,
        "ровно два вхождения: сама реплика и цитата на неё\n{}",
        transcript.text
    );

    // ИНВАРИАНТ: адрес из `[URL=…]` не покидает бэкенд — от него остаётся
    // только подпись ссылки.
    assert!(
        !transcript.text.contains("https://"),
        "адрес просочился в промпт: {}",
        transcript.text
    );
    assert!(!transcript.text.contains("portal.example"));
    assert!(lines[2].contains("смотри файл"), "{}", lines[2]);

    // Имя чата для шапки промпта — первый входящий автор, без единого лишнего
    // запроса к порталу.
    assert_eq!(context::title_from_messages(&messages), "Анна Петрова");
}

// ---------------------------------------------------------------------------
// Хаб: сохранение только на успехе, отмена убивает процесс.
// ---------------------------------------------------------------------------

/// Скрипт-заглушка вместо Claude CLI.
fn mock_cli(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let cli = dir.path().join("mock-claude");
    std::fs::write(&cli, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = std::fs::metadata(&cli).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&cli, perms).unwrap();
    cli
}

/// Состояние с подменённым CLI и без портала — запуск хаба портала не требует.
fn state_with_cli(cli: std::path::PathBuf) -> (tempfile::TempDir, Arc<B24State>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("bitrix.toml"),
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: cli,
        // Запуски `claude -p` по умолчанию выключены глобально (см.
        // forge_claude_runner): стенд с мок-CLI включает их явно.
        claude_enabled: true,
        // Дефолт конфига — облако polza.ai; стенд гоняет мок-CLI, поэтому
        // бэкенд фиксируем на claude явно.
        llm_backend: forge_claude_runner::LlmBackend::Claude,
        ..BitrixConfig::default()
    };
    (dir, B24State::new(cfg))
}

async fn seed_run(state: &Arc<B24State>, dialog_id: &str) -> String {
    state
        .analysis
        .start(
            state.analysis_sink(),
            forge_bitrix::analysis::StartRequest {
                dialog_id: dialog_id.to_string(),
                kind: forge_bitrix::analysis::KIND_ANALYZE.to_string(),
                prompt: "<task>x</task>".into(),
                messages_used: 10,
                truncated: false,
                newest_id: Some(7042),
                question: None,
            },
        )
        .await
        .unwrap()
        .run_id()
        .to_string()
}

/// ИНВАРИАНТ: при `ok == false` разбор НЕ сохраняется.
///
/// Проверяется на самом дорогом случае — CLI, который что-то напечатал и умер
/// с ненулевым кодом. Сохранить такой обрубок значило бы затереть хороший
/// прошлый разбор половиной нового, причём молча: пользователь увидел бы
/// оборванный текст и решил, что модель так ответила.
#[tokio::test]
async fn a_failed_run_never_overwrites_the_saved_analysis() {
    let probe = tempfile::tempdir().unwrap();
    // Печатает кусок ответа в формате stream-json и падает.
    let cli = mock_cli(
        &probe,
        r#"cat >/dev/null
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"ОБРУБОК"}]}}'
exit 3"#,
    );
    let (_dir, state) = state_with_cli(cli);

    // Прошлый разбор на месте.
    {
        let mut guard = state.prompts.lock().await;
        guard.set_analysis("chat27", "ХОРОШИЙ РАЗБОР", 7042, 137, 1_800_000_000);
        forge_bitrix::analysis::prompts::save(&state.config.prompts_path, &guard).unwrap();
    }

    seed_run(&state, "chat27").await;

    // Ждём завершения run'а — опросом до факта, а не отсчётом фиксированных
    // 4 с: момент снятия записи задаёт планировщик, и под нагрузкой полного
    // прогона workspace он в короткий бюджет не укладывается.
    wait_until!("run завершился", state.analysis.active_count().await == 0);

    let saved = state.chat_analysis("chat27").await.expect("разбор на месте");
    assert_eq!(
        saved.text, "ХОРОШИЙ РАЗБОР",
        "неуспешный run затёр сохранённый разбор"
    );
    assert_eq!(saved.last_message_id, 7042, "курсор REFRESH не сдвинулся");

    state.analysis.shutdown().await;
}

/// ОТМЕНА РЕАЛЬНО УБИВАЕТ ПРОЦЕСС — проверка по НАСТОЯЩЕМУ pid, а не по записи
/// в реестре.
///
/// Это пункт живой приёмки («после STOP `pgrep claude` пуст»), сведённый к
/// воспроизводимому тесту. Реестр хаба может опустеть, а процесс остаться —
/// именно так выглядит утечка, которую этот тест ловит: осиротевший
/// `claude -p` продолжает жечь токены уже после того, как пользователь нажал
/// «стоп».
#[tokio::test]
async fn cancelling_a_run_actually_kills_the_process() {
    let probe = tempfile::tempdir().unwrap();
    let pidfile = probe.path().join("pid");
    let cli = mock_cli(
        &probe,
        &format!("echo $$ > {}\nsleep 60", pidfile.display()),
    );
    let (_dir, state) = state_with_cli(cli);

    let run_id = seed_run(&state, "chat27").await;

    // Ждём, пока mock успеет записать pid (spawn асинхронный) — опросом с
    // общим потолком, см. `wait_for_pid`.
    let pid = wait_for_pid(&pidfile).await;
    assert!(alive(pid), "перед отменой процесс обязан быть живым");

    // `cancel` отдаёт dialog_id отменённого run'а (не run_id) — на этом же
    // построен `cancel_dialog`, который делает обратное отображение.
    assert_eq!(
        state.analysis.cancel(&run_id).await.as_deref(),
        Some("chat27"),
        "отмена обязана найти идущий run и вернуть его диалог"
    );

    // Смерть процесса асинхронна: ждём её опросом, а не предполагаем. Отсчёт
    // фиксированного бюджета здесь врал бы под нагрузкой — reap дочернего
    // процесса делает ядро, а не тест.
    wait_until!(
        format!("процесс {pid} умер после отмены (иначе утечка «claude -p»)"),
        !alive(pid)
    );
    assert_eq!(state.analysis.active_count().await, 0, "слот освобождён");
}

/// TEARDOWN гасит идущий анализ: осиротевший процесс не переживает devforge.
///
/// Проверяется именно `forge_bitrix::shutdown`, а не `analysis.shutdown()`
/// напрямую: важно, что полный teardown плагина до хаба ДОХОДИТ — забытый шаг
/// в этой цепочке выглядит ровно как «devforge закрыт, а `claude` в
/// `ps` висит».
///
/// Чего тест НЕ утверждает: что хаб после teardown отказывает новым стартам.
/// Он их не отказывает — `shutdown` гасит идущие run'ы и раннер, но флага «я
/// закрыт» у хаба нет. Поведение общее для трёх мессенджеров (хаб живёт в
/// `forge-chat-analysis`), и менять его из плагина было бы правкой чужого
/// контракта; практического следствия у него нет, потому что после teardown
/// роуты уже не обслуживаются.
#[tokio::test]
async fn shutdown_kills_a_running_analysis() {
    let probe = tempfile::tempdir().unwrap();
    let pidfile = probe.path().join("pid");
    let cli = mock_cli(
        &probe,
        &format!("echo $$ > {}\nsleep 60", pidfile.display()),
    );
    let (_dir, state) = state_with_cli(cli);
    seed_run(&state, "chat27").await;

    // Опрос до появления pid — тот же приём и тот же общий потолок.
    let pid = wait_for_pid(&pidfile).await;

    // Полный teardown плагина, а не только хаба: проверяется именно то, что
    // `forge_bitrix::shutdown` до хаба доходит.
    forge_bitrix::registry::shutdown_portal(&state).await;

    // Снова опрос: смерть процесса — событие ядра, не таймер теста.
    wait_until!(
        format!("процесс {pid} умер после teardown плагина"),
        !alive(pid)
    );

    assert_eq!(
        state.analysis.active_count().await,
        0,
        "реестр run'ов обязан опустеть"
    );

    // Идемпотентность: второй teardown не паникует и не находит, что убивать.
    forge_bitrix::registry::shutdown_portal(&state).await;
    assert_eq!(state.analysis.active_count().await, 0);
}

/// Жив ли процесс: `kill -0` ничего не посылает, только проверяет.
///
/// Через `/bin/kill`, а не через `libc::kill`: ради одной проверки в одном
/// тесте заводить в плагине зависимость `libc` (и блок `unsafe`) — плохой
/// обмен. Утилита есть на любой POSIX-системе, а стоимость запуска здесь
/// никого не волнует.
fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
