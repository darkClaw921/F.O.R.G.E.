# codex_memory::limits

On-demand memory reads support only memories and agent-memory sources, at most 1024 Markdown files and eight nested directory levels, one MiB per file and 16 MiB total. These limits protect the interactive server from unusually large generated stores; CodexMemory.truncated informs users when content is omitted. Writes are limited to one MiB and existing regular files only.
