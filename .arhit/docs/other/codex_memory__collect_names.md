# codex_memory::collect_names

Enumerates only memories and agent-memory subtrees under root, skipping symlinks, non-regular files, non-Markdown names and invalid Unicode keys. Mutates DTO exists and truncated flags, bounds nested depth and number of candidates, and returns source-qualified sorted keys. Disk contents are later reopened using descriptor-relative no-follow access rather than trusting enumeration.
