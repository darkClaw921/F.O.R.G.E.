# clear_chat_thread

VkState::clear_chat_thread(dialog_id) -> anyhow::Result<bool> (plugins/vk/src/state.rs) — стирает тред вопросов беседы и СРАЗУ сохраняет файл. Мапа и файл меняются под ОДНИМ lock'ом (та же дисциплина, что у set_chat_prompt): иначе две вкладки могли бы записать файлы в порядке, обратном порядку правок. Ни заметку, ни разбор не трогает. Возвращает, был ли тред непустым; ошибка — только запись файла, наверх она уходит как 500 storage_failed. Зовётся из handler'а delete_thread (DELETE /api/vk/dialogs/:id/analysis/thread).
