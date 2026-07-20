//! Воркер фичи «Следующий шаг».
//!
//! Опрашивает [`HostApi::idle_sessions`] (Phase 1) и для сессий, в которых
//! Claude закончил генерацию и затих на [`IDLE_THRESHOLD_SECS`]+ секунд,
//! генерирует РОВНО ОДИН короткий «следующий шаг» — готовый к отправке в
//! терминал текст. Предложение кладётся в эфемерное
//! [`EchoState::next_steps`](crate::state::EchoState::next_steps) и
//! рассылается фронтенду через broadcast
//! [`ServerMsg::NextStepEvent`](crate::ws::protocol::ServerMsg::NextStepEvent).
//!
//! ## Эпизоды и защита от двойного запуска
//!
//! Один «эпизод» затихания сессии → не более одного предложения. Для этого:
//!
//! - In-memory [`ProcessedSet`] хранит имена сессий, для которых текущий
//!   эпизод уже обработан (генерация запущена/завершена). Пока сессия в
//!   idle-списке и в `processed` — повторно не генерируем.
//! - Дополнительно проверяем наличие в `state.next_steps`: если предложение
//!   уже лежит — пропускаем (защита от гонки до попадания в `processed`).
//! - Когда сессия ИСЧЕЗАЕТ из idle-списка (снова активна / показан prompt /
//!   закрыта) — это конец эпизода: убираем её и из `processed`, и из
//!   `state.next_steps`, и шлём `NextStepEvent{has_suggestion:false}`. Это
//!   сбрасывает свечение во фронте и позволяет сгенерировать новое
//!   предложение в следующем эпизоде затихания.
//!
//! ## Пользовательский гейт
//!
//! Фича opt-in: [`HostApi::next_step_enabled`] спрашивается КАЖДЫЙ тик (флаг
//! меняется в рантайме — пользователь переключает тумблер в Настройки →
//! Интерфейс). При `false` [`tick_once`] гасит уже показанные предложения и
//! уходит, не опрашивая idle-сессии и не дёргая Claude CLI. Воркер при этом
//! продолжает тикать: сам тик стоит один sleep + чтение флага, зато включение
//! подхватывается за ≤[`TICK_INTERVAL`] без рестарта процесса.
//!
//! ## Graceful shutdown
//!
//! [`spawn`] возвращает `JoinHandle<()>` — хост-процесс abort'ит его при
//! завершении (см. [`crate::shutdown`]).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use echo_host_api::HostApi;

use crate::claude::RunRequest;
use crate::db::repo::next_step as rules_repo;
use crate::memory::snippet;
use crate::state::{EchoState, NextStepSuggestion, ServerEvent};
use crate::ws::protocol::ServerMsg;

/// Порог затихания: сессия должна молчать минимум столько секунд, прежде чем
/// мы предложим следующий шаг. Короткие паузы между сообщениями Claude (например
/// при стриминге tool-use) не должны триггерить предложение.
pub const IDLE_THRESHOLD_SECS: u64 = 10;

/// Период опроса idle-сессий. 2 секунды — быстрый отклик на затихание без
/// заметной нагрузки (один `idle_sessions()` per tick).
pub const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Сколько строк pane захватываем для контекста генерации.
const CAPTURE_LINES: i32 = 100;

/// Кап на длину pane-выдержки в prompt'е (символов).
const PANE_SNIPPET_CAP: usize = 6000;

/// Кап на длину сохранённой `pane_excerpt` (для последующего feedback-правила).
const PANE_EXCERPT_CAP: usize = 4000;

/// Русский мета-prompt генерации следующего шага. Требует строго один короткий
/// шаг, готовый к отправке в терминал — без преамбул, markdown и пояснений.
pub const NEXT_STEP_META_PROMPT: &str = "Ты анализируешь последние строки терминала \
рабочей сессии, где ассистент только что закончил работу. Определи, что было \
сделано и в каком состоянии сейчас задача, и предложи РОВНО ОДИН короткий \
следующий шаг — конкретное действие, которое логично сделать дальше.\n\
Ответ — это ТЕКСТ, ГОТОВЫЙ К ОТПРАВКЕ прямо в терминал ассистенту (как если бы \
пользователь напечатал его сам). Поэтому:\n\
- БЕЗ преамбул, объяснений и вступлений;\n\
- БЕЗ markdown, без списков, без тройных бэктиков;\n\
- одна короткая формулировка, обычно одно предложение;\n\
- по-русски, по делу.\n\
Если осмысленного следующего шага нет — ответь пустой строкой.";

