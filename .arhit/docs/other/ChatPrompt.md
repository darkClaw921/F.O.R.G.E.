# ChatPrompt

Запись одной беседы VK в хранилище ~/.forge/vk-chat-prompts.json (plugins/vk/src/analysis/prompts.rs). ТРИ независимые половины:
- prompt: String — заметка пользователя к беседе (подмешивается в промпт анализа), sanitize, потолок MAX_PROMPT_CHARS = 4 000 символов; пустая строка = «заметки нет»;
- analysis: Option<ChatAnalysis> — последний УСПЕШНЫЙ разбор (текст, last_message_id как точка отсчёта REFRESH, messages_used, updated_at), skip_serializing_if = Option::is_none;
- thread: Vec<QaTurn> — сохранённый тред вопросов Q&A, от старых к новым, skip_serializing_if = Vec::is_empty;
плюс updated_at: i64 — ось вытеснения по MAX_CHATS и «сохранено тогда-то» для фронта; двигают его set, set_analysis и push_turn.

Оба skip_serializing_if — не косметика: без них у беседы, где есть только заметка, появились бы "analysis": null и "thread": [], и файл, написанный до соответствующей доработки, перестал бы совпадать сам с собой после первой же перезаписи. Версия формата остаётся PROMPTS_VERSION = 1, оба поля опциональны, миграции нет — проверено round-trip-тестом на старом файле и живой проверкой на реальном инстансе.

ИНВАРИАНТ НЕЗАВИСИМОСТИ (главное требование доработки «сохраняемый тред»): правка или очистка любой из трёх половин не трогает две другие. set() не трогает analysis и thread, set_analysis() — prompt и thread, push_turn() — prompt и analysis, clear_analysis() — prompt и thread, clear_thread() — prompt и analysis. Запись живёт, пока жива хотя бы одна половина, и удаляется целиком, когда пусты все три.
