# codex_memory::save_into

Edits one existing Markdown file under explicit Codex home. Validates source-qualified key and one MiB content limit before opening, uses no-follow descriptor traversal, checks regular-file type, truncates and writes UTF-8 bytes. No directories/files are created. Invalid or unsafe keys map to HTTP 400, missing file/home to 404, oversized body to 413, other I/O errors to 500. It never consults project paths or writes actual fixture-external memory in tests.
