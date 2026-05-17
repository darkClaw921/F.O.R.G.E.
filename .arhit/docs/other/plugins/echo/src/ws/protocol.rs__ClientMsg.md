# plugins/echo/src/ws/protocol.rs::ClientMsg

Сериализуется как serde-tagged enum (tag='type', snake_case). Варианты: user_message{text, conversation_id, model?, ctx_opts?} — юзер послал сообщение; cancel{run_id} — прервать run; action_invoke{action_id, params?} — Phase 5 stub; pong — ответ на heartbeat. Round-trip протестировано.
