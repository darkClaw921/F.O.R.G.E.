# tmux-web/tests/agents_frontend_suite.rs

Cargo entry point поведенческих тестов новой статистики агентов. Автоматически собирает agent_stats_*.test.mjs, проверяет наличие ключевых core/live/view/history/settings/browser сценариев и запускает каждый отдельным Node процессом. Ошибка любого файла делает Cargo test неуспешным с полным stdout/stderr. Browser probe использует настоящий установленный Chrome при наличии, остальные тесты герметичны и обязательны при доступном Node. Отсутствие Node явно сообщается, как в существующем analytics frontend runner.
