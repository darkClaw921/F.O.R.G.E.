# PtyHandle::child_pid

Возвращает PID дочернего процесса tmux/lazygit, если он ещё запущен. Помечен #[allow(dead_code)]: используется только в unit-тесте spawn_for_missing_session_does_not_panic (тест-only code), поэтому в non-test сборке rustc флагует его как dead — explicit allow устраняет ложный warning.
