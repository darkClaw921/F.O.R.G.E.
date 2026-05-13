# tasks::detect_cli

Автодетект CLI для beads: 'bd' (go-beads) если есть .beads/bd.sock, иначе 'br' (beads_rust). Маркер сокета означает что в проекте запущен bd daemon, и обращение через 'br' заблокируется на 30с write-lock timeout. Используется в list_tasks и run_br. Возвращает &'static str.
