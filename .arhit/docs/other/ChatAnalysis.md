# ChatAnalysis

Сохранённый разбор беседы VK (plugins/vk/src/analysis/prompts.rs).

Поля: text (текст разбора, прошедший sanitize_analysis), last_message_id (максимальный id разобранного сообщения — точка отсчёта инкрементального обновления), messages_used (сколько сообщений было в транскрипте), updated_at (unix).

Живёт ВНУТРИ ChatPrompt полем analysis: Option<ChatAnalysis> в файле ~/.forge/vk-chat-prompts.json. Версия формата остаётся 1: поле опциональное и помечено skip_serializing_if, поэтому старые файлы (без analysis) читаются и переписываются без потерь — тест an_old_file_without_analysis_round_trips.

Пишется ТОЛЬКО пампером analysis::hub по успешно завершённому run'у вида analyze/refresh. При отмене, таймауте и ошибке НЕ пишется: оборванный текст стал бы «прошлым разбором» и отравил бы следующий REFRESH (промпт Task::Refresh кладёт его в <previous_analysis>).

Потолок текста — MAX_ANALYSIS_CHARS = 12 000 code points. Из-за него MAX_FILE_BYTES поднят до 40 МиБ: худший случай 500 чатов × (4 000 + 12 000) символов × 4 байта = 32 МБ, и прежние 4 МиБ означали бы, что полное хранилище молча читается как пустое (тест the_file_cap_covers_the_worst_case).

Операции хранилища: PromptStore::analysis / set_analysis / clear_analysis; через состояние — VkState::chat_analysis / set_chat_analysis / clear_chat_analysis (мапа и файл под одним Mutex). Заметка и разбор независимы: стирание одного не уносит другое, запись без обеих половин удаляется целиком.
