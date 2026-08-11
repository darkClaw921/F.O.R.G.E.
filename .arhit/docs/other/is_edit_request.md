# is_edit_request

Опознаёт строку-заголовок prompt'а Claude Code на изменение файла (tmux-web/src/auto_approve.rs). Формы перечислены явно: «do you want to make this edit to», «do you want to create», «do you want to overwrite», «do you want to write to», «do you want to apply this edit to»; сравнение по префиксу в нижнем регистре. Явный список, а не префикс «Do you want to», намеренно: им же начинается «Do you want to proceed?» — заголовок ИМЕННО bash-запроса, и считать его «команды нет» было бы дырой в блоклисте. Используется в danger_scope.
