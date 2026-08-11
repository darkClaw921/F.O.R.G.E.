# SavedAnalysisResp

SavedAnalysisResp (plugins/vk/src/routes/analysis.rs) — ответ GET /api/vk/dialogs/:id/analysis: сохранённый разбор беседы VK ПЛЮС тред вопросов.

Поля: text (пустая строка — разбора нет), last_message_id (Option, точка отсчёта REFRESH), messages_used, updated_at (Option), max_chars (= MAX_ANALYSIS_CHARS, приезжает с сервера, чтобы фронт не хранил свою копию константы) и thread: Vec<SavedTurnResp> = [{q, a, at}], от старых пар к новым.

Форма «пусто» — не 404 и не null-тело, а та же структура с пустым текстом и ПУСТЫМ МАССИВОМ треда: попап открывается до того, как пользователь хоть раз разбирал беседу или задавал вопрос, и ошибка здесь была бы враньём о нормальном состоянии. Тред отдаётся массивом всегда (в отличие от файла, где пустое поле опускается) — фронту проще безусловно его перебрать.

Собирается функцией saved_resp(analysis, thread) из снимков VkState::chat_analysis и VkState::chat_thread.
