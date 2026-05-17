# plugins/echo/src/scheduler/runner.rs

Исполнитель одного autonomous-run'а — async-функция run_task(state, host, task) -> anyhow::Result<()>.

# Жизненный цикл одного run'а

1. autonomous::insert_run(db, task.id, now) → создаёт TaskRun со status='running'.
2. autonomous::set_next_run(task.id, now + interval) — сдвигает заранее, чтобы следующий tick не счёл эту задачу due пока она исполняется (даже при interval < длительности run'а). При ошибке внутри run этот next_run_at сохраняется — защита от hot-loop.
3. broadcast ServerMsg::AutonomousTaskEvent { status: 'running' }.
4. ensure_autonomous_conversation — get_or_create chat_session с детерминированным id '__autonomous__/<task_id>' (через chats::create_with_id).
5. prompt_builder::build(task.prompt_template, CtxOpts, host, db) — собирает финальный prompt с capture-pane сессий и memories. project_id из task передаётся в CtxOpts.
6. ClaudeRunner::one_shot(RunRequest { prompt, model: Some(task.model), run_id: 'autonomous:<run_id>' }).
7. messages::insert(conv_id, 'assistant', text, ..., usage) — записывает ответ в служебную conversation.
8. autonomous::finish_run(run_id, 'success', message_id, tokens_in, tokens_out, None).
9. stats::add_tokens(now, tokens_in, tokens_out, cache_creation, cache_read) — minute-bucket агрегация.
10. broadcast ServerMsg::AutonomousTaskEvent { status: 'success', message_preview: first 200 chars }.

# Error path

finish_with_error: любая ошибка на шагах 4-7 → finish_run(status='error', error=Some(msg)), broadcast event со status='error' и preview ошибки. next_run_at УЖЕ сдвинут в шаге 2, поэтому задача не залипает.

# Хелперы

- autonomous_conversation_id(task_id) -> 'autonomous/<task_id>' — детерминированный id служебной conversation.
- ensure_autonomous_conversation — get-or-create через chats::get + chats::create_with_id.
- preview_text(s, n) — обрезает текст по UTF-8 границам с многоточием.

# Unit-тесты

- run_task_success_writes_assistant_msg_and_finishes_run — happy path; проверяет TaskRun.status=success, messages в conversation, token_stats bucket, broadcast running+success events.
- run_task_advances_next_run_at_on_success — next_run_at сдвигается на interval.
- run_task_records_error_when_cli_missing — missing CLI → finish_run(error), next_run_at всё равно сдвинут.
- autonomous_conversation_is_reused_across_runs — два run'а пишут в одну conversation.
