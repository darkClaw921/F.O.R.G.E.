# plugins/claude-runner/src/lib.rs::RunRequest

Запрос на запуск Claude CLI. Переехал вместе с раннером в forge-claude-runner; Echo видит его как crate::claude::RunRequest через re-export.

Поля: prompt (полный, уже собранный вызывающей стороной; в Echo — prompt_builder), model (Option<String> -> --model), system (Option<String> -> --append-system-prompt), run_id (RunId = String, ключ для cancel), prompt_via_stdin (bool).

prompt_via_stdin — ЕДИНСТВЕННОЕ функциональное отличие от версии, жившей в Echo. false (дефолт, поведение Echo не изменилось) — prompt уходит последним positional-аргументом argv, stdin процесса = Stdio::null(). true — промпта в argv нет вообще, stdin = Stdio::piped(), prompt пишется туда целиком и пишущий конец закрывается shutdown(). Мотивация: чат-транскрипт на десятки тысяч символов в argv виден в ps aux любому локальному пользователю и упирается в ARG_MAX.

Две ловушки stdin-режима: (1) писать ОБЯЗАТЕЛЬНО отдельной таской — на промпте больше pipe-буфера write_all блокируется до вычитывания дочерним процессом, а тот пишет в stdout, который мы ещё не читаем (взаимная блокировка); (2) shutdown() обязателен — без EOF на stdin claude -p не начинает генерацию.

RunRequest::new(prompt) — конструктор с авто-UUID; with_prompt_via_stdin(bool) — билдер; impl Default существует ради ..Default::default() в струкnbsp-литералах вызывающих плагинов. Сборка argv вынесена в чистую build_args(&RunRequest) -> Vec<String>, чтобы тест мог напрямую проверить инвариант «промпта в argv нет».
