# forge-echo::shutdown

Phase 6 hardening — graceful shutdown плагина Echo. Шаги: 1) state.shutdown.cancel() — будит долгоживущие задачи через CancellationToken; 2) shutdown_workers() — abort'ит сохранённые JoinHandle'ы scheduler+memory loop'ов; 3) runner.shutdown() — abort'ит активные Claude streams, kill_on_drop на дочернем процессе CLI убивает claude binary. Безопасно вызывать многократно. Вызывается из tmux-web/main.rs при SIGTERM/Ctrl-C через axum::serve.with_graceful_shutdown.
