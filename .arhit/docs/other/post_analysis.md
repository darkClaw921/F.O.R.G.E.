# post_analysis

POST /api/vk/dialogs/:id/analysis (plugins/vk/src/routes/analysis.rs) — запустить разбор, обновление или вопрос.

kind: analyze | refresh | ask. Порядок отказов: require_ready (503) → resolve_peer (404) → тело и kind (400) → идущий run по беседе (attached) → наличие Claude CLI (409 claude_unavailable) → свободный слот (409 invalid_state) → выгрузка истории → старт.

kind=refresh:
* требует сохранённого ChatAnalysis, иначе 400 invalid_input («сначала выполните полный разбор») — build_task(req, saved);
* контекст берётся через context::fetch_since(since_id = saved.last_message_id), то есть выгружаются ТОЛЬКО новые сообщения;
* промпт — Task::Refresh { previous } с блоком <previous_analysis>; модель обязана вернуть цельный разбор, он заменит прошлый;
* новых сообщений нет (transcript.messages_used == 0) → модель НЕ запускается, ответ 200 {run_id: null, no_new_messages: true, messages_used: 0, truncated: false, attached: false}. Это штатный ответ, а не ошибка.

AnalysisResp.run_id — Option<String>: null бывает только в случае no_new_messages. Поле truncated складывает обрезку выгрузки (fetch_since) и обрезку транскрипта по символам.

В StartRequest уезжает newest_id транскрипта — по нему пампер hub сохраняет ChatAnalysis при успешном завершении.
