# codex_memory::CodexMemory

Global memory response: dir identifies CODEX_HOME, exists means an allowed source directory exists, scope is global. index_name is memories/MEMORY.md only when that native index was read, otherwise null; index contains its body. files includes remaining Markdown in both source trees. truncated warns when enumeration, depth, per-file or aggregate read limits were reached. Missing home/store returns an empty valid DTO.
