# codex_memory::open_memory_file

Opens an existing regular file under an already-open Codex-home descriptor. name must have passed valid_name or bounded enumeration; write selects O_WRONLY rather than O_RDONLY. Each intermediate component uses openat O_DIRECTORY and O_NOFOLLOW with owned descriptors, while final open uses O_NOFOLLOW and O_NONBLOCK, then verifies regular-file metadata. No creation or truncation happens here. The pinned descriptor chain prevents symlink swaps from escaping memory sources.
