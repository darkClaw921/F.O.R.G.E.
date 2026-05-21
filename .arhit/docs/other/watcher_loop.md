# watcher_loop

Фоновый async loop в tmux-web/src/attention.rs (запуск через tokio::spawn из main.rs). Каждые 1500мс обходит все tmux-сессии активной/всех проектов и обновляет два независимых сигнала в AttentionState: needs_attention (Claude permission/plan/question prompt) и is_generating (pane изменился за последний тик).

PIPELINE (после Phase 3 fix):
1) list_sessions → Vec<SessionInfo>.
2) Цикл по сессиям: capture_pane (видимая часть) → detect_claude_prompt + hash_pane → pane_hash; capture_pane_full(name, 50) → hash_pane → gen_hash; changed = attention.update_generation(name, gen_hash) (сырой prev≠current, не пишет в generating). Складывает Vec<SessionAttention> и Vec<GenSnapshot>.
3) Дедуп needs_attention: deduplicate_attention(&collected) → set per name.
4) Дедуп is_generating: deduplicate_generating(&gens) → set_generating per name (включая false для сброса стабилизации).
5) Cleanup: HashSet живых имён сессий; last_gen_hash.retain и generating.retain удаляют записи исчезнувших сессий (без этого ложный changed=true при переиспользовании имени).
6) Summary-лог indicator summary: одна info-строка с парами session[a=…,g=…].

УСТОЙЧИВОСТЬ: tmux ошибки игнорируются через unwrap_or_default; loop никогда не завершается штатно.
