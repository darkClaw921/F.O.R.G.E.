# next_step

Воркер фичи «Следующий шаг» (plugins/echo/src/next_step/mod.rs).

## Что делает

Опрашивает HostApi::idle_sessions() и для сессий, где Claude закончил генерацию и затих на IDLE_THRESHOLD_SECS=10+ секунд, генерирует РОВНО ОДИН короткий следующий шаг — готовый к отправке в терминал текст (NEXT_STEP_META_PROMPT, русский). Предложение кладётся в эфемерный EchoState::next_steps и рассылается через broadcast ServerMsg::NextStepEvent{has_suggestion:true} — фронт зажигает голубое свечение .has-next-step.

Константы: TICK_INTERVAL=2s, IDLE_THRESHOLD_SECS=10, CAPTURE_LINES=100, PANE_SNIPPET_CAP=6000, PANE_EXCERPT_CAP=4000.

## Пользовательский гейт (фича opt-in, по умолчанию ВЫКЛЮЧЕНА)

tick_once первым делом спрашивает HostApi::next_step_enabled() — КАЖДЫЙ тик, т.к. флаг меняется в рантайме (тумблер Настройки → Интерфейс, поле next_step_enabled в UserSettings). При false:

    reset_stale_episodes(state, processed, &HashSet::new()).await;
    return;

Пустой idle_names помечает ВСЕ живые предложения stale → чистятся processed + next_steps + уходит broadcast has_suggestion=false, гася свечение у тех, кто светился на момент выключения. Корректно благодаря инварианту processed ⊇ keys(next_steps): generate_for_session кладёт в next_steps только ПОСЛЕ processed.insert, а routes/next_step.rs удаляет из next_steps не трогая processed.

В steady-state бесплатно: со второго выключенного тика processed пуст и reset_stale_episodes выходит на stale.is_empty(). Воркер спавнится безусловно и продолжает тикать (sleep + чтение флага) → включение подхватывается за ≤2с без рестарта процесса, а Claude CLI не дёргается вовсе.

Default-impl HostApi::next_step_enabled возвращает true (= поведение до флага), поэтому тестовые stub'ы не ломаются; реальный гейт даёт только EchoHostAdapter, читающий UserSettingsStore. Развилка «гейт прямо в idle_sessions» отвергнута — см. [[interface-settings-toggles]].

## Эпизоды и защита от двойного запуска

Один эпизод затихания сессии → не более одного предложения:
- ProcessedSet (in-memory) хранит имена сессий с уже обработанным эпизодом;
- дополнительно проверяется наличие в state.next_steps (защита от гонки);
- reset_stale_episodes: сессия ИСЧЕЗЛА из idle-списка (снова активна / показан prompt / закрыта) → конец эпизода, чистка + has_suggestion:false.
Пустой ответ модели НЕ сохраняется, но эпизод остаётся обработанным (не дёргаем модель повторно).

## Тесты

idle_session_generates_suggestion, suggestion_inherits_project_id_from_idle_session, below_threshold_does_not_generate, does_not_regenerate_within_same_episode, episode_reset_clears_suggestion, empty_model_output_stores_nothing, disabled_feature_does_not_generate (выключенная фича не генерирует и не помечает эпизод; mock CLI — несуществующий путь), disabling_feature_clears_live_suggestion (выключение при живом предложении гасит его + broadcast, повторный тик — no-op).

StubHost::new(idle) — фича включена; StubHost::disabled(idle) — выключена.

## Graceful shutdown

spawn() возвращает JoinHandle<()>, хост abort'ит при завершении (crate::shutdown).
