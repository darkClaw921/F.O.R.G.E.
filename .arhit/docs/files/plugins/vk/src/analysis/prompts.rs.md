# plugins/vk/src/analysis/prompts.rs

Пер-чатовое хранилище пользовательского контекста для анализа чата VK.

ФАЙЛ: путь из VkConfig.prompts_path (по умолчанию ~/.forge/vk-chat-prompts.json, env-override FORGE_VK_PROMPTS_PATH). Формат: {"version":1,"chats":{"2000000001":{"prompt":"…","updated_at":1800000000}}}. Ключ — dialog_id СТРОКОЙ (инвариант «id — строки» из ws/protocol.rs). Плоская мапа на BTreeMap: правка одного чата не переписывает соседей, порядок ключей в файле стабилен (иначе каждое сохранение давало бы перетасованный, нечитаемый diff).

КОНСТАНТЫ: PROMPTS_VERSION=1 (чужая версия ⇒ читаем как пустой store, молча интерпретировать формат будущего нельзя); MAX_PROMPT_CHARS=4000 в CODE POINTS; MAX_CHATS=500 (вытеснение по updated_at, тай-брейк по ключу); MAX_FILE_BYTES=4 МиБ (проверка ДО чтения в память).

ТИПЫ: ChatPrompt{prompt, updated_at}; PromptStore{version, chats: BTreeMap<String, ChatPrompt>} с методами get / text / set(dialog_id, raw, now) -> Option<ChatPrompt> / приватными evict и normalize.

load(&Path) -> PromptStore НИКОГДА не возвращает Err: нет файла / stat-ошибка / не JSON / чужая version / размер больше MAX_FILE_BYTES ⇒ warn-лог и пустой store. Образец поведения — config::read_table. Сломанный файл не имеет права ломать вкладку. После разбора вызывается normalize: sanitize каждой заметки, отбрасывание пустых, вытеснение сверх MAX_CHATS (файл могли править руками).

sanitize(&str) -> String: CRLF и одиночный CR → \n; управляющие символы кроме \n и \t выброшены; trim; обрезка по code points через chars().take(). Байтовый срез паниковал бы на кириллице и emoji — для панели, где половина заметок по-русски, это падение хендлера на обычном вводе. Пустой результат означает удаление записи (PromptStore::set).

save(&Path, &PromptStore) -> anyhow::Result<()> — атомарно через config::write_bytes_atomic (tmp рядом → fsync → rename → 0600). Права 0600, потому что это заметки пользователя о конкретных ЛЮДЯХ. При ошибке tmp убирается, старое содержимое цело.

ИНВАРИАНТ УТЕЧКИ: в tracing уходят только пути, размеры и количества — текст заметки не логируется никогда.

ПОТРЕБИТЕЛИ: state::VkState.prompts (Mutex<PromptStore>, синхронная загрузка в конструкторе), routes::analysis (GET/PUT /api/vk/dialogs/:id/prompt), сборщик промпта (следующая фаза).

Тесты модуля (10): round-trip, сохранность соседей, четыре режима отказа load, обрезка кириллицы/emoji, нормализация переводов строк, удаление пустой записью, вытеснение по MAX_CHATS, нормализация правленого руками файла, права 0600 и отсутствие tmp после успеха И после неудачи.
