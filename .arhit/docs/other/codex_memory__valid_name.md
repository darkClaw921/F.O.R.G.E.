# codex_memory::valid_name

Validates root-relative API file keys before any file write: source must be memories or agent-memory, at least one filename follows, path has bounded depth, components are nonempty and neither dot nor dot-dot, backslashes and NUL are forbidden, suffix must be .md case-insensitively. Absolute/traversal/non-source keys fail. Directory symlink validation is additionally enforced at descriptor open.
