# codex_memory::MemoryFile

DTO for one editable Markdown file. name is a slash-separated key relative to CODEX_HOME with memories/ or agent-memory/ prefix; content is its original UTF-8 body. Returned files sort lexicographically. UI submits the same key to PUT, so duplicate MEMORY.md basenames from different sub-agents remain distinct.