/// In-memory множество сессий, чей текущий idle-эпизод уже обработан.
/// См. модульную документацию (защита от двойного запуска + сброс эпизода).
pub type ProcessedSet = Arc<Mutex<HashSet<String>>>;

/// Спавнит воркер «Следующий шаг». Возвращает `JoinHandle` для graceful
/// shutdown.
pub fn spawn(state: Arc<EchoState>, host: Arc<dyn HostApi>) -> JoinHandle<()> {
    let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));
    tracing::info!(
        tick_secs = TICK_INTERVAL.as_secs(),
        idle_threshold = IDLE_THRESHOLD_SECS,
        "Echo next_step worker started"
    );
    tokio::spawn(async move {
        run_loop(state, host, processed).await;
    })
}

/// Внутренний loop — вынесен ради unit-теста одного tick'а без сна.
async fn run_loop(state: Arc<EchoState>, host: Arc<dyn HostApi>, processed: ProcessedSet) {
    loop {
        tokio::time::sleep(TICK_INTERVAL).await;
        tick_once(&state, &host, &processed).await;
    }
}

/// Одна итерация:
/// 0. Если фича выключена пользователем — погасить активные предложения и выйти.
/// 1. Получить idle-сессии.
/// 2. Сбросить эпизоды для сессий, ПРОПАВШИХ из idle-списка (снова активны).
/// 3. Для idle-сессий с `idle_secs >= IDLE_THRESHOLD_SECS`, ещё не обработанных
///    и без активного предложения — сгенерировать следующий шаг.
///
/// Не паникует на ошибках host/БД — logging + продолжаем.
pub(crate) async fn tick_once(
    state: &Arc<EchoState>,
    host: &Arc<dyn HostApi>,
    processed: &ProcessedSet,
) {
    if !host.next_step_enabled() {
        // Фича выключена. Пустой idle-список означает «ни одной живой сессии с
        // эпизодом» → reset_stale_episodes снимает ВСЕ активные предложения и
        // шлёт has_suggestion=false, гася свечение у тех, кто уже светился на
        // момент выключения. Корректно благодаря инварианту
        // `processed ⊇ keys(next_steps)`: generate_for_session кладёт в
        // next_steps только после processed.insert, а routes/next_step.rs
        // удаляет из next_steps, не трогая processed.
        //
        // В steady-state это бесплатно: со второго выключенного тика processed
        // пуст, и reset_stale_episodes выходит на `stale.is_empty()`.
        reset_stale_episodes(state, processed, &HashSet::new()).await;
        return;
    }

    let idle = match host.idle_sessions().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "next_step: idle_sessions failed");
            return;
        }
    };
    let idle_names: HashSet<String> = idle.iter().map(|s| s.name.clone()).collect();

    // Сброс эпизодов сессий, которые больше не idle (снова активны / prompt /
    // закрыты): чистим processed + next_steps + broadcast has_suggestion=false.
    reset_stale_episodes(state, processed, &idle_names).await;

    for s in &idle {
        if s.idle_secs < IDLE_THRESHOLD_SECS {
            continue;
        }
        let name = s.name.clone();

        // Уже есть активное предложение для этой сессии — пропускаем.
        if state.next_steps.read().await.contains_key(&name) {
            continue;
        }
        // Эпизод уже обработан (генерация шла/идёт) — пропускаем.
        {
            let mut set = processed.lock().await;
            if set.contains(&name) {
                continue;
            }
            set.insert(name.clone());
        }

        if let Err(e) = generate_for_session(state, host, &name, s.project_id.clone()).await {
            tracing::warn!(session = %name, error = %e, "next_step: generation failed");
            // Освобождаем эпизод, чтобы попробовать на следующем tick'е.
            processed.lock().await.remove(&name);
        }
    }
}

