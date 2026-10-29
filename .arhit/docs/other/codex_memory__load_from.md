# codex_memory::load_from

Loads memory for an explicit root, enabling isolated tests without HOME mutation. Opens the home descriptor, enumerates both supported source trees and reads UTF-8 regular Markdown using open_memory_file. Missing home yields a valid empty result; disappearing/unsafe paths are skipped, other I/O failures propagate. Per-file one MiB and combined 16 MiB limits set truncated rather than loading oversized content. Native memories/MEMORY.md fills the optional index; remaining keys preserve their source directory.
