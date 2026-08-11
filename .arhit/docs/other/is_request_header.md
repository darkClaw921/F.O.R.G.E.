# is_request_header

auto_approve::is_request_header(cleaned: &str) -> bool — распознаёт строку-заголовок блока запроса Claude Code: 'Bash command…' или 'Claude in Chrome…'. Используется в extract_command_region (верхняя граница области блоклиста) и extract_subject (граница сути запроса для истории апрувов).

Почему по префиксу, а не по равенству: prompt, инициированный СУБ-АГЕНТОМ, подписан источником — 'Bash command · from the run-phase agent'. Прежнее точное сравнение (cleaned == "Bash command") такой заголовок не находило → extract_command_region возвращал None → is_dangerous откатывался на проверку ВСЕЙ панели → в прокрутке выше находился 'rm -rf' из вывода предыдущей команды → каждый запрос суб-агента молча уходил в Decision::Blocked. То есть авто-апрув не работал именно там, где он и нужен: в фоновом окне (живой кейс — worktree-окно wt:1785950679 сессии 765, запрос 'pkill -f fakevk.py' от run-phase).

Связи: вызывается из extract_command_region и extract_subject; вместе со stability_region закрывает две независимые причины «авто-апрув не срабатывает в wt-окнах».

Тесты: subagent_header_scopes_blocklist_to_its_own_block (регион содержит pkill и НЕ содержит rm -rf, decide → Press("2")), subagent_header_bounds_subject.
