# clear_thread

PromptStore::clear_thread(dialog_id) -> bool (plugins/vk/src/analysis/prompts.rs) — кнопка «CLEAR Q&A» на уровне хранилища.

Стирает ТОЛЬКО тред беседы. Заметку (prompt) и разбор (analysis) не трогает; запись, в которой не осталось ни одной из трёх половин, удаляется целиком, чтобы не занимать слот вытеснения MAX_CHATS ничем.

Возвращает true, если тред действительно был непустым. Пустой тред и неизвестная беседа — не ошибка: кнопку жмут повторно и из двух вкладок сразу.

Зеркальный близнец clear_analysis, который так же не трогает заметку и тред. Независимость обеих очисток — главное функциональное требование доработки, закрытое тестами на двух уровнях: PromptStore (the_note_the_analysis_and_the_thread_are_independent, the_thread_alone_keeps_the_entry_alive) и HTTP после round-trip через файл (the_two_clears_never_touch_each_other_nor_the_note).

Кто зовёт: VkState::clear_chat_thread, который под ОДНИМ Mutex меняет мапу и сразу сохраняет файл; выше — handler delete_thread роута DELETE /api/vk/dialogs/:id/analysis/thread.
