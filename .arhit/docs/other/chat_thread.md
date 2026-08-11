# chat_thread

VkState::chat_thread(dialog_id) -> Vec<QaTurn> (plugins/vk/src/state.rs) — снимок сохранённого треда вопросов беседы, а не ссылка: держать Mutex хранилища на время рендера ответа значило бы сериализовать все запросы вкладки на одном локальном файле. Зовётся из handler'а GET /api/vk/dialogs/:id/analysis рядом с chat_analysis.
