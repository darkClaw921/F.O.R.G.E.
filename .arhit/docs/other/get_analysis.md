# get_analysis

GET /api/vk/dialogs/:id/analysis (plugins/vk/src/routes/analysis.rs) — последний сохранённый разбор беседы.

Лестница КАК У /prompt: авторизация (409 not_authorized) → resolve_peer (404 unknown_dialog). require_ready НЕТ: разбор лежит в локальном файле, и 503 на его чтение прятал бы разбор ровно тогда, когда сеть отвалилась. Тест saved_analysis_routes_answer_200_with_a_dead_network.

Ответ SavedAnalysisResp { text, last_message_id: Option<i32>, messages_used, updated_at: Option<i64>, max_chars }. Разбора нет — 200 с пустым текстом и null-полями, а НЕ 404: «ещё не разбирали» это нормальное состояние. max_chars = MAX_ANALYSIS_CHARS приезжает с сервера, чтобы фронт не хранил копию константы.
