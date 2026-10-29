# agents::codex_terminal::matches

Binds the ephemeral native name into a read-only SQL SELECT projecting id and cwd from threads.name. Compares canonical cwd and retains all matching native IDs so duplicated names cannot silently choose a session. Query row-limit overflow, legacy schema missing name, locking and parse errors supply no evidence. Prompt-bearing title/first_user_message columns are not queried.
