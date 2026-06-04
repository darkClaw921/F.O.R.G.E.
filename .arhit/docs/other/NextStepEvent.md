# NextStepEvent

Вариант ServerMsg::NextStepEvent{session:String, has_suggestion:bool} (plugins/echo/src/ws/protocol.rs), broadcast WS-событие фичи «Следующий шаг». Сериализуется serde-tagged как {type:next_step_event, session, has_suggestion}. has_suggestion=true — для сессии появилось предложение (воркер сгенерировал); false — снято (send/dismiss/feedback или сессия снова активна). Фронтенд по событию перефетчивает GET /api/echo/next-steps и обновляет голубое свечение/попап сессии.
