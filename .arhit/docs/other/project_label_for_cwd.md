# project_label_for_cwd

project_label_for_cwd(cwd) -> Option<String> (tmux-web/src/echo_host.rs) — вычисляет ярлык проекта (scope правил фичи «Следующий шаг») по cwd сессии: 1) git -C cwd rev-parse --show-toplevel → git-корень (сессии в одном репо, в т.ч. разные подкаталоги, делят ярлык → общие правила); 2) если не git-репо — сам cwd (изоляция по директории); 3) пустой cwd → None (глобальный scope). Используется в idle_sessions для заполнения IdleSession.project_id.
