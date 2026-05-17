# plugins/echo/src/ws/protocol.rs::ServerMsg

Сериализуется как serde-tagged enum (tag='type', snake_case). Варианты: assistant_chunk{run_id, kind, delta} — стрим-чанк; assistant_done{run_id, usage, message_id} — финал ответа; action_buttons{message_id, actions} — Phase 5 stub; notification{level, title, body} — Phase 5 stub; stats_update{tokens_in_per_min, tokens_out_per_min} — sparkline; autonomous_task_event{task_id, run_id, status, message_preview?} — Phase 4 stub; error{code, message}; ping — heartbeat. Поля ChunkKind=text/thinking/tool_use, NotificationLevel=info/warn/error.