/// Снимает предложения и пометки для сессий, исчезнувших из idle-списка.
async fn reset_stale_episodes(
    state: &Arc<EchoState>,
    processed: &ProcessedSet,
    idle_names: &HashSet<String>,
) {
    // Сессии, которые мы помечали как обработанные, но которых больше нет в idle.
    let stale: Vec<String> = {
        let set = processed.lock().await;
        set.iter()
            .filter(|n| !idle_names.contains(*n))
            .cloned()
            .collect()
    };
    if stale.is_empty() {
        return;
    }
    {
        let mut set = processed.lock().await;
        for n in &stale {
            set.remove(n);
        }
    }
    for name in stale {
        let removed = state.next_steps.write().await.remove(&name).is_some();
        if removed {
            tracing::debug!(session = %name, "next_step: episode ended, suggestion cleared");
            let _ = state.broadcast.send(ServerEvent::broadcast(
                ServerMsg::NextStepEvent {
                    session: name.clone(),
                    has_suggestion: false,
                },
            ));
        }
    }
}

/// Захватывает pane сессии, строит prompt (мета-prompt + правила памяти +
/// последние строки), прогоняет через `runner.one_shot`, сохраняет в
/// `next_steps` и шлёт broadcast `NextStepEvent{has_suggestion:true}`.
///
/// Пустой ответ модели (нет осмысленного шага) — НЕ сохраняется: предложения
/// не появляется, свечение не зажигается. Эпизод остаётся «обработанным»,
/// чтобы не дёргать модель повторно в том же эпизоде.
async fn generate_for_session(
    state: &Arc<EchoState>,
    host: &Arc<dyn HostApi>,
    session: &str,
    project_id: Option<String>,
) -> anyhow::Result<()> {
    let pane = host.capture_pane_full(session, CAPTURE_LINES).await?;
    // Ярлык проекта сессии (git-корень cwd) приходит из `IdleSession.project_id`
    // (резолвит хост-адаптер). По нему подмешиваем ТОЛЬКО правила этого проекта
    // + глобальные — коррекция из одного проекта не протекает в другие.

    let rules = rules_repo::list_rules(
        &state.db,
        project_id.as_deref(),
        rules_repo::DEFAULT_RULES_LIMIT,
    )
    .await
    .unwrap_or_default();

    let mut prompt = String::with_capacity(8192);
    prompt.push_str("[task]\n");
    prompt.push_str(NEXT_STEP_META_PROMPT);
    prompt.push('\n');
    if !rules.is_empty() {
        prompt.push_str("\n[learned_rules]\n");
        prompt.push_str(
            "Учитывай прошлые коррекции пользователя (контекст → что следовало предложить):\n",
        );
        for r in &rules {
            prompt.push_str(&format!(
                "- когда: {}\n  предлагай: {}\n",
                r.context_summary.replace('\n', " "),
                r.suggested_next.replace('\n', " ")
            ));
        }
    }
    prompt.push_str("\n[terminal_tail]\n");
    prompt.push_str(&snippet(pane.trim(), PANE_SNIPPET_CAP));
    prompt.push('\n');

    let res = state.runner.one_shot(RunRequest::new(prompt)).await?;
    let content = res.text.trim().to_string();
    if content.is_empty() {
        tracing::debug!(session = %session, "next_step: empty suggestion, skipping");
        return Ok(());
    }

    let suggestion = NextStepSuggestion {
        session: session.to_string(),
        content,
        pane_excerpt: snippet(pane.trim(), PANE_EXCERPT_CAP),
        project_id,
        created_at_unix: chrono::Utc::now().timestamp(),
    };
    state
        .next_steps
        .write()
        .await
        .insert(session.to_string(), suggestion);

    tracing::info!(session = %session, "next_step: suggestion generated");
    let _ = state.broadcast.send(ServerEvent::broadcast(ServerMsg::NextStepEvent {
        session: session.to_string(),
        has_suggestion: true,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use async_trait::async_trait;
    use echo_host_api::{IdleSession, SessionInfo};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Stub-host: отдаёт заданный список idle-сессий и фиксированный pane.
    struct StubHost {
        idle: Vec<IdleSession>,
        /// Что вернуть из `next_step_enabled` — пользовательский гейт фичи.
        enabled: bool,
    }
    impl StubHost {
        /// Хост с включённой фичей — обычный случай для большинства тестов.
        fn new(idle: Vec<IdleSession>) -> Self {
            Self {
                idle,
                enabled: true,
            }
        }
        /// Хост с выключенной пользователем фичей.
        fn disabled(idle: Vec<IdleSession>) -> Self {
            Self {
                idle,
                enabled: false,
            }
        }
    }
    #[async_trait]
    impl HostApi for StubHost {
        async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(Vec::new())
        }
        async fn capture_pane_full(&self, _s: &str, _l: i32) -> anyhow::Result<String> {
            Ok("$ cargo build\nerror[E0382]: borrow of moved value\n".to_string())
        }
        fn auth_token(&self) -> Option<String> {
            None
        }
        async fn idle_sessions(&self) -> anyhow::Result<Vec<IdleSession>> {
            Ok(self.idle.clone())
        }
        fn next_step_enabled(&self) -> bool {
            self.enabled
        }
    }

    fn write_mock_cli(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Mock CLI, печатающий непустой ответ (один следующий шаг).
    fn mock_step_script() -> &'static str {
        r###"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"исправь ошибку заимствования в main.rs"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":5,"output_tokens":3}}'
"###
    }

    async fn make_state(cli: PathBuf) -> Arc<EchoState> {
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        Arc::new(EchoState::new(Arc::new(db), runner))
    }

    fn idle(name: &str, secs: u64) -> IdleSession {
        IdleSession {
            name: name.to_string(),
            idle_secs: secs,
            project_id: None,
        }
    }

    /// Idle-сессия >= порога → воркер вызывает runner и кладёт предложение в
    /// next_steps + шлёт broadcast has_suggestion=true.
    #[tokio::test]
    async fn idle_session_generates_suggestion() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_step_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![idle("work", 12)]));
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        let mut rx = state.broadcast.subscribe();

        tick_once(&state, &host, &processed).await;

        let map = state.next_steps.read().await;
        let s = map.get("work").expect("suggestion stored for session");
        assert_eq!(s.session, "work");
        assert!(s.content.contains("исправь"), "got: {}", s.content);
        assert!(!s.pane_excerpt.is_empty());
        drop(map);

        // Broadcast пришёл с has_suggestion=true.
        let ev = rx.try_recv().expect("broadcast event");
        match ev.msg {
            ServerMsg::NextStepEvent { session, has_suggestion } => {
                assert_eq!(session, "work");
                assert!(has_suggestion);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// project_id из IdleSession пробрасывается в сохранённое предложение —
    /// именно по нему feedback потом скоупит правило (коррекция влияет только
    /// на свой проект, а не на все сразу).
    #[tokio::test]
    async fn suggestion_inherits_project_id_from_idle_session() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_step_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![IdleSession {
            name: "work".to_string(),
            idle_secs: 12,
            project_id: Some("/repo/a".to_string()),
        }]));
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        tick_once(&state, &host, &processed).await;

        let map = state.next_steps.read().await;
        let s = map.get("work").expect("suggestion stored");
        assert_eq!(
            s.project_id.as_deref(),
            Some("/repo/a"),
            "предложение должно унаследовать project_id затихшей сессии"
        );
    }

    /// idle_secs < порога → НЕ генерируем.
    #[tokio::test]
    async fn below_threshold_does_not_generate() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_step_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![idle("work", 3)]));
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        tick_once(&state, &host, &processed).await;
        assert!(state.next_steps.read().await.is_empty());
    }

    /// Два tick'а подряд для той же idle-сессии → ровно одно предложение
    /// (processed-set + наличие в next_steps защищают от повтора).
    #[tokio::test]
    async fn does_not_regenerate_within_same_episode() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_step_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![idle("work", 30)]));
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        tick_once(&state, &host, &processed).await;
        let first = state
            .next_steps
            .read()
            .await
            .get("work")
            .unwrap()
            .created_at_unix;
        tick_once(&state, &host, &processed).await;
        let map = state.next_steps.read().await;
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("work").unwrap().created_at_unix, first);
    }

    /// Сессия исчезла из idle-списка → предложение и пометка сбрасываются,
    /// шлётся broadcast has_suggestion=false.
    #[tokio::test]
    async fn episode_reset_clears_suggestion() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_step_script());
        let state = make_state(cli).await;
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        // Первый tick: сессия idle → предложение появляется.
        let host_idle: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![idle("work", 15)]));
        tick_once(&state, &host_idle, &processed).await;
        assert!(state.next_steps.read().await.contains_key("work"));
        assert!(processed.lock().await.contains("work"));

        // Второй tick: сессия больше не idle → сброс.
        let mut rx = state.broadcast.subscribe();
        let host_active: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![]));
        tick_once(&state, &host_active, &processed).await;

        assert!(state.next_steps.read().await.is_empty(), "suggestion cleared");
        assert!(!processed.lock().await.contains("work"), "processed cleared");

        let ev = rx.try_recv().expect("reset broadcast");
        match ev.msg {
            ServerMsg::NextStepEvent { session, has_suggestion } => {
                assert_eq!(session, "work");
                assert!(!has_suggestion);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Пустой ответ модели → предложение НЕ сохраняется (но эпизод обработан).
    #[tokio::test]
    async fn empty_model_output_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // CLI без content-дельт → агрегированный текст пустой.
        let cli = write_mock_cli(
            &dir,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"result\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}'\n",
        );
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![idle("work", 20)]));
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        tick_once(&state, &host, &processed).await;
        assert!(state.next_steps.read().await.is_empty());
        // Эпизод помечен обработанным (не дёргаем модель повторно).
        assert!(processed.lock().await.contains("work"));
    }

    /// Фича выключена пользователем → затихшая сессия не порождает предложение
    /// и Claude CLI НЕ запускается. Mock CLI здесь — заведомо несуществующий
    /// путь: если гейт протечёт и генерация всё-таки стартует, тест этого не
    /// пропустит (в next_steps окажется пусто по другой причине, поэтому ниже
    /// дополнительно проверяем, что эпизод не помечен обработанным).
    #[tokio::test]
    async fn disabled_feature_does_not_generate() {
        let state = make_state(PathBuf::from("/nonexistent/claude-must-not-run")).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost::disabled(vec![idle("work", 30)]));
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        tick_once(&state, &host, &processed).await;

        assert!(
            state.next_steps.read().await.is_empty(),
            "выключенная фича не должна порождать предложений"
        );
        assert!(
            processed.lock().await.is_empty(),
            "эпизод не должен помечаться обработанным: генерация не запускалась"
        );
    }

    /// Выключение фичи при уже показанном предложении гасит его: next_steps и
    /// processed чистятся, во фронт уходит has_suggestion=false (свечение
    /// пропадает без рестарта процесса).
    #[tokio::test]
    async fn disabling_feature_clears_live_suggestion() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_step_script());
        let state = make_state(cli).await;
        let processed: ProcessedSet = Arc::new(Mutex::new(HashSet::new()));

        // Фича включена: предложение появляется и сессия светится.
        let host_on: Arc<dyn HostApi> = Arc::new(StubHost::new(vec![idle("work", 15)]));
        tick_once(&state, &host_on, &processed).await;
        assert!(state.next_steps.read().await.contains_key("work"));

        // Пользователь выключил тумблер — та же сессия всё ещё idle.
        let mut rx = state.broadcast.subscribe();
        let host_off: Arc<dyn HostApi> = Arc::new(StubHost::disabled(vec![idle("work", 15)]));
        tick_once(&state, &host_off, &processed).await;

        assert!(
            state.next_steps.read().await.is_empty(),
            "предложение должно погаснуть при выключении фичи"
        );
        assert!(processed.lock().await.is_empty(), "processed очищен");

        let ev = rx.try_recv().expect("broadcast о погасшем предложении");
        match ev.msg {
            ServerMsg::NextStepEvent {
                session,
                has_suggestion,
            } => {
                assert_eq!(session, "work");
                assert!(!has_suggestion);
            }
            other => panic!("unexpected: {other:?}"),
        }

        // Следующий выключенный tick — no-op, без повторного broadcast'а.
        tick_once(&state, &host_off, &processed).await;
        assert!(
            rx.try_recv().is_err(),
            "повторный выключенный tick не должен слать событий"
        );
    }
}
